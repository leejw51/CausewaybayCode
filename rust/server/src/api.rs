//! The API: two POST routes whose bodies are Cap'n Proto messages (see cwbcode_proto), and
//! the web client's files. Who may use the API is decided in auth.rs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Request as HttpRequest, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Version, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::{get, post};
use cwbcode_proto::{CALL_PATH, CHAT_PATH, Method, PROTOCOL_HEADER, Project, Request, Response, Session, Status, Tool, What};

use crate::agent;
use crate::kb;
use crate::store::Store;
use crate::worker::{Job, Shared, doc_of};

#[derive(rust_embed::RustEmbed)]
#[folder = "../web/dist"]
struct Web;

#[derive(Clone)]
pub struct App {
    pub shared: Arc<Shared>,
    pub auth: Arc<crate::auth::Auth>,
    pub control: Arc<crate::control::Control>,
    /// The port, for the Alt-Svc header that tells browsers about HTTP/3 (None: no HTTP/3).
    pub h3_port: Option<u16>,
    /// This machine's certificate authority, as PEM: given to whoever asks, to install.
    pub ca_pem: Arc<String>,
    /// Its SHA-256, in hex.
    pub ca_fingerprint: Arc<String>,
}

/// Where a device downloads the certificate authority to trust (no sign-in needed: it is
/// public, and is what makes the sign-in page trustworthy).
pub const CA_PATH: &str = "/ca.crt";

pub fn router(app: App) -> Router {
    Router::new()
        .route(CALL_PATH, post(call))
        .route(CHAT_PATH, post(chat))
        .route(CA_PATH, get(ca))
        .fallback(get(web))
        .layer(DefaultBodyLimit::max(cwbcode_proto::MAX_MESSAGE_BYTES))
        .layer(axum::middleware::from_fn_with_state(app.clone(), stamp))
        .with_state(app)
}

/// Where the desktop app asks the backend on its port to show that it holds the token, before
/// the app gives that page the token (whatever else listens there must not get it).
pub const APP_PROOF_PATH: &str = "/app-proof";

/// The answer to `GET /app-proof?nonce=N` on the plain-HTTP port `port`: HMAC-SHA256 keyed
/// with the token, of a fixed label, that port and the nonce, in hex. Bound to the port the
/// backend itself listens on (never one a request names), so that whatever holds another
/// port cannot pass a question on to a real backend and hand its answer back as its own.
pub fn app_proof(token: &str, port: u16, nonce: &str) -> String {
    use hmac::Mac;
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("cwbcode app proof\n{port}\n{nonce}").as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// The router for the desktop app's plain-HTTP port (127.0.0.1 only): the backend's, plus
/// the app's proof, and only for requests addressed to 127.0.0.1 or localhost at that port.
/// (A site whose name a rebinding DNS server points at 127.0.0.1 would otherwise be the
/// same origin as itself here, and its pages could read and call the backend.)
pub fn local_router(app: App, token: String, port: u16) -> Router {
    Router::new()
        .route(APP_PROOF_PATH, get(proof))
        .with_state((Arc::new(token), port))
        .merge(router(app))
        .layer(axum::middleware::from_fn_with_state(port, local_host))
}

async fn proof(State((token, port)): State<(Arc<String>, u16)>, request: HttpRequest) -> HttpResponse {
    let nonce = request.uri().query().unwrap_or("").split('&').find_map(|pair| pair.strip_prefix("nonce=")).unwrap_or("");
    if !(32..=128).contains(&nonce.len()) || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (StatusCode::BAD_REQUEST, "nonce: 32 to 128 hex digits").into_response();
    }
    // No token (there always is one), no proof: an HMAC with an empty key anyone can make.
    if token.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "no token").into_response();
    }
    ([(header::CONTENT_TYPE, "text/plain"), (header::CACHE_CONTROL, "no-store")], app_proof(&token, port, nonce)).into_response()
}

async fn local_host(State(port): State<u16>, request: HttpRequest, next: Next) -> HttpResponse {
    let named = request.uri().authority().map(|a| a.as_str().to_string()).or_else(|| request.headers().get(header::HOST).and_then(|v| v.to_str().ok()).map(String::from));
    let ok = named.is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
    });
    if !ok {
        return (StatusCode::MISDIRECTED_REQUEST, format!("this port answers for 127.0.0.1:{port} and localhost:{port} only")).into_response();
    }
    next.run(request).await
}

fn protocol(version: Version) -> &'static str {
    match version {
        Version::HTTP_3 => "h3",
        Version::HTTP_2 => "h2",
        _ => "http/1.1",
    }
}

/// Say on every response how the request arrived. HTTP/3 is not advertised (and an earlier
/// advertisement is withdrawn): a browser does not carry its "visit anyway" exception for the
/// self-signed certificate over to QUIC, so one that was told about HTTP/3 would try it, fail,
/// and show the site as unreachable. The terminal client needs no advertisement: it tries
/// HTTP/3 by itself, with the certificate pinned.
async fn stamp(State(_app): State<App>, request: HttpRequest, next: Next) -> HttpResponse {
    let version = request.version();
    let (method, path, from) = (request.method().clone(), request.uri().path().to_string(), peer(&request));
    let agent = browser(request.headers().get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or(""));
    let mut response = next.run(request).await;
    // A line for what another machine asks for, other than calls that went well: when a
    // browser there "cannot open" the backend, this says how far it got (a page with nothing
    // after it is a browser that took the page and refused the rest).
    if from.is_some_and(|ip| !ip.is_loopback()) && (method != axum::http::Method::POST || !response.status().is_success()) {
        eprintln!("cwbcode: {} {method} {path} over {} -> {} ({agent})", from.map(|ip| ip.to_string()).unwrap_or_default(), protocol(version), response.status().as_u16());
    }
    response.headers_mut().insert(PROTOCOL_HEADER, HeaderValue::from_static(protocol(version)));
    if version != Version::HTTP_3 {
        response.headers_mut().insert(header::ALT_SVC, HeaderValue::from_static("clear"));
    }
    response
}

/// Which browser a User-Agent names, for the log (the whole header is not kept).
fn browser(agent: &str) -> &'static str {
    let has = |word: &str| agent.contains(word);
    if has("CriOS") || has("Chrome/") { "Chrome" }
    else if has("FxiOS") || has("Firefox/") { "Firefox" }
    else if has("Safari/") { "Safari" }
    else if agent.is_empty() { "no user agent" }
    else { "another client" }
}

/// What a request without credentials may send: enough for a sign-in, no more.
pub const UNSIGNED_BYTES: usize = 64 << 10;

impl App {
    /// May this request use the API?
    pub fn allowed(&self, headers: &HeaderMap, uri: &axum::http::Uri, peer: Option<std::net::IpAddr>) -> bool {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        self.auth.allowed(headers, uri.authority().map(|a| a.as_str()).or(host), peer)
    }
}

fn peer(request: &HttpRequest) -> Option<std::net::IpAddr> {
    request.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|info| info.0.ip())
}

fn refused() -> HttpResponse {
    (StatusCode::UNAUTHORIZED, "sign in first: the token or session is missing or wrong").into_response()
}

fn capnp(bytes: Vec<u8>) -> HttpResponse {
    ([(header::CONTENT_TYPE, "application/x-capnp"), (header::CACHE_CONTROL, "no-store")], bytes).into_response()
}

async fn call(State(app): State<App>, request: HttpRequest) -> HttpResponse {
    let from = peer(&request);
    let allowed = app.allowed(request.headers(), request.uri(), from);
    let version = request.version();
    let headers = request.headers().clone();
    // The host the request names: over HTTP/2 and HTTP/3 it is in the address, not a header.
    let authority = request.uri().authority().map(|a| a.to_string()).or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(String::from));
    // Without credentials only a sign-in is read, and it is small.
    let limit = if allowed { cwbcode_proto::MAX_MESSAGE_BYTES } else { UNSIGNED_BYTES };
    let Ok(body) = axum::body::to_bytes(request.into_body(), limit).await else {
        return if allowed { (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large").into_response() } else { refused() };
    };
    let request = match Request::decode(&body) {
        Ok(request) => request,
        Err(_) if !allowed => return refused(),
        Err(e) => return (StatusCode::BAD_REQUEST, format!("the request is not a cwbcode message: {e}")).into_response(),
    };
    // Signing in needs no credentials, but still comes from the backend's own page or a client.
    if matches!(request.method, Method::Hello | Method::Challenge | Method::Login | Method::Redeem) {
        if !crate::auth::same_origin(&headers, authority.as_deref()) {
            return refused();
        }
        return capnp(sign_in(&app, &request, from).unwrap_or_else(|e| Response::error(format!("{e:#}"))).encode());
    }
    if !allowed {
        return refused();
    }
    if request.method == Method::Logout {
        app.auth.logout(&headers);
        return capnp(Response::default().encode());
    }
    if request.method == Method::Setup {
        // Remote mode on or off: by whoever may use the backend now (in local mode, a client
        // on this machine with the backend's token; in remote mode, the wallet's owner).
        let response = match app.control.setup(&app.auth, &request.text) {
            Ok(said) => Response { text: said, status: Some(hello(&app)), ..Response::default() },
            Err(e) => Response::error(format!("{e:#}")),
        };
        return capnp(response.encode());
    }
    let response = match request.method {
        // These need the model thread, in turn.
        Method::KbAdd | Method::Compact | Method::Undo => queued(&app, request).await,
        Method::KbSearch if !request.flag => queued(&app, request).await,
        Method::Chat => Response::error(format!("chat is a stream: POST it to {CHAT_PATH}")),
        // The rest is answered at once, also while the model is busy.
        _ => {
            let shared = app.shared.clone();
            let known = hello(&app);
            tokio::task::spawn_blocking(move || direct(&shared, &known, &request, protocol(version)).unwrap_or_else(|e| Response::error(format!("{e:#}"))))
                .await
                .unwrap_or_else(|e| Response::error(format!("the call failed: {e}")))
        }
    };
    capnp(response.encode())
}

/// What a client may know before it has credentials: how to sign in, and to what.
fn hello(app: &App) -> Status {
    Status {
        version: env!("CARGO_PKG_VERSION").to_string(),
        auth: app.auth.label().to_string(),
        wallet: app.auth.wallet(),
        fingerprint: app.auth.fingerprint.clone(),
        bind: app.control.bind.lock().unwrap().to_string(),
        wallet_source: app.control.source.lock().unwrap().clone(),
        // (Before signing in too: a device compares it before it trusts the authority.)
        ca_fingerprint: app.ca_fingerprint.as_str().to_owned(),
        ..Status::default()
    }
}

/// Hello, challenge, login, and a link's code: what a client does before it has credentials.
/// `from` is where the request came from.
fn sign_in(app: &App, request: &Request, from: Option<std::net::IpAddr>) -> Result<Response> {
    let auth = &app.auth;
    Ok(match request.method {
        Method::Hello => Response { status: Some(hello(app)), ..Response::default() },
        Method::Challenge => Response::text(auth.challenge()?),
        Method::Login => Response::text(auth.login(&request.text, &request.name)?),
        Method::Redeem => Response::text(auth.redeem(&request.text, from)?),
        _ => bail!("not a sign-in call"),
    })
}

async fn queued(app: &App, request: Request) -> Response {
    let (reply, answer) = tokio::sync::oneshot::channel();
    if let Err(e) = app.shared.submit(Job::Call { request, reply }) {
        return Response::error(format!("{e:#}"));
    }
    answer.await.unwrap_or_else(|_| Response::error("the model thread dropped the call"))
}

async fn chat(State(app): State<App>, request: HttpRequest) -> HttpResponse {
    if !app.allowed(request.headers(), request.uri(), peer(&request)) {
        return refused();
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), cwbcode_proto::MAX_MESSAGE_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large").into_response();
    };
    let request = match Request::decode(&body) {
        Ok(request) if request.method == Method::Chat => request,
        Ok(_) => return (StatusCode::BAD_REQUEST, "only chat is a stream").into_response(),
        Err(e) => return (StatusCode::BAD_REQUEST, format!("the request is not a cwbcode message: {e}")).into_response(),
    };
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    let waiting = app.shared.busy.load(Ordering::Relaxed);
    if waiting {
        let _ = events.send(What::Note("The model is busy with another request; this one is next.".into()).into());
    }
    if let Err(e) = app.shared.submit(Job::Chat { request, events: events.clone() }) {
        let _ = events.send(What::Error(format!("{e:#}")).into());
        let _ = events.send(What::Done.into());
    }
    drop(events);
    // Events as frames, with a heartbeat when nothing happens. When the client goes away a
    // write fails, the stream is dropped, the receiver with it, and the model thread sees
    // its sends fail: that is how a closed tab stops the model.
    let beat = tokio::time::interval(std::time::Duration::from_secs(1));
    let frames = futures::stream::unfold((receiver, beat), |(mut receiver, mut beat)| async move {
        tokio::select! {
            biased;
            event = receiver.recv() => event.map(|event| (Ok::<_, std::convert::Infallible>(Bytes::from(event.frame())), (receiver, beat))),
            _ = beat.tick() => Some((Ok(Bytes::from_static(&cwbcode_proto::HEARTBEAT)), (receiver, beat))),
        }
    });
    ([(header::CONTENT_TYPE, "application/x-capnp-stream"), (header::CACHE_CONTROL, "no-store")], Body::from_stream(frames)).into_response()
}

/// The project's store for a call that only reads: None if the project has none yet.
fn existing_store(shared: &Shared, request: &Request) -> Result<Option<Store>> {
    Store::open_project_existing(&shared.roots.resolve(&request.project)?)
}

fn project_store(shared: &Shared, request: &Request) -> Result<(std::path::PathBuf, Store)> {
    let path = shared.roots.resolve(&request.project)?;
    let store = Store::open_project(&path)?;
    Ok((path, store))
}

fn project_info(path: &str, opened: &str) -> Project {
    let sessions = Store::open_project_existing(std::path::Path::new(path)).ok().flatten().and_then(|s| s.sessions(1000).ok()).map_or(0, |s| s.len());
    Project { path: path.to_string(), name: std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), sessions: sessions as u32, updated: opened.to_string() }
}

/// The calls answered without the model thread.
fn direct(shared: &Shared, known: &Status, request: &Request, protocol: &str) -> Result<Response> {
    let mut out = Response::default();
    match request.method {
        Method::Status => {
            let config = &shared.config;
            // Each lock taken once, and let go here: a guard made inside the struct below
            // would live to the end of the statement, and a second lock of the same mutex
            // would wait for it forever.
            let (model_path, model_bytes, model_info) = shared.model_about.lock().unwrap().clone();
            let (gen_tps, prompt_tps) = *shared.speed.lock().unwrap();
            out.status = Some(Status {
                version: env!("CARGO_PKG_VERSION").to_string(),
                model: config.model.clone(),
                model_state: shared.model_state.lock().unwrap().clone(),
                protocol: protocol.to_string(),
                busy: shared.busy.load(Ordering::Relaxed),
                busy_project: shared.busy_project.lock().unwrap().clone(),
                context_tokens: shared.context_tokens.load(Ordering::Relaxed),
                context_limit: config.context_limit as u64,
                memory_active: shared.memory_active.load(Ordering::Relaxed),
                memory_peak: shared.memory_peak.load(Ordering::Relaxed),
                uptime_seconds: shared.started.elapsed().as_secs(),
                sandbox: shared.sandbox_label(),
                home: config.home.display().to_string(),
                h3: config.h3,
                queued: shared.queued.load(Ordering::Relaxed),
                auth: known.auth.clone(),
                wallet: known.wallet.clone(),
                fingerprint: known.fingerprint.clone(),
                bind: known.bind.clone(),
                wallet_source: known.wallet_source.clone(),
                user_home: shared.roots.home().display().to_string(),
                ca_fingerprint: known.ca_fingerprint.clone(),
                model_path,
                model_bytes,
                model_info,
                gen_tps,
                prompt_tps,
            });
        }
        Method::Projects => {
            let global = Store::open(&shared.config.home)?;
            for (path, opened) in global.projects()? {
                // A project whose folder is gone is forgotten.
                if std::path::Path::new(&path).is_dir() {
                    out.projects.push(project_info(&path, &opened));
                } else {
                    global.forget_project(&path)?;
                }
            }
        }
        Method::Run => {
            // The user's own "run it": no model, no approval; the sandbox as for any command.
            let path = shared.roots.resolve(&request.project)?;
            let mut policy = crate::sandbox::Policy::new(crate::sandbox::Mode::Auto, &path, &shared.config.home)?;
            policy.sandbox &= shared.config.sandbox;
            policy.network = shared.config.network;
            out.text = crate::runner::run_project(&policy, &request.text)?;
        }
        Method::RemoveProject => {
            // Off the list only: the folder, its files and its own records are not touched,
            // and opening it again brings it back with its conversations.
            let path = shared.roots.resolve(&request.project)?;
            Store::open(&shared.config.home)?.forget_project(&path.to_string_lossy())?;
        }
        Method::OpenProject => {
            // A folder that does not exist yet is made (with those above it).
            let (path, created) = shared.roots.resolve_or_create(&request.text)?;
            if created {
                out.text = format!("created {}", path.display());
            }
            Store::open_project(&path)?;
            let global = Store::open(&shared.config.home)?;
            let text = path.to_string_lossy().into_owned();
            global.touch_project(&text)?;
            out.projects.push(project_info(&text, ""));
        }
        Method::Sessions => {
            if let Some(store) = existing_store(shared, request)? {
                out.sessions = store.sessions(200)?.into_iter().map(|s| Session { id: s.id, name: s.name, updated: s.updated, messages: s.messages as u32, first: s.first }).collect();
            }
        }
        Method::Transcript => {
            let store = existing_store(shared, request)?.with_context(|| format!("this project has no conversation #{}", request.session))?;
            if !store.has_session(request.session)? {
                bail!("this project has no conversation #{}", request.session);
            }
            out.events = store.events(request.session)?;
            out.text = store.todos(request.session)?;
        }
        Method::NewSession => {
            let (path, store) = project_store(shared, request)?;
            out.id = store.new_session(&path.to_string_lossy())?;
        }
        Method::NameSession => {
            let (_, store) = project_store(shared, request)?;
            if !store.has_session(request.session)? {
                bail!("this project has no conversation #{}", request.session);
            }
            store.name_session(request.session, &request.name)?;
        }
        Method::KbList => {
            if let Some(store) = existing_store(shared, request)? {
                for d in store.docs()? {
                    out.docs.push(cwbcode_proto::Doc { id: d.id, kind: d.kind, title: d.title, source: d.source, text: d.text, created: d.created, has_file: d.blob.is_some() });
                }
            }
        }
        Method::KbSearch => {
            // Keywords only: no model needed.
            let limit = if request.limit == 0 { 5 } else { request.limit.min(50) } as usize;
            let hits = match existing_store(shared, request)? {
                Some(store) => kb::search(&store, None, &request.text, limit)?,
                None => vec![],
            };
            for hit in hits {
                out.hits.push(cwbcode_proto::Hit { doc: hit.chunk.doc, kind: hit.chunk.kind, title: hit.chunk.title, snippet: hit.chunk.text.chars().take(600).collect(), bm25_rank: hit.bm25_rank.unwrap_or(0) as u32, semantic_rank: 0, score: hit.score });
            }
        }
        Method::KbGet => {
            let (_, store) = project_store(shared, request)?;
            let doc = store.doc(request.id)?;
            if let Some(blob) = doc.blob {
                let (name, _, data) = store.blob(blob)?;
                out.data = data;
                out.text = name;
            }
            out.docs.push(doc_of(&store, request.id)?);
        }
        Method::KbDelete => {
            let (_, store) = project_store(shared, request)?;
            store.delete_doc(request.id)?;
        }
        Method::Tools => {
            out.tools = Store::open(&shared.config.home)?.tools()?.into_iter().map(|t| Tool { name: t.name, description: t.description, parameters: t.parameters, source: t.source }).collect();
        }
        Method::ToolDelete => {
            if !Store::open(&shared.config.home)?.delete_tool(&request.name)? {
                bail!("there is no tool named {}", request.name);
            }
        }
        Method::Approve => {
            if !shared.answer(request.id, &request.text) {
                bail!("nothing is waiting for that answer (it was answered already, or it timed out)");
            }
        }
        Method::Interrupt => {
            if shared.busy.load(Ordering::Relaxed) {
                agent::interrupt();
                out.text = "stopping".into();
            } else {
                out.text = "nothing is running".into();
            }
        }
        Method::Changes => {
            if let Some(store) = existing_store(shared, request)? {
                out.changes = store.changes(request.session)?;
            }
        }
        Method::KbAdd | Method::Compact | Method::Undo | Method::Chat | Method::Hello | Method::Challenge | Method::Login | Method::Logout | Method::Setup | Method::Redeem => bail!("{:?} is not answered here", request.method),
    }
    Ok(out)
}

/// How long a browser may keep a file of the web client. Only a file whose own name carries
/// a hash of its contents never changes; the page, and the scripts beside it whose folder is
/// named after the crate and not after what they hold (`snippets/`), must be asked for again
/// each time, or a browser pairs a new program with an old script and draws nothing. Fonts
/// and pictures change rarely: a day.
fn cache_rule(name: &str) -> &'static str {
    let file = name.rsplit('/').next().unwrap_or(name);
    let stem = file.split('.').next().unwrap_or("").trim_end_matches("_bg");
    let hashed = stem.rsplit('-').next().is_some_and(|tail| tail.len() == 16 && tail.chars().all(|c| c.is_ascii_hexdigit())) && !name.contains('/');
    if hashed {
        "public, max-age=31536000, immutable"
    } else if name.starts_with("fonts/") || name.starts_with("art/") {
        "public, max-age=86400"
    } else {
        "no-cache"
    }
}

/// The page loads nothing but its own files, and runs no inline script but the page's own two
/// (the loader Trunk writes and the error reporter), allowed by their SHA-256: a script
/// injected into the page would not run.
static CSP: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    use sha2::Digest;
    let page = Web::get("index.html").map(|f| String::from_utf8_lossy(&f.data).into_owned()).unwrap_or_default();
    let hashes: String = inline_scripts(&page).iter().map(|s| format!(" 'sha256-{}'", base64(&sha2::Sha256::digest(s.as_bytes())))).collect();
    format!("default-src 'self'; script-src 'self'{hashes} 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'")
});

/// The bodies of a page's inline scripts (those without a `src`), as the browser hashes them.
fn inline_scripts(page: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("<script") {
        rest = &rest[start..];
        let Some(open_end) = rest.find('>') else { break };
        let attributes = &rest[..open_end];
        let Some(close) = rest[open_end..].find("</script>") else { break };
        if !attributes.contains("src=") {
            out.push(&rest[open_end + 1..open_end + close]);
        }
        rest = &rest[open_end + close..];
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// The certificate authority, as a file a phone or a computer offers to install. (By its type
/// alone: Safari on an iPhone or iPad then offers it as a profile, which an attachment might
/// only save to Files.)
async fn ca(State(app): State<App>) -> HttpResponse {
    (
        [(header::CONTENT_TYPE, "application/x-x509-ca-cert"), (header::CACHE_CONTROL, "no-cache")],
        app.ca_pem.as_str().to_owned(),
    )
        .into_response()
}

/// The web client: its files are built into the program. Any other path is the app itself.
async fn web(uri: axum::http::Uri) -> HttpResponse {
    let path = uri.path().trim_start_matches('/');
    let (name, file) = match Web::get(path) {
        Some(file) if !path.is_empty() => (path, Some(file)),
        // A file that is not there is not there: only a route of the app is answered with the
        // page. (An iPad asking for an icon was once given the page, and showed a letter.)
        None if path.rsplit('/').next().is_some_and(|last| last.contains('.')) => return (StatusCode::NOT_FOUND, "there is no such file").into_response(),
        _ => ("index.html", Web::get("index.html")),
    };
    match file.context("the web client was not built into this binary (run `make build`)") {
        Ok(file) => {
            let mime = mime_guess::from_path(name).first_or_octet_stream();
            let cache = cache_rule(name);
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                    // The page is not to be framed by another site, nor to load anything but its own files.
                    (header::X_FRAME_OPTIONS, "DENY".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                    (header::CONTENT_SECURITY_POLICY, CSP.clone()),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, format!("{e:#}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{base64, cache_rule, inline_scripts};

    #[test]
    fn only_the_pages_own_inline_scripts_are_allowed_by_their_hash() {
        let page = "<head><script>\n  one();\n</script><script type=\"module\">two()</script><script src=\"/x.js\"></script></head>";
        assert_eq!(inline_scripts(page), vec!["\n  one();\n", "two()"]);
        // The digest as a browser writes it in a policy.
        use sha2::Digest;
        assert_eq!(base64(&sha2::Sha256::digest(b"two()")), "iMrPAWirDC2X0OhZzLsM2MRIWSmepJGI4r7D8ZlwBOw=");
        assert_eq!((base64(b"a"), base64(b"ab"), base64(b"abc")), ("YQ==".into(), "YWI=".into(), "YWJj".into()));
        let csp = super::CSP.as_str();
        assert!(!csp.contains("script-src 'self' 'unsafe-inline'") && csp.contains("'sha256-"), "{csp}");
    }

    #[test]
    fn only_files_named_after_their_contents_are_kept_for_good() {
        for kept in ["cwbcode-web-ad37c27284e67636.js", "cwbcode-web-ad37c27284e67636_bg.wasm", "style-accd60e5fcb47ea7.css"] {
            assert!(cache_rule(kept).contains("immutable"), "{kept}");
        }
        for fresh in ["index.html", "snippets/cwbcode-web-e67fe8b22f4b6772/inline0.js", "motion.min.js", "style.css"] {
            assert_eq!(cache_rule(fresh), "no-cache", "{fresh}");
        }
        assert_eq!(cache_rule("fonts/VT323-Regular.ttf"), "public, max-age=86400");
    }
}
