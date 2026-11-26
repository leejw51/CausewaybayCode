//! Causewaybay Code in the browser: a dashboard of the backend (model, memory, protocol,
//! projects) and a client for talking to the agent in any project, with its knowledge base
//! and the tools it has made. Built with Leptos; movement by Motion (motion.dev).

mod api;
mod i18n;
mod motion;

use cwbcode_proto::{Doc, Hit, Method, Project, Request, Session, Status, Tool, Upload, What};
use i18n::{Lang, tr, tr_with};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

// What the browser keeps, so that a refresh brings everything back:
/// the session (or the backend's token),
const TOKEN_KEY: &str = "cwbcode-token";
/// the preferences and where the user was: theme, layout, language, view, project, conversation,
const PREFS_KEY: &str = "cwbcode-prefs";
/// Where earlier versions kept the wallet's recovery phrase. It is never stored now (it
/// controls the whole wallet), and a phrase left there is deleted when the page loads.
const PHRASE_KEY: &str = "cwbcode-phrase";
/// The page's title, as index.html gives it.
const TITLE: &str = "CC · Causewaybay Code";

#[derive(Clone, Copy, PartialEq)]
enum View {
    Dashboard,
    Chat,
    Knowledge,
    Tools,
}

impl View {
    fn code(self) -> &'static str {
        match self {
            View::Dashboard => "dashboard",
            View::Chat => "chat",
            View::Knowledge => "knowledge",
            View::Tools => "tools",
        }
    }

    fn from_code(code: &str) -> View {
        match code {
            "dashboard" => View::Dashboard,
            "knowledge" => View::Knowledge,
            "tools" => View::Tools,
            _ => View::Chat,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    User,
    Thinking,
    Text,
    Tool,
    Note,
    Warning,
    Error,
    Approval,
}

/// One thing in a conversation, as the page shows it.
#[derive(Clone)]
struct Item {
    id: usize,
    kind: Kind,
    text: RwSignal<String>,
    /// A tool's result: (ok, output, seconds).
    result: RwSignal<Option<(bool, String, f64)>>,
    /// An approval: its id and whether "always" is offered.
    approval: Option<(i64, bool)>,
    /// What was answered to the approval ("" while it waits).
    answered: RwSignal<String>,
    /// Shown in history rather than as it happens (no entrance, approvals not answerable).
    past: bool,
}

#[derive(Clone, Copy)]
struct App {
    /// How the backend wants clients to sign in: "" (not known yet), "open" (local mode: no
    /// login), "wallet" or "token".
    auth: RwSignal<String>,
    /// The wallet the backend belongs to, its certificate's fingerprint, and its certificate
    /// authority's.
    wallet: RwSignal<String>,
    fingerprint: RwSignal<String>,
    ca_fingerprint: RwSignal<String>,
    token: RwSignal<String>,
    /// Preferences: "dark" or "light"; "horizontal" (side by side) or "vertical" (stacked);
    /// the interface language.
    theme: RwSignal<String>,
    layout: RwSignal<String>,
    /// The typeface (one of FONTS) and the size (one of SIZES).
    font: RwSignal<String>,
    size: RwSignal<String>,
    /// Whether the sidebar is shown (folded away, the view has the whole window).
    side: RwSignal<bool>,
    lang: RwSignal<Lang>,
    /// The project and conversation to come back to after a refresh, until they are restored.
    resume: RwSignal<(String, i64)>,
    /// Something to tell the user that is not an error.
    notice: RwSignal<String>,
    /// Tokens of context the conversation shown last reported (0: not known yet).
    ctx: RwSignal<u64>,
    /// Whether the settings sheet is open.
    settings: RwSignal<bool>,
    /// The console: whether it is shown, the command to run ("": guessed from the folder),
    /// what the last runs printed, and whether one is running now.
    console: RwSignal<bool>,
    console_cmd: RwSignal<String>,
    console_out: RwSignal<String>,
    running: RwSignal<bool>,
    view: RwSignal<View>,
    status: RwSignal<Option<Status>>,
    projects: RwSignal<Vec<Project>>,
    project: RwSignal<String>,
    sessions: RwSignal<Vec<Session>>,
    /// The conversation shown (0: a new one starts with the next message).
    session: RwSignal<i64>,
    items: RwSignal<Vec<Item>>,
    /// The item that pieces of thinking or of an answer are being added to.
    open: RwSignal<Option<usize>>,
    todos: RwSignal<String>,
    busy: RwSignal<bool>,
    mode: RwSignal<String>,
    think: RwSignal<String>,
    error: RwSignal<String>,
    next: RwSignal<usize>,
    /// Bumped when the conversation grows: the page follows it down.
    grew: RwSignal<u32>,
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn stored(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

fn store(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

fn forget(key: &str) {
    if let Some(storage) = storage() {
        let _ = storage.remove_item(key);
    }
}

/// The preferences kept from the last visit (an empty object the first time).
fn stored_prefs() -> serde_json::Value {
    stored(PREFS_KEY).and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_else(|| serde_json::json!({}))
}


/// Does the system ask for a light interface?
fn system_prefers_light() -> bool {
    web_sys::window().and_then(|w| w.match_media("(prefers-color-scheme: light)").ok().flatten()).is_some_and(|m| m.matches())
}

/// What a link to the page may carry after its #: the one-time code `cwbcode url` prints, to
/// exchange for the backend's token, or the token itself, typed by hand.
enum Link {
    Code(String),
    Token(String),
}

/// The code or token in the address, if there is one. It is taken out of the address at once,
/// before anything is asked of the backend: the address is kept in the browser's history.
fn take_link() -> Option<Link> {
    let window = web_sys::window()?;
    let hash = window.location().hash().unwrap_or_default();
    let link = if let Some(code) = hash.strip_prefix("#code=") {
        Link::Code(code.to_string())
    } else {
        Link::Token(hash.strip_prefix("#token=")?.to_string())
    };
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
    }
    Some(link)
}

impl App {
    fn new() -> Self {
        forget(PHRASE_KEY);
        // Preferences and place from the last visit; for a first visit, what the system says.
        let prefs = stored_prefs();
        let text = |key: &str, fallback: &str| prefs[key].as_str().unwrap_or(fallback).to_string();
        let browser = web_sys::window().and_then(|w| w.navigator().language()).unwrap_or_default();
        Self {
            auth: RwSignal::new(String::new()),
            wallet: RwSignal::new(String::new()),
            fingerprint: RwSignal::new(String::new()),
            ca_fingerprint: RwSignal::new(String::new()),
            // (From an earlier visit; one in the address is taken once it has been checked.)
            token: RwSignal::new(stored(TOKEN_KEY).unwrap_or_default()),
            theme: RwSignal::new(text("theme", if system_prefers_light() { "light" } else { "dark" })),
            layout: RwSignal::new(text("layout", "horizontal")),
            font: RwSignal::new(Some(text("font", "pixel")).filter(|f| FONTS.iter().any(|(code, ..)| code == f)).unwrap_or_else(|| "pixel".into())),
            side: RwSignal::new(prefs["side"].as_bool().unwrap_or(true)),
            size: RwSignal::new(Some(text("size", "m")).filter(|s| SIZES.contains(&s.as_str())).unwrap_or_else(|| "m".into())),
            lang: RwSignal::new(prefs["lang"].as_str().and_then(Lang::from_code).unwrap_or_else(|| Lang::from_browser(&browser))),
            resume: RwSignal::new((text("project", ""), prefs["session"].as_i64().unwrap_or(0))),
            notice: RwSignal::new(String::new()),
            ctx: RwSignal::new(0),
            settings: RwSignal::new(false),
            console: RwSignal::new(false),
            console_cmd: RwSignal::new(String::new()),
            console_out: RwSignal::new(String::new()),
            running: RwSignal::new(false),
            view: RwSignal::new(View::from_code(&text("view", "chat"))),
            status: RwSignal::new(None),
            projects: RwSignal::new(Vec::new()),
            project: RwSignal::new(String::new()),
            sessions: RwSignal::new(Vec::new()),
            session: RwSignal::new(0),
            items: RwSignal::new(Vec::new()),
            open: RwSignal::new(None),
            todos: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            mode: RwSignal::new(text("mode", "ask")),
            think: RwSignal::new(text("think", "")),
            error: RwSignal::new(String::new()),
            next: RwSignal::new(0),
            grew: RwSignal::new(0),
        }
    }

    /// The text for `key` in the interface language, as something a view can show: it
    /// changes when the language does.
    fn t(self, key: &'static str) -> impl Fn() -> &'static str + Copy + Send + Sync + 'static {
        let lang = self.lang;
        move || tr(lang.get(), key)
    }

    /// Write the preferences and the place the user is at to the browser's storage.
    fn save_prefs(&self) {
        // While a refresh is still finding its way back, the place to return to is kept.
        let (resume_project, resume_session) = self.resume.get();
        let project = self.project.get();
        let (project, session) = if project.is_empty() && !resume_project.is_empty() { (resume_project, resume_session) } else { (project, self.session.get()) };
        let prefs = serde_json::json!({
            "theme": self.theme.get(), "layout": self.layout.get(), "lang": self.lang.get().code(),
            "font": self.font.get(), "size": self.size.get(), "side": self.side.get(),
            "view": self.view.get().code(), "project": project, "session": session,
            "mode": self.mode.get(), "think": self.think.get(),
        });
        store(PREFS_KEY, &prefs.to_string());
    }

    /// Say something that is not an error, for a few seconds.
    fn tell(&self, text: String) {
        self.notice.set(text);
        let notice = self.notice;
        set_timeout(move || notice.set(String::new()), std::time::Duration::from_secs(5));
    }

    /// May the app be shown: is there a token (local mode too asks for the backend's), or a
    /// session?
    fn ready(&self) -> bool {
        match self.auth.get().as_str() {
            "" => false,
            _ => !self.token.get().is_empty(),
        }
    }

    fn ready_untracked(&self) -> bool {
        match self.auth.get_untracked().as_str() {
            "" => false,
            _ => !self.token.get_untracked().is_empty(),
        }
    }

    /// Ask the backend how to sign in (needs no credentials).
    async fn hello(&self) {
        match api::call("", &Request::new(Method::Hello)).await {
            Ok(response) => {
                let status = response.status.unwrap_or_default();
                self.wallet.set(status.wallet);
                self.fingerprint.set(status.fingerprint);
                self.ca_fingerprint.set(status.ca_fingerprint);
                self.auth.set(status.auth);
            }
            Err(e) => self.fail(e),
        }
    }

    /// Take the code or token a link carried. A code is exchanged for the backend's token; a
    /// token is tried first. Only one that works is kept: a link that does not (used already,
    /// expired, or another backend's) leaves the token this browser has alone.
    async fn adopt(&self, link: Link) {
        let token = match link {
            Link::Code(code) => api::call("", &Request::new(Method::Redeem).text(&code)).await.map(|response| response.text),
            Link::Token(token) if token == self.token.get_untracked() => return,
            // (Not through `call`: a refusal there forgets the token kept.)
            Link::Token(token) => api::call(&token, &Request::new(Method::Status)).await.map(|_| token),
        };
        match token {
            Ok(token) => self.remember(&token, true),
            Err(e) if e.starts_with("cannot reach") => self.fail(e),
            // Refused by the backend.
            Err(_) => self.fail(tr(self.lang.get_untracked(), "link_refused").to_string()),
        }
    }

    /// Use this token (the backend's, or a session from signing in); with `keep`, also after
    /// a refresh, until it ends or the user signs out.
    fn remember(&self, token: &str, keep: bool) {
        if keep {
            store(TOKEN_KEY, token);
        } else {
            forget(TOKEN_KEY);
        }
        self.token.set(token.to_string());
    }

    /// Sign in with the wallet's recovery phrase. The key is derived here, in the browser;
    /// the backend gets a signature of its one-time challenge and nothing else. The phrase
    /// is never stored: it controls the whole wallet, not only this backend. With `keep`,
    /// the session (good for 30 days, ended by signing out) is kept in this browser.
    async fn sign_in(&self, phrase: String, index: u32, keep: bool) -> Result<(), String> {
        use cwbcode_proto::wallet;
        let key = wallet::key_from_phrase(&phrase, index)?;
        drop(phrase);
        let address = wallet::address(&key)?;
        let expected = self.wallet.get_untracked();
        if !wallet::same_address(&address, &expected) {
            return Err(format!("That phrase is wallet {address} (account {index}); this backend belongs to {}.", expected.to_lowercase()));
        }
        let challenge = api::call("", &Request::new(Method::Challenge)).await?.text;
        // Sign only what is a sign-in for this wallet at this server.
        let for_us = challenge.starts_with("Causewaybay Code sign-in")
            && wallet::challenge_field(&challenge, "wallet").is_some_and(|w| wallet::same_address(w, &address))
            && wallet::challenge_field(&challenge, "server") == Some(self.fingerprint.get_untracked().as_str());
        if !for_us {
            return Err("The backend sent something that is not a sign-in request for this wallet; nothing was signed.".to_string());
        }
        let signature = wallet::sign(&key, &challenge)?;
        let session = api::call("", &Request::new(Method::Login).text(&challenge).name(&signature)).await?.text;
        self.remember(&session, keep);
        Ok(())
    }

    /// Turn remote mode on (a wallet address) or off (""). The backend applies it at once:
    /// what it says about signing in afterwards is taken from its answer.
    async fn setup(&self, value: String) -> Result<String, String> {
        let response = api::call(&self.token.get_untracked(), &Request::new(Method::Setup).text(&value)).await?;
        if let Some(status) = response.status {
            // The session of the mode before is over, either way.
            forget(TOKEN_KEY);
            self.token.set(String::new());
            self.status.set(None);
            self.wallet.set(status.wallet);
            self.fingerprint.set(status.fingerprint);
            self.ca_fingerprint.set(status.ca_fingerprint);
            self.auth.set(status.auth);
        }
        Ok(response.text)
    }

    async fn sign_out(&self) {
        let _ = api::call(&self.token.get_untracked(), &Request::new(Method::Logout)).await;
        // Signing out forgets the session kept on this device.
        forget(TOKEN_KEY);
        self.resume.set((String::new(), 0));
        self.token.set(String::new());
        self.projects.set(Vec::new());
        self.project.set(String::new());
        self.sessions.set(Vec::new());
        self.new_conversation();
    }

    fn request(&self, method: Method) -> Request {
        Request::new(method).project(&self.project.get_untracked())
    }

    /// Show an error; a refused token sends the user back to the token page.
    fn fail(&self, error: String) {
        if error == "unauthorized" {
            // The session has ended (or the token is wrong): back to the sign-in page.
            forget(TOKEN_KEY);
            self.token.set(String::new());
            return;
        }
        self.error.set(error);
        let error = self.error;
        set_timeout(move || error.set(String::new()), std::time::Duration::from_secs(8));
    }

    async fn call(&self, request: Request) -> Option<cwbcode_proto::Response> {
        let sent = self.token.get_untracked();
        match api::call(&sent, &request).await {
            Ok(response) => Some(response),
            Err(e) => {
                self.fail_with(e, &sent);
                None
            }
        }
    }

    /// `fail`, for a request made with the token `sent`: a refusal of a token that has been
    /// replaced since (a link's code exchanged meanwhile) does not send the user back.
    fn fail_with(&self, error: String, sent: &str) {
        if error == "unauthorized" && sent != self.token.get_untracked() {
            return;
        }
        self.fail(error);
    }

    /// Edits and commands waiting for the user's answer in this conversation.
    fn waiting(&self) -> usize {
        self.items.with(|items| items.iter().filter(|i| i.kind == Kind::Approval && !i.past && i.answered.with(String::is_empty)).count())
    }

    /// At this width and below the sidebar is a drawer over the view, closed after a choice in
    /// it: the same width as the stylesheet's `max-width: 640px`.
    fn narrow() -> bool {
        web_sys::window().and_then(|w| w.inner_width().ok()).and_then(|w| w.as_f64()).is_some_and(|w| w <= 640.0)
    }

    /// On a phone, close the drawer once something in it is chosen.
    fn chosen(&self) {
        if Self::narrow() {
            self.side.set(false);
        }
    }

    fn push(&self, kind: Kind, text: &str, approval: Option<(i64, bool)>, past: bool) -> usize {
        let id = self.next.get_untracked();
        self.next.set(id + 1);
        self.items.update(|items| items.push(Item { id, kind, text: RwSignal::new(text.to_string()), result: RwSignal::new(None), approval, answered: RwSignal::new(String::new()), past }));
        id
    }

    /// Put an event into the conversation shown. `past`: from the log, where thinking and
    /// answers are whole messages.
    fn apply(&self, what: What, past: bool) {
        match what {
            What::Session(id) => self.session.set(id),
            What::User(text) => {
                self.push(Kind::User, &text, None, past);
            }
            What::Thinking(text) => self.piece(Kind::Thinking, &text),
            What::Text(text) => self.piece(Kind::Text, &text),
            What::EndMessage => self.open.set(None),
            What::ToolStart { summary, .. } => {
                self.open.set(None);
                self.push(Kind::Tool, &summary, None, past);
            }
            What::ToolEnd { ok, text, seconds, .. } => {
                if let Some(tool) = self.items.get_untracked().iter().rev().find(|i| i.kind == Kind::Tool && i.result.get_untracked().is_none()) {
                    tool.result.set(Some((ok, text, seconds)));
                }
            }
            What::Note(text) => {
                self.push(Kind::Note, &text, None, past);
            }
            What::Warning(text) => {
                self.push(Kind::Warning, &text, None, past);
            }
            What::Error(text) => {
                self.push(Kind::Error, &text, None, past);
            }
            What::Approval { id, what, offer_always } => {
                self.open.set(None);
                self.push(Kind::Approval, &what, Some((id, offer_always)), past);
            }
            What::Answered(answer) => {
                if let Some(asked) = self.items.get_untracked().iter().rev().find(|i| i.kind == Kind::Approval) {
                    asked.answered.set(if answer.is_empty() { "(no answer)".to_string() } else { answer });
                }
            }
            What::Usage(u) => {
                self.ctx.set(u.context_tokens);
                let speed = if u.tokens_per_second > 0.0 { format!(" · {:.0} tok/s", u.tokens_per_second) } else { String::new() };
                let tools = if u.tool_calls > 0 { format!(" · {} tool call{}", u.tool_calls, if u.tool_calls == 1 { "" } else { "s" }) } else { String::new() };
                self.push(Kind::Note, &format!("{} tokens in context · wrote {}{speed}{tools} · {:.1}s", u.context_tokens, u.generated, u.seconds), None, past);
            }
            What::Todos(json) => self.todos.set(json),
            What::Done => self.open.set(None),
        }
        self.grew.update(|n| *n += 1);
    }

    /// A piece of thinking or of an answer: added to the message being written.
    fn piece(&self, kind: Kind, text: &str) {
        let items = self.items.get_untracked();
        match self.open.get_untracked().and_then(|id| items.iter().find(|i| i.id == id && i.kind == kind)) {
            Some(item) => item.text.update(|t| t.push_str(text)),
            None => {
                let id = self.push(kind, text, None, false);
                self.open.set(Some(id));
            }
        }
    }

    async fn refresh_status(&self) {
        if self.auth.get_untracked().is_empty() {
            // The backend was not reachable when the page loaded: ask again.
            self.hello().await;
        }
        if !self.ready_untracked() {
            return;
        }
        let sent = self.token.get_untracked();
        match api::call(&sent, &Request::new(Method::Status)).await {
            Ok(response) => self.status.set(response.status),
            Err(e) if e == "unauthorized" => self.fail_with(e, &sent),
            // The server may be restarting: the page keeps asking.
            Err(_) => self.status.set(None),
        }
    }

    async fn refresh_projects(&self) {
        if let Some(response) = self.call(Request::new(Method::Projects)).await {
            if self.project.get_untracked().is_empty() {
                // After a refresh: the project and conversation the user was in, if still there.
                let (wanted, conversation) = self.resume.get_untracked();
                let back = response.projects.iter().find(|p| p.path == wanted);
                if let Some(project) = back.or(response.projects.first()) {
                    self.select_project(project.path.clone()).await;
                    if back.is_some() && conversation != 0 {
                        self.load_session(conversation).await;
                    }
                }
                self.resume.set((String::new(), 0));
            }
            self.projects.set(response.projects);
        }
    }

    async fn refresh_sessions(&self) {
        if self.project.get_untracked().is_empty() {
            return;
        }
        if let Some(response) = self.call(self.request(Method::Sessions)).await {
            self.sessions.set(response.sessions);
        }
    }

    async fn select_project(&self, path: String) {
        self.project.set(path);
        self.new_conversation();
        self.refresh_sessions().await;
    }

    /// A path as it is shown: under the home folder it starts with `~/`, so that the name of
    /// the account is not on the screen.
    fn tilde(&self, path: &str) -> String {
        let home = self.status.get().map(|s| s.user_home).unwrap_or_default();
        match path.strip_prefix(home.as_str()) {
            Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
            _ => path.to_string(),
        }
    }

    /// How full the context is: (tokens, limit, share 0..1).
    fn context(&self) -> (u64, u64, f64) {
        let limit = self.status.get().map(|s| s.context_limit).unwrap_or(0);
        let used = self.ctx.get();
        (used, limit, if limit == 0 { 0.0 } else { (used as f64 / limit as f64).min(1.0) })
    }

    /// Whether the context is at least this full (0..1).
    fn ctx_at(&self, share: f64) -> bool {
        self.context().2 >= share
    }

    /// Run the project's code (the command in the console, or one the backend guesses from
    /// the folder) and show what it printed in the console.
    async fn run_code(&self) {
        if self.project.get_untracked().is_empty() || self.running.get_untracked() {
            return;
        }
        self.console.set(true);
        self.running.set(true);
        let answer = api::call(&self.token.get_untracked(), &self.request(Method::Run).text(self.console_cmd.get_untracked().trim())).await;
        self.running.set(false);
        let said = match answer {
            Ok(response) => response.text,
            Err(e) => format!("! {e}"),
        };
        self.console_out.update(|out| {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&said);
        });
        after_draw(|| motion::to_end("console-out"));
    }

    async fn remove_project(&self, path: String) {
        if self.call(Request::new(Method::RemoveProject).project(&path)).await.is_some() {
            let shown = self.tilde(&path);
            if self.project.get_untracked() == path {
                self.project.set(String::new());
                self.sessions.set(Vec::new());
                self.new_conversation();
            }
            self.refresh_projects().await;
            self.tell(tr_with(self.lang.get_untracked(), "removed", &shown));
        }
    }

    fn new_conversation(&self) {
        self.ctx.set(0);
        self.session.set(0);
        self.items.set(Vec::new());
        self.todos.set(String::new());
        self.open.set(None);
    }

    async fn open_project(&self, path: String) {
        if let Some(response) = self.call(Request::new(Method::OpenProject).text(&path)).await {
            // A folder that did not exist was made.
            if let Some(made) = response.text.strip_prefix("created ") {
                self.tell(tr_with(self.lang.get_untracked(), "created", &self.tilde(made)));
            }
            if let Some(project) = response.projects.first() {
                self.select_project(project.path.clone()).await;
            }
            self.refresh_projects().await;
            self.view.set(View::Chat);
        }
    }

    async fn select_session(&self, id: i64) {
        if self.load_session(id).await {
            self.view.set(View::Chat);
        }
    }

    /// Show a conversation from its log (without leaving the view the user is on).
    async fn load_session(&self, id: i64) -> bool {
        match api::call(&self.token.get_untracked(), &self.request(Method::Transcript).session(id)).await {
            Ok(response) => {
                self.new_conversation();
                self.session.set(id);
                for event in response.events {
                    self.apply_past(event.what);
                }
                self.todos.set(response.text);
                true
            }
            Err(e) if e == "unauthorized" => {
                self.fail(e);
                false
            }
            // (A conversation that is gone is simply not come back to.)
            Err(_) => false,
        }
    }

    fn apply_past(&self, what: What) {
        match what {
            What::Thinking(text) => {
                self.push(Kind::Thinking, &text, None, true);
            }
            What::Text(text) => {
                self.push(Kind::Text, &text, None, true);
            }
            other => self.apply(other, true),
        }
        self.grew.update(|n| *n += 1);
    }

    async fn send(&self, text: String, images: Vec<Upload>) {
        if self.busy.get_untracked() || self.project.get_untracked().is_empty() {
            return;
        }
        self.busy.set(true);
        let shown = if images.is_empty() { text.clone() } else { format!("{text}\n[{}]", images.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")) };
        self.apply(What::User(shown), false);
        let mut request = self.request(Method::Chat).session(self.session.get_untracked()).text(&text);
        request.mode = self.mode.get_untracked();
        request.think = self.think.get_untracked();
        request.files = images;
        let app = *self;
        if let Err(e) = api::chat(&self.token.get_untracked(), &request, move |event| app.apply(event.what, false)).await {
            self.fail(e);
        }
        self.open.set(None);
        self.busy.set(false);
        self.refresh_sessions().await;
        self.refresh_status().await;
    }
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1e9
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(Root);
}

/// Run something once the page has drawn what was just changed.
/// Does this key press open what has the focus, as a click would (Enter, or Space)?
fn activates(ev: &web_sys::KeyboardEvent) -> bool {
    ev.key() == "Enter" || ev.key() == " "
}

fn after_draw(f: impl FnOnce() + 'static) {
    set_timeout(f, std::time::Duration::from_millis(30));
}

#[component]
fn Root() -> impl IntoView {
    let app = App::new();
    provide_context(app);
    // The backend's state: every three seconds, and every second while the model downloads
    // or loads (the download's bar moves with it).
    let ticks = StoredValue::new(0u32);
    let tick = Closure::<dyn Fn()>::new(move || {
        ticks.update_value(|n| *n = n.wrapping_add(1));
        let moving = app.status.with_untracked(|s| s.as_ref().is_some_and(|s| s.model_state == "downloading" || s.model_state == "loading"));
        if moving || ticks.get_value() % 3 == 0 {
            spawn_local(async move { app.refresh_status().await });
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), 1000);
    }
    tick.forget();
    spawn_local(async move { app.hello().await });
    // Once signed in (or in local mode, at once): what the backend knows.
    Effect::new(move |_| {
        if app.ready() {
            spawn_local(async move {
                app.refresh_status().await;
                app.refresh_projects().await;
            });
        }
    });
    // An edit or command waiting for an answer is in the tab's title, seen from another tab
    // or another view: the page's own title, as index.html has it, with the count in front.
    Effect::new(move |_| {
        let waiting = app.waiting();
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            doc.set_title(&if waiting > 0 { format!("({waiting}) {TITLE}") } else { TITLE.to_string() });
        }
    });
    // On a phone the sidebar is a drawer: it starts closed, and closes when the window becomes
    // that narrow from wider. Other changes of size leave it as it is: on a phone the height
    // changes whenever the keyboard or the browser's bars come and go. Settings close with
    // Escape, or a click outside them.
    if App::narrow() {
        app.side.set(false);
    }
    let was_narrow = StoredValue::new(App::narrow());
    let _ = window_event_listener(leptos::ev::resize, move |_| {
        let narrow = App::narrow();
        if narrow && !was_narrow.get_value() && app.side.get_untracked() {
            app.side.set(false);
        }
        was_narrow.set_value(narrow);
    });
    // A link with a code (or a token) in the address; pasted into this same tab it changes only
    // what follows the #, so it is read then too, as a fresh load does.
    if let Some(link) = take_link() {
        spawn_local(async move { app.adopt(link).await });
    }
    let _ = window_event_listener(leptos::ev::hashchange, move |_| {
        if let Some(link) = take_link() {
            spawn_local(async move { app.adopt(link).await });
        }
    });
    let _ = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && app.settings.get_untracked() {
            app.settings.set(false);
        }
    });
    let _ = window_event_listener(leptos::ev::click, move |ev| {
        if !app.settings.get_untracked() {
            return;
        }
        let inside = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).is_some_and(|el| el.closest("#settings-sheet, #settings").ok().flatten().is_some());
        if !inside {
            app.settings.set(false);
        }
    });
    // The theme and language are the page's; every change of a preference, or of where the
    // user is, is written down, so that a refresh comes back to the same place.
    Effect::new(move |_| {
        if let Some(root) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.document_element()) {
            let _ = root.set_attribute("data-theme", &app.theme.get());
            let _ = root.set_attribute("lang", app.lang.get().code());
            let font = app.font.get();
            let _ = root.set_attribute("data-font", &font);
            // The face itself, and the scale that makes it read as the others do.
            if let Some((_, _, family, scale)) = FONTS.iter().find(|(code, ..)| *code == font) {
                let stack = if family.is_empty() { FONT_FALLBACK.replace("\"NeoDGM\", ", "") } else { format!("\"{family}\", {FONT_FALLBACK}") };
                if let Some(style) = root.dyn_ref::<web_sys::HtmlElement>().map(|el| el.style()) {
                    // (The whole page is set in it; the size, though, is the conversation's alone.)
                    let _ = style.set_property("--body", &stack);
                    let _ = style.set_property("--face", &scale.to_string());
                    // Code and tool calls are set in it too: a conversation that is mostly code
                    // would otherwise not change at all. (A code face stands behind it for
                    // the system's own, which names no face.)
                    let code = if family.is_empty() { "\"JetBrainsMonoCode\", ui-monospace, Menlo, monospace".to_string() } else { format!("\"{family}\", \"JetBrainsMonoCode\", \"NeoDGM\", ui-monospace, monospace") };
                    let _ = style.set_property("--mono", &code);
                }
            }
            let _ = root.set_attribute("data-size", &app.size.get());
        }
    });
    Effect::new(move |_| app.save_prefs());
    Effect::new(move |_| after_draw(motion::install));
    Effect::new(move |_| after_draw(motion::outline_install));
    view! {
        <div class="glow a"></div>
        <div class="glow b"></div>
        <Show when=move || app.ready() fallback=move || view! { <Login/> }>
            <div class="app" class:vertical=move || app.layout.get() == "vertical" class:noside=move || !app.side.get()>
                <AppBar/>
                <Sidebar/>
                <div class="main"><ViewHost/></div>
                <div class="sheet glass" id="settings-sheet" class:open=move || app.settings.get()>
                    <div class="sheet-head"><b>{app.t("settings")}</b><button class="ghost round" id="settings-close" title=app.t("close") aria-label=app.t("close") on:click=move |_| app.settings.set(false)>"×"</button></div>
                    <Prefs/>
                    // (On a phone the bar has no room for it: signing out is here.)
                    <Show when=move || app.auth.get() != "open">
                        <div class="sheet-signout"><button class="ghost" id="signout-sheet" on:click=move |_| { app.settings.set(false); spawn_local(async move { app.sign_out().await }) }>{app.t("sign_out")}</button></div>
                    </Show>
                </div>
            </div>
        </Show>
        <Show when=move || !app.error.get().is_empty()>
            <Toast id="toast" class="toast" text=app.error/>
        </Show>
        <Show when=move || !app.notice.get().is_empty()>
            <Toast id="notice" class="toast good" text=app.notice/>
        </Show>
    }
}

#[component]
fn Toast(id: &'static str, class: &'static str, text: RwSignal<String>) -> impl IntoView {
    let node = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            motion::sheet(&el);
        }
    });
    view! { <div class=class id=id node_ref=node>{move || text.get()}</div> }
}

/// The view the user is on. A change of view arrives from the side it lies on.
#[component]
fn ViewHost() -> impl IntoView {
    let app = expect_context::<App>();
    let host = NodeRef::<leptos::html::Div>::new();
    let before = StoredValue::new(app.view.get_untracked() as i32);
    Effect::new(move |_| {
        let now = app.view.get() as i32;
        let was = before.get_value();
        before.set_value(now);
        if let Some(el) = host.get() {
            if now != was {
                motion::view_in(&el, if now > was { 36.0 } else { -36.0 });
            }
        }
    });
    view! {
        <div class="view" node_ref=host>
            {move || match app.view.get() {
                View::Dashboard => view! { <Dashboard/> }.into_any(),
                View::Chat => view! { <Chat/> }.into_any(),
                View::Knowledge => view! { <Knowledge/> }.into_any(),
                View::Tools => view! { <Tools/> }.into_any(),
            }}
        </div>
    }
}

fn icon(path: &'static str) -> impl IntoView {
    view! { <svg class="ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d=path></path></svg> }
}

const ICON_SUN: &str = "M12 4V2m0 20v-2m8-8h2M2 12h2m13.7-5.7 1.4-1.4M4.9 19.1l1.4-1.4m0-11.4L4.9 4.9m14.2 14.2-1.4-1.4M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z";
const ICON_MOON: &str = "M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z";
const ICON_COLUMNS: &str = "M4 5h16v14H4zM10 5v14";
const ICON_GEAR: &str = "M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6zM19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.400-2.300 1a7 7 0 0 0-2.100-1.200L14 3h-4l-.5 2.700a7 7 0 0 0-2.100 1.200l-2.300-1-2 3.400 2 1.500a7 7 0 0 0 0 2.400l-2 1.500 2 3.400 2.300-1a7 7 0 0 0 2.100 1.200L10 21h4l.5-2.700a7 7 0 0 0 2.100-1.200l2.300 1 2-3.400-2-1.500c.1-.4.1-.800.1-1.200z";
const ICON_SIDE: &str = "M4 5h16v14H4zM9 5v14M6 9h1M6 12h1";
const ICON_ROWS: &str = "M4 5h16v14H4zM4 10h16";
const ICON_GLOBE: &str = "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zm-9 9h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9S9.500 5.600 12 3z";
const ICON_DASH: &str = "M4 13h6V4H4zm10 7h6v-9h-6zM4 20h6v-3H4zM14 7h6V4h-6z";
const ICON_CHAT: &str = "M5 5h14a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-7l-5 4v-4H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z";
const ICON_BOOK: &str = "M5 4h10a3 3 0 0 1 3 3v13H8a3 3 0 0 1-3-3zm0 13a3 3 0 0 1 3-3h10";
const ICON_TOOL: &str = "M14.5 6.500a4 4 0 0 0-5.200 5.200L3.500 17.500l3 3 5.800-5.800a4 4 0 0 0 5.200-5.200l-2.700 2.700-2.300-.700-.700-2.300z";

/// Light or dark, side by side or stacked, and the language: three small controls.
/// The typefaces the page can be set in (the size chosen is the conversation's alone): the code kept in the preferences, the name shown (an empty
/// name is "the system's own", in the language chosen), the CSS family, and how much larger
/// it must be set to read as the others do. All of them are in the program
/// (assets/fonts, from Causewaybay Hacker and FruitNation Messenger); the browser fetches one only when it is chosen.
const FONTS: [(&str, &str, &str, f64); 55] = [
    ("pixel", "VT323", "VT323", 1.34),
    ("dgm", "NeoDunggeunmo", "NeoDGM", 1.06),
    ("dgmcode", "NeoDunggeunmo Code", "NeoDGMCode", 1.06),
    ("galmuri", "Galmuri 11", "Galmuri11", 1.0),
    ("galmurimono", "Galmuri Mono 11", "GalmuriMono11", 1.0),
    ("dosvga", "IBM VGA 8x16", "DOSVGA", 1.12),
    ("apple2", "Apple II", "AppleIIPR3", 1.0),
    ("msx", "MSX1", "MSX1Code", 1.0),
    ("topaz", "Amiga Topaz", "TopazUnicode", 1.0),
    ("unifont", "GNU Unifont", "Unifont", 1.12),
    ("press", "Press Start 2P", "PressStart2P", 0.72),
    ("mono", "JetBrains Mono", "JetBrainsMonoCode", 0.92),
    ("iosevka", "Iosevka", "IosevkaCode", 1.0),
    ("plexmono", "IBM Plex Mono", "PlexMono", 0.92),
    ("victor", "Victor Mono", "VictorMono", 0.92),
    ("victorscript", "Victor Mono Italic", "VictorMonoScript", 0.92),
    ("fantasque", "Fantasque Sans Mono", "FantasqueSansMono", 1.0),
    ("radon", "Monaspace Radon", "MonaspaceRadon", 0.92),
    ("comic", "Comic Mono", "ComicMono", 0.95),
    ("readable", "Atkinson Hyperlegible", "Atkinson", 1.0),
    ("nunito", "Nunito Sans", "NunitoSans", 1.0),
    ("dmsans", "DM Sans", "DMSans", 1.0),
    ("geist", "Geist", "Geist", 1.0),
    ("plexsans", "IBM Plex Sans", "PlexSans", 1.0),
    ("instrument", "Instrument Sans", "InstrumentSans", 1.0),
    ("grotesk", "Space Grotesk", "SpaceGrotesk", 1.0),
    ("lora", "Lora", "Lora", 1.0),
    ("fraunces", "Fraunces", "Fraunces", 1.0),
    ("newsreader", "Newsreader", "Newsreader", 1.0),
    ("notocjk", "Noto Sans CJK", "NotoSansCJK", 1.0),
    ("petme", "Commodore 64 (Pet Me)", "PetMe64", 0.8),
    ("ibmega", "IBM EGA 8x14", "IBMEGA", 1.12),
    ("ibmcga", "IBM CGA", "IBMCGA", 0.8),
    ("ibmmda", "IBM MDA", "IBMMDA", 1.12),
    ("tandy", "Tandy 1000", "Tandy1000", 0.8),
    ("amstrad", "Amstrad PC", "AmstradPC", 0.8),
    ("ibm3270", "IBM 3270", "IBM3270", 1.12),
    ("cousine", "Cousine Bold", "CousineBold", 0.92),
    ("fira", "Fira Code", "FiraCode", 0.92),
    ("sourcecode", "Source Code Pro", "SourceCodePro", 0.92),
    ("cascadia", "Cascadia Code", "CascadiaCode", 0.92),
    ("hack", "Hack", "Hack", 0.92),
    ("inconsolata", "Inconsolata", "Inconsolata", 1.0),
    ("robotomono", "Roboto Mono", "RobotoMono", 0.92),
    ("ubuntumono", "Ubuntu Mono", "UbuntuMono", 1.0),
    ("spacemono", "Space Mono", "SpaceMono", 0.92),
    ("nanumgothic", "Nanum Gothic", "NanumGothic", 1.0),
    ("nanummyeongjo", "Nanum Myeongjo", "NanumMyeongjo", 1.0),
    ("nunitoround", "Nunito", "Nunito", 1.0),
    ("fredoka", "Fredoka", "Fredoka", 1.0),
    ("baloo", "Baloo 2", "Baloo2", 1.0),
    ("jua", "Jua", "Jua", 1.0),
    ("gaegu", "Gaegu", "Gaegu", 1.1),
    ("dohyeon", "Do Hyeon", "DoHyeon", 1.0),
    ("system", "", "", 1.0),
];
/// What stands in where a face has no glyph: the Korean pixel face, then the system's.
const FONT_FALLBACK: &str = "\"NeoDGM\", -apple-system, BlinkMacSystemFont, \"Segoe UI\", \"Apple SD Gothic Neo\", \"Hiragino Sans\", \"PingFang HK\", \"PingFang SC\", sans-serif";
const SIZES: [&str; 4] = ["s", "m", "l", "xl"];

#[component]
fn Prefs() -> impl IntoView {
    let app = expect_context::<App>();
    let flip = move |signal: RwSignal<String>, a: &'static str, b: &'static str| {
        // The page dips, changes, and comes back.
        let apply = Closure::once_into_js(move || signal.update(|v| *v = if v == a { b.to_string() } else { a.to_string() }));
        motion::retheme(apply.unchecked_ref());
    };
    view! {
        <div class="prefs">
            <button class="ghost round" id="theme-toggle" title=app.t("theme") on:click=move |_| flip(app.theme, "dark", "light")>
                {move || if app.theme.get() == "dark" { icon(ICON_SUN).into_any() } else { icon(ICON_MOON).into_any() }}
            </button>
            <button class="ghost round" id="layout-toggle" title=app.t("layout") on:click=move |_| flip(app.layout, "horizontal", "vertical")>
                {move || if app.layout.get() == "vertical" { icon(ICON_COLUMNS).into_any() } else { icon(ICON_ROWS).into_any() }}
            </button>
            <label class="lang" title=app.t("font")>
                <select id="font" on:change=move |ev| app.font.set(event_target_value(&ev)) prop:value=move || app.font.get()>
                    {FONTS.into_iter().map(|(code, name, ..)| view! { <option value=code selected=move || app.font.get() == code>{move || if name.is_empty() { tr(app.lang.get(), "font_system").to_string() } else { name.to_string() }}</option> }).collect_view()}
                </select>
            </label>
            <button class="ghost round" id="font-smaller" title=app.t("font_smaller") disabled=move || app.size.get() == SIZES[0] on:click=move |_| step_size(app, -1)>"A-"</button>
            <span class="fontsize" id="font-size">{move || app.size.get().to_uppercase()}</span>
            <button class="ghost round" id="font-larger" title=app.t("font_larger") disabled=move || app.size.get() == SIZES[SIZES.len() - 1] on:click=move |_| step_size(app, 1)>"A+"</button>
            <label class="lang" title=app.t("language")>
                {icon(ICON_GLOBE)}
                <select id="lang" on:change=move |ev| if let Some(lang) = Lang::from_code(&event_target_value(&ev)) { app.lang.set(lang) } prop:value=move || app.lang.get().code()>
                    {Lang::ALL.into_iter().map(|lang| view! { <option value=lang.code() selected=move || app.lang.get() == lang>{lang.name()}</option> }).collect_view()}
                </select>
            </label>
        </div>
    }
}

/// One size smaller or larger, within what there is.
fn step_size(app: App, by: i32) {
    let now = SIZES.iter().position(|s| *s == app.size.get_untracked()).unwrap_or(1) as i32;
    app.size.set(SIZES[(now + by).clamp(0, SIZES.len() as i32 - 1) as usize].to_string());
}

/// The bar across the top: the sidebar's switch, the name, the views, the backend's state,
/// settings and signing out.
#[component]
fn AppBar() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    // (Named for screen readers and in a tip, also when the words are hidden on a narrow
    // screen. Chat carries a badge while an edit or command waits for an answer.)
    let nav = move |view: View, id: &'static str, key: &'static str, shape: &'static str| {
        let badge = move || view == View::Chat && app.waiting() != 0 && app.view.get() != View::Chat;
        view! {
            <button id=id class:on=move || app.view.get() == view title=t(key) aria-label=t(key) on:click=move |_| app.view.set(view)>
                {icon(shape)}<span>{t(key)}</span>
                <Show when=badge>
                    <b class="badge" id="waiting-badge">{move || app.waiting()}</b>
                </Show>
            </button>
        }
    };
    // The selection slides to the chosen view, also when the words change size.
    Effect::new(move |_| {
        app.view.get();
        app.layout.get();
        app.lang.get();
        app.font.get();
        app.size.get();
        app.side.get();
        after_draw(|| motion::slide_selection("#nav"));
    });
    view! {
        <header class="appbar glass">
            <button class="ghost round" id="side-toggle" title=t("sidebar") aria-label=t("sidebar") on:click=move |_| app.side.update(|shown| *shown = !*shown)>{icon(ICON_SIDE)}</button>
            {brand(app)}
            <div class="nav seg" id="nav">
                <div class="selection"></div>
                {nav(View::Dashboard, "nav-dashboard", "nav_dashboard", ICON_DASH)}
                {nav(View::Chat, "nav-chat", "nav_chat", ICON_CHAT)}
                {nav(View::Knowledge, "nav-knowledge", "nav_knowledge", ICON_BOOK)}
                {nav(View::Tools, "nav-tools", "nav_tools", ICON_TOOL)}
            </div>
            <span class="grow"></span>
            <div class="pill" id="status-pill">
                    {move || match app.status.get() {
                        None => view! { <span><span class="dot bad"></span>{t("offline")}</span> }.into_any(),
                        Some(s) => {
                            // An answer the agent waits for comes before everything else.
                            let waiting = app.waiting() > 0;
                            let dot = if waiting { "dot ask" } else if s.busy { "dot busy" } else if s.model_state == "ready" || s.model_state == "scripted" { "dot ok" } else if s.model_state.starts_with("failed") { "dot bad" } else { "dot warn" };
                            let lang = app.lang.get();
                            let state = if waiting { tr(lang, "waiting_you").to_string() } else if s.busy { tr(lang, "working").to_string() } else { s.model_state.clone() };
                            view! {
                                <span class:asking=waiting><span class=dot></span>{state}" · "{s.protocol.clone()}</span>
                                <div id="login-state">{match s.auth.as_str() {
                                    "open" => tr(lang, "local_short").to_string(),
                                    "wallet" => format!("wallet {}…{}", s.wallet.get(..6).unwrap_or(""), s.wallet.get(s.wallet.len().saturating_sub(4)..).unwrap_or("")),
                                    _ => "token".to_string(),
                                }}</div>
                            }.into_any()
                        }
                    }}
                </div>
            <button class="ghost round" id="settings" title=t("settings") aria-label=t("settings") on:click=move |_| app.settings.update(|open| *open = !*open)>{icon(ICON_GEAR)}</button>
            <Show when=move || app.auth.get() != "open">
                <button class="ghost" id="signout" on:click=move |_| spawn_local(async move { app.sign_out().await })>{t("sign_out")}</button>
            </Show>
        </header>
    }
}

fn brand(app: App) -> impl IntoView {
    view! { <div class="brand"><div class="logo">"CC"</div><div>"Causewaybay Code"<small>{app.t("brand_sub")}</small></div></div> }
}

#[component]
fn Login() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let typed = RwSignal::new(String::new());
    let index = RwSignal::new(String::new());
    let problem = RwSignal::new(String::new());
    let working = RwSignal::new(false);
    let shown = RwSignal::new(false);
    let keep = RwSignal::new(true);
    let enter_token = move || {
        let token = typed.get_untracked().trim().to_string();
        if !token.is_empty() {
            app.remember(&token, true);
        }
    };
    let enter_phrase = move || {
        let phrase = typed.get_untracked();
        if phrase.trim().is_empty() || working.get_untracked() {
            return;
        }
        let account: u32 = index.get_untracked().trim().parse().unwrap_or(0);
        working.set(true);
        problem.set(String::new());
        spawn_local(async move {
            match app.sign_in(phrase, account, keep.get_untracked()).await {
                Ok(()) => typed.set(String::new()),
                Err(e) => problem.set(e),
            }
            working.set(false);
        });
    };
    let card = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = card.get() {
            motion::sheet(&el);
        }
    });
    view! {
        <div class="login">
            <div class="login-prefs"><Prefs/></div>
            <div class="card glass" node_ref=card>
                {move || match app.auth.get().as_str() {
                    "" => view! { {brand(app)}<p id="connecting">{t("connecting")}</p> }.into_any(),
                    "wallet" => view! {
                        {brand(app)}
                        <p>{t("login_belongs")}</p>
                        <code id="backend-wallet" style="word-break:break-all">{move || app.wallet.get()}</code>
                        <p>{t("login_explain")}</p>
                        <textarea id="phrase" rows="3" autocomplete="off" autocapitalize="off" spellcheck="false" placeholder=t("phrase_ph")
                            class:masked=move || !shown.get()
                            prop:value=move || typed.get() on:input=move |ev| typed.set(event_target_value(&ev))
                            on:keydown=move |ev| if ev.key() == "Enter" && !ev.shift_key() { ev.prevent_default(); enter_phrase() }></textarea>
                        <div class="rowf wrap">
                            <label class="chip"><input type="checkbox" on:change=move |ev| shown.set(event_target_checked(&ev))/>{t("show")}</label>
                            <label class="chip"><input type="checkbox" id="remember" prop:checked=move || keep.get() on:change=move |ev| keep.set(event_target_checked(&ev))/>{t("remember")}</label>
                            <span class="sub">{t("account")}</span>
                            <input id="phrase-index" style="width:64px;flex:0" inputmode="numeric" placeholder="0" prop:value=move || index.get() on:input=move |ev| index.set(event_target_value(&ev))/>
                        </div>
                        <Show when=move || !problem.get().is_empty()><div class="note bad" id="login-problem" style="text-align:left">{move || problem.get()}</div></Show>
                        <button class="primary" id="signin" disabled=move || working.get() on:click=move |_| enter_phrase()>{move || if working.get() { tr(app.lang.get(), "signing_in") } else { tr(app.lang.get(), "sign_in") }}</button>
                        <p class="sub fine">{t("cert_note")}" "<span style="word-break:break-all">{move || app.fingerprint.get()}</span></p>
                        <details class="trust"><summary class="sub">{t("trust_title")}</summary><TrustDevice/></details>
                    }.into_any(),
                    _ => view! {
                        {brand(app)}
                        <p>{t("token_explain")}</p>
                        <input id="token" type="password" placeholder="token" prop:value=move || typed.get() on:input=move |ev| typed.set(event_target_value(&ev))
                            on:keydown=move |ev| if ev.key() == "Enter" { enter_token() }/>
                        <button class="primary" id="signin" on:click=move |_| enter_token()>{t("connect")}</button>
                    }.into_any(),
                }}
            </div>
        </div>
    }
}

/// How to make this device trust the backend: install its certificate authority once. Then
/// the browser shows no warning, and the page can be installed as an app.
#[component]
fn TrustDevice() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    view! {
        <div class="sub trust-steps">
            <p>{t("trust_why")}</p>
            <a class="button ghost" id="trust-download" href="/ca.crt">{t("trust_download")}</a>
            // What it is, to compare before trusting it: this page could be another's, but the
            // line `cwbcode url` prints on the backend's own machine is not.
            <p>{t("trust_compare")}</p>
            <code id="trust-fingerprint" style="word-break:break-all">{move || app.ca_fingerprint.get()}</code>
            <p>{t("trust_ios")}</p>
            <p>{t("trust_mac")}</p>
        </div>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let path = RwSignal::new(String::new());
    let nav = move |view: View, id: &'static str, key: &'static str, shape: &'static str| {
        view! { <button id=id class:on=move || app.view.get() == view on:click=move |_| app.view.set(view)>{icon(shape)}<span>{t(key)}</span></button> }
    };
    // The selection slides to the chosen view, also when the layout or the words change size.
    Effect::new(move |_| {
        app.view.get();
        app.layout.get();
        app.lang.get();
        app.font.get();
        app.size.get();
        app.side.get();
        after_draw(|| motion::slide_selection("#nav"));
    });
    let open = move || {
        // Projects live under the home folder: what is typed is a path under it, however it
        // was written (`x`, `~/x`, or the whole path).
        let typed = path.get_untracked().trim().to_string();
        if !typed.is_empty() {
            path.set(String::new());
            let home = app.status.get_untracked().map(|s| s.user_home).unwrap_or_default();
            let under = match typed.strip_prefix(home.as_str()).filter(|rest| !home.is_empty() && (rest.is_empty() || rest.starts_with('/'))) {
                Some(rest) => format!("~{rest}"),
                None if typed == "~" || typed.starts_with("~/") || typed.starts_with('/') => typed,
                None => format!("~/{typed}"),
            };
            spawn_local(async move { app.open_project(under).await });
        }
    };
    let side = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = side.get() {
            motion::slide_in(&el);
        }
    });
    view! {
        <div class="side glass" node_ref=side>
            <div class="group">
                <h4>{t("projects")}</h4>
                <div class="open">
                    <span class="prefix">"~/"</span>
                    <input id="open-path" placeholder=t("open_ph") prop:value=move || path.get() on:input=move |ev| path.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { open() }/>
                    <button id="open-go" on:click=move |_| open()>{t("open")}</button>
                </div>
                <div class="list projects">
                    <For each=move || app.projects.get() key=|p| p.path.clone() children=move |p: Project| {
                        let gone = p.path.clone();
                        let (selected, shown) = (p.path.clone(), p.path.clone());
                        let target = StoredValue::new(p.path.clone());
                        let pick = move || {
                            let path = target.get_value();
                            app.chosen();
                            spawn_local(async move { app.select_project(path).await; app.view.set(View::Chat); })
                        };
                        view! {
                            // (Reached with Tab, and opened with Enter or Space too.)
                            <div class="row project" class:on=move || app.project.get() == selected tabindex="0" role="button"
                                on:click=move |_| pick() on:keydown=move |ev| if activates(&ev) { ev.prevent_default(); pick() }>
                                <span class="what">{p.name.clone()}<small>{move || app.tilde(&shown)}</small></span>
                                <button class="ghost round remove" title=t("remove_project") aria-label=t("remove_project")
                                    on:click=move |ev| { ev.stop_propagation(); let path = gone.clone(); spawn_local(async move { app.remove_project(path).await }) }
                                    on:keydown=move |ev| ev.stop_propagation()>"×"</button>
                            </div>
                        }
                    }/>
                </div>
            </div>
            <div class="group grow">
                <h4>{t("conversations")}<button class="ghost" id="new-chat" on:click=move |_| { app.new_conversation(); app.view.set(View::Chat); app.chosen(); }>{t("new")}</button></h4>
                <div class="list sessions">
                    <For each=move || app.sessions.get() key=|s| (s.id, s.name.clone(), s.messages) children=move |s: Session| {
                        let id = s.id;
                        let label = if s.name.is_empty() { s.first.clone() } else { s.name.clone() };
                        view! {
                            <div class="row session" class:on=move || app.session.get() == id tabindex="0" role="button"
                                on:click=move |_| { app.chosen(); spawn_local(async move { app.select_session(id).await }) }
                                on:keydown=move |ev| if activates(&ev) { ev.prevent_default(); app.chosen(); spawn_local(async move { app.select_session(id).await }) }>
                                {if label.is_empty() { format!("#{id}") } else { label }}
                                <small>{format!("#{id} · {} · {}", s.updated.get(5..16).unwrap_or(""), s.messages)}</small>
                            </div>
                        }
                    }/>
                </div>
            </div>
</div>
    }
}

/// Is the model being downloaded, or to be: what the chat view shows instead of its empty
/// page (and the dashboard on the model's card). Also just after a download, while it loads.
fn model_coming(s: &Status) -> bool {
    s.model_state == "downloading" || s.model_state == "missing" || (s.model_state == "loading" && !s.download_note.is_empty())
}

/// "1:02:03", "12:34": how long, in hours, minutes and seconds.
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// The model's download: how far it is (a bar that fills, with the bytes, the speed, the time
/// left and where it comes from), or, when the model is not here and nothing is downloading
/// it, a button that starts it, with why the last one stopped and a retry when one did.
/// `card`: the smaller one on the dashboard's model card.
#[component]
fn ModelDownload(card: bool) -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let s = move || app.status.get().unwrap_or_default();
    let starting = RwSignal::new(false);
    let start = move |_| {
        if starting.get_untracked() {
            return;
        }
        starting.set(true);
        spawn_local(async move {
            app.call(Request::new(Method::DownloadModel)).await;
            app.refresh_status().await;
            starting.set(false);
        });
    };
    let share = move || {
        let s = s();
        if s.download_total == 0 { 0.0 } else { (s.download_done as f64 / s.download_total as f64 * 100.0).clamp(0.0, 100.0) }
    };
    let left = move || {
        let s = s();
        if s.download_speed > 1.0 && s.download_total > s.download_done { tr_with(app.lang.get(), "dl_left", &clock((s.download_total - s.download_done) as f64 / s.download_speed)) } else { tr(app.lang.get(), "dl_left_unknown").to_string() }
    };
    let bytes = move || {
        let s = s();
        let total = if s.download_total == 0 { "…".to_string() } else { format!("{:.1} GB", gb(s.download_total)) };
        tr(app.lang.get(), "dl_bytes").replacen("{}", &format!("{:.1} GB", gb(s.download_done)), 1).replacen("{}", &total, 1)
    };
    let id = if card { "model-download-card" } else { "model-download" };
    view! {
        <div class="download" class:card=card class:glass=!card id=id>
            {move || {
                let st = s();
                if st.model_state == "downloading" {
                    view! {
                        <div class="dl-head">
                            <Show when=move || !card><img src="/art/empty.png" alt=""/></Show>
                            <div>
                                <h3>{t("dl_title")}</h3>
                                <Show when=move || !card><p class="dim">{t("dl_why")}</p></Show>
                            </div>
                        </div>
                        <div class="dl-bar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow=move || format!("{:.0}", share()) aria-label=t("dl_title")>
                            <i style:width=move || format!("{:.2}%", share())></i>
                        </div>
                        <div class="dl-stats">
                            <b class="dl-percent" id="dl-percent">{move || format!("{:.1}%", share())}</b>
                            <span id="dl-bytes">{bytes}</span>
                            <span id="dl-speed">{move || if s().download_speed > 0.0 { format!("{:.1} MB/s", s().download_speed / 1e6) } else { "— MB/s".to_string() }}</span>
                            <span id="dl-left">{left}</span>
                        </div>
                        <div class="dl-from sub" id="dl-source">{move || tr_with(app.lang.get(), "dl_from", &s().download_source)}{move || if s().download_note.is_empty() { String::new() } else { format!(" · {}", s().download_note) }}</div>
                    }.into_any()
                } else if st.model_state == "loading" {
                    view! {
                        <div class="dl-head"><div>
                            <h3>{t("dl_loading_title")}</h3>
                            <p class="dim" id="dl-done">{move || s().download_note}</p>
                            <p class="dim">{t("dl_loading")}</p>
                        </div></div>
                        <div class="dl-bar done"><i style="width:100%"></i></div>
                    }.into_any()
                } else if !st.download_error.is_empty() {
                    view! {
                        <div class="dl-head"><div>
                            <h3>{t("dl_failed_title")}</h3>
                            <p class="note bad" id="dl-error">{move || s().download_error}</p>
                            <p class="dim">{t("dl_failed")}</p>
                        </div></div>
                        <div class="dl-acts"><button class="primary" id="dl-retry" disabled=move || starting.get() on:click=start>{t("dl_retry")}</button></div>
                    }.into_any()
                } else {
                    view! {
                        <div class="dl-head">
                            <Show when=move || !card><img src="/art/empty.png" alt=""/></Show>
                            <div>
                                <h3>{t("dl_missing_title")}</h3>
                                <p class="dim">{t("dl_missing")}</p>
                            </div>
                        </div>
                        <div class="dl-acts"><button class="primary" id="dl-start" disabled=move || starting.get() on:click=start>{t("dl_button")}</button></div>
                    }.into_any()
                }
            }}
        </div>
    }
}

/// A number that counts to its new value.
#[component]
fn Counter(value: Signal<f64>, decimals: u32, suffix: Signal<String>) -> impl IntoView {
    let node = NodeRef::<leptos::html::Span>::new();
    Effect::new(move |_| {
        let (v, suffix) = (value.get(), suffix.get());
        if let Some(el) = node.get() {
            motion::count_to(&el, v, decimals, &suffix);
        }
    });
    view! { <span node_ref=node>"0"</span> }
}

#[component]
fn Dashboard() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    Effect::new(move |_| {
        spawn_local(async move { app.refresh_projects().await });
        motion::enter_all(".cards .card");
    });
    let s = move || app.status.get().unwrap_or_default();
    let context_share = move || {
        let s = s();
        if s.context_limit == 0 { 0.0 } else { (s.context_tokens as f64 / s.context_limit as f64 * 100.0).min(100.0) }
    };
    let fixed = |text: &'static str| Signal::derive(move || text.to_string());
    view! {
        <div class="top"><h2>{t("nav_dashboard")}<small>{move || app.tilde(&s().home)}</small></h2></div>
        <div class="scroll">
            <div class="col wide">
                <div class="cards">
                    // The model: its state, its name, what it is, and where its files are and how
                    // large (the last two once it is loaded).
                    <div class="card glass model-card" id="model-card"><h5>{t("d_model")}</h5><div class="big" id="model-state">{move || if app.status.get().is_none() { tr(app.lang.get(), "d_offline").to_string() } else if s().busy { tr(app.lang.get(), "working").to_string() } else { s().model_state }}</div>
                        <div class="sub" id="model-name">{move || s().model}</div>
                        <Show when=move || !s().model_info.is_empty()>
                            <div class="sub" id="model-info">{move || s().model_info}</div>
                        </Show>
                        <Show when=move || !s().model_path.is_empty()>
                            <div class="sub" id="model-where">{move || format!("{} · {:.1} GB", app.tilde(&s().model_path), s().model_bytes as f64 / 1e9)}</div>
                        </Show>
                        // The model's download, or the button that starts it.
                        <Show when=move || app.status.get().is_some_and(|s| model_coming(&s))>
                            <ModelDownload card=true/>
                        </Show></div>
                    // How fast it writes, and reads a prompt, in the last turn.
                    <div class="card glass" id="speed-card"><h5>{t("d_speed")}</h5>
                        <div class="big" id="gen-tps"><Counter value=Signal::derive(move || s().gen_tps) decimals=1 suffix=fixed(" tok/s")/></div>
                        <div class="sub" id="prompt-tps">{move || if s().gen_tps == 0.0 { tr(app.lang.get(), "speed_none").to_string() } else if s().prompt_tps == 0.0 { tr(app.lang.get(), "speed_last").to_string() } else { tr_with(app.lang.get(), "speed_read", &format!("{:.0}", s().prompt_tps)) }}</div></div>
                    <div class="card glass"><h5>{t("d_memory")}</h5><div class="big"><Counter value=Signal::derive(move || gb(s().memory_active)) decimals=1 suffix=fixed(" GB")/></div>
                        <div class="sub">{move || tr_with(app.lang.get(), "peak", &format!("{:.1}", gb(s().memory_peak)))}</div>
                        <div class="bar2"><i style:width=move || format!("{:.0}%", (gb(s().memory_active) / 36.0 * 100.0).min(100.0))></i></div></div>
                    <div class="card glass"><h5>{t("d_context")}</h5><div class="big"><Counter value=Signal::derive(move || s().context_tokens as f64) decimals=0 suffix=fixed("")/></div>
                        <div class="sub">{move || tr_with(app.lang.get(), "ctx_sub", &s().context_limit.to_string())}</div>
                        <div class="bar2"><i style:width=move || format!("{:.0}%", context_share())></i></div></div>
                    <div class="card glass"><h5>{t("d_transport")}</h5><div class="big" id="protocol">{move || s().protocol.to_uppercase()}</div>
                        <div class="sub">{move || if s().h3 { tr(app.lang.get(), "transport_both") } else { tr(app.lang.get(), "transport_h2") }}</div></div>
                    <div class="card glass"><h5>{t("d_safety")}</h5><div class="big small">{move || s().sandbox}</div>
                        <div class="sub">{move || tr(app.lang.get(), match s().auth.as_str() { "open" => "safety_open", "wallet" => "safety_wallet", _ => "safety_token" })}</div></div>
                    <div class="card glass"><h5>{t("d_uptime")}</h5><div class="big"><Counter value=Signal::derive(move || s().uptime_seconds as f64 / 60.0) decimals=0 suffix=Signal::derive(move || tr(app.lang.get(), "min").to_string())/></div>
                        <div class="sub">{move || format!("cwbcode {} · {}", s().version, tr_with(app.lang.get(), "waiting", &s().queued.to_string()))}</div></div>
                </div>
                <RemoteAccess/>
                <div class="card glass" id="trust"><h5>{t("trust_title")}</h5><TrustDevice/></div>
                <div class="card glass">
                    <h5>{t("projects")}</h5>
                    <Show when=move || !app.projects.get().is_empty() fallback=move || view! { <div class="empty">{t("no_projects")}</div> }>
                        <table>
                            <thead><tr><th>{t("th_project")}</th><th>{t("th_folder")}</th><th>{t("conversations")}</th><th>{t("th_opened")}</th></tr></thead>
                            <tbody>
                                <For each=move || app.projects.get() key=|p| (p.path.clone(), p.sessions) children=move |p: Project| {
                                    let target = StoredValue::new(p.path.clone());
                                    let pick = move || { let path = target.get_value(); spawn_local(async move { app.select_project(path).await; app.view.set(View::Chat); }) };
                                    view! {
                                        <tr class="click" tabindex="0" role="button" on:click=move |_| pick() on:keydown=move |ev| if activates(&ev) { ev.prevent_default(); pick() }>
                                            <td>{p.name.clone()}</td><td class="dim">{let shown = p.path.clone(); move || app.tilde(&shown)}</td><td>{p.sessions}</td><td class="dim">{p.updated.clone()}</td>
                                        </tr>
                                    }
                                }/>
                            </tbody>
                        </table>
                    </Show>
                </div>
            </div>
        </div>
    }
}

/// Remote mode: in local mode, set a wallet to open the backend to other machines; in
/// remote mode, see it and turn it off.
#[component]
fn RemoteAccess() -> impl IntoView {
    use cwbcode_proto::wallet;
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let typed = RwSignal::new(String::new());
    let problem = RwSignal::new(String::new());
    let working = RwSignal::new(false);
    // What was typed is an address, or a recovery phrase whose address is worked out here.
    let address = move || -> Result<String, String> {
        let text = typed.get();
        let text = text.trim();
        if text.split_whitespace().count() > 11 {
            wallet::address(&wallet::key_from_phrase(text, 0)?)
        } else if text.len() == 42 && text.starts_with("0x") && text[2..].chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(text.to_string())
        } else {
            Err(tr(app.lang.get(), "remote_bad").to_string())
        }
    };
    let apply = move |value: String| {
        working.set(true);
        problem.set(String::new());
        spawn_local(async move {
            if let Err(e) = app.setup(value).await {
                problem.set(e);
            }
            working.set(false);
        });
    };
    let turn_on = move || match address() {
        Ok(address) => {
            typed.set(String::new());
            apply(address)
        }
        Err(e) => problem.set(e),
    };
    let s = move || app.status.get().unwrap_or_default();
    let looks_like_phrase = move || typed.get().split_whitespace().count() > 11;
    // (More than one word is a recovery phrase being typed: not shown in the clear.)
    let several_words = move || typed.get().split_whitespace().count() > 1;
    view! {
        <div class="card glass form" id="remote">
            <h5>{t("remote_title")}</h5>
            {move || match app.auth.get().as_str() {
                "open" => view! {
                    <div><b>{t("local_mode")}</b>{t("local_desc")}<span class="sub">{move || tr_with(app.lang.get(), "listening", &s().bind)}</span></div>
                    <div class="sub">{t("remote_explain")}</div>
                    <div class="rowf">
                        <input id="remote-wallet" autocomplete="off" spellcheck="false" placeholder=t("remote_ph")
                            class:masked=several_words
                            prop:value=move || typed.get() on:input=move |ev| { problem.set(String::new()); typed.set(event_target_value(&ev)) } on:keydown=move |ev| if ev.key() == "Enter" { turn_on() }/>
                        <button class="primary fit" id="remote-on" disabled=move || working.get() on:click=move |_| turn_on()>{t("remote_on")}</button>
                    </div>
                    <Show when=looks_like_phrase>
                        <div class="sub" id="remote-derived">{move || match address() { Ok(a) => tr_with(app.lang.get(), "remote_derived", &a), Err(e) => e }}</div>
                    </Show>
                }.into_any(),
                "wallet" => view! {
                    <div><b>{t("remote_mode")}</b>" · "<code id="remote-wallet-shown">{move || app.wallet.get()}</code></div>
                    <div class="sub">{move || tr_with(app.lang.get(), "remote_sub", &s().bind)}</div>
                    {move || if s().wallet_source == "settings" {
                        view! { <div class="rowf"><button class="danger fit" id="remote-off" disabled=move || working.get() on:click=move |_| apply(String::new())>{t("remote_off")}</button><span></span></div> }.into_any()
                    } else {
                        view! { <div class="sub">{move || tr_with(app.lang.get(), "remote_fixed", &s().wallet_source)}</div> }.into_any()
                    }}
                }.into_any(),
                _ => view! { <div class="sub">{t("token_explain")}</div> }.into_any(),
            }}
            <Show when=move || !problem.get().is_empty()><div class="note bad" id="remote-problem" style="text-align:left">{move || problem.get()}</div></Show>
        </div>
    }
}

/// An answer's text: plain, with fenced code as code.
fn rendered(text: String) -> impl IntoView {
    text.split("```")
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                // The first line of a fence names the language.
                let code = part.split_once('\n').map_or(part, |(_, rest)| rest).trim_end().to_string();
                view! { <pre>{code}</pre> }.into_any()
            } else {
                view! { <span>{part.trim_matches('\n').to_string()}</span> }.into_any()
            }
        })
        .collect_view()
}

#[component]
fn ItemView(item: Item) -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let node = NodeRef::<leptos::html::Div>::new();
    let (kind, past) = (item.kind, item.past);
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            if !past {
                if kind == Kind::Approval {
                    motion::sheet(&el);
                    motion::attention(&el);
                } else {
                    motion::enter(&el, 0.0);
                }
            }
        }
    });
    let text = item.text;
    let body = match item.kind {
        Kind::User => view! { <div class="msg user">{move || text.get()}</div> }.into_any(),
        Kind::Thinking => view! { <div class="msg thinking">{move || text.get()}</div> }.into_any(),
        Kind::Text => view! { <div class="msg text glass">{move || rendered(text.get())}</div> }.into_any(),
        Kind::Note => view! { <div class="note">{move || text.get()}</div> }.into_any(),
        Kind::Warning => view! { <div class="note warn">{move || text.get()}</div> }.into_any(),
        Kind::Error => view! { <div class="note bad">{move || text.get()}</div> }.into_any(),
        Kind::Tool => {
            let result = item.result;
            view! {
                <details class="tool glass" class:bad=move || result.get().is_some_and(|r| !r.0)>
                    <summary>
                        <span class=move || match result.get() { None => "dot busy", Some((true, ..)) => "dot ok", Some((false, ..)) => "dot bad" }></span>
                        <span class="t">{move || text.get()}</span>
                        <small>{move || result.get().map(|r| if r.2 >= 0.1 { format!("{:.1}s", r.2) } else { String::new() })}</small>
                    </summary>
                    <pre>{move || result.get().map(|r| r.1).unwrap_or_else(|| tr(app.lang.get(), "running").to_string())}</pre>
                </details>
            }
            .into_any()
        }
        Kind::Approval => {
            let (id, offer_always) = item.approval.unwrap_or((0, false));
            let answered = item.answered;
            let instead = RwSignal::new(String::new());
            let answer = move |text: String| {
                answered.set(text.clone());
                spawn_local(async move {
                    app.call(Request::new(Method::Approve).id(id).text(&text)).await;
                });
            };
            // A new question is scrolled into sight, Allow and No included, on a phone too; one
            // answered already is not, when the conversation is shown again. (Read untracked:
            // answering it is not a reason to scroll.)
            let card = NodeRef::<leptos::html::Div>::new();
            Effect::new(move |_| {
                if let Some(el) = card.get() {
                    if !past && answered.with_untracked(String::is_empty) {
                        after_draw(move || el.scroll_into_view_with_bool(false));
                    }
                }
            });
            view! {
                <div class="approval" node_ref=card class:done=move || !answered.get().is_empty() || past>
                    <pre>{move || text.get()}</pre>
                    <Show when=move || answered.get().is_empty() && !past fallback=move || view! { <div class="note">{move || tr_with(app.lang.get(), "answered", &if answered.get().is_empty() { "—".to_string() } else { answered.get() })}</div> }>
                        <div class="acts">
                            <button class="primary approve-yes" on:click=move |_| answer("y".to_string())>{t("allow")}</button>
                            <Show when=move || offer_always><button class="approve-always" on:click=move |_| answer("a".to_string())>{t("always")}</button></Show>
                            <button class="danger approve-no" on:click=move |_| answer("n".to_string())>{t("no")}</button>
                            <input placeholder=t("instead_ph") prop:value=move || instead.get() on:input=move |ev| instead.set(event_target_value(&ev))
                                on:keydown=move |ev| if ev.key() == "Enter" && !instead.get_untracked().trim().is_empty() { answer(instead.get_untracked()) }/>
                        </div>
                    </Show>
                </div>
            }
            .into_any()
        }
    };
    view! { <div node_ref=node class="item">{body}</div> }
}

#[component]
fn Chat() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let draft = RwSignal::new(String::new());
    let attached = RwSignal::new(Vec::<Upload>::new());
    let scroller = NodeRef::<leptos::html::Div>::new();
    // Follow the conversation down as it grows.
    Effect::new(move |_| {
        app.grew.get();
        if let Some(el) = scroller.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });
    let send = move || {
        let text = draft.get_untracked().trim().to_string();
        if text.is_empty() || app.busy.get_untracked() {
            return;
        }
        draft.set(String::new());
        let images = attached.get_untracked();
        attached.set(Vec::new());
        spawn_local(async move { app.send(text, images).await });
    };
    let pick = move |ev: leptos::ev::Event| {
        let input: web_sys::HtmlInputElement = event_target(&ev);
        let Some(files) = input.files() else { return };
        for i in 0..files.length() {
            if let Some(file) = files.get(i) {
                spawn_local(async move {
                    match api::file_bytes(&file).await {
                        Ok(data) => attached.update(|a| a.push(Upload { name: file.name(), data })),
                        Err(e) => app.fail(e),
                    }
                });
            }
        }
        input.set_value("");
    };
    let act = move |method: Method| {
        spawn_local(async move {
            if let Some(response) = app.call(app.request(method).session(app.session.get_untracked())).await {
                if method == Method::Compact {
                    // What the context holds now is not known until the next turn says.
                    app.ctx.set(0);
                }
                app.apply(What::Note(response.text), false);
            }
        })
    };
    let todo_lines = move || {
        let todos: serde_json::Value = serde_json::from_str(&app.todos.get()).unwrap_or_default();
        todos.as_array().cloned().unwrap_or_default().into_iter().map(|t| (t["content"].as_str().unwrap_or("").to_string(), t["status"].as_str().unwrap_or("").to_string())).collect::<Vec<_>>()
    };
    view! {
        <div class="top">
            <h2 title=move || app.tilde(&app.project.get())>{move || { let p = app.project.get(); p.rsplit('/').next().unwrap_or("").to_string() }}<small>{move || if app.session.get() == 0 { tr(app.lang.get(), "new_conversation").to_string() } else { tr_with(app.lang.get(), "conversation_n", &app.session.get().to_string()) }}</small></h2>
            <div class="meter" id="ctx" title=t("context") class:warn=move || app.ctx_at(0.75) class:full=move || app.ctx_at(0.9)>
                <span>{t("context")}</span>
                <div class="bar2"><i style:width=move || format!("{:.0}%", app.context().2 * 100.0)></i></div>
                <span id="ctx-text">{move || { let (used, limit, _) = app.context(); if used == 0 { format!("– / {}", short_count(limit)) } else { format!("{} / {}", short_count(used), short_count(limit)) } }}</span>
            </div>
            <button class="primary" id="run" disabled=move || app.project.get().is_empty() || app.running.get() on:click=move |_| spawn_local(async move { app.run_code().await })>{move || if app.running.get() { t("run_running")() } else { t("run")() }}</button>
            <button id="console-toggle" class:lit=move || app.console.get() on:click=move |_| app.console.update(|shown| *shown = !*shown)>{t("console")}</button>
            <button id="clear" disabled=move || app.busy.get() on:click=move |_| { app.console_out.set(String::new()); app.new_conversation(); }>{t("clear")}</button>
            <button id="copy-all" disabled=move || app.items.get().is_empty() on:click=move |_| {
                // (Copied at once, in the tap itself: a browser lets nothing else at the clipboard.)
                let text = whole_text(&app.items.get_untracked());
                if motion::copy_text(&text) { app.tell(tr(app.lang.get_untracked(), "copied").to_string()) } else { app.fail(tr(app.lang.get_untracked(), "copy_failed").to_string()) }
            }>{t("copy_all")}</button>
            <button id="undo" disabled=move || app.session.get() == 0 || app.busy.get() on:click=move |_| act(Method::Undo)>{t("undo")}</button>
            <button id="compact" disabled=move || app.session.get() == 0 || app.busy.get() on:click=move |_| act(Method::Compact)>{t("summarize")}</button>
        </div>
        <Show when=move || app.ctx_at(0.9)>
            <div class="approval ctx-warning" id="ctx-warning">
                <span class="grow">{move || if app.ctx_at(1.0) { t("ctx_full")() } else { t("ctx_near")() }}</span>
                <button class="primary" id="ctx-new" on:click=move |_| app.new_conversation()>{t("new_conversation")}</button>
            </div>
        </Show>
        <div class="chatbody">
        <div class="scroll" node_ref=scroller id="transcript">
            <div class="col">
                // A model on its way (or to be fetched) comes first: without it nothing answers.
                <Show when=move || app.status.get().is_some_and(|s| model_coming(&s))>
                    <ModelDownload card=false/>
                </Show>
                <Show when=move || app.project.get().is_empty() && !app.status.get().is_some_and(|s| model_coming(&s))>
                    <div class="empty"><h3>{t("empty_open_title")}</h3>{t("empty_open")}</div>
                </Show>
                <Show when=move || !app.project.get().is_empty() && app.items.get().is_empty() && !app.status.get().is_some_and(|s| model_coming(&s))>
                    <div class="empty" id="empty-chat"><img src="/art/empty.png" alt=""/><h3>{t("empty_chat_title")}</h3>{move || app.tilde(&app.project.get())}</div>
                </Show>
                <For each=move || app.items.get() key=|item| item.id children=move |item: Item| { let id = item.id; view! { <div class="it" id=format!("item-{id}")><ItemView item=item/></div> } }/>
                <Show when=move || !todo_lines().is_empty()>
                    <div class="todos glass" id="todos">
                        {move || todo_lines().into_iter().map(|(content, status)| {
                            let (class, mark) = match status.as_str() { "completed" => ("done", "☒ "), "in_progress" => ("doing", "◐ "), _ => ("", "☐ ") };
                            view! { <div class=class>{mark}{content}</div> }
                        }).collect_view()}
                    </div>
                </Show>
            </div>
        </div>
        // The outline: one bar for each thing said or done; a tap goes there.
        <div class="outline glass" id="outline" aria-hidden="true">
            <div class="viewport" id="outline-view"></div>
            <For each=move || app.items.get() key=|item| item.id children=move |item: Item| {
                let class = match item.kind { Kind::User => "o user", Kind::Tool => "o tool", Kind::Approval => "o ask", Kind::Text => "o text", Kind::Error | Kind::Warning => "o bad", _ => "o quiet" };
                let text = item.text;
                view! { <i class=class style:width=move || format!("{}%", 25 + (text.get().len() / 12).min(75))></i> }
            }/>
        </div>
        </div>
        // The console: what running the code printed, and the command that is run.
        <Show when=move || app.console.get()>
            <div class="console glass" id="console">
                <div class="console-head">
                    <span class="lbl">{t("console")}</span>
                    <input id="console-cmd" placeholder=t("console_ph") prop:value=move || app.console_cmd.get() on:input=move |ev| app.console_cmd.set(event_target_value(&ev))
                        on:keydown=move |ev| if ev.key() == "Enter" { spawn_local(async move { app.run_code().await }) }/>
                    <button class="ghost" id="console-clear" on:click=move |_| app.console_out.set(String::new())>{t("clear")}</button>
                    <button class="ghost round" id="console-close" aria-label=t("console") on:click=move |_| app.console.set(false)>"×"</button>
                </div>
                <pre id="console-out">{move || { let out = app.console_out.get(); if out.is_empty() { t("console_empty")().to_string() } else { out } }}</pre>
            </div>
        </Show>
        <div class="composer">
            <div class="box glass">
                <textarea id="draft" rows="2" placeholder=t("draft_ph")
                    prop:value=move || draft.get() on:input=move |ev| draft.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" && !ev.shift_key() { ev.prevent_default(); send() }></textarea>
                <div class="bar">
                    <label class="chip">{t("image")}<input type="file" accept="image/*" multiple style="display:none" on:change=pick/></label>
                    <For each=move || attached.get() key=|u| (u.name.clone(), u.data.len()) children=move |u: Upload| view! { <span class="chip">{u.name.clone()}</span> }/>
                    <span class="sp"></span>
                    // (Auto, where nothing is asked first, is marked: it is the one to notice.)
                    <select id="mode" class:auto=move || app.mode.get() == "auto" title=t("mode_label") aria-label=t("mode_label")
                        on:change=move |ev| app.mode.set(event_target_value(&ev)) prop:value=move || app.mode.get()>
                        <option value="ask" selected=move || app.mode.get() == "ask">{t("mode_ask")}</option>
                        <option value="auto" selected=move || app.mode.get() == "auto">{t("mode_auto")}</option>
                        <option value="read-only" selected=move || app.mode.get() == "read-only">{t("mode_ro")}</option>
                        <option value="plan" selected=move || app.mode.get() == "plan">{t("mode_plan")}</option>
                    </select>
                    <select id="think" on:change=move |ev| app.think.set(event_target_value(&ev)) prop:value=move || app.think.get()>
                        <option value="">{t("think_default")}</option><option value="none">"none"</option><option value="low">"low"</option><option value="medium">"medium"</option><option value="high">"high"</option>
                    </select>
                    <Show when=move || app.busy.get() fallback=move || view! { <button class="primary" id="send" disabled=move || app.project.get().is_empty() || app.ctx_at(1.0) on:click=move |_| send()>{t("send")}</button> }>
                        <button class="danger" id="stop" on:click=move |_| spawn_local(async move { app.call(Request::new(Method::Interrupt)).await; })>{t("stop")}</button>
                    </Show>
                </div>
            </div>
        </div>
    }
}

/// 6638 -> "6.6k".
fn short_count(n: u64) -> String {
    if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() }
}

/// A conversation as text, to copy: what was asked, what was answered, what was run.
fn whole_text(items: &[Item]) -> String {
    let mut out = String::new();
    for item in items {
        let text = item.text.get_untracked();
        match item.kind {
            Kind::User => out.push_str(&format!("> {text}\n\n")),
            Kind::Text => out.push_str(&format!("{text}\n\n")),
            Kind::Tool => {
                out.push_str(&format!("[{text}]\n"));
                if let Some((_, output, _)) = item.result.get_untracked() {
                    if !output.trim().is_empty() {
                        out.push_str(&format!("{}\n", output.trim_end()));
                    }
                }
                out.push('\n');
            }
            _ => {}
        }
    }
    out.trim_end().to_string()
}

#[component]
fn Knowledge() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let docs = RwSignal::new(Vec::<Doc>::new());
    let hits = RwSignal::new(None::<Vec<Hit>>);
    let query = RwSignal::new(String::new());
    let (title, note) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let shown = RwSignal::new(None::<Doc>);
    let working = RwSignal::new(false);
    let load = move || {
        spawn_local(async move {
            if app.project.get_untracked().is_empty() {
                return;
            }
            if let Some(response) = app.call(app.request(Method::KbList)).await {
                docs.set(response.docs);
                motion::enter_all("#kb-list tr");
            }
        })
    };
    Effect::new(move |_| {
        app.project.get();
        load();
    });
    let search = move || {
        let text = query.get_untracked().trim().to_string();
        if text.is_empty() {
            hits.set(None);
            return;
        }
        working.set(true);
        spawn_local(async move {
            if let Some(response) = app.call(app.request(Method::KbSearch).text(&text)).await {
                hits.set(Some(response.hits));
                motion::enter_all("#kb-hits .card");
            }
            working.set(false);
        })
    };
    let save = move |mut request: Request| {
        working.set(true);
        spawn_local(async move {
            request.project = app.project.get_untracked();
            if app.call(request).await.is_some() {
                title.set(String::new());
                note.set(String::new());
                load();
            }
            working.set(false);
        })
    };
    let upload = move |ev: leptos::ev::Event| {
        let input: web_sys::HtmlInputElement = event_target(&ev);
        let Some(files) = input.files() else { return };
        for i in 0..files.length() {
            if let Some(file) = files.get(i) {
                spawn_local(async move {
                    match api::file_bytes(&file).await {
                        Ok(data) => {
                            let mut request = Request::new(Method::KbAdd);
                            request.files.push(Upload { name: file.name(), data });
                            save(request);
                        }
                        Err(e) => app.fail(e),
                    }
                });
            }
        }
        input.set_value("");
    };
    let open = move |id: i64| {
        spawn_local(async move {
            if let Some(response) = app.call(app.request(Method::KbGet).id(id)).await {
                shown.set(response.docs.into_iter().next());
            }
        })
    };
    let remove = move |id: i64| {
        spawn_local(async move {
            if app.call(app.request(Method::KbDelete).id(id)).await.is_some() {
                shown.set(None);
                load();
            }
        })
    };
    view! {
        <div class="top"><h2>{t("nav_knowledge")}<small>{move || app.tilde(&app.project.get())}</small></h2><span class="chip">{move || if working.get() { tr(app.lang.get(), "working_dots") } else { tr(app.lang.get(), "kb_chip") }}</span></div>
        <div class="scroll">
            <div class="col mid">
                <div class="card glass form">
                    <div class="rowf">
                        <input id="kb-query" placeholder=t("kb_search_ph") prop:value=move || query.get() on:input=move |ev| query.set(event_target_value(&ev)) on:keydown=move |ev| if ev.key() == "Enter" { search() }/>
                        <button class="fit" id="kb-search" on:click=move |_| search()>{t("search")}</button>
                    </div>
                </div>
                <Show when=move || hits.get().is_some()>
                    <div id="kb-hits" class="col">
                        {move || {
                            let found = hits.get().unwrap_or_default();
                            if found.is_empty() {
                                view! { <div class="note">{t("no_matches")}</div> }.into_any()
                            } else {
                                found.into_iter().map(|h| {
                                    let how = match (h.bm25_rank, h.semantic_rank) { (0, 0) => String::new(), (b, 0) => format!("bm25 #{b}"), (0, s) => format!("semantic #{s}"), (b, s) => format!("bm25 #{b} · semantic #{s}") };
                                    let id = h.doc;
                                    view! { <div class="card glass pressable" on:click=move |_| open(id)><h5>{format!("#{} {} · {how}", h.doc, h.kind)}</h5><b>{h.title}</b><div class="sub pre">{h.snippet}</div></div> }
                                }).collect_view().into_any()
                            }
                        }}
                    </div>
                </Show>
                <div class="card glass form">
                    <h5>{t("kb_save_title")}</h5>
                    <input id="kb-title" placeholder=t("kb_title_ph") prop:value=move || title.get() on:input=move |ev| title.set(event_target_value(&ev))/>
                    <textarea id="kb-note" rows="3" placeholder=t("kb_note_ph") prop:value=move || note.get() on:input=move |ev| note.set(event_target_value(&ev))></textarea>
                    <div class="rowf">
                        <button class="primary" id="kb-save" disabled=move || note.get().trim().is_empty() || working.get()
                            on:click=move |_| save(Request::new(Method::KbAdd).name(title.get_untracked().trim()).text(note.get_untracked().trim()))>{t("save_note")}</button>
                        <label class="chip center">{t("upload")}<input type="file" multiple style="display:none" on:change=upload/></label>
                    </div>
                </div>
                <Show when=move || shown.get().is_some()>
                    {move || shown.get().map(|d| { let id = d.id; view! {
                        <div class="card glass" id="kb-doc"><h5>{format!("#{} {} · {}", d.id, d.kind, d.created)}</h5><b>{d.title}</b>
                            <pre class="doc">{d.text}</pre>
                            <div class="rowf"><button class="danger" on:click=move |_| remove(id)>{t("delete")}</button><button on:click=move |_| shown.set(None)>{t("close")}</button></div>
                        </div>
                    }})}
                </Show>
                <div class="card glass">
                    <h5>{move || tr_with(app.lang.get(), "entries", &docs.get().len().to_string())}</h5>
                    <table id="kb-list"><tbody>
                        <For each=move || docs.get() key=|d| d.id children=move |d: Doc| {
                            let id = d.id;
                            view! { <tr class="click" tabindex="0" role="button" on:click=move |_| open(id) on:keydown=move |ev| if activates(&ev) { ev.prevent_default(); open(id) }><td class="dim">{format!("#{}", d.id)}</td><td><span class="chip">{d.kind.clone()}</span></td><td>{d.title.clone()}</td><td class="dim">{d.text.chars().take(80).collect::<String>()}</td></tr> }
                        }/>
                    </tbody></table>
                </div>
            </div>
        </div>
    }
}

#[component]
fn Tools() -> impl IntoView {
    let app = expect_context::<App>();
    let t = move |key: &'static str| app.t(key);
    let tools = RwSignal::new(Vec::<Tool>::new());
    let load = move || {
        spawn_local(async move {
            if let Some(response) = app.call(Request::new(Method::Tools)).await {
                tools.set(response.tools);
                motion::enter_all("#tools .card");
            }
        })
    };
    Effect::new(move |_| load());
    view! {
        <div class="top"><h2>{t("nav_tools")}<small>{t("tools_sub")}</small></h2></div>
        <div class="scroll">
            <div class="col mid" id="tools">
                <Show when=move || tools.get().is_empty()>
                    <div class="empty" id="no-tools"><h3>{t("no_tools_title")}</h3>{t("no_tools")}</div>
                </Show>
                <For each=move || tools.get() key=|t| (t.name.clone(), t.source.len()) children=move |tool: Tool| {
                    let name = tool.name.clone();
                    view! {
                        <div class="card glass">
                            <div class="tool-head"><b class="mono">{tool.name.clone()}</b><span class="sub grow">{tool.description.clone()}</span>
                                <button class="danger" on:click=move |_| { let name = name.clone(); spawn_local(async move { if app.call(Request::new(Method::ToolDelete).name(&name)).await.is_some() { load(); } }) }>{t("delete")}</button></div>
                            <details><summary class="sub">{t("args_source")}</summary>
                                <pre class="pre">{tool.parameters.clone()}</pre><pre class="scrollx">{tool.source.clone()}</pre></details>
                        </div>
                    }
                }/>
            </div>
        </div>
    }
}
