//! The API: two POST routes whose bodies are Cap'n Proto messages (see cwbcode_proto), and
//! the web client's files. Every API request must carry the server's token.

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
    pub token: Arc<String>,
    /// The port, for the Alt-Svc header that tells browsers about HTTP/3 (None: no HTTP/3).
    pub h3_port: Option<u16>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route(CALL_PATH, post(call))
        .route(CHAT_PATH, post(chat))
        .fallback(get(web))
        .layer(DefaultBodyLimit::max(cwbcode_proto::MAX_MESSAGE_BYTES))
        .layer(axum::middleware::from_fn_with_state(app.clone(), stamp))
        .with_state(app)
}

fn protocol(version: Version) -> &'static str {
    match version {
        Version::HTTP_3 => "h3",
        Version::HTTP_2 => "h2",
        _ => "http/1.1",
    }
}

/// Say on every response how the request arrived, and that HTTP/3 is offered.
async fn stamp(State(app): State<App>, request: HttpRequest, next: Next) -> HttpResponse {
    let version = request.version();
    let mut response = next.run(request).await;
    response.headers_mut().insert(PROTOCOL_HEADER, HeaderValue::from_static(protocol(version)));
    if let (Some(port), true) = (app.h3_port, version != Version::HTTP_3) {
        if let Ok(value) = HeaderValue::from_str(&format!("h3=\":{port}\"; ma=3600")) {
            response.headers_mut().insert(header::ALT_SVC, value);
        }
    }
    response
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl App {
    /// Does a request carry the server's token?
    pub fn authorized(&self, headers: &HeaderMap) -> bool {
        authorized(self, headers)
    }
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).is_some_and(|given| same(given.trim().as_bytes(), app.token.as_bytes()))
}

fn capnp(bytes: Vec<u8>) -> HttpResponse {
    ([(header::CONTENT_TYPE, "application/x-capnp"), (header::CACHE_CONTROL, "no-store")], bytes).into_response()
}

async fn call(State(app): State<App>, request: HttpRequest) -> HttpResponse {
    if !authorized(&app, request.headers()) {
        return (StatusCode::UNAUTHORIZED, "the token is missing or wrong").into_response();
    }
    let version = request.version();
    let Ok(body) = axum::body::to_bytes(request.into_body(), cwbcode_proto::MAX_MESSAGE_BYTES).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large").into_response();
    };
    let request = match Request::decode(&body) {
        Ok(request) => request,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("the request is not a cwbcode message: {e}")).into_response(),
    };
    let response = match request.method {
        // These need the model thread, in turn.
        Method::KbAdd | Method::Compact | Method::Undo => queued(&app, request).await,
        Method::KbSearch if !request.flag => queued(&app, request).await,
        Method::Chat => Response::error(format!("chat is a stream: POST it to {CHAT_PATH}")),
        // The rest is answered at once, also while the model is busy.
        _ => {
            let shared = app.shared.clone();
            tokio::task::spawn_blocking(move || direct(&shared, &request, protocol(version)).unwrap_or_else(|e| Response::error(format!("{e:#}"))))
                .await
                .unwrap_or_else(|e| Response::error(format!("the call failed: {e}")))
        }
    };
    capnp(response.encode())
}

async fn queued(app: &App, request: Request) -> Response {
    let (reply, answer) = tokio::sync::oneshot::channel();
    if let Err(e) = app.shared.submit(Job::Call { request, reply }) {
        return Response::error(format!("{e:#}"));
    }
    answer.await.unwrap_or_else(|_| Response::error("the model thread dropped the call"))
}

async fn chat(State(app): State<App>, request: HttpRequest) -> HttpResponse {
    if !authorized(&app, request.headers()) {
        return (StatusCode::UNAUTHORIZED, "the token is missing or wrong").into_response();
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
fn direct(shared: &Shared, request: &Request, protocol: &str) -> Result<Response> {
    let mut out = Response::default();
    match request.method {
        Method::Status => {
            let config = &shared.config;
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
        Method::OpenProject => {
            let path = shared.roots.resolve(&request.text)?;
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
        Method::KbAdd | Method::Compact | Method::Undo | Method::Chat => bail!("{:?} is not answered here", request.method),
    }
    Ok(out)
}

/// The web client: its files are built into the program. Any other path is the app itself.
async fn web(uri: axum::http::Uri) -> HttpResponse {
    let path = uri.path().trim_start_matches('/');
    let (name, file) = match Web::get(path) {
        Some(file) if !path.is_empty() => (path, Some(file)),
        _ => ("index.html", Web::get("index.html")),
    };
    match file.context("the web client was not built into this binary (run `make build`)") {
        Ok(file) => {
            let mime = mime_guess::from_path(name).first_or_octet_stream();
            // The app's files carry a hash in their names; only the page itself must be fresh.
            let cache = if name == "index.html" { "no-cache" } else { "public, max-age=31536000, immutable" };
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                    // The page is not to be framed by another site, nor to load anything but its own files.
                    (header::X_FRAME_OPTIONS, "DENY".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                    (header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'".to_string()),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, format!("{e:#}")).into_response(),
    }
}
