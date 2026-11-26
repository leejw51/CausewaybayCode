//! The messages of cwbcode's API (schema/cwbcode.capnp) as plain Rust values, with their
//! Cap'n Proto encoding. Shared by the backend, the terminal client and the web client (it
//! compiles to WebAssembly).
//!
//!   POST /api/call   body: Request  ->  body: Response
//!   POST /api/chat   body: Request  ->  body: Events, each framed by `frame`
//!   POST /api/events (any body)     ->  body: Progress messages, each framed by `frame`
//!
//! The token goes in the `authorization: Bearer <token>` header.

#[allow(clippy::all, dead_code, unused)]
pub mod cwbcode_capnp {
    include!(concat!(env!("OUT_DIR"), "/cwbcode_capnp.rs"));
}

pub mod wallet;

use capnp::message::{Builder, ReaderOptions};
use cwbcode_capnp as schema;
pub use cwbcode_capnp::Method;

pub const CALL_PATH: &str = "/api/call";
pub const CHAT_PATH: &str = "/api/chat";
/// The model's state and its download, pushed to every client that listens (see Progress).
pub const EVENTS_PATH: &str = "/api/events";
/// The response header that says how a request arrived: h3, h2 or http/1.1.
pub const PROTOCOL_HEADER: &str = "x-cwbcode-protocol";
/// The UDP port HTTP/3 is served on, for a backend whose HTTPS (TCP) port is `port`: the next
/// one. Not the same number: a browser that expects HTTP/3 where it found HTTPS will try it,
/// refuse the self-signed certificate over QUIC (its "visit anyway" exception covers TCP
/// only), and show the site as unreachable. On another port only the terminal client, which
/// pins the certificate, ever asks.
pub fn quic_port(port: u16) -> u16 {
    port.checked_add(1).unwrap_or(port - 1)
}

/// Largest request the server reads, and largest message a reader accepts (bytes).
pub const MAX_MESSAGE_BYTES: usize = 48 << 20;
/// Largest file sent in a request.
pub const MAX_UPLOAD_BYTES: usize = 32 << 20;

pub type Error = capnp::Error;
pub type Result<T> = std::result::Result<T, Error>;

fn text(reader: capnp::Result<capnp::text::Reader>) -> Result<String> {
    reader?.to_string().map_err(|e| capnp::Error::failed(format!("text is not UTF-8: {e}")))
}

fn options() -> ReaderOptions {
    let mut o = ReaderOptions::new();
    o.traversal_limit_in_words(Some(MAX_MESSAGE_BYTES / 8 * 2));
    o
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Upload {
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: Method,
    pub project: String,
    pub session: i64,
    pub id: i64,
    pub text: String,
    pub name: String,
    pub mode: String,
    pub think: String,
    pub limit: u32,
    pub flag: bool,
    pub files: Vec<Upload>,
}

impl Request {
    pub fn new(method: Method) -> Self {
        Self { method, project: String::new(), session: 0, id: 0, text: String::new(), name: String::new(), mode: String::new(), think: String::new(), limit: 0, flag: false, files: Vec::new() }
    }

    pub fn project(mut self, project: &str) -> Self {
        self.project = project.to_string();
        self
    }

    pub fn session(mut self, session: i64) -> Self {
        self.session = session;
        self
    }

    pub fn text(mut self, text: &str) -> Self {
        self.text = text.to_string();
        self
    }

    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    pub fn id(mut self, id: i64) -> Self {
        self.id = id;
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut message = Builder::new_default();
        let mut b = message.init_root::<schema::request::Builder>();
        b.set_method(self.method);
        b.set_project(&self.project[..]);
        b.set_session(self.session);
        b.set_id(self.id);
        b.set_text(&self.text[..]);
        b.set_name(&self.name[..]);
        b.set_mode(&self.mode[..]);
        b.set_think(&self.think[..]);
        b.set_limit(self.limit);
        b.set_flag(self.flag);
        let mut files = b.init_files(self.files.len() as u32);
        for (i, file) in self.files.iter().enumerate() {
            let mut f = files.reborrow().get(i as u32);
            f.set_name(&file.name[..]);
            f.set_data(&file.data);
        }
        capnp::serialize::write_message_to_words(&message)
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        let message = capnp::serialize::read_message(&mut bytes, options())?;
        let r = message.get_root::<schema::request::Reader>()?;
        let mut files = Vec::new();
        for f in r.get_files()?.iter() {
            files.push(Upload { name: text(f.get_name())?, data: f.get_data()?.to_vec() });
        }
        Ok(Self {
            method: r.get_method().map_err(|e| capnp::Error::failed(format!("unknown method {}", e.0)))?,
            project: text(r.get_project())?,
            session: r.get_session(),
            id: r.get_id(),
            text: text(r.get_text())?,
            name: text(r.get_name())?,
            mode: text(r.get_mode())?,
            think: text(r.get_think())?,
            limit: r.get_limit(),
            flag: r.get_flag(),
            files,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub version: String,
    pub model: String,
    pub model_state: String,
    pub protocol: String,
    pub busy: bool,
    pub busy_project: String,
    pub context_tokens: u64,
    pub context_limit: u64,
    pub memory_active: u64,
    pub memory_peak: u64,
    pub uptime_seconds: u64,
    pub sandbox: String,
    pub home: String,
    pub h3: bool,
    pub queued: u32,
    /// How clients sign in: "open" (local mode, no login), "wallet" or "token".
    pub auth: String,
    /// The wallet address that may sign in ("" without one).
    pub wallet: String,
    /// SHA-256 of the server's certificate, in hex.
    pub fingerprint: String,
    /// The address the server listens on.
    pub bind: String,
    /// Where the wallet is set: "" (nowhere), "settings" (set up while running, changeable
    /// with Method::Setup), or the name of what fixes it (the .env file, the environment).
    pub wallet_source: String,
    /// The home folder projects live under: clients show paths under it as `~/...`.
    pub user_home: String,
    /// SHA-256 of the server's certificate authority, in hex: what a device compares with
    /// `cwbcode url` before it trusts the authority.
    pub ca_fingerprint: String,
    /// Where the model's files are (the store it was found in); "" when none is loaded.
    pub model_path: String,
    /// How large the model's files are, in bytes.
    pub model_bytes: u64,
    /// What the model is: architecture, layers, experts, quantization, vision, draft network.
    pub model_info: String,
    /// Tokens per second while writing, in the last turn (0: none yet).
    pub gen_tps: f64,
    /// Tokens per second while reading the prompt, in the last turn.
    pub prompt_tps: f64,
    /// The model's download: bytes here, bytes in all (0: not known yet), bytes per second
    /// over the last few seconds, where it comes from, why it stopped ("" if it did not), and
    /// what it is doing (or, when it is done, what the check against Ollama's registry found).
    pub download_done: u64,
    pub download_total: u64,
    pub download_speed: f64,
    pub download_source: String,
    pub download_error: String,
    pub download_note: String,
    /// The Hugging Face token: "" none found, "refused", else where it came from.
    pub download_token: String,
    /// The same, as fields a client words in its own language: tensor files written and in
    /// all, where from ("huggingface"), whether a Hugging Face token is used and where it was
    /// found ("env HF_TOKEN", "~/.cache/huggingface/token"; never the token), whether one was
    /// refused, and whether every file matched Ollama's registry.
    pub download_files_done: u32,
    pub download_files_total: u32,
    pub download_source_kind: String,
    pub download_token_used: bool,
    pub download_token_from: String,
    pub download_token_refused: bool,
    pub download_verified: bool,
}

/// One message of the events stream: where the model is, the same for every client.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    /// Counts up with every message the backend sends.
    pub seq: u64,
    /// missing | downloading | verifying | loading | ready | failed | none | scripted
    pub state: String,
    /// failed: why; missing: why the last download stopped ("" if none).
    pub error: String,
    pub done: u64,
    pub total: u64,
    pub speed: f64,
    pub files_done: u32,
    pub files_total: u32,
    pub source_kind: String,
    pub token: bool,
    pub token_from: String,
    pub token_refused: bool,
    pub verified: bool,
    /// The backend's own words on what it is doing (English).
    pub note: String,
    pub model: String,
}

impl Progress {
    /// Is the model there to use: nothing more will change (the stream ends after this).
    pub fn settled(&self) -> bool {
        matches!(self.state.as_str(), "ready" | "none" | "scripted")
    }

    /// Bytes done, as a share of all of them (0 when the total is not known yet).
    pub fn fraction(&self) -> f64 {
        if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut message = Builder::new_default();
        let mut b = message.init_root::<schema::progress::Builder>();
        b.set_seq(self.seq);
        b.set_state(&self.state[..]);
        b.set_error(&self.error[..]);
        b.set_done(self.done);
        b.set_total(self.total);
        b.set_speed(self.speed);
        b.set_files_done(self.files_done);
        b.set_files_total(self.files_total);
        b.set_source_kind(&self.source_kind[..]);
        b.set_token(self.token);
        b.set_token_from(&self.token_from[..]);
        b.set_token_refused(self.token_refused);
        b.set_verified(self.verified);
        b.set_note(&self.note[..]);
        b.set_model(&self.model[..]);
        capnp::serialize::write_message_to_words(&message)
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        let message = capnp::serialize::read_message(&mut bytes, options())?;
        let r = message.get_root::<schema::progress::Reader>()?;
        Ok(Self {
            seq: r.get_seq(),
            state: text(r.get_state())?,
            error: text(r.get_error())?,
            done: r.get_done(),
            total: r.get_total(),
            speed: r.get_speed(),
            files_done: r.get_files_done(),
            files_total: r.get_files_total(),
            source_kind: text(r.get_source_kind())?,
            token: r.get_token(),
            token_from: text(r.get_token_from())?,
            token_refused: r.get_token_refused(),
            verified: r.get_verified(),
            note: text(r.get_note())?,
            model: text(r.get_model())?,
        })
    }

    /// The message as one frame of the events stream.
    pub fn frame(&self) -> Vec<u8> {
        frame(&self.encode())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Project {
    pub path: String,
    pub name: String,
    pub sessions: u32,
    pub updated: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Session {
    pub id: i64,
    pub name: String,
    pub updated: String,
    pub messages: u32,
    pub first: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Doc {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub source: String,
    pub text: String,
    pub created: String,
    pub has_file: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hit {
    pub doc: i64,
    pub kind: String,
    pub title: String,
    pub snippet: String,
    pub bm25_rank: u32,
    pub semantic_rank: u32,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Change {
    pub id: i64,
    pub path: String,
    pub action: String,
    pub bytes_before: i64,
    pub bytes_after: i64,
    pub created: String,
    pub undone: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub context_tokens: u64,
    pub generated: u64,
    pub tokens_per_second: f64,
    pub tool_calls: u32,
    pub seconds: f64,
    pub steps: u32,
}

/// What happened in a conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    Session(i64),
    User(String),
    Thinking(String),
    Text(String),
    EndMessage,
    ToolStart { name: String, summary: String, args: String },
    ToolEnd { ok: bool, text: String, images: u32, seconds: f64 },
    Note(String),
    Warning(String),
    Error(String),
    Approval { id: i64, what: String, offer_always: bool },
    Usage(Usage),
    Todos(String),
    Done,
    Answered(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// When it happened (set on stored events).
    pub created: String,
    pub what: What,
}

impl From<What> for Event {
    fn from(what: What) -> Self {
        Self { created: String::new(), what }
    }
}

impl Event {
    fn build(&self, mut b: schema::event::Builder) {
        b.set_created(&self.created[..]);
        match &self.what {
            What::Session(id) => b.set_session(*id),
            What::User(t) => b.set_user(&t[..]),
            What::Thinking(t) => b.set_thinking(&t[..]),
            What::Text(t) => b.set_text(&t[..]),
            What::EndMessage => b.set_end_message(()),
            What::ToolStart { name, summary, args } => {
                let mut t = b.init_tool_start();
                t.set_name(&name[..]);
                t.set_summary(&summary[..]);
                t.set_args(&args[..]);
            }
            What::ToolEnd { ok, text, images, seconds } => {
                let mut t = b.init_tool_end();
                t.set_ok(*ok);
                t.set_text(&text[..]);
                t.set_images(*images);
                t.set_seconds(*seconds);
            }
            What::Note(t) => b.set_note(&t[..]),
            What::Warning(t) => b.set_warning(&t[..]),
            What::Error(t) => b.set_error(&t[..]),
            What::Approval { id, what, offer_always } => {
                let mut a = b.init_approval();
                a.set_id(*id);
                a.set_what(&what[..]);
                a.set_offer_always(*offer_always);
            }
            What::Usage(u) => {
                let mut b = b.init_usage();
                b.set_context_tokens(u.context_tokens);
                b.set_generated(u.generated);
                b.set_tokens_per_second(u.tokens_per_second);
                b.set_tool_calls(u.tool_calls);
                b.set_seconds(u.seconds);
                b.set_steps(u.steps);
            }
            What::Todos(t) => b.set_todos(&t[..]),
            What::Done => b.set_done(()),
            What::Answered(t) => b.set_answered(&t[..]),
        }
    }

    fn read(r: schema::event::Reader) -> Result<Self> {
        use schema::event::Which;
        let what = match r.which().map_err(|e| capnp::Error::failed(format!("unknown event {}", e.0)))? {
            Which::Session(id) => What::Session(id),
            Which::User(t) => What::User(text(t)?),
            Which::Thinking(t) => What::Thinking(text(t)?),
            Which::Text(t) => What::Text(text(t)?),
            Which::EndMessage(()) => What::EndMessage,
            Which::ToolStart(t) => {
                let t = t?;
                What::ToolStart { name: text(t.get_name())?, summary: text(t.get_summary())?, args: text(t.get_args())? }
            }
            Which::ToolEnd(t) => {
                let t = t?;
                What::ToolEnd { ok: t.get_ok(), text: text(t.get_text())?, images: t.get_images(), seconds: t.get_seconds() }
            }
            Which::Note(t) => What::Note(text(t)?),
            Which::Warning(t) => What::Warning(text(t)?),
            Which::Error(t) => What::Error(text(t)?),
            Which::Approval(a) => {
                let a = a?;
                What::Approval { id: a.get_id(), what: text(a.get_what())?, offer_always: a.get_offer_always() }
            }
            Which::Usage(u) => {
                let u = u?;
                What::Usage(Usage { context_tokens: u.get_context_tokens(), generated: u.get_generated(), tokens_per_second: u.get_tokens_per_second(), tool_calls: u.get_tool_calls(), seconds: u.get_seconds(), steps: u.get_steps() })
            }
            Which::Todos(t) => What::Todos(text(t)?),
            Which::Done(()) => What::Done,
            Which::Answered(t) => What::Answered(text(t)?),
        };
        Ok(Self { created: text(r.get_created())?, what })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut message = Builder::new_default();
        self.build(message.init_root::<schema::event::Builder>());
        capnp::serialize::write_message_to_words(&message)
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        let message = capnp::serialize::read_message(&mut bytes, options())?;
        Self::read(message.get_root::<schema::event::Reader>()?)
    }

    /// The event as one frame of a chat stream.
    pub fn frame(&self) -> Vec<u8> {
        frame(&self.encode())
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Response {
    /// "" when the call succeeded.
    pub error: String,
    pub status: Option<Status>,
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub events: Vec<Event>,
    pub docs: Vec<Doc>,
    pub hits: Vec<Hit>,
    pub tools: Vec<Tool>,
    pub changes: Vec<Change>,
    pub id: i64,
    pub text: String,
    pub data: Vec<u8>,
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Self { error: message.into(), ..Self::default() }
    }

    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Self::default() }
    }

    /// The response, or its error.
    pub fn ok(self) -> std::result::Result<Self, String> {
        if self.error.is_empty() { Ok(self) } else { Err(self.error) }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut message = Builder::new_default();
        let mut b = message.init_root::<schema::response::Builder>();
        b.set_error(&self.error[..]);
        if let Some(s) = &self.status {
            let mut t = b.reborrow().init_status();
            t.set_version(&s.version[..]);
            t.set_model(&s.model[..]);
            t.set_model_state(&s.model_state[..]);
            t.set_protocol(&s.protocol[..]);
            t.set_busy(s.busy);
            t.set_busy_project(&s.busy_project[..]);
            t.set_context_tokens(s.context_tokens);
            t.set_context_limit(s.context_limit);
            t.set_memory_active(s.memory_active);
            t.set_memory_peak(s.memory_peak);
            t.set_uptime_seconds(s.uptime_seconds);
            t.set_sandbox(&s.sandbox[..]);
            t.set_home(&s.home[..]);
            t.set_h3(s.h3);
            t.set_queued(s.queued);
            t.set_auth(&s.auth[..]);
            t.set_wallet(&s.wallet[..]);
            t.set_fingerprint(&s.fingerprint[..]);
            t.set_bind(&s.bind[..]);
            t.set_wallet_source(&s.wallet_source[..]);
            t.set_user_home(&s.user_home[..]);
            t.set_ca_fingerprint(&s.ca_fingerprint[..]);
            t.set_model_path(&s.model_path[..]);
            t.set_model_bytes(s.model_bytes);
            t.set_model_info(&s.model_info[..]);
            t.set_gen_tps(s.gen_tps);
            t.set_prompt_tps(s.prompt_tps);
            t.set_download_done(s.download_done);
            t.set_download_total(s.download_total);
            t.set_download_speed(s.download_speed);
            t.set_download_source(&s.download_source[..]);
            t.set_download_error(&s.download_error[..]);
            t.set_download_note(&s.download_note[..]);
            t.set_download_token(&s.download_token[..]);
            t.set_download_files_done(s.download_files_done);
            t.set_download_files_total(s.download_files_total);
            t.set_download_source_kind(&s.download_source_kind[..]);
            t.set_download_token_used(s.download_token_used);
            t.set_download_token_from(&s.download_token_from[..]);
            t.set_download_token_refused(s.download_token_refused);
            t.set_download_verified(s.download_verified);
        }
        let mut list = b.reborrow().init_projects(self.projects.len() as u32);
        for (i, p) in self.projects.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_path(&p.path[..]);
            t.set_name(&p.name[..]);
            t.set_sessions(p.sessions);
            t.set_updated(&p.updated[..]);
        }
        let mut list = b.reborrow().init_sessions(self.sessions.len() as u32);
        for (i, s) in self.sessions.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_id(s.id);
            t.set_name(&s.name[..]);
            t.set_updated(&s.updated[..]);
            t.set_messages(s.messages);
            t.set_first(&s.first[..]);
        }
        let mut list = b.reborrow().init_events(self.events.len() as u32);
        for (i, e) in self.events.iter().enumerate() {
            e.build(list.reborrow().get(i as u32));
        }
        let mut list = b.reborrow().init_docs(self.docs.len() as u32);
        for (i, d) in self.docs.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_id(d.id);
            t.set_kind(&d.kind[..]);
            t.set_title(&d.title[..]);
            t.set_source(&d.source[..]);
            t.set_text(&d.text[..]);
            t.set_created(&d.created[..]);
            t.set_has_file(d.has_file);
        }
        let mut list = b.reborrow().init_hits(self.hits.len() as u32);
        for (i, h) in self.hits.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_doc(h.doc);
            t.set_kind(&h.kind[..]);
            t.set_title(&h.title[..]);
            t.set_snippet(&h.snippet[..]);
            t.set_bm25_rank(h.bm25_rank);
            t.set_semantic_rank(h.semantic_rank);
            t.set_score(h.score);
        }
        let mut list = b.reborrow().init_tools(self.tools.len() as u32);
        for (i, x) in self.tools.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_name(&x.name[..]);
            t.set_description(&x.description[..]);
            t.set_parameters(&x.parameters[..]);
            t.set_source(&x.source[..]);
        }
        let mut list = b.reborrow().init_changes(self.changes.len() as u32);
        for (i, c) in self.changes.iter().enumerate() {
            let mut t = list.reborrow().get(i as u32);
            t.set_id(c.id);
            t.set_path(&c.path[..]);
            t.set_action(&c.action[..]);
            t.set_bytes_before(c.bytes_before);
            t.set_bytes_after(c.bytes_after);
            t.set_created(&c.created[..]);
            t.set_undone(c.undone);
        }
        b.set_id(self.id);
        b.set_text(&self.text[..]);
        b.set_data(&self.data);
        capnp::serialize::write_message_to_words(&message)
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        let message = capnp::serialize::read_message(&mut bytes, options())?;
        let r = message.get_root::<schema::response::Reader>()?;
        let status = if r.has_status() {
            let s = r.get_status()?;
            Some(Status {
                version: text(s.get_version())?,
                model: text(s.get_model())?,
                model_state: text(s.get_model_state())?,
                protocol: text(s.get_protocol())?,
                busy: s.get_busy(),
                busy_project: text(s.get_busy_project())?,
                context_tokens: s.get_context_tokens(),
                context_limit: s.get_context_limit(),
                memory_active: s.get_memory_active(),
                memory_peak: s.get_memory_peak(),
                uptime_seconds: s.get_uptime_seconds(),
                sandbox: text(s.get_sandbox())?,
                home: text(s.get_home())?,
                h3: s.get_h3(),
                queued: s.get_queued(),
                auth: text(s.get_auth())?,
                wallet: text(s.get_wallet())?,
                fingerprint: text(s.get_fingerprint())?,
                bind: text(s.get_bind())?,
                wallet_source: text(s.get_wallet_source())?,
                user_home: text(s.get_user_home())?,
                ca_fingerprint: text(s.get_ca_fingerprint())?,
                model_path: text(s.get_model_path())?,
                model_bytes: s.get_model_bytes(),
                model_info: text(s.get_model_info())?,
                gen_tps: s.get_gen_tps(),
                prompt_tps: s.get_prompt_tps(),
                download_done: s.get_download_done(),
                download_total: s.get_download_total(),
                download_speed: s.get_download_speed(),
                download_source: text(s.get_download_source())?,
                download_error: text(s.get_download_error())?,
                download_note: text(s.get_download_note())?,
                download_token: text(s.get_download_token())?,
                download_files_done: s.get_download_files_done(),
                download_files_total: s.get_download_files_total(),
                download_source_kind: text(s.get_download_source_kind())?,
                download_token_used: s.get_download_token_used(),
                download_token_from: text(s.get_download_token_from())?,
                download_token_refused: s.get_download_token_refused(),
                download_verified: s.get_download_verified(),
            })
        } else {
            None
        };
        let mut out = Self { error: text(r.get_error())?, status, id: r.get_id(), text: text(r.get_text())?, data: r.get_data()?.to_vec(), ..Self::default() };
        for p in r.get_projects()?.iter() {
            out.projects.push(Project { path: text(p.get_path())?, name: text(p.get_name())?, sessions: p.get_sessions(), updated: text(p.get_updated())? });
        }
        for s in r.get_sessions()?.iter() {
            out.sessions.push(Session { id: s.get_id(), name: text(s.get_name())?, updated: text(s.get_updated())?, messages: s.get_messages(), first: text(s.get_first())? });
        }
        for e in r.get_events()?.iter() {
            out.events.push(Event::read(e)?);
        }
        for d in r.get_docs()?.iter() {
            out.docs.push(Doc { id: d.get_id(), kind: text(d.get_kind())?, title: text(d.get_title())?, source: text(d.get_source())?, text: text(d.get_text())?, created: text(d.get_created())?, has_file: d.get_has_file() });
        }
        for h in r.get_hits()?.iter() {
            out.hits.push(Hit { doc: h.get_doc(), kind: text(h.get_kind())?, title: text(h.get_title())?, snippet: text(h.get_snippet())?, bm25_rank: h.get_bm25_rank(), semantic_rank: h.get_semantic_rank(), score: h.get_score() });
        }
        for t in r.get_tools()?.iter() {
            out.tools.push(Tool { name: text(t.get_name())?, description: text(t.get_description())?, parameters: text(t.get_parameters())?, source: text(t.get_source())? });
        }
        for c in r.get_changes()?.iter() {
            out.changes.push(Change { id: c.get_id(), path: text(c.get_path())?, action: text(c.get_action())?, bytes_before: c.get_bytes_before(), bytes_after: c.get_bytes_after(), created: text(c.get_created())?, undone: c.get_undone() });
        }
        Ok(out)
    }
}

/// The frame that carries nothing (see `Frames::next`).
pub const HEARTBEAT: [u8; 4] = [0; 4];

/// A message as a frame of a stream: its length (4 bytes, little-endian), then its bytes.
pub fn frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + message.len());
    out.extend_from_slice(&(message.len() as u32).to_le_bytes());
    out.extend_from_slice(message);
    out
}

/// Puts the frames of a stream back together, however the transport cut it into pieces.
#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
}

impl Frames {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add bytes as they arrive.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next complete message, if all of it has arrived.
    pub fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([self.buffer[0], self.buffer[1], self.buffer[2], self.buffer[3]]) as usize;
        if len > MAX_MESSAGE_BYTES {
            return Err(capnp::Error::failed(format!("a frame of {len} bytes is larger than the limit")));
        }
        if self.buffer.len() < 4 + len {
            return Ok(None);
        }
        let message = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(..4 + len);
        if message.is_empty() {
            // An empty frame is a heartbeat: the server sends one when nothing else is
            // happening, which is also how it notices a client that has gone away.
            return self.next();
        }
        Ok(Some(message))
    }

    /// The next complete event.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        match self.next()? {
            Some(message) => Event::decode(&message).map(Some),
            None => Ok(None),
        }
    }

    /// The next complete Progress message (of the events stream).
    pub fn next_progress(&mut self) -> Result<Option<Progress>> {
        match self.next()? {
            Some(message) => Progress::decode(&message).map(Some),
            None => Ok(None),
        }
    }

    /// Bytes of an unfinished frame (at the end of a stream: it was cut off).
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events() -> Vec<Event> {
        vec![
            What::Session(7).into(),
            What::User("고쳐줘 🙂".into()).into(),
            What::Thinking("hm".into()).into(),
            What::Text("answer".into()).into(),
            What::EndMessage.into(),
            What::ToolStart { name: "bash".into(), summary: "bash(ls)".into(), args: "{\"command\":\"ls\"}".into() }.into(),
            What::ToolEnd { ok: true, text: "a\nb".into(), images: 2, seconds: 1.5 }.into(),
            What::Note("n".into()).into(),
            What::Warning("w".into()).into(),
            What::Error("e".into()).into(),
            What::Approval { id: 42, what: "Run: ls".into(), offer_always: true }.into(),
            What::Usage(Usage { context_tokens: 4100, generated: 265, tokens_per_second: 95.2, tool_calls: 3, seconds: 5.3, steps: 4 }).into(),
            What::Todos("[{\"content\":\"x\",\"status\":\"pending\"}]".into()).into(),
            What::Answered("y".into()).into(),
            Event { created: "2026-11-25 19:00:00".into(), what: What::Done },
        ]
    }

    #[test]
    fn every_message_survives_encoding() {
        let mut request = Request::new(Method::Chat).project("/Users/x/proj").session(3).text("hello").name("title").id(9);
        request.mode = "auto".into();
        request.think = "low".into();
        request.limit = 5;
        request.flag = true;
        request.files = vec![Upload { name: "a.png".into(), data: vec![0, 1, 2, 255] }, Upload::default()];
        assert_eq!(Request::decode(&request.encode()).unwrap(), request);
        assert_eq!(Request::decode(&Request::new(Method::Status).encode()).unwrap(), Request::new(Method::Status));

        for event in events() {
            assert_eq!(Event::decode(&event.encode()).unwrap(), event);
        }

        let response = Response {
            error: String::new(),
            status: Some(Status { version: "0.1.0".into(), model: "inkling".into(), model_state: "ready".into(), protocol: "h3".into(), busy: true, busy_project: "/p".into(), context_tokens: 1, context_limit: 2, memory_active: 3 << 33, memory_peak: 4 << 33, uptime_seconds: 5, sandbox: "on".into(), home: "/h".into(), h3: true, queued: 2, auth: "wallet".into(), wallet: "0xabc".into(), fingerprint: "ff00".into(), bind: "0.0.0.0:7443".into(), wallet_source: "settings".into(), user_home: "/Users/someone".into(), ca_fingerprint: "aa11".into(), model_path: "/m".into(), model_bytes: 20 << 30, model_info: "inkling2".into(), gen_tps: 81.5, prompt_tps: 1250.25, download_done: 7 << 30, download_total: 19 << 30, download_speed: 24.5e6, download_source: "Hugging Face, without a token".into(), download_error: "e".into(), download_note: "412 of 1250 files".into(), download_files_done: 412, download_files_total: 1250, download_source_kind: "huggingface".into(), download_token: "~/.cache/huggingface/token".into(), download_token_used: true, download_token_from: "~/.cache/huggingface/token".into(), download_token_refused: true, download_verified: true }),
            projects: vec![Project { path: "/p".into(), name: "p".into(), sessions: 2, updated: "now".into() }],
            sessions: vec![Session { id: 1, name: "demo".into(), updated: "now".into(), messages: 8, first: "hi".into() }],
            events: events(),
            docs: vec![Doc { id: 1, kind: "image".into(), title: "t".into(), source: "/s".into(), text: "x".into(), created: "c".into(), has_file: true }],
            hits: vec![Hit { doc: 1, kind: "text".into(), title: "t".into(), snippet: "s".into(), bm25_rank: 1, semantic_rank: 0, score: 0.016 }],
            tools: vec![Tool { name: "wc".into(), description: "d".into(), parameters: "{}".into(), source: "fn run".into() }],
            changes: vec![Change { id: 1, path: "a.rs".into(), action: "edit".into(), bytes_before: -1, bytes_after: 10, created: "c".into(), undone: true }],
            id: -5,
            text: "text".into(),
            data: vec![9; 1000],
        };
        assert_eq!(Response::decode(&response.encode()).unwrap(), response);
        assert_eq!(Response::decode(&Response::default().encode()).unwrap(), Response::default());
        assert_eq!(Response::error("no").ok(), Err("no".to_string()));
    }

    #[test]
    fn progress_survives_encoding_and_framing() {
        let p = Progress { seq: 9, state: "downloading".into(), error: "e".into(), done: 7 << 30, total: 20 << 30, speed: 42.5e6, files_done: 468, files_total: 1250, source_kind: "huggingface".into(), token: true, token_from: "env HF_TOKEN".into(), token_refused: false, verified: false, note: "468 of 1250 files".into(), model: "m".into() };
        assert_eq!(Progress::decode(&p.encode()).unwrap(), p);
        assert_eq!(Progress::decode(&Progress::default().encode()).unwrap(), Progress::default());
        let mut frames = Frames::new();
        frames.push(&[HEARTBEAT.to_vec(), p.frame(), HEARTBEAT.to_vec()].concat());
        assert_eq!(frames.next_progress().unwrap(), Some(p.clone()));
        assert_eq!(frames.next_progress().unwrap(), None);
        assert!((p.fraction() - 0.35).abs() < 1e-9 && !p.settled());
        assert!(Progress { state: "ready".into(), ..p }.settled());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(Request::decode(&[]).is_err());
        assert!(Request::decode(&[1, 2, 3]).is_err());
        // A message read from a buffer that is not word-aligned (as network buffers are not).
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&Response::text("aligned or not").encode());
        assert_eq!(Response::decode(&shifted[1..]).unwrap().text, "aligned or not");
        assert!(Event::decode(&[0xff; 64]).is_err());
        let mut truncated = Response::text("hello").encode();
        truncated.truncate(truncated.len() - 8);
        assert!(Response::decode(&truncated).is_err());
    }

    #[test]
    fn a_stream_is_reassembled_wherever_it_is_cut() {
        let all = events();
        // With heartbeats between the events, which are skipped.
        let stream: Vec<u8> = all.iter().flat_map(|e| [HEARTBEAT.to_vec(), e.frame(), HEARTBEAT.to_vec()].concat()).collect();
        // Cut in two at every offset.
        for cut in 0..=stream.len() {
            let mut frames = Frames::new();
            let mut got = Vec::new();
            for part in [&stream[..cut], &stream[cut..]] {
                frames.push(part);
                while let Some(event) = frames.next_event().unwrap() {
                    got.push(event);
                }
            }
            assert_eq!(got, all, "cut at {cut}");
            assert_eq!(frames.pending(), 0);
        }
        // One byte at a time.
        let mut frames = Frames::new();
        let mut got = Vec::new();
        for byte in &stream {
            frames.push(&[*byte]);
            while let Some(event) = frames.next_event().unwrap() {
                got.push(event);
            }
        }
        assert_eq!(got, all);
        // A stream that ends inside a frame leaves it pending; an absurd length is refused.
        let mut frames = Frames::new();
        frames.push(&stream[..stream.len() - 3]);
        while frames.next_event().unwrap().is_some() {}
        assert!(frames.pending() > 0);
        let mut frames = Frames::new();
        frames.push(&u32::MAX.to_le_bytes());
        assert!(frames.next().is_err());
    }
}
