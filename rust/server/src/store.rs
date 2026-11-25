//! Everything cwbcode keeps is in SQLite, in two places with the same schema:
//!
//!   ~/.cwbcode/cwbcode.db            the server's own: the projects it has opened and the
//!                                    tools the agent has made (they work in every project)
//!   <project>/.cwbcode/cwbcode.db    one per project: its conversations (the model's
//!                                    messages and an append-only log of every event), every
//!                                    version of every file the agent read or changed, the
//!                                    knowledge base (documents, chunks, an FTS5 index for
//!                                    BM25, one embedding per chunk) and stored files and
//!                                    images, byte for byte

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use cwbcode_proto::{Change, Event};
use sha2::{Digest, Sha256};

use crate::prompt::Message;

pub struct Store {
    conn: Connection,
    pub home: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Doc {
    pub id: i64,
    /// "text", "file" or "image".
    pub kind: String,
    pub title: String,
    pub source: String,
    pub text: String,
    pub blob: Option<i64>,
    pub created: String,
}

#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: i64,
    pub doc: i64,
    pub seq: i64,
    pub text: String,
    pub title: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: i64,
    pub name: String,
    pub cwd: String,
    pub updated: String,
    pub messages: i64,
    pub first: String,
}

#[derive(Debug, Clone)]
pub struct SavedTool {
    pub name: String,
    pub description: String,
    pub parameters: String,
    pub source: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS blobs (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL, mime TEXT NOT NULL, data BLOB NOT NULL,
    sha TEXT NOT NULL DEFAULT '',
    created TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE INDEX IF NOT EXISTS blobs_sha ON blobs(sha);
CREATE TABLE IF NOT EXISTS projects (
    path TEXT PRIMARY KEY, opened TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL, body BLOB NOT NULL,
    created TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE INDEX IF NOT EXISTS events_session ON events(session);
CREATE TABLE IF NOT EXISTS file_versions (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn INTEGER NOT NULL, path TEXT NOT NULL, action TEXT NOT NULL,
    before INTEGER REFERENCES blobs(id), after INTEGER REFERENCES blobs(id),
    undone INTEGER NOT NULL DEFAULT 0,
    created TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE INDEX IF NOT EXISTS file_versions_session ON file_versions(session);
CREATE TABLE IF NOT EXISTS docs (
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, source TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL, blob INTEGER REFERENCES blobs(id),
    created TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY, doc INTEGER NOT NULL REFERENCES docs(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL, text TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS chunks_doc ON chunks(doc);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(text, title, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE IF NOT EXISTS vectors (
    chunk INTEGER PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    embedder TEXT NOT NULL, vec BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT '', cwd TEXT NOT NULL,
    todos TEXT NOT NULL DEFAULT '[]',
    created TEXT NOT NULL DEFAULT (datetime('now','localtime')),
    updated TEXT NOT NULL DEFAULT (datetime('now','localtime')));
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    body TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS messages_session ON messages(session);
CREATE TABLE IF NOT EXISTS tools (
    name TEXT PRIMARY KEY, description TEXT NOT NULL, parameters TEXT NOT NULL, source TEXT NOT NULL,
    updated TEXT NOT NULL DEFAULT (datetime('now','localtime')));
";

/// The name of a project's own folder, and of the server's under the home folder.
pub const DIR: &str = ".cwbcode";

/// What kind of event this is, as the log names it.
fn kind(event: &Event) -> &'static str {
    use cwbcode_proto::What::*;
    match &event.what {
        Session(_) => "session",
        User(_) => "user",
        Thinking(_) => "thinking",
        Text(_) => "text",
        EndMessage => "end",
        ToolStart { .. } => "tool_start",
        ToolEnd { .. } => "tool_end",
        Note(_) => "note",
        Warning(_) => "warning",
        Error(_) => "error",
        Approval { .. } => "approval",
        Usage(_) => "usage",
        Todos(_) => "todos",
        Done => "done",
        Answered(_) => "answered",
    }
}

/// Where the server keeps its files: $CWBCODE_HOME, or ~/.cwbcode.
pub fn default_home() -> PathBuf {
    match std::env::var_os("CWBCODE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cwbcode"),
    }
}

impl Store {
    pub fn open(home: &Path) -> Result<Self> {
        std::fs::create_dir_all(home).with_context(|| format!("cannot create {}", home.display()))?;
        // Conversations and file contents are in here: for the owner only.
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700));
        }
        let conn = Connection::open(home.join("cwbcode.db"))?;
        Self::init(conn, home)
    }

    /// A project's store if it has one: asking about a folder does not make records in it.
    pub fn open_project_existing(project: &Path) -> Result<Option<Self>> {
        if project.join(DIR).join("cwbcode.db").exists() { Self::open(&project.join(DIR)).map(Some) } else { Ok(None) }
    }

    /// A project's own store, in its `.cwbcode` folder (made on first use, and kept out of
    /// git by an ignore file of its own).
    pub fn open_project(project: &Path) -> Result<Self> {
        let dir = project.join(DIR);
        if !dir.exists() {
            std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
            std::fs::write(dir.join(".gitignore"), "# cwbcode's data for this project: conversations, file versions, knowledge base\n*\n")?;
        }
        Self::open(&dir)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!("cwbcode-test-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&home)?;
        Self::init(Connection::open_in_memory()?, &home)
    }

    fn init(conn: Connection, home: &Path) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn, home: home.to_path_buf() })
    }

    // ---------- blobs ----------

    pub fn put_blob(&self, name: &str, mime: &str, data: &[u8]) -> Result<i64> {
        self.conn.execute("INSERT INTO blobs (name, mime, data) VALUES (?1, ?2, ?3)", params![name, mime, data])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// (name, mime, bytes)
    pub fn blob(&self, id: i64) -> Result<(String, String, Vec<u8>)> {
        self.conn
            .query_row("SELECT name, mime, data FROM blobs WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
            .with_context(|| format!("no stored file #{id}"))
    }

    /// The blob as a file (the image encoder reads files): written once under the cache folder.
    pub fn blob_file(&self, id: i64) -> Result<PathBuf> {
        let (name, _, data) = self.blob(id)?;
        let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let dir = self.home.join("cache");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("blob-{id}.{ext}"));
        if std::fs::metadata(&path).map(|m| m.len() != data.len() as u64).unwrap_or(true) {
            std::fs::write(&path, data)?;
        }
        Ok(path)
    }

    /// Store bytes once: the same content under the same name is the same blob.
    pub fn put_blob_once(&self, name: &str, data: &[u8]) -> Result<i64> {
        let sha = format!("{:x}", Sha256::digest(data));
        if let Some(id) = self.conn.query_row("SELECT id FROM blobs WHERE sha = ?1 AND name = ?2", params![sha, name], |r| r.get(0)).optional()? {
            return Ok(id);
        }
        self.conn.execute("INSERT INTO blobs (name, mime, data, sha) VALUES (?1, 'application/octet-stream', ?2, ?3)", params![name, data, sha])?;
        Ok(self.conn.last_insert_rowid())
    }

    // ---------- projects (the server's store) ----------

    pub fn touch_project(&self, path: &str) -> Result<()> {
        self.conn.execute("INSERT INTO projects (path) VALUES (?1) ON CONFLICT(path) DO UPDATE SET opened = datetime('now','localtime')", [path])?;
        Ok(())
    }

    /// (path, last opened), most recent first.
    pub fn projects(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, opened FROM projects ORDER BY opened DESC, path")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn forget_project(&self, path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM projects WHERE path = ?1", [path])?;
        Ok(())
    }

    // ---------- the log of everything that happened ----------

    /// Append an event to a conversation's log. The log is never rewritten: it outlives the
    /// summarizing of the model's context.
    pub fn log_event(&self, session: i64, event: &Event) -> Result<()> {
        self.conn.execute("INSERT INTO events (session, kind, body) VALUES (?1, ?2, ?3)", params![session, kind(event), event.encode()])?;
        Ok(())
    }

    pub fn events(&self, session: i64) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare("SELECT body, created FROM events WHERE session = ?1 ORDER BY id")?;
        let rows = stmt.query_map([session], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (body, created) = row?;
            let mut event = Event::decode(&body).map_err(|e| anyhow::anyhow!("conversation #{session} holds an event this version cannot read: {e}"))?;
            event.created = created;
            out.push(event);
        }
        Ok(out)
    }

    /// The number of the user's messages so far: the turn that file versions belong to.
    pub fn turn(&self, session: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM events WHERE session = ?1 AND kind = 'user'", [session], |r| r.get(0))?)
    }

    // ---------- every version of every file the agent touched ----------

    /// Record a file as the agent read it (`after`), or as it was before and after a change.
    /// `before` is None for a file that did not exist.
    pub fn record_file(&self, session: i64, path: &str, action: &str, before: Option<&[u8]>, after: Option<&[u8]>) -> Result<i64> {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let blob = |data: Option<&[u8]>| -> Result<Option<i64>> { data.map(|d| self.put_blob_once(&name, d)).transpose() };
        let (before, after) = (blob(before)?, blob(after)?);
        self.conn.execute(
            "INSERT INTO file_versions (session, turn, path, action, before, after) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session, self.turn(session)?, path, action, before, after],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn changes(&self, session: i64) -> Result<Vec<Change>> {
        let mut stmt = self.conn.prepare(
            "SELECT v.id, v.path, v.action, coalesce((SELECT length(data) FROM blobs WHERE id = v.before), -1),
                    coalesce((SELECT length(data) FROM blobs WHERE id = v.after), -1), v.created, v.undone
             FROM file_versions v WHERE v.session = ?1 ORDER BY v.id",
        )?;
        let rows = stmt.query_map([session], |r| Ok(Change { id: r.get(0)?, path: r.get(1)?, action: r.get(2)?, bytes_before: r.get(3)?, bytes_after: r.get(4)?, created: r.get(5)?, undone: r.get::<_, i64>(6)? != 0 }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The changes of the latest turn that has changes not yet undone, newest first:
    /// (version id, path, the file's bytes before the change or None if it did not exist,
    /// its bytes after the change).
    #[allow(clippy::type_complexity)]
    pub fn changes_to_undo(&self, session: i64) -> Result<Vec<(i64, String, Option<Vec<u8>>, Option<Vec<u8>>)>> {
        let turn: Option<i64> = self.conn.query_row("SELECT max(turn) FROM file_versions WHERE session = ?1 AND undone = 0 AND action != 'read'", [session], |r| r.get(0))?;
        let Some(turn) = turn else { return Ok(vec![]) };
        let mut stmt = self.conn.prepare(
            "SELECT v.id, v.path, (SELECT data FROM blobs WHERE id = v.before), (SELECT data FROM blobs WHERE id = v.after) FROM file_versions v
             WHERE v.session = ?1 AND v.turn = ?2 AND v.undone = 0 AND v.action != 'read' ORDER BY v.id DESC",
        )?;
        let rows = stmt.query_map(params![session, turn], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_undone(&self, version: i64) -> Result<()> {
        self.conn.execute("UPDATE file_versions SET undone = 1 WHERE id = ?1", [version])?;
        Ok(())
    }

    // ---------- the model's task list ----------

    pub fn set_todos(&self, session: i64, json: &str) -> Result<()> {
        self.conn.execute("UPDATE sessions SET todos = ?2 WHERE id = ?1", params![session, json])?;
        Ok(())
    }

    pub fn todos(&self, session: i64) -> Result<String> {
        Ok(self.conn.query_row("SELECT todos FROM sessions WHERE id = ?1", [session], |r| r.get(0)).optional()?.unwrap_or_else(|| "[]".into()))
    }

    // ---------- the knowledge base ----------

    pub fn add_doc(&self, kind: &str, title: &str, source: &str, text: &str, blob: Option<i64>) -> Result<i64> {
        self.conn.execute("INSERT INTO docs (kind, title, source, text, blob) VALUES (?1, ?2, ?3, ?4, ?5)", params![kind, title, source, text, blob])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_chunk(&self, doc: i64, seq: i64, text: &str, title: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO chunks (doc, seq, text) VALUES (?1, ?2, ?3)", params![doc, seq, text])?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute("INSERT INTO chunks_fts (rowid, text, title) VALUES (?1, ?2, ?3)", params![id, text, title])?;
        Ok(id)
    }

    pub fn set_vector(&self, chunk: i64, embedder: &str, vec: &[f32]) -> Result<()> {
        let bytes: Vec<u8> = vec.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.conn.execute("INSERT OR REPLACE INTO vectors (chunk, embedder, vec) VALUES (?1, ?2, ?3)", params![chunk, embedder, bytes])?;
        Ok(())
    }

    /// Every chunk's embedding made by `embedder`.
    pub fn vectors(&self, embedder: &str) -> Result<Vec<(i64, Vec<f32>)>> {
        let mut stmt = self.conn.prepare("SELECT chunk, vec FROM vectors WHERE embedder = ?1")?;
        let rows = stmt.query_map([embedder], |r| {
            let bytes: Vec<u8> = r.get(1)?;
            Ok((r.get(0)?, bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Chunks that have no embedding by `embedder` yet.
    pub fn chunks_without_vector(&self, embedder: &str) -> Result<Vec<Chunk>> {
        self.chunks_where("c.id NOT IN (SELECT chunk FROM vectors WHERE embedder = ?1)", params![embedder])
    }

    pub fn chunk(&self, id: i64) -> Result<Option<Chunk>> {
        Ok(self.chunks_where("c.id = ?1", params![id])?.pop())
    }

    fn chunks_where(&self, condition: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Chunk>> {
        let sql = format!("SELECT c.id, c.doc, c.seq, c.text, d.title, d.kind FROM chunks c JOIN docs d ON d.id = c.doc WHERE {condition} ORDER BY c.id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(args, |r| Ok(Chunk { id: r.get(0)?, doc: r.get(1)?, seq: r.get(2)?, text: r.get(3)?, title: r.get(4)?, kind: r.get(5)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The best chunks for an FTS5 query by BM25 (titles count double), best first.
    pub fn bm25(&self, fts_query: &str, limit: usize) -> Result<Vec<(i64, f64)>> {
        if fts_query.is_empty() {
            return Ok(vec![]);
        }
        let mut stmt = self.conn.prepare("SELECT rowid, bm25(chunks_fts, 1.0, 2.0) AS score FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY score LIMIT ?2")?;
        let rows = stmt.query_map(params![fts_query, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn doc_from(r: &rusqlite::Row) -> rusqlite::Result<Doc> {
        Ok(Doc { id: r.get(0)?, kind: r.get(1)?, title: r.get(2)?, source: r.get(3)?, text: r.get(4)?, blob: r.get(5)?, created: r.get(6)? })
    }

    pub fn doc(&self, id: i64) -> Result<Doc> {
        self.conn
            .query_row("SELECT id, kind, title, source, text, blob, created FROM docs WHERE id = ?1", [id], Self::doc_from)
            .optional()?
            .with_context(|| format!("the knowledge base has no entry #{id}"))
    }

    /// All entries, newest first, without their text.
    pub fn docs(&self) -> Result<Vec<Doc>> {
        let mut stmt = self.conn.prepare("SELECT id, kind, title, source, substr(text, 1, 200), blob, created FROM docs ORDER BY id DESC")?;
        let rows = stmt.query_map([], Self::doc_from)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_doc(&self, id: i64) -> Result<()> {
        let doc = self.doc(id)?;
        self.conn.execute("DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE doc = ?1)", [id])?;
        self.conn.execute("DELETE FROM docs WHERE id = ?1", [id])?;
        if let Some(blob) = doc.blob {
            self.conn.execute("DELETE FROM blobs WHERE id = ?1", [blob])?;
        }
        Ok(())
    }

    /// (entries, chunks, embedded chunks, bytes of stored files)
    pub fn counts(&self) -> Result<(i64, i64, i64, i64)> {
        let one = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok((one("SELECT count(*) FROM docs")?, one("SELECT count(*) FROM chunks")?, one("SELECT count(*) FROM vectors")?, one("SELECT coalesce(sum(length(data)), 0) FROM blobs")?))
    }

    pub fn meta(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &[u8]) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    // ---------- conversations ----------

    pub fn new_session(&self, cwd: &str) -> Result<i64> {
        self.conn.execute("INSERT INTO sessions (cwd) VALUES (?1)", [cwd])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn append_message(&self, session: i64, message: &Message) -> Result<()> {
        self.conn.execute("INSERT INTO messages (session, body) VALUES (?1, ?2)", params![session, message.to_json().to_string()])?;
        self.conn.execute("UPDATE sessions SET updated = datetime('now','localtime') WHERE id = ?1", [session])?;
        Ok(())
    }

    /// Replace a conversation's messages (after it was summarized).
    pub fn replace_messages(&mut self, session: i64, messages: &[Message]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM messages WHERE session = ?1", [session])?;
        for m in messages {
            tx.execute("INSERT INTO messages (session, body) VALUES (?1, ?2)", params![session, m.to_json().to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn has_session(&self, session: i64) -> Result<bool> {
        Ok(self.conn.query_row("SELECT 1 FROM sessions WHERE id = ?1", [session], |_| Ok(())).optional()?.is_some())
    }

    pub fn name_session(&self, session: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE sessions SET name = ?2 WHERE id = ?1", params![session, name])?;
        Ok(())
    }

    /// A conversation by number or by name (the most recent of that name).
    pub fn find_session(&self, key: &str) -> Result<i64> {
        let found: Option<i64> = match key.parse::<i64>() {
            Ok(id) => self.conn.query_row("SELECT id FROM sessions WHERE id = ?1", [id], |r| r.get(0)).optional()?,
            Err(_) => self.conn.query_row("SELECT id FROM sessions WHERE name = ?1 ORDER BY updated DESC, id DESC LIMIT 1", [key], |r| r.get(0)).optional()?,
        };
        found.with_context(|| format!("no saved conversation {key:?} (see /sessions)"))
    }

    /// The most recent conversation with messages, started in `cwd`.
    pub fn last_session(&self, cwd: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT s.id FROM sessions s WHERE s.cwd = ?1 AND EXISTS (SELECT 1 FROM messages m WHERE m.session = s.id) ORDER BY s.updated DESC, s.id DESC LIMIT 1",
                [cwd],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn session_messages(&self, session: i64) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare("SELECT body FROM messages WHERE session = ?1 ORDER BY id")?;
        let rows = stmt.query_map([session], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for body in rows {
            let value: serde_json::Value = serde_json::from_str(&body?)?;
            match Message::from_json(&value) {
                Some(m) => out.push(m),
                None => bail!("conversation #{session} holds a message this version cannot read"),
            }
        }
        Ok(out)
    }

    /// Conversations with messages, most recent first.
    pub fn sessions(&self, limit: usize) -> Result<Vec<SessionInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.name, s.cwd, s.updated, (SELECT count(*) FROM messages m WHERE m.session = s.id) AS n,
                    coalesce((SELECT body FROM messages m WHERE m.session = s.id ORDER BY m.id LIMIT 1), '')
             FROM sessions s WHERE n > 0 ORDER BY s.updated DESC, s.id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| Ok(SessionInfo { id: r.get(0)?, name: r.get(1)?, cwd: r.get(2)?, updated: r.get(3)?, messages: r.get(4)?, first: r.get(5)? }))?;
        let mut out: Vec<SessionInfo> = rows.collect::<rusqlite::Result<_>>()?;
        for s in &mut out {
            let first: serde_json::Value = serde_json::from_str(&s.first).unwrap_or_default();
            s.first = first["text"].as_str().unwrap_or("").chars().take(60).collect::<String>().replace('\n', " ");
        }
        Ok(out)
    }

    // ---------- tools made at runtime ----------

    pub fn save_tool(&self, tool: &SavedTool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tools (name, description, parameters, source) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET description = ?2, parameters = ?3, source = ?4, updated = datetime('now','localtime')",
            params![tool.name, tool.description, tool.parameters, tool.source],
        )?;
        Ok(())
    }

    pub fn tools(&self) -> Result<Vec<SavedTool>> {
        let mut stmt = self.conn.prepare("SELECT name, description, parameters, source FROM tools ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok(SavedTool { name: r.get(0)?, description: r.get(1)?, parameters: r.get(2)?, source: r.get(3)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tool(&self, name: &str) -> Result<Option<SavedTool>> {
        Ok(self.tools()?.into_iter().find(|t| t.name == name))
    }

    pub fn delete_tool(&self, name: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM tools WHERE name = ?1", [name])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_found_by_bm25_and_removed_with_their_index() {
        let s = Store::in_memory().unwrap();
        let doc = s.add_doc("text", "Deploy notes", "", "whole text", None).unwrap();
        let a = s.add_chunk(doc, 0, "The pipeline pushes a Docker image to the registry.", "Deploy notes").unwrap();
        let b = s.add_chunk(doc, 1, "서울의 지하철 2호선은 순환선이다.", "Deploy notes").unwrap();
        assert_eq!(s.bm25("\"docker\"*", 5).unwrap()[0].0, a);
        // A Korean word is found by its stem: the query term is a prefix of "지하철 2호선은".
        assert_eq!(s.bm25("\"2호선\"*", 5).unwrap()[0].0, b);
        s.set_vector(a, "e1", &[1.0, -2.5]).unwrap();
        assert_eq!(s.vectors("e1").unwrap(), vec![(a, vec![1.0, -2.5])]);
        assert_eq!(s.chunks_without_vector("e1").unwrap().iter().map(|c| c.id).collect::<Vec<_>>(), vec![b]);
        assert_eq!(s.counts().unwrap().0, 1);
        s.delete_doc(doc).unwrap();
        assert!(s.bm25("\"docker\"*", 5).unwrap().is_empty());
        assert!(s.vectors("e1").unwrap().is_empty());
    }

    #[test]
    fn a_conversation_is_saved_named_and_read_back() {
        let mut s = Store::in_memory().unwrap();
        let id = s.new_session("/work").unwrap();
        assert_eq!(s.last_session("/work").unwrap(), None);
        let user = Message::User { text: "hello there".into(), images: vec![] };
        s.append_message(id, &user).unwrap();
        s.name_session(id, "greeting").unwrap();
        assert_eq!(s.find_session("greeting").unwrap(), id);
        assert_eq!(s.find_session(&id.to_string()).unwrap(), id);
        assert_eq!(s.last_session("/work").unwrap(), Some(id));
        assert_eq!(s.session_messages(id).unwrap(), vec![user.clone()]);
        assert_eq!(s.sessions(10).unwrap()[0].first, "hello there");
        s.replace_messages(id, &[user.clone(), user.clone()]).unwrap();
        assert_eq!(s.session_messages(id).unwrap().len(), 2);
        assert!(s.find_session("nope").is_err());
    }

    #[test]
    fn every_event_and_file_version_is_kept_and_a_turn_can_be_undone() {
        use cwbcode_proto::What;
        let mut s = Store::in_memory().unwrap();
        let id = s.new_session("/work").unwrap();
        let log = |s: &Store, what: What| s.log_event(id, &what.into()).unwrap();
        log(&s, What::User("first".into()));
        s.record_file(id, "a.txt", "read", None, Some(b"one")).unwrap();
        s.record_file(id, "a.txt", "edit", Some(b"one"), Some(b"two")).unwrap();
        log(&s, What::User("second".into()));
        s.record_file(id, "a.txt", "edit", Some(b"two"), Some(b"three")).unwrap();
        s.record_file(id, "new.txt", "create", None, Some(b"hello")).unwrap();
        log(&s, What::ToolStart { name: "bash".into(), summary: "bash(ls)".into(), args: "{}".into() });
        // The log reads back in order, with its times, and survives the summarizing of the
        // model's messages.
        s.replace_messages(id, &[]).unwrap();
        let events = s.events(id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].what, What::User("second".into()));
        assert!(!events[0].created.is_empty());
        assert_eq!(s.turn(id).unwrap(), 2);
        // The same content is stored once.
        assert_eq!(s.put_blob_once("a.txt", b"two").unwrap(), s.put_blob_once("a.txt", b"two").unwrap());
        let changes = s.changes(id).unwrap();
        assert_eq!(changes.iter().map(|c| (c.action.as_str(), c.bytes_before, c.bytes_after)).collect::<Vec<_>>(), [("read", -1, 3), ("edit", 3, 3), ("edit", 3, 5), ("create", -1, 5)]);
        // Undo takes the latest turn first, newest change first, and gives the bytes back.
        let undo = s.changes_to_undo(id).unwrap();
        assert_eq!(undo.iter().map(|u| (u.1.as_str(), u.2.as_deref())).collect::<Vec<_>>(), [("new.txt", None), ("a.txt", Some(&b"two"[..]))]);
        for (version, ..) in &undo {
            s.mark_undone(*version).unwrap();
        }
        let undo = s.changes_to_undo(id).unwrap();
        assert_eq!(undo.iter().map(|u| (u.1.as_str(), u.2.as_deref())).collect::<Vec<_>>(), [("a.txt", Some(&b"one"[..]))]);
        s.mark_undone(undo[0].0).unwrap();
        assert!(s.changes_to_undo(id).unwrap().is_empty());
        s.set_todos(id, "[1]").unwrap();
        assert_eq!(s.todos(id).unwrap(), "[1]");
    }

    #[test]
    fn projects_are_remembered_and_have_their_own_ignored_folder() {
        let s = Store::in_memory().unwrap();
        s.touch_project("/Users/x/a").unwrap();
        s.touch_project("/Users/x/b").unwrap();
        s.touch_project("/Users/x/a").unwrap();
        assert_eq!(s.projects().unwrap().len(), 2);
        s.forget_project("/Users/x/b").unwrap();
        assert_eq!(s.projects().unwrap().len(), 1);
        let project = s.home.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::open_project(&project).unwrap();
        assert_eq!(store.home, project.join(".cwbcode"));
        assert!(std::fs::read_to_string(project.join(".cwbcode/.gitignore")).unwrap().ends_with("*\n"));
        assert!(project.join(".cwbcode/cwbcode.db").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(project.join(".cwbcode")).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn blobs_and_tools_round_trip() {
        let s = Store::in_memory().unwrap();
        let id = s.put_blob("a.png", "image/png", &[1, 2, 3]).unwrap();
        assert_eq!(s.blob(id).unwrap(), ("a.png".to_string(), "image/png".to_string(), vec![1, 2, 3]));
        let path = s.blob_file(id).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3]);
        assert!(path.to_string_lossy().ends_with(".png"));
        let tool = SavedTool { name: "wc".into(), description: "count".into(), parameters: "{}".into(), source: "fn run".into() };
        s.save_tool(&tool).unwrap();
        s.save_tool(&SavedTool { description: "count words".into(), ..tool.clone() }).unwrap();
        assert_eq!(s.tools().unwrap().len(), 1);
        assert_eq!(s.tool("wc").unwrap().unwrap().description, "count words");
        assert!(s.delete_tool("wc").unwrap());
    }
}
