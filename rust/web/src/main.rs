//! Causeway Bay Code in the browser: a dashboard of the backend (model, memory, protocol,
//! projects) and a client for talking to the agent in any project, with its knowledge base
//! and the tools it has made. Built with Leptos; movement by Motion (motion.dev).

mod api;
mod motion;

use cwbcode_proto::{Doc, Hit, Method, Project, Request, Session, Status, Tool, Upload, What};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

const TOKEN_KEY: &str = "cwbcode-token";

#[derive(Clone, Copy, PartialEq)]
enum View {
    Dashboard,
    Chat,
    Knowledge,
    Tools,
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
    token: RwSignal<String>,
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

/// The token: from the address (`#token=...`, then removed from it) or from an earlier visit.
fn initial_token() -> String {
    let Some(window) = web_sys::window() else { return String::new() };
    let hash = window.location().hash().unwrap_or_default();
    if let Some(token) = hash.strip_prefix("#token=") {
        if let Some(storage) = storage() {
            let _ = storage.set_item(TOKEN_KEY, token);
        }
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
        }
        return token.to_string();
    }
    storage().and_then(|s| s.get_item(TOKEN_KEY).ok().flatten()).unwrap_or_default()
}

impl App {
    fn new() -> Self {
        Self {
            token: RwSignal::new(initial_token()),
            view: RwSignal::new(View::Chat),
            status: RwSignal::new(None),
            projects: RwSignal::new(Vec::new()),
            project: RwSignal::new(String::new()),
            sessions: RwSignal::new(Vec::new()),
            session: RwSignal::new(0),
            items: RwSignal::new(Vec::new()),
            open: RwSignal::new(None),
            todos: RwSignal::new(String::new()),
            busy: RwSignal::new(false),
            mode: RwSignal::new("ask".to_string()),
            think: RwSignal::new(String::new()),
            error: RwSignal::new(String::new()),
            next: RwSignal::new(0),
            grew: RwSignal::new(0),
        }
    }

    fn request(&self, method: Method) -> Request {
        Request::new(method).project(&self.project.get_untracked())
    }

    /// Show an error; a refused token sends the user back to the token page.
    fn fail(&self, error: String) {
        if error == "unauthorized" {
            if let Some(storage) = storage() {
                let _ = storage.remove_item(TOKEN_KEY);
            }
            self.token.set(String::new());
            return;
        }
        self.error.set(error);
        let error = self.error;
        set_timeout(move || error.set(String::new()), std::time::Duration::from_secs(8));
    }

    async fn call(&self, request: Request) -> Option<cwbcode_proto::Response> {
        match api::call(&self.token.get_untracked(), &request).await {
            Ok(response) => Some(response),
            Err(e) => {
                self.fail(e);
                None
            }
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
        if self.token.get_untracked().is_empty() {
            return;
        }
        match api::call(&self.token.get_untracked(), &Request::new(Method::Status)).await {
            Ok(response) => self.status.set(response.status),
            Err(e) if e == "unauthorized" => self.fail(e),
            // The server may be restarting: the page keeps asking.
            Err(_) => self.status.set(None),
        }
    }

    async fn refresh_projects(&self) {
        if let Some(response) = self.call(Request::new(Method::Projects)).await {
            if self.project.get_untracked().is_empty() {
                if let Some(first) = response.projects.first() {
                    self.select_project(first.path.clone()).await;
                }
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

    fn new_conversation(&self) {
        self.session.set(0);
        self.items.set(Vec::new());
        self.todos.set(String::new());
        self.open.set(None);
    }

    async fn open_project(&self, path: String) {
        if let Some(response) = self.call(Request::new(Method::OpenProject).text(&path)).await {
            if let Some(project) = response.projects.first() {
                self.select_project(project.path.clone()).await;
            }
            self.refresh_projects().await;
            self.view.set(View::Chat);
        }
    }

    async fn select_session(&self, id: i64) {
        if let Some(response) = self.call(self.request(Method::Transcript).session(id)).await {
            self.new_conversation();
            self.session.set(id);
            for event in response.events {
                self.apply_past(event.what);
            }
            self.todos.set(response.text);
            self.view.set(View::Chat);
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

#[component]
fn Root() -> impl IntoView {
    let app = App::new();
    provide_context(app);
    // The backend's state, every few seconds.
    let tick = Closure::<dyn Fn()>::new(move || spawn_local(async move { app.refresh_status().await }));
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), 3000);
    }
    tick.forget();
    // Once there is a token: what the backend knows.
    Effect::new(move |_| {
        if !app.token.get().is_empty() {
            spawn_local(async move {
                app.refresh_status().await;
                app.refresh_projects().await;
            });
        }
    });
    view! {
        <Show when=move || !app.token.get().is_empty() fallback=|| view! { <Login/> }>
            <div class="app">
                <Sidebar/>
                <div class="main">
                    {move || match app.view.get() {
                        View::Dashboard => view! { <Dashboard/> }.into_any(),
                        View::Chat => view! { <Chat/> }.into_any(),
                        View::Knowledge => view! { <Knowledge/> }.into_any(),
                        View::Tools => view! { <Tools/> }.into_any(),
                    }}
                </div>
            </div>
        </Show>
        <Show when=move || !app.error.get().is_empty()>
            <div class="toast" id="toast">{move || app.error.get()}</div>
        </Show>
    }
}

#[component]
fn Login() -> impl IntoView {
    let app = expect_context::<App>();
    let typed = RwSignal::new(String::new());
    let enter = move || {
        let token = typed.get_untracked().trim().to_string();
        if !token.is_empty() {
            if let Some(storage) = storage() {
                let _ = storage.set_item(TOKEN_KEY, &token);
            }
            app.token.set(token);
        }
    };
    let card = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = card.get() {
            motion::enter(&el, 0.05);
        }
    });
    view! {
        <div class="login">
            <div class="card" node_ref=card>
                <div class="brand" style="padding:0"><div class="logo"></div><div>"Causeway Bay Code"<small>"local AI coding agent"</small></div></div>
                <p>"Enter the backend's token. On the machine it runs on: " <code>"cwbcode url"</code></p>
                <input id="token" type="password" placeholder="token" prop:value=move || typed.get() on:input=move |ev| typed.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { enter() }/>
                <button class="primary" id="signin" on:click=move |_| enter()>"Connect"</button>
            </div>
        </div>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let app = expect_context::<App>();
    let path = RwSignal::new(String::new());
    let nav = move |view: View, label: &'static str| {
        view! { <button class:on=move || app.view.get() == view on:click=move |_| app.view.set(view)>{label}</button> }
    };
    let open = move || {
        let typed = path.get_untracked().trim().to_string();
        if !typed.is_empty() {
            path.set(String::new());
            spawn_local(async move { app.open_project(typed).await });
        }
    };
    let side = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(el) = side.get() {
            motion::slide_in(&el);
        }
    });
    view! {
        <div class="side" node_ref=side>
            <div class="brand"><div class="logo"></div><div>"Causeway Bay Code"<small>"local AI coding agent"</small></div></div>
            <div class="nav">
                {nav(View::Dashboard, "Dashboard")}
                {nav(View::Chat, "Chat")}
                {nav(View::Knowledge, "Knowledge")}
                {nav(View::Tools, "Tools")}
            </div>
            <h4>"Projects"</h4>
            <div class="open">
                <input id="open-path" placeholder="~/folder to open" prop:value=move || path.get() on:input=move |ev| path.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { open() }/>
                <button id="open-go" on:click=move |_| open()>"Open"</button>
            </div>
            <div class="list" style="max-height:28vh">
                <For each=move || app.projects.get() key=|p| p.path.clone() children=move |p: Project| {
                    let (path, title) = (p.path.clone(), p.path.clone());
                    let selected = p.path.clone();
                    view! {
                        <div class="row project" class:on=move || app.project.get() == selected title=title
                            on:click=move |_| { let path = path.clone(); spawn_local(async move { app.select_project(path).await; app.view.set(View::Chat); }) }>
                            {p.name.clone()}<small>{p.path.clone()}</small>
                        </div>
                    }
                }/>
            </div>
            <h4>"Conversations"<button class="ghost" id="new-chat" on:click=move |_| { app.new_conversation(); app.view.set(View::Chat); }>"+ new"</button></h4>
            <div class="list grow">
                <For each=move || app.sessions.get() key=|s| (s.id, s.name.clone(), s.messages) children=move |s: Session| {
                    let id = s.id;
                    let label = if s.name.is_empty() { s.first.clone() } else { s.name.clone() };
                    view! {
                        <div class="row session" class:on=move || app.session.get() == id on:click=move |_| spawn_local(async move { app.select_session(id).await })>
                            {if label.is_empty() { format!("conversation #{id}") } else { label }}
                            <small>{format!("#{id} · {} · {} msgs", s.updated.get(5..16).unwrap_or(""), s.messages)}</small>
                        </div>
                    }
                }/>
            </div>
            <div class="pill" id="status-pill">
                {move || match app.status.get() {
                    None => view! { <span><span class="dot bad"></span>"backend not reachable"</span> }.into_any(),
                    Some(s) => {
                        let dot = if s.busy { "dot busy" } else if s.model_state == "ready" || s.model_state == "scripted" { "dot ok" } else if s.model_state.starts_with("failed") { "dot bad" } else { "dot warn" };
                        view! {
                            <span><span class=dot></span>{if s.busy { "working".to_string() } else { s.model_state.clone() }}" · "{s.protocol.clone()}</span>
                            <div>{format!("{:.1} GB · {} ctx", gb(s.memory_active), s.context_tokens)}</div>
                        }.into_any()
                    }
                }}
            </div>
        </div>
    }
}

/// A number that counts to its new value.
#[component]
fn Counter(value: Signal<f64>, decimals: u32, suffix: &'static str) -> impl IntoView {
    let node = NodeRef::<leptos::html::Span>::new();
    Effect::new(move |_| {
        let v = value.get();
        if let Some(el) = node.get() {
            motion::count_to(&el, v, decimals, suffix);
        }
    });
    view! { <span node_ref=node>"0"</span> }
}

#[component]
fn Dashboard() -> impl IntoView {
    let app = expect_context::<App>();
    Effect::new(move |_| {
        spawn_local(async move { app.refresh_projects().await });
        motion::enter_all(".cards .card");
    });
    let s = move || app.status.get().unwrap_or_default();
    let context_share = move || {
        let s = s();
        if s.context_limit == 0 { 0.0 } else { (s.context_tokens as f64 / s.context_limit as f64 * 100.0).min(100.0) }
    };
    view! {
        <div class="top"><h2>"Dashboard"<small>{move || s().home}</small></h2></div>
        <div class="scroll">
            <div class="col" style="max-width:1100px">
                <div class="cards">
                    <div class="card"><h5>"Model"</h5><div class="big" id="model-state">{move || if app.status.get().is_none() { "offline".to_string() } else if s().busy { "working".to_string() } else { s().model_state }}</div>
                        <div class="sub">{move || s().model}</div></div>
                    <div class="card"><h5>"Memory"</h5><div class="big"><Counter value=Signal::derive(move || gb(s().memory_active)) decimals=1 suffix=" GB"/></div>
                        <div class="sub">{move || format!("peak {:.1} GB", gb(s().memory_peak))}</div>
                        <div class="bar2"><i style:width=move || format!("{:.0}%", (gb(s().memory_active) / 36.0 * 100.0).min(100.0))></i></div></div>
                    <div class="card"><h5>"Context"</h5><div class="big"><Counter value=Signal::derive(move || s().context_tokens as f64) decimals=0 suffix=""/></div>
                        <div class="sub">{move || format!("of {} tokens before summarizing", s().context_limit)}</div>
                        <div class="bar2"><i style:width=move || format!("{:.0}%", context_share())></i></div></div>
                    <div class="card"><h5>"Transport"</h5><div class="big" id="protocol">{move || s().protocol.to_uppercase()}</div>
                        <div class="sub">{move || if s().h3 { "Cap'n Proto · server offers HTTP/3 and HTTP/2" } else { "Cap'n Proto · HTTP/2 only" }}</div></div>
                    <div class="card"><h5>"Safety"</h5><div class="big" style="font-size:15px;line-height:1.9">{move || s().sandbox}</div>
                        <div class="sub">"token required · folders under ~ only"</div></div>
                    <div class="card"><h5>"Uptime"</h5><div class="big"><Counter value=Signal::derive(move || s().uptime_seconds as f64 / 60.0) decimals=0 suffix=" min"/></div>
                        <div class="sub">{move || format!("cwbcode {} · {} waiting", s().version, s().queued)}</div></div>
                </div>
                <div class="card">
                    <h5>"Projects"</h5>
                    <Show when=move || !app.projects.get().is_empty() fallback=|| view! { <div class="empty">"No project yet. Open a folder under your home folder from the sidebar."</div> }>
                        <table>
                            <thead><tr><th>"Project"</th><th>"Folder"</th><th>"Conversations"</th><th>"Last opened"</th></tr></thead>
                            <tbody>
                                <For each=move || app.projects.get() key=|p| (p.path.clone(), p.sessions) children=move |p: Project| {
                                    let path = p.path.clone();
                                    view! {
                                        <tr class="click" on:click=move |_| { let path = path.clone(); spawn_local(async move { app.select_project(path).await; app.view.set(View::Chat); }) }>
                                            <td>{p.name.clone()}</td><td style="color:var(--dim)">{p.path.clone()}</td><td>{p.sessions}</td><td style="color:var(--dim)">{p.updated.clone()}</td>
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
    let node = NodeRef::<leptos::html::Div>::new();
    let (kind, past) = (item.kind, item.past);
    Effect::new(move |_| {
        if let Some(el) = node.get() {
            if !past {
                motion::enter(&el, 0.0);
                if kind == Kind::Approval {
                    motion::attention(&el);
                }
            }
        }
    });
    let text = item.text;
    let body = match item.kind {
        Kind::User => view! { <div class="msg user">{move || text.get()}</div> }.into_any(),
        Kind::Thinking => view! { <div class="msg thinking">{move || text.get()}</div> }.into_any(),
        Kind::Text => view! { <div class="msg text">{move || rendered(text.get())}</div> }.into_any(),
        Kind::Note => view! { <div class="note">{move || text.get()}</div> }.into_any(),
        Kind::Warning => view! { <div class="note warn">{move || text.get()}</div> }.into_any(),
        Kind::Error => view! { <div class="note bad">{move || text.get()}</div> }.into_any(),
        Kind::Tool => {
            let result = item.result;
            view! {
                <details class="tool" class:bad=move || result.get().is_some_and(|r| !r.0)>
                    <summary>
                        <span class=move || match result.get() { None => "dot busy", Some((true, ..)) => "dot ok", Some((false, ..)) => "dot bad" }></span>
                        <span class="t">{move || text.get()}</span>
                        <small>{move || result.get().map(|r| if r.2 >= 0.1 { format!("{:.1}s", r.2) } else { String::new() })}</small>
                    </summary>
                    <pre>{move || result.get().map(|r| r.1).unwrap_or_else(|| "running ...".to_string())}</pre>
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
            view! {
                <div class="approval" class:done=move || !answered.get().is_empty() || past>
                    <pre>{move || text.get()}</pre>
                    <Show when=move || answered.get().is_empty() && !past fallback=move || view! { <div class="note">{move || format!("answered: {}", if answered.get().is_empty() { "—".to_string() } else { answered.get() })}</div> }>
                        <div class="acts">
                            <button class="primary approve-yes" on:click=move |_| answer("y".to_string())>"Allow"</button>
                            <Show when=move || offer_always><button class="approve-always" on:click=move |_| answer("a".to_string())>"Always this session"</button></Show>
                            <button class="danger approve-no" on:click=move |_| answer("n".to_string())>"No"</button>
                            <input placeholder="or say what to do instead, then Enter" prop:value=move || instead.get() on:input=move |ev| instead.set(event_target_value(&ev))
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
            <h2>{move || { let p = app.project.get(); p.rsplit('/').next().unwrap_or("").to_string() }}<small>{move || if app.session.get() == 0 { "new conversation".to_string() } else { format!("conversation #{}", app.session.get()) }}</small></h2>
            <button class="ghost" id="undo" disabled=move || app.session.get() == 0 || app.busy.get() on:click=move |_| act(Method::Undo)>"Undo changes"</button>
            <button class="ghost" id="compact" disabled=move || app.session.get() == 0 || app.busy.get() on:click=move |_| act(Method::Compact)>"Summarize"</button>
        </div>
        <div class="scroll" node_ref=scroller id="transcript">
            <div class="col">
                <Show when=move || app.project.get().is_empty()>
                    <div class="empty"><h3>"Open a project"</h3>"Type a folder under your home folder in the sidebar (for example ~/code/myapp) and press Open."</div>
                </Show>
                <Show when=move || !app.project.get().is_empty() && app.items.get().is_empty()>
                    <div class="empty"><h3>"What shall we work on?"</h3>{move || app.project.get()}</div>
                </Show>
                <For each=move || app.items.get() key=|item| item.id children=move |item: Item| view! { <ItemView item=item/> }/>
                <Show when=move || !todo_lines().is_empty()>
                    <div class="todos" id="todos">
                        {move || todo_lines().into_iter().map(|(content, status)| {
                            let (class, mark) = match status.as_str() { "completed" => ("done", "☒ "), "in_progress" => ("doing", "◐ "), _ => ("", "☐ ") };
                            view! { <div class=class>{mark}{content}</div> }
                        }).collect_view()}
                    </div>
                </Show>
            </div>
        </div>
        <div class="composer">
            <div class="box">
                <textarea id="draft" rows="2" placeholder="Ask, or describe what to change ...  (Enter sends, Shift+Enter for a new line)"
                    prop:value=move || draft.get() on:input=move |ev| draft.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" && !ev.shift_key() { ev.prevent_default(); send() }></textarea>
                <div class="bar">
                    <label class="chip" style="cursor:pointer">"+ image"<input type="file" accept="image/*" multiple style="display:none" on:change=pick/></label>
                    <For each=move || attached.get() key=|u| (u.name.clone(), u.data.len()) children=move |u: Upload| view! { <span class="chip">{u.name.clone()}</span> }/>
                    <span class="sp"></span>
                    <select id="mode" on:change=move |ev| app.mode.set(event_target_value(&ev)) prop:value=move || app.mode.get()>
                        <option value="ask">"ask first"</option><option value="auto">"auto"</option><option value="read-only">"read-only"</option>
                    </select>
                    <select id="think" on:change=move |ev| app.think.set(event_target_value(&ev)) prop:value=move || app.think.get()>
                        <option value="">"thinking: default"</option><option value="none">"none"</option><option value="low">"low"</option><option value="medium">"medium"</option><option value="high">"high"</option>
                    </select>
                    <Show when=move || app.busy.get() fallback=move || view! { <button class="primary" id="send" disabled=move || app.project.get().is_empty() on:click=move |_| send()>"Send"</button> }>
                        <button class="danger" id="stop" on:click=move |_| spawn_local(async move { app.call(Request::new(Method::Interrupt)).await; })>"Stop"</button>
                    </Show>
                </div>
            </div>
        </div>
    }
}

#[component]
fn Knowledge() -> impl IntoView {
    let app = expect_context::<App>();
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
        <div class="top"><h2>"Knowledge"<small>{move || app.project.get()}</small></h2><span class="chip">{move || if working.get() { "working ..." } else { "BM25 + semantic" }}</span></div>
        <div class="scroll">
            <div class="col" style="max-width:1000px">
                <div class="card form">
                    <div class="rowf">
                        <input id="kb-query" placeholder="Search by keywords and meaning ..." prop:value=move || query.get() on:input=move |ev| query.set(event_target_value(&ev)) on:keydown=move |ev| if ev.key() == "Enter" { search() }/>
                        <button style="flex:0" id="kb-search" on:click=move |_| search()>"Search"</button>
                    </div>
                </div>
                <Show when=move || hits.get().is_some()>
                    <div id="kb-hits" class="col">
                        {move || {
                            let found = hits.get().unwrap_or_default();
                            if found.is_empty() {
                                view! { <div class="note">"No matches."</div> }.into_any()
                            } else {
                                found.into_iter().map(|h| {
                                    let how = match (h.bm25_rank, h.semantic_rank) { (0, 0) => String::new(), (b, 0) => format!("bm25 #{b}"), (0, s) => format!("semantic #{s}"), (b, s) => format!("bm25 #{b} · semantic #{s}") };
                                    let id = h.doc;
                                    view! { <div class="card" style="cursor:pointer" on:click=move |_| open(id)><h5>{format!("#{} {} · {how}", h.doc, h.kind)}</h5><b>{h.title}</b><div class="sub" style="white-space:pre-wrap">{h.snippet}</div></div> }
                                }).collect_view().into_any()
                            }
                        }}
                    </div>
                </Show>
                <div class="card form">
                    <h5>"Save a note or a file"</h5>
                    <input id="kb-title" placeholder="Title (optional)" prop:value=move || title.get() on:input=move |ev| title.set(event_target_value(&ev))/>
                    <textarea id="kb-note" rows="3" placeholder="Something to remember ..." prop:value=move || note.get() on:input=move |ev| note.set(event_target_value(&ev))></textarea>
                    <div class="rowf">
                        <button class="primary" id="kb-save" disabled=move || note.get().trim().is_empty() || working.get()
                            on:click=move |_| save(Request::new(Method::KbAdd).name(title.get_untracked().trim()).text(note.get_untracked().trim()))>"Save note"</button>
                        <label class="chip" style="cursor:pointer;text-align:center;padding:8px">"Upload files (text, images, anything)"<input type="file" multiple style="display:none" on:change=upload/></label>
                    </div>
                </div>
                <Show when=move || shown.get().is_some()>
                    {move || shown.get().map(|d| { let id = d.id; view! {
                        <div class="card" id="kb-doc"><h5>{format!("#{} {} · {}", d.id, d.kind, d.created)}</h5><b>{d.title}</b>
                            <pre style="white-space:pre-wrap;max-height:300px;overflow:auto">{d.text}</pre>
                            <div class="rowf"><button class="danger" on:click=move |_| remove(id)>"Delete"</button><button on:click=move |_| shown.set(None)>"Close"</button></div>
                        </div>
                    }})}
                </Show>
                <div class="card">
                    <h5>{move || format!("{} entries", docs.get().len())}</h5>
                    <table id="kb-list"><tbody>
                        <For each=move || docs.get() key=|d| d.id children=move |d: Doc| {
                            let id = d.id;
                            view! { <tr class="click" on:click=move |_| open(id)><td style="color:var(--dim)">{format!("#{}", d.id)}</td><td><span class="chip">{d.kind.clone()}</span></td><td>{d.title.clone()}</td><td style="color:var(--dim)">{d.text.chars().take(80).collect::<String>()}</td></tr> }
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
        <div class="top"><h2>"Tools"<small>"written by the agent in Rust, compiled, kept for every project"</small></h2></div>
        <div class="scroll">
            <div class="col" id="tools" style="max-width:1000px">
                <Show when=move || tools.get().is_empty()>
                    <div class="empty"><h3>"No tools yet"</h3>"Ask the agent for something repetitive, or say \"make a tool that ...\"."</div>
                </Show>
                <For each=move || tools.get() key=|t| (t.name.clone(), t.source.len()) children=move |t: Tool| {
                    let name = t.name.clone();
                    view! {
                        <div class="card">
                            <div style="display:flex;align-items:center;gap:10px"><b style="font-family:var(--mono)">{t.name.clone()}</b><span class="sub" style="flex:1">{t.description.clone()}</span>
                                <button class="danger" on:click=move |_| { let name = name.clone(); spawn_local(async move { if app.call(Request::new(Method::ToolDelete).name(&name)).await.is_some() { load(); } }) }>"Delete"</button></div>
                            <details><summary class="sub" style="cursor:pointer;margin-top:6px">"arguments and source"</summary>
                                <pre style="white-space:pre-wrap">{t.parameters.clone()}</pre><pre style="overflow:auto">{t.source.clone()}</pre></details>
                        </div>
                    }
                }/>
            </div>
        </div>
    }
}
