//! The agent's tools: files, search, commands, images, the knowledge base, and the tools it
//! makes for itself (dyntool.rs). None of them reaches the network.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::dyntool;
use crate::kb;
use crate::llm::Engine;
use crate::prompt::{Call, ImageRef, ToolSpec};
use crate::sandbox::{Action, Policy, run};
use crate::store::Store;

/// Most characters of a tool's result given to the model.
pub const MAX_RESULT_CHARS: usize = 12_000;
const MAX_READ_LINES: usize = 400;
const MAX_LINE_CHARS: usize = 400;
const MAX_MATCHES: usize = 120;
const MAX_ENTRIES: usize = 300;
/// A command's changes to files are versioned (and can be undone) when the project is at most
/// this many files outside build folders, of at most this size each and in total.
const SNAPSHOT_FILES: usize = 4000;
const SNAPSHOT_FILE_BYTES: u64 = 256 << 10;
const SNAPSHOT_TOTAL_BYTES: u64 = 48 << 20;
/// Stands for the description of the sandbox in a tool's description (see `confinement`).
const SANDBOXED: &str = "{sandbox}";
const SKIP_DIRS: [&str; 10] = [".cwbcode", ".git", "target", "node_modules", ".venv", "venv", "__pycache__", "dist", "build", ".next"];

pub struct Ctx<'a> {
    pub policy: &'a mut Policy,
    /// The project's store: its conversations, file versions and knowledge base.
    pub store: &'a Store,
    /// The server's store: the tools made at runtime.
    pub global: &'a Store,
    /// The conversation the calls belong to.
    pub session: i64,
    /// Things to show the user besides the result (the task list).
    pub events: Vec<cwbcode_proto::What>,
    /// The model, for embeddings and image descriptions (absent in tests).
    pub engine: Option<&'a Engine>,
    /// True once the user pressed Ctrl-C.
    pub stop: &'a dyn Fn() -> bool,
}

pub struct ToolOutput {
    pub text: String,
    pub images: Vec<ImageRef>,
    pub ok: bool,
}

fn spec(name: &str, description: &str, properties: Value, required: &[&str]) -> ToolSpec {
    ToolSpec { name: name.to_string(), description: description.to_string(), parameters: json!({"type": "object", "properties": properties, "required": required}) }
}

/// The built-in tools.
pub fn builtin() -> Vec<ToolSpec> {
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    let b = |d: &str| json!({"type": "boolean", "description": d});
    vec![
        spec("read_file", "Read a text file. Returns its lines with line numbers. Long files are returned in parts: use offset and limit.",
            json!({"path": s("File path, relative to the workspace or absolute"), "offset": n("First line to read, from 1"), "limit": n("Number of lines (default 400)")}), &["path"]),
        spec("write_file", "Create a file or replace its whole contents. Parent folders are created.",
            json!({"path": s("File path"), "content": s("The full new contents")}), &["path", "content"]),
        spec("edit_file", "Replace an exact piece of text in a file. old_string must match the file exactly (including indentation) and occur once, unless replace_all is true. Read the file first.",
            json!({"path": s("File path"), "old_string": s("Text to replace"), "new_string": s("Replacement text"), "replace_all": b("Replace every occurrence")}), &["path", "old_string", "new_string"]),
        spec("list_dir", "List a folder: names, with / after folders, and file sizes.", json!({"path": s("Folder (default: the workspace)")}), &[]),
        spec("glob", "Find files by name pattern, newest first. * matches within a name, ** across folders, ? one character. Example: **/*.rs",
            json!({"pattern": s("Glob pattern"), "path": s("Folder to search (default: the workspace)")}), &["pattern"]),
        spec("grep", "Search file contents with a regular expression. Returns file:line:text for each match.",
            json!({"pattern": s("Regular expression"), "path": s("File or folder to search (default: the workspace)"), "glob": s("Only files whose path matches this glob, e.g. *.rs"), "ignore_case": b("Case-insensitive")}), &["pattern"]),
        spec("bash", &format!("Run a shell command (zsh) in the workspace and return its output and exit status. {SANDBOXED} Do not create or edit files with it (no heredocs, redirection or sed): use write_file and edit_file."),
            json!({"command": s("The command"), "timeout": n("Seconds before the command is stopped (default 120, at most 600)")}), &["command"]),
        spec("view_image", "Look at an image file (PNG, JPEG, WebP, GIF, BMP). The image is shown to you in the result.", json!({"path": s("Image file path")}), &["path"]),
        spec("memory_save", "Save a note in the local knowledge base (SQLite) so it can be found in later conversations: facts, decisions, how-tos, preferences.",
            json!({"title": s("Short title"), "text": s("The text to remember")}), &["title", "text"]),
        spec("memory_save_file", "Save a file in the local knowledge base: text files are indexed for search, images are stored with a description, other files are stored as they are.",
            json!({"path": s("File path"), "title": s("Title (default: the file name)")}), &["path"]),
        spec("memory_search", "Search the local knowledge base with keywords (BM25) and meaning (semantic) together. Returns the best entries with their id.",
            json!({"query": s("What to look for"), "limit": n("Number of results (default 5)")}), &["query"]),
        spec("memory_get", "Get a knowledge base entry in full by id: a note or file's text, or an image (shown to you).", json!({"id": n("Entry id")}), &["id"]),
        spec("make_tool", &format!("Make a new tool for yourself by writing it in Rust. It is compiled, saved for later sessions, and can then be called with run_tool. Use it when a task needs a capability the other tools lack or a step you will repeat. {}", dyntool::CONTRACT),
            json!({"tool": s("Name for the new tool: lowercase letters, digits and _"), "description": s("What the tool does and when to use it"), "parameters": {"type": "object", "description": "JSON Schema of the tool's arguments object"}, "source": s("The Rust source defining fn run")}),
            &["tool", "description", "parameters", "source"]),
        spec("run_tool", "Call a tool made with make_tool.", json!({"tool": s("The made tool's name"), "input": {"type": "object", "description": "The arguments for that tool"}}), &["tool", "input"]),
        spec("list_tools", "List the tools made with make_tool, with their descriptions and arguments.", json!({}), &[]),
        spec("todo_write", "Write or update your task list for the work at hand, and show it to the user. Use it for work of three steps or more: list the steps first, mark one in_progress before you start it and completed as soon as it is done. Send the whole list each time.",
            json!({"todos": {"type": "array", "description": "The whole list, in order", "items": {"type": "object", "properties": {"content": s("The step, in a few words"), "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}}, "required": ["content", "status"]}}}), &["todos"]),
    ]
}

/// How commands are confined, as the model is told.
pub fn confinement(policy: &Policy) -> &'static str {
    match (policy.sandbox, policy.network) {
        (true, false) => "Commands run in a sandbox: no network access, and files can be written only inside the workspace and temporary folders.",
        (true, true) => "Commands run in a sandbox: files can be written only inside the workspace and temporary folders. The network is open to commands.",
        (false, _) => "Commands are NOT sandboxed in this session: they run with the user's full access, and the user approves each one first.",
    }
}

/// The built-in tools and the ones made in earlier sessions (declared under their own names).
pub fn specs(global: &Store, policy: &Policy) -> Vec<ToolSpec> {
    let mut all = builtin();
    for tool in &mut all {
        tool.description = tool.description.replace(SANDBOXED, confinement(policy));
    }
    for tool in global.tools().unwrap_or_default() {
        if all.iter().any(|t| t.name == tool.name) {
            continue;
        }
        all.push(ToolSpec {
            name: tool.name,
            description: format!("{} (made with make_tool)", tool.description),
            parameters: serde_json::from_str(&tool.parameters).unwrap_or_else(|_| json!({"type": "object"})),
        });
    }
    all
}

/// Keep the head and the tail of a long text.
pub fn clip(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars * 2 / 3).collect();
    let tail: String = text.chars().skip(count - max_chars / 3).collect();
    format!("{head}\n... [{} characters left out] ...\n{tail}", count - max_chars)
}

/// One line that says what a call does, for the terminal.
pub fn summary(call: &Call) -> String {
    let a = &call.args;
    let short = |v: &Value, n: usize| -> String {
        let s = v.as_str().map(String::from).unwrap_or_else(|| if v.is_null() { String::new() } else { v.to_string() });
        let s = s.replace('\n', "⏎");
        if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s }
    };
    let detail = match call.name.as_str() {
        "bash" => short(&a["command"], 120),
        "read_file" | "write_file" | "edit_file" | "list_dir" | "view_image" | "memory_save_file" => short(&a["path"], 100),
        "glob" | "grep" => format!("{}{}", short(&a["pattern"], 80), a["path"].as_str().map(|p| format!(" in {p}")).unwrap_or_default()),
        "memory_save" => short(&a["title"], 80),
        "memory_search" => short(&a["query"], 80),
        "memory_get" => short(&a["id"], 20),
        "todo_write" => format!("{} items", a["todos"].as_array().map_or(0, |t| t.len())),
        "make_tool" => short(&a["tool"], 40),
        "run_tool" => format!("{} {}", short(&a["tool"], 40), short(&a["input"], 80)),
        _ => short(a, 100),
    };
    format!("{}({detail})", call.name)
}

/// Run a tool call. Errors become the result's text, for the model to act on.
pub fn execute(ctx: &mut Ctx, call: &Call) -> ToolOutput {
    if let Some(error) = &call.error {
        return ToolOutput { text: format!("Error: {error}"), images: vec![], ok: false };
    }
    let mut images = Vec::new();
    match dispatch(ctx, call, &mut images) {
        Ok(text) => ToolOutput { text: clip(&text, MAX_RESULT_CHARS), images, ok: true },
        Err(e) => ToolOutput { text: clip(&format!("Error: {e:#}"), MAX_RESULT_CHARS), images: vec![], ok: false },
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key].as_str().with_context(|| format!("missing string argument \"{key}\""))
}

fn arg_int(args: &Value, key: &str) -> Option<i64> {
    args[key].as_i64().or_else(|| args[key].as_f64().map(|f| f as i64)).or_else(|| args[key].as_str().and_then(|s| s.trim().parse().ok()))
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args[key].as_bool().unwrap_or_else(|| args[key].as_str() == Some("true"))
}

fn dispatch(ctx: &mut Ctx, call: &Call, images: &mut Vec<ImageRef>) -> Result<String> {
    let a = &call.args;
    match call.name.as_str() {
        "read_file" => read_file(ctx, arg_str(a, "path")?, arg_int(a, "offset"), arg_int(a, "limit")),
        "write_file" => write_file(ctx, arg_str(a, "path")?, arg_str(a, "content")?),
        "edit_file" => edit_file(ctx, arg_str(a, "path")?, arg_str(a, "old_string")?, arg_str(a, "new_string")?, arg_bool(a, "replace_all")),
        "list_dir" => list_dir(ctx, a["path"].as_str().unwrap_or("")),
        "glob" => glob(ctx, arg_str(a, "pattern")?, a["path"].as_str().unwrap_or("")),
        "grep" => grep(ctx, arg_str(a, "pattern")?, a["path"].as_str().unwrap_or(""), a["glob"].as_str(), arg_bool(a, "ignore_case")),
        "bash" => bash(ctx, arg_str(a, "command")?, arg_int(a, "timeout")),
        "view_image" => {
            let path = ctx.policy.check_read(arg_str(a, "path")?)?;
            let (blob, mime, _) = kb::store_file(ctx.store, &path)?;
            if !kb::is_image(mime) {
                bail!("{} is not an image (it is {mime}); use read_file for text", ctx.policy.display(&path));
            }
            let name = ctx.policy.display(&path);
            images.push(ImageRef { blob, name: name.clone() });
            Ok(format!("The image {name} follows."))
        }
        "memory_save" => {
            let id = kb::save(ctx.store, ctx.engine, "text", arg_str(a, "title")?, "", arg_str(a, "text")?, None)?;
            Ok(format!("Saved as entry #{id}."))
        }
        "memory_save_file" => {
            let path = ctx.policy.check_read(arg_str(a, "path")?)?;
            let (id, what) = save_file(ctx.store, ctx.engine.context("the model is not loaded")?, &path, a["title"].as_str())?;
            Ok(format!("Saved {} as entry #{id} ({what}).", ctx.policy.display(&path)))
        }
        "memory_search" => {
            let limit = arg_int(a, "limit").unwrap_or(5).clamp(1, 20) as usize;
            Ok(kb::format_hits(&kb::search(ctx.store, ctx.engine, arg_str(a, "query")?, limit)?, 700))
        }
        "memory_get" => {
            let doc = ctx.store.doc(arg_int(a, "id").context("missing integer argument \"id\"")?)?;
            let mut out = format!("#{} {} \"{}\" (saved {}){}\n\n", doc.id, doc.kind, doc.title, doc.created, if doc.source.is_empty() { String::new() } else { format!(" from {}", doc.source) });
            if doc.kind == "image" {
                if let Some(blob) = doc.blob {
                    images.push(ImageRef { blob, name: doc.title.clone() });
                    out.push_str("The image follows. Its saved description: ");
                }
            }
            out.push_str(&doc.text);
            Ok(out)
        }
        "make_tool" => {
            let (name, source) = (arg_str(a, "tool").or_else(|_| arg_str(a, "name"))?, arg_str(a, "source")?);
            let description = arg_str(a, "description")?;
            if builtin().iter().any(|t| t.name == name) {
                bail!("\"{name}\" is a built-in tool; choose another name");
            }
            // The arguments' schema may arrive as an object or as its JSON text.
            let parameters = match &a["parameters"] {
                Value::String(s) => serde_json::from_str(s).context("\"parameters\" is not valid JSON")?,
                Value::Null => json!({"type": "object", "properties": {}}),
                other => other.clone(),
            };
            let preview: String = source.lines().take(40).map(|l| format!("    {l}\n")).collect();
            let more = source.lines().count().saturating_sub(40);
            ctx.policy.ask(Action::MakeTool, &format!("Make the tool \"{name}\": {description}\n{preview}{}", if more > 0 { format!("    ... {more} more lines\n") } else { String::new() }))?;
            dyntool::make(ctx.policy, ctx.global, name, description, &parameters, source)?;
            Ok(format!("The tool \"{name}\" is compiled and saved. Call it with run_tool, tool \"{name}\" and its arguments as input."))
        }
        "run_tool" => {
            let name = arg_str(a, "tool").or_else(|_| arg_str(a, "name"))?;
            let args = match a.get("input").or_else(|| a.get("args")).unwrap_or(&Value::Null) {
                Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| json!({"input": s})),
                Value::Null => json!({}),
                other => other.clone(),
            };
            run_made(ctx, name, &args)
        }
        "list_tools" => {
            let tools = ctx.global.tools()?;
            if tools.is_empty() {
                return Ok("No tools have been made yet. Make one with make_tool.".into());
            }
            Ok(tools.iter().map(|t| format!("{}: {}\n  arguments: {}", t.name, t.description, t.parameters)).collect::<Vec<_>>().join("\n"))
        }
        // A tool made at runtime, called by its own name.
        "todo_write" => todo_write(ctx, &a["todos"]),
        name if ctx.global.tool(name)?.is_some() => run_made(ctx, name, a),
        name => bail!("there is no tool named \"{name}\". The tools are: {}", specs(ctx.global, ctx.policy).iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

fn run_made(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    ctx.policy.ask(Action::Command, &format!("Run the tool {name} with {}", clip(&args.to_string(), 300)))?;
    let before = snapshot(ctx.policy);
    let result = dyntool::call(ctx.policy, ctx.global, name, args, ctx.stop);
    record_command_changes(ctx, before)?;
    let out = result?;
    Ok(if out.is_empty() { "(the tool printed nothing)".into() } else { out })
}

/// Save a file in the knowledge base. Returns the entry id and what was made of the file.
pub fn save_file(store: &Store, engine: &Engine, path: &Path, title: Option<&str>) -> Result<(i64, String)> {
    let (blob, mime, data) = kb::store_file(store, path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = title.filter(|t| !t.is_empty()).unwrap_or(&name);
    let source = path.display().to_string();
    if kb::is_image(mime) {
        let description = crate::agent::describe_image(engine, path)?;
        let id = kb::save(store, Some(engine), "image", title, &source, &description, Some(blob))?;
        return Ok((id, format!("image, described as: {}", clip(&description, 200))));
    }
    match String::from_utf8(data) {
        Ok(text) => {
            let id = kb::save(store, Some(engine), "file", title, &source, &text, Some(blob))?;
            let count = text.chars().count();
            let note = if count > kb::MAX_INDEX_CHARS { format!("text, {count} characters stored, the first {} indexed for search", kb::MAX_INDEX_CHARS) } else { format!("text, {count} characters indexed") };
            Ok((id, note))
        }
        Err(e) => {
            let id = kb::save(store, Some(engine), "file", title, &source, &format!("{title} ({mime}, {} bytes; not text, stored as it is)", e.as_bytes().len()), Some(blob))?;
            Ok((id, format!("{mime}, stored as it is; found by its title")))
        }
    }
}

/// The task list as the user sees it.
pub fn render_todos(todos: &Value) -> String {
    todos
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|t| {
                    let mark = match t["status"].as_str() {
                        Some("completed") => "[x]",
                        Some("in_progress") => "[~]",
                        _ => "[ ]",
                    };
                    format!("{mark} {}", t["content"].as_str().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn todo_write(ctx: &mut Ctx, todos: &Value) -> Result<String> {
    // The list may arrive as an array or as its JSON text.
    let todos: Value = match todos {
        Value::String(s) => serde_json::from_str(s).context("\"todos\" is not valid JSON")?,
        other => other.clone(),
    };
    let items = todos.as_array().context("\"todos\" must be an array of {content, status}")?;
    for item in items {
        if item["content"].as_str().is_none_or(|c| c.trim().is_empty()) || !matches!(item["status"].as_str(), Some("pending" | "in_progress" | "completed")) {
            bail!("every item needs a \"content\" and a \"status\" of pending, in_progress or completed");
        }
    }
    ctx.store.set_todos(ctx.session, &todos.to_string())?;
    ctx.events.push(cwbcode_proto::What::Todos(todos.to_string()));
    let open = items.iter().filter(|t| t["status"] != "completed").count();
    Ok(format!("The task list is saved ({} of {} left):\n{}", open, items.len(), render_todos(&todos)))
}

/// The project's files as they are (path in the project -> contents), for telling what a
/// command changed. None when the project is too large for that (see SNAPSHOT_FILES).
fn snapshot(policy: &Policy) -> Option<std::collections::HashMap<String, Vec<u8>>> {
    let mut files = std::collections::HashMap::new();
    let (mut total, mut fits) = (0u64, true);
    walk(&policy.workspace, |path, rel| {
        let size = std::fs::metadata(path).map_or(u64::MAX, |m| m.len());
        // A large file is left out on its own; too many files, and nothing is versioned.
        if size <= SNAPSHOT_FILE_BYTES {
            total += size;
            if files.len() >= SNAPSHOT_FILES || total > SNAPSHOT_TOTAL_BYTES {
                fits = false;
                return false;
            }
            if let Ok(data) = std::fs::read(path) {
                files.insert(rel.to_string(), data);
            }
        }
        true
    });
    fits.then_some(files)
}

/// Record what a command changed: every file it created, changed or deleted, with the
/// contents before and after. Returns how many files that was.
fn record_command_changes(ctx: &Ctx, before: Option<std::collections::HashMap<String, Vec<u8>>>) -> Result<usize> {
    let (Some(before), Some(after)) = (before, snapshot(ctx.policy)) else { return Ok(0) };
    let mut paths: Vec<&String> = before.keys().chain(after.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut changed = 0;
    for path in paths {
        let (was, is) = (before.get(path), after.get(path));
        if was == is {
            continue;
        }
        let action = match (was, is) {
            (None, _) => "create",
            (_, None) => "delete",
            _ => "edit",
        };
        ctx.store.record_file(ctx.session, path, action, was.map(|d| d.as_slice()), is.map(|d| d.as_slice()))?;
        changed += 1;
    }
    Ok(changed)
}

/// Undo the file changes of the latest turn that has any: every file goes back to what it
/// was before the agent's change (a file it created is removed). A file that is no longer as
/// the agent left it (edited since, by hand or by a command) is left alone: undoing would
/// throw that later work away. Returns what was done with each file.
pub fn undo(policy: &Policy, store: &Store, session: i64) -> Result<Vec<String>> {
    let mut done = Vec::new();
    for (version, shown, before, after) in store.changes_to_undo(session)? {
        let path = policy.resolve(&shown);
        if !policy.in_workspace(&path) {
            bail!("{shown} is outside the workspace; it was not restored");
        }
        let now = std::fs::read(&path).ok();
        if now != after {
            done.push(format!("left {shown} alone: it has changed since the agent wrote it"));
        } else {
            match before {
                Some(bytes) => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, bytes)?;
                    done.push(format!("restored {shown}"));
                }
                None => {
                    std::fs::remove_file(&path)?;
                    done.push(format!("removed {shown} (it did not exist before)"));
                }
            }
        }
        // Either way this change is dealt with: the next undo goes to the turn before.
        store.mark_undone(version)?;
    }
    Ok(done)
}

fn read_text(path: &Path) -> Result<String> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
    if meta.is_dir() {
        bail!("{} is a folder; use list_dir", path.display());
    }
    if meta.len() > 32 << 20 {
        bail!("{} is {} MB, too large to read whole; use grep, or bash with head/sed", path.display(), meta.len() >> 20);
    }
    let data = std::fs::read(path)?;
    if image::guess_format(&data).is_ok() {
        bail!("{} is an image; use view_image", path.display());
    }
    String::from_utf8(data).map_err(|_| anyhow::anyhow!("{} is not text (binary file)", path.display()))
}

fn read_file(ctx: &mut Ctx, path: &str, offset: Option<i64>, limit: Option<i64>) -> Result<String> {
    let path = ctx.policy.check_read(path)?;
    let text = read_text(&path)?;
    // What the agent read is kept, as it was when it read it.
    ctx.store.record_file(ctx.session, &ctx.policy.display(&path), "read", None, Some(text.as_bytes()))?;
    let total = text.lines().count();
    let first = offset.unwrap_or(1).max(1) as usize;
    let limit = limit.unwrap_or(MAX_READ_LINES as i64).clamp(1, 2000) as usize;
    if total == 0 {
        return Ok("(the file is empty)".into());
    }
    if first > total {
        bail!("the file has {total} lines; offset {first} is past the end");
    }
    let mut out = String::new();
    let mut last = first - 1;
    for (i, line) in text.lines().enumerate().skip(first - 1).take(limit) {
        let shown: String = line.chars().take(MAX_LINE_CHARS).collect();
        let cut = if line.chars().count() > MAX_LINE_CHARS { " …[line cut]" } else { "" };
        out.push_str(&format!("{:>5}\t{shown}{cut}\n", i + 1));
        last = i + 1;
        if out.len() > MAX_RESULT_CHARS - 200 {
            break;
        }
    }
    if last < total {
        out.push_str(&format!("[lines {first}-{last} of {total}; read on with offset {}]", last + 1));
    }
    Ok(out)
}

fn preview(text: &str, lines: usize, mark: &str) -> String {
    let mut out: String = text.lines().take(lines).map(|l| format!("  {mark} {}\n", l.chars().take(160).collect::<String>())).collect();
    let more = text.lines().count().saturating_sub(lines);
    if more > 0 {
        out.push_str(&format!("  {mark} ... {more} more lines\n"));
    }
    out
}

fn write_file(ctx: &mut Ctx, path: &str, content: &str) -> Result<String> {
    let shown = ctx.policy.display(&ctx.policy.resolve(path));
    let exists = ctx.policy.resolve(path).exists();
    let what = format!("{} {shown} ({} lines)\n{}", if exists { "Overwrite" } else { "Create" }, content.lines().count(), preview(content, 12, "+"));
    let path = ctx.policy.check_write(path, &what)?;
    if path.is_dir() {
        bail!("{shown} is a folder");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Both versions are kept, so the change can be undone.
    let before = if exists { Some(std::fs::read(&path)?) } else { None };
    ctx.store.record_file(ctx.session, &shown, if exists { "write" } else { "create" }, before.as_deref(), Some(content.as_bytes()))?;
    std::fs::write(&path, content)?;
    Ok(format!("{} {shown}: {} lines, {} bytes.", if exists { "Overwrote" } else { "Created" }, content.lines().count(), content.len()))
}

fn edit_file(ctx: &mut Ctx, path: &str, old: &str, new: &str, replace_all: bool) -> Result<String> {
    let resolved = ctx.policy.check_read(path)?;
    let shown = ctx.policy.display(&resolved);
    let text = read_text(&resolved)?;
    if old.is_empty() {
        bail!("old_string is empty; to create a file use write_file");
    }
    if old == new {
        bail!("old_string and new_string are the same");
    }
    let count = text.matches(old).count();
    if count == 0 {
        // Say whether only the whitespace differs: the most common reason.
        let squeeze = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let hint = if squeeze(&text).contains(&squeeze(old)) { " The text is there with different spacing or indentation: copy it exactly as read_file shows it (without the line numbers)." } else { " Read the file again and copy the text exactly." };
        bail!("old_string was not found in {shown}.{hint}");
    }
    if count > 1 && !replace_all {
        bail!("old_string occurs {count} times in {shown}; add surrounding lines to make it unique, or set replace_all");
    }
    let what = format!("Edit {shown}{}\n{}{}", if count > 1 { format!(" ({count} places)") } else { String::new() }, preview(old, 8, "-"), preview(new, 8, "+"));
    let path = ctx.policy.check_write(path, &what)?;
    let updated = if replace_all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    ctx.store.record_file(ctx.session, &shown, "edit", Some(text.as_bytes()), Some(updated.as_bytes()))?;
    std::fs::write(&path, updated)?;
    let line = text[..text.find(old).unwrap_or(0)].matches('\n').count() + 1;
    Ok(format!("Edited {shown}: {count} replacement{} (first at line {line}).", if count == 1 { "" } else { "s" }))
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

fn list_dir(ctx: &mut Ctx, path: &str) -> Result<String> {
    let dir = ctx.policy.check_read(path)?;
    let mut entries: Vec<(bool, String, u64)> = std::fs::read_dir(&dir)
        .with_context(|| format!("cannot list {}", dir.display()))?
        .flatten()
        // cwbcode's own records are not part of the project.
        .filter(|e| e.file_name() != crate::store::DIR)
        .map(|e| {
            let meta = e.metadata().ok();
            (meta.as_ref().is_some_and(|m| m.is_dir()), e.file_name().to_string_lossy().into_owned(), meta.map_or(0, |m| m.len()))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    if entries.is_empty() {
        return Ok(format!("{} is empty.", ctx.policy.display(&dir)));
    }
    let mut out = format!("{}:\n", ctx.policy.display(&dir));
    for (is_dir, name, size) in entries.iter().take(MAX_ENTRIES) {
        out.push_str(&if *is_dir { format!("  {name}/\n") } else { format!("  {name}  ({})\n", human(*size)) });
    }
    if entries.len() > MAX_ENTRIES {
        out.push_str(&format!("  ... {} more\n", entries.len() - MAX_ENTRIES));
    }
    Ok(out)
}

/// Does `path` (with / separators) match the glob? `*` and `?` stay within a name, `**`
/// crosses folders.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn segs(p: &[&str], s: &[&str]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((&"**", rest)) => (0..=s.len()).any(|i| segs(rest, &s[i..])),
            Some((first, rest)) => !s.is_empty() && name(first.as_bytes(), s[0].as_bytes()) && segs(rest, &s[1..]),
        }
    }
    fn name(p: &[u8], s: &[u8]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some((b'*', rest)) => (0..=s.len()).any(|i| name(rest, &s[i..])),
            Some((b'?', rest)) => !s.is_empty() && name(rest, &s[1..]),
            Some((c, rest)) => s.first() == Some(c) && name(rest, &s[1..]),
        }
    }
    let p: Vec<&str> = pattern.split('/').filter(|x| !x.is_empty()).collect();
    let s: Vec<&str> = path.split('/').filter(|x| !x.is_empty()).collect();
    segs(&p, &s)
}

/// Every file under `root` (not following symlinks, skipping build and VCS folders), with
/// its path relative to `root`.
fn walk(root: &Path, mut each: impl FnMut(&Path, &str) -> bool) {
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            let name = entry.file_name();
            if kind.is_dir() {
                if !SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                    stack.push(path);
                }
            } else if kind.is_file() {
                visited += 1;
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
                if !each(&path, &rel) || visited > 200_000 {
                    return;
                }
            }
        }
    }
}

fn glob(ctx: &mut Ctx, pattern: &str, path: &str) -> Result<String> {
    let root = ctx.policy.check_read(path)?;
    // A pattern without a folder part matches names anywhere below.
    let pattern = if pattern.contains('/') { pattern.trim_start_matches("./").to_string() } else { format!("**/{pattern}") };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    walk(&root, |file, rel| {
        if glob_match(&pattern, rel) {
            found.push((file.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), file.to_path_buf()));
        }
        true
    });
    if found.is_empty() {
        return Ok(format!("No files match {pattern} under {}.", ctx.policy.display(&root)));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut out: String = found.iter().take(MAX_ENTRIES).map(|(_, p)| format!("{}\n", ctx.policy.display(p))).collect();
    if found.len() > MAX_ENTRIES {
        out.push_str(&format!("... {} more files; narrow the pattern\n", found.len() - MAX_ENTRIES));
    }
    Ok(out)
}

fn grep(ctx: &mut Ctx, pattern: &str, path: &str, only: Option<&str>, ignore_case: bool) -> Result<String> {
    let root = ctx.policy.check_read(path)?;
    let re = regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build().map_err(|e| anyhow::anyhow!("bad regular expression: {e}"))?;
    let only = only.map(|g| if g.contains('/') { g.to_string() } else { format!("**/{g}") });
    let mut out = String::new();
    let (mut matches, mut files) = (0, 0);
    let mut search = |file: &Path, shown: &str| -> bool {
        if std::fs::metadata(file).map_or(true, |m| m.len() > 4 << 20) {
            return true;
        }
        let Ok(data) = std::fs::read(file) else { return true };
        if data.iter().take(4096).any(|&b| b == 0) {
            return true;
        }
        let text = String::from_utf8_lossy(&data);
        let mut any = false;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                any = true;
                matches += 1;
                if matches > MAX_MATCHES {
                    return false;
                }
                out.push_str(&format!("{shown}:{}:{}\n", i + 1, line.trim_end().chars().take(240).collect::<String>()));
            }
        }
        files += any as usize;
        true
    };
    if root.is_file() {
        search(&root, &ctx.policy.display(&root));
    } else {
        let policy = &*ctx.policy;
        walk(&root, |file, rel| match &only {
            Some(g) if !glob_match(g, rel) => true,
            _ => search(file, &policy.display(file)),
        });
    }
    if matches == 0 {
        return Ok(format!("No matches for /{pattern}/."));
    }
    if matches > MAX_MATCHES {
        out.push_str(&format!("[stopped at {MAX_MATCHES} matches; narrow the pattern or the path]"));
    } else {
        out.push_str(&format!("[{matches} match{} in {files} file{}]", if matches == 1 { "" } else { "es" }, if files == 1 { "" } else { "s" }));
    }
    Ok(out)
}

fn bash(ctx: &mut Ctx, command: &str, timeout: Option<i64>) -> Result<String> {
    ctx.policy.ask(Action::Command, &format!("Run: {command}"))?;
    let timeout = Duration::from_secs(timeout.unwrap_or(120).clamp(1, 600) as u64);
    // What the command does to the project's files is versioned like an edit.
    let before = snapshot(ctx.policy);
    let out = run(ctx.policy.command(Path::new("/bin/zsh"), &["-c", command]), None, timeout, ctx.stop)?;
    record_command_changes(ctx, before)?;
    let mut text = String::new();
    if !out.stdout.is_empty() {
        text.push_str(out.stdout.trim_end());
        text.push('\n');
    }
    if !out.stderr.is_empty() {
        text.push_str(&format!("[stderr]\n{}\n", out.stderr.trim_end()));
    }
    text = clip(&text, MAX_RESULT_CHARS - 300);
    match (out.interrupted, out.timed_out, out.status) {
        (true, _, _) => text.push_str("[interrupted by the user]"),
        (_, true, _) => text.push_str(&format!("[stopped after {} seconds: the command did not finish]", timeout.as_secs())),
        (_, _, Some(0)) => {
            if text.is_empty() {
                text.push_str("(no output)");
            }
        }
        (_, _, Some(code)) => {
            text.push_str(&format!("[exit status {code}]"));
            if ctx.policy.sandbox && (text.contains("Operation not permitted") || text.contains("Could not resolve host") || text.contains("network")) {
                text.push_str("\n[Commands run in a sandbox: no network, and writes only inside the workspace. If that is what stopped this, tell the user; they can rerun cwbcode with --allow-network or --no-sandbox.]");
            }
        }
        (_, _, None) => text.push_str("[the command was killed]"),
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_names_and_folders() {
        for (pattern, path, want) in [
            ("**/*.rs", "src/main.rs", true),
            ("**/*.rs", "main.rs", true),
            ("**/*.rs", "src/a/b/c.rs", true),
            ("**/*.rs", "src/main.rsx", false),
            ("src/*.rs", "src/main.rs", true),
            ("src/*.rs", "src/a/main.rs", false),
            ("src/**/test_?.py", "src/x/y/test_1.py", true),
            ("src/**/test_?.py", "src/x/y/test_10.py", false),
            ("*.toml", "Cargo.toml", true),
            ("*.toml", "rust/Cargo.toml", false),
            ("**", "a/b", true),
            ("docs/**", "docs/a/b.md", true),
            ("한*.txt", "한글.txt", true),
        ] {
            assert_eq!(glob_match(pattern, path), want, "{pattern} vs {path}");
        }
    }

    #[test]
    fn long_text_keeps_its_head_and_tail() {
        assert_eq!(clip("short", 10), "short");
        let clipped = clip(&"a".repeat(50).chars().chain("z".repeat(50).chars()).collect::<String>(), 30);
        assert!(clipped.starts_with("aaaaaaaaaaaaaaaaaaaa\n... [70 characters left out] ...\n") && clipped.ends_with("zzzzzzzzzz"));
        assert!(clip("한글한글한글", 3).contains("left out"));
    }

    /// A workspace in a temporary folder, with tools that ask nobody (auto mode) and no model.
    fn workspace(name: &str) -> (Policy, Store) {
        let base = std::env::temp_dir().join(format!("cwbcode-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws/src")).unwrap();
        let policy = Policy::new(crate::sandbox::Mode::Auto, &base.join("ws"), &base.join("home")).unwrap();
        let store = Store::in_memory().unwrap();
        // Session 1 of the store.
        store.new_session("ws").unwrap();
        (policy, store)
    }

    fn call(policy: &mut Policy, store: &Store, name: &str, args: Value) -> ToolOutput {
        let mut ctx = Ctx { policy, store, global: store, session: 1, events: vec![], engine: None, stop: &|| false };
        execute(&mut ctx, &Call { name: name.into(), args, error: None })
    }

    #[test]
    fn files_are_written_read_edited_and_found() {
        let (mut p, s) = workspace("files");
        let mut go = |name: &str, args: Value| call(&mut p, &s, name, args);
        let made = go("write_file", json!({"path": "src/lib.rs", "content": "fn one() -> u32 {\n    1\n}\n\nfn two() -> u32 {\n    1\n}\n"}));
        assert!(made.ok && made.text.starts_with("Created src/lib.rs: 7 lines"), "{}", made.text);
        assert_eq!(go("read_file", json!({"path": "src/lib.rs", "offset": 5, "limit": 2})).text, "    5\tfn two() -> u32 {\n    6\t    1\n[lines 5-6 of 7; read on with offset 7]");
        // An edit must name one place.
        let twice = go("edit_file", json!({"path": "src/lib.rs", "old_string": "    1\n", "new_string": "    2\n"}));
        assert!(!twice.ok && twice.text.contains("occurs 2 times"), "{}", twice.text);
        let spaced = go("edit_file", json!({"path": "src/lib.rs", "old_string": "fn two()  -> u32 {", "new_string": "x"}));
        assert!(spaced.text.contains("different spacing"), "{}", spaced.text);
        let edited = go("edit_file", json!({"path": "src/lib.rs", "old_string": "fn two() -> u32 {\n    1", "new_string": "fn two() -> u32 {\n    2"}));
        assert!(edited.ok && edited.text.contains("1 replacement (first at line 5)"), "{}", edited.text);
        assert!(go("read_file", json!({"path": "src/lib.rs"})).text.contains("    6\t    2\n"));
        go("write_file", json!({"path": "notes/한글.md", "content": "TODO: call Dana\n"}));
        assert_eq!(go("glob", json!({"pattern": "*.rs"})).text, "src/lib.rs\n");
        assert_eq!(go("glob", json!({"pattern": "notes/*.md"})).text, "notes/한글.md\n");
        let found = go("grep", json!({"pattern": "fn \\w+\\(\\)", "glob": "*.rs"}));
        assert_eq!(found.text, "src/lib.rs:1:fn one() -> u32 {\nsrc/lib.rs:5:fn two() -> u32 {\n[2 matches in 1 file]");
        assert_eq!(go("grep", json!({"pattern": "todo", "ignore_case": true})).text, "notes/한글.md:1:TODO: call Dana\n[1 match in 1 file]");
        assert!(go("grep", json!({"pattern": "("})).text.contains("bad regular expression"));
        let listing = go("list_dir", json!({})).text;
        assert!(listing.contains("  notes/\n  src/\n"), "{listing}");
        // Errors are answers, not crashes.
        assert!(!go("read_file", json!({"path": "missing.txt"})).ok);
        assert!(go("read_file", json!({})).text.contains("missing string argument \"path\""));
        assert!(go("no_such_tool", json!({})).text.contains("there is no tool named"));
        let broken = execute(&mut Ctx { policy: &mut p, store: &s, global: &s, session: 1, events: vec![], engine: None, stop: &|| false }, &Call { name: "grep".into(), args: Value::Null, error: Some("bad JSON".into()) });
        assert_eq!(broken.text, "Error: bad JSON");
    }

    #[test]
    fn every_version_is_kept_and_a_turn_is_undone_byte_for_byte() {
        use cwbcode_proto::What;
        let (mut p, s) = workspace("undo");
        let original = "line one\r\nline two\n\u{feff}끝\n";
        std::fs::write(p.workspace.join("keep.txt"), original).unwrap();
        s.log_event(1, &What::User("first".into()).into()).unwrap();
        assert!(call(&mut p, &s, "read_file", json!({"path": "keep.txt"})).ok);
        s.log_event(1, &What::User("second".into()).into()).unwrap();
        assert!(call(&mut p, &s, "edit_file", json!({"path": "keep.txt", "old_string": "line two", "new_string": "LINE 2"})).ok);
        assert!(call(&mut p, &s, "write_file", json!({"path": "keep.txt", "content": "replaced"})).ok);
        assert!(call(&mut p, &s, "write_file", json!({"path": "sub/new.txt", "content": "new"})).ok);
        let actions: Vec<String> = s.changes(1).unwrap().iter().map(|c| format!("{} {}", c.action, c.path)).collect();
        assert_eq!(actions, ["read keep.txt", "edit keep.txt", "write keep.txt", "create sub/new.txt"]);
        let restored = undo(&p, &s, 1).unwrap();
        assert_eq!(restored, ["removed sub/new.txt (it did not exist before)", "restored keep.txt", "restored keep.txt"]);
        assert_eq!(std::fs::read(p.workspace.join("keep.txt")).unwrap(), original.as_bytes());
        assert!(!p.workspace.join("sub/new.txt").exists());
        assert!(undo(&p, &s, 1).unwrap().is_empty());
        assert!(s.changes(1).unwrap().iter().filter(|c| c.action != "read").all(|c| c.undone));

        // What the user changed after the agent is never undone over.
        s.log_event(1, &What::User("third".into()).into()).unwrap();
        assert!(call(&mut p, &s, "edit_file", json!({"path": "keep.txt", "old_string": "line one", "new_string": "LINE 1"})).ok);
        assert!(call(&mut p, &s, "write_file", json!({"path": "made.txt", "content": "by the agent"})).ok);
        assert!(call(&mut p, &s, "write_file", json!({"path": "gone.txt", "content": "by the agent"})).ok);
        std::fs::write(p.workspace.join("keep.txt"), "my own later edit").unwrap();
        std::fs::write(p.workspace.join("made.txt"), "by the agent, then me").unwrap();
        std::fs::remove_file(p.workspace.join("gone.txt")).unwrap();
        let left = undo(&p, &s, 1).unwrap();
        assert!(left.iter().all(|line| line.contains("left ") && line.contains("alone")), "{left:?}");
        assert_eq!(left.len(), 3);
        assert_eq!(std::fs::read_to_string(p.workspace.join("keep.txt")).unwrap(), "my own later edit");
        assert_eq!(std::fs::read_to_string(p.workspace.join("made.txt")).unwrap(), "by the agent, then me");
        assert!(!p.workspace.join("gone.txt").exists());
    }

    #[test]
    fn what_a_command_changes_is_versioned_and_undone_too() {
        use cwbcode_proto::What;
        let (mut p, s) = workspace("bashundo");
        let ws = p.workspace.clone();
        std::fs::write(ws.join("edit.txt"), "before\n").unwrap();
        std::fs::write(ws.join("delete.txt"), "to be deleted").unwrap();
        std::fs::write(ws.join("same.txt"), "untouched").unwrap();
        std::fs::create_dir_all(ws.join("target")).unwrap();
        s.log_event(1, &What::User("go".into()).into()).unwrap();
        let script = "printf 'after\\n' > edit.txt; rm delete.txt; mkdir -p made && echo new > made/new.txt; echo built > target/out.bin; cat same.txt";
        assert_eq!(call(&mut p, &s, "bash", json!({"command": script})).text.trim(), "untouched");
        let changes: Vec<String> = s.changes(1).unwrap().iter().map(|c| format!("{} {} {}>{}", c.action, c.path, c.bytes_before, c.bytes_after)).collect();
        // Build output is not versioned; a command that changes nothing records nothing.
        assert_eq!(changes, ["delete delete.txt 13>-1", "edit edit.txt 7>6", "create made/new.txt -1>4"]);
        assert!(call(&mut p, &s, "bash", json!({"command": "ls -la && cat same.txt"})).ok);
        assert_eq!(s.changes(1).unwrap().len(), 3);
        let restored = undo(&p, &s, 1).unwrap();
        assert_eq!(restored, ["removed made/new.txt (it did not exist before)", "restored edit.txt", "restored delete.txt"]);
        assert_eq!(std::fs::read_to_string(ws.join("edit.txt")).unwrap(), "before\n");
        assert_eq!(std::fs::read_to_string(ws.join("delete.txt")).unwrap(), "to be deleted");
        assert!(!ws.join("made/new.txt").exists());
        // A project too large to snapshot is simply not versioned.
        for i in 0..SNAPSHOT_FILES + 1 {
            std::fs::write(ws.join(format!("made/f{i}")), "x").unwrap();
        }
        assert!(snapshot(&p).is_none());
        assert!(call(&mut p, &s, "bash", json!({"command": "echo more >> edit.txt"})).ok);
        assert_eq!(s.changes(1).unwrap().len(), 3);
    }

    #[test]
    fn the_task_list_is_saved_and_shown() {
        let (mut p, s) = workspace("todos");
        let todos = json!([{"content": "read the code", "status": "completed"}, {"content": "fix the bug", "status": "in_progress"}, {"content": "run the tests", "status": "pending"}]);
        let mut ctx = Ctx { policy: &mut p, store: &s, global: &s, session: 1, events: vec![], engine: None, stop: &|| false };
        let out = execute(&mut ctx, &Call { name: "todo_write".into(), args: json!({"todos": todos}), error: None });
        assert_eq!(out.text, "The task list is saved (2 of 3 left):\n[x] read the code\n[~] fix the bug\n[ ] run the tests");
        assert_eq!(ctx.events, [cwbcode_proto::What::Todos(todos.to_string())]);
        assert_eq!(s.todos(1).unwrap(), todos.to_string());
        assert!(!call(&mut p, &s, "todo_write", json!({"todos": [{"content": "x", "status": "someday"}]})).ok);
        // The project's own records are not searched or listed into.
        std::fs::create_dir_all(p.workspace.join(".cwbcode")).unwrap();
        std::fs::write(p.workspace.join(".cwbcode/secret.txt"), "needle").unwrap();
        std::fs::write(p.workspace.join("open.txt"), "needle").unwrap();
        assert_eq!(call(&mut p, &s, "grep", json!({"pattern": "needle"})).text, "open.txt:1:needle\n[1 match in 1 file]");
        assert!(!call(&mut p, &s, "read_file", json!({"path": ".cwbcode/secret.txt"})).ok);
        // Nor are the records of a project nested inside this one.
        std::fs::create_dir_all(p.workspace.join("sub/.cwbcode")).unwrap();
        std::fs::write(p.workspace.join("sub/.cwbcode/inner.txt"), "needle").unwrap();
        assert!(!call(&mut p, &s, "read_file", json!({"path": "sub/.cwbcode/inner.txt"})).ok);
        assert!(!call(&mut p, &s, "write_file", json!({"path": "sub/.cwbcode/x.txt", "content": "x"})).ok);
        assert_eq!(call(&mut p, &s, "grep", json!({"pattern": "needle"})).text, "open.txt:1:needle\n[1 match in 1 file]");
        assert!(!call(&mut p, &s, "list_dir", json!({"path": "sub"})).text.contains(".cwbcode"));
    }

    #[test]
    fn nothing_is_written_outside_the_workspace_or_in_read_only_mode() {
        let (mut p, s) = workspace("limits");
        let outside = p.workspace.parent().unwrap().join("outside.txt");
        let out = call(&mut p, &s, "write_file", json!({"path": outside.to_string_lossy(), "content": "x"}));
        assert!(!out.ok && !outside.exists(), "{}", out.text);
        assert!(!call(&mut p, &s, "write_file", json!({"path": "../outside.txt", "content": "x"})).ok);
        assert!(!call(&mut p, &s, "read_file", json!({"path": "~/.ssh/id_ed25519"})).ok);
        p.mode = crate::sandbox::Mode::ReadOnly;
        for (tool, args) in [("write_file", json!({"path": "a.txt", "content": "x"})), ("bash", json!({"command": "echo hi > a.txt"})), ("make_tool", json!({"tool": "t1", "description": "d", "parameters": {}, "source": "fn run(a: &Json) -> Result<String, String> { Ok(String::new()) }"}))] {
            let out = call(&mut p, &s, tool, args);
            assert!(!out.ok && out.text.contains("read-only"), "{tool}: {}", out.text);
        }
        assert!(!p.workspace.join("a.txt").exists());
        assert!(call(&mut p, &s, "list_dir", json!({})).ok);
    }

    #[test]
    fn a_command_runs_in_the_workspace_and_reports_its_status() {
        let (mut p, s) = workspace("bash");
        let ok = call(&mut p, &s, "bash", json!({"command": "echo hello > made.txt; cat made.txt; pwd"}));
        assert!(ok.ok && ok.text.starts_with("hello\n") && ok.text.trim_end().ends_with("/ws"), "{}", ok.text);
        let failed = call(&mut p, &s, "bash", json!({"command": "echo oops >&2; exit 3"}));
        assert_eq!(failed.text, "[stderr]\noops\n[exit status 3]");
        let slow = call(&mut p, &s, "bash", json!({"command": "sleep 30", "timeout": 1}));
        assert!(slow.text.contains("stopped after 1 seconds"), "{}", slow.text);
        // The knowledge base works by keywords without the model.
        assert!(call(&mut p, &s, "memory_save", json!({"title": "Wifi", "text": "The router is behind the sofa."})).text.contains("Saved as entry #1"));
        assert!(call(&mut p, &s, "memory_search", json!({"query": "router"})).text.contains("\"Wifi\" · bm25 #1"));
        assert!(call(&mut p, &s, "memory_get", json!({"id": 1})).text.ends_with("The router is behind the sofa."));
    }

    #[test]
    fn every_tool_is_declared_with_an_object_schema_and_a_summary() {
        let tools = builtin();
        assert_eq!(tools.len(), 16);
        for tool in &tools {
            assert_eq!(tool.parameters["type"], "object", "{}", tool.name);
            for required in tool.parameters["required"].as_array().unwrap() {
                assert!(tool.parameters["properties"].get(required.as_str().unwrap()).is_some(), "{} requires an undeclared {required}", tool.name);
            }
            assert!(dyntool::valid_name(&tool.name));
        }
        let call = Call { name: "bash".into(), args: json!({"command": "ls -la\npwd"}), error: None };
        assert_eq!(summary(&call), "bash(ls -la⏎pwd)");
        assert_eq!(summary(&Call { name: "grep".into(), args: json!({"pattern": "fn main", "path": "src"}), error: None }), "grep(fn main in src)");
    }
}
