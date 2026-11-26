//! The one thread that owns the model. MLX's state is per thread and the model is 20 GB, so
//! everything that needs it (a chat turn, embeddings for the knowledge base, a summary) is a
//! job on this thread's queue, one at a time. One conversation's KV cache is in memory at a
//! time; asking for another conversation reads it back from its store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cwbcode_proto::{Doc, Event, Hit, Method, Request, Response, Upload, Usage, What};

use crate::agent::{self, Agent, Brain, Options};
use crate::kb;
use crate::llm::{Engine, Sampler};
use crate::projects::Roots;
use crate::prompt::{self, ImageRef};
use crate::sandbox::{Answer, Mode, Policy};
use crate::store::Store;
use crate::tools;
use crate::ui::Ui;

/// How long the model waits for an answer to an approval before taking it as "no".
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub struct Config {
    /// The server's folder (~/.cwbcode).
    pub home: PathBuf,
    pub model: String,
    pub temp: f32,
    pub top_k: i32,
    pub think: String,
    pub max_tokens: usize,
    pub max_steps: usize,
    pub context_limit: usize,
    pub sandbox: bool,
    pub network: bool,
    /// Take the model's turns from this script instead of loading a model (tests).
    pub script: Option<PathBuf>,
    /// Start without a model: everything works but chat and semantic search.
    pub no_model: bool,
    /// Download the model at start when it is missing (download.rs), then load it: the
    /// default (`--no-download-model` turns it off).
    pub download_model: bool,
    /// Tests: the script stands in for the model only once the model has been downloaded
    /// (from a fake Hugging Face), so that a download can be watched to the end, "ready".
    pub script_after_download: bool,
    /// HTTP/3 is offered.
    pub h3: bool,
}

pub enum Job {
    Chat { request: Request, events: tokio::sync::mpsc::UnboundedSender<Event> },
    Call { request: Request, reply: tokio::sync::oneshot::Sender<Response> },
    /// Load the model (after it has been downloaded).
    Load,
}

/// What the request handlers and the model thread share.
pub struct Shared {
    pub config: Config,
    pub roots: Roots,
    pub started: Instant,
    /// loading | ready | none | scripted | missing (in no store; it can be downloaded) |
    /// downloading | verifying | failed: ... Changed only by `set_model_state`, which tells
    /// every client listening.
    pub model_state: Mutex<String>,
    /// Where the model is, pushed to every client of the events stream (api.rs): one message
    /// on each change of state, and every half second while it downloads.
    pub events: tokio::sync::broadcast::Sender<cwbcode_proto::Progress>,
    /// The number of the last message sent there.
    seq: AtomicU64,
    /// Held while a message is made and sent: they go out in the order of their numbers.
    publishing: Mutex<()>,
    /// The model's download, when one runs (or ran): how far it is.
    pub download: crate::download::Download,
    pub busy: AtomicBool,
    pub busy_project: Mutex<String>,
    pub queued: AtomicU32,
    pub context_tokens: AtomicU64,
    pub memory_active: AtomicU64,
    pub memory_peak: AtomicU64,
    /// The loaded model: where its files are, how large, and what it is ("" and 0 before).
    pub model_about: Mutex<(String, u64, String)>,
    /// Tokens per second in the last turn: (writing, reading the prompt).
    pub speed: Mutex<(f64, f64)>,
    /// Approvals the model is waiting on: where the answer goes.
    pub approvals: Mutex<HashMap<i64, mpsc::Sender<String>>>,
    next_approval: AtomicI64,
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl Shared {
    pub fn submit(&self, job: Job) -> Result<()> {
        self.queued.fetch_add(1, Ordering::Relaxed);
        self.jobs.lock().unwrap().send(job).map_err(|_| anyhow::anyhow!("the model thread has stopped"))
    }

    /// Give the model the user's answer to an approval. False: nothing was waiting for it.
    pub fn answer(&self, id: i64, answer: &str) -> bool {
        match self.approvals.lock().unwrap().remove(&id) {
            Some(tx) => tx.send(answer.to_string()).is_ok(),
            None => false,
        }
    }

    /// Change the model's state, and tell every client listening.
    pub fn set_model_state(&self, state: &str) {
        *self.model_state.lock().unwrap() = state.to_string();
        self.publish();
    }

    /// Change what is known of the download, and tell every client listening.
    pub fn download_update(&self, f: impl FnOnce(&mut crate::download::Info)) {
        self.download.update(f);
        self.publish();
    }

    /// Where the model is now, as a message of the events stream (numbered as the last one
    /// sent: a client that has that one skips it).
    pub fn progress(&self) -> cwbcode_proto::Progress {
        let state = self.model_state.lock().unwrap().clone();
        let (done, info) = self.download.snapshot();
        let (state, error) = match state.strip_prefix("failed: ") {
            Some(why) => ("failed".to_string(), why.to_string()),
            None if state == "missing" => (state, info.error.clone()),
            None => (state, String::new()),
        };
        cwbcode_proto::Progress {
            seq: self.seq.load(Ordering::SeqCst),
            state,
            error,
            done,
            total: info.total,
            speed: info.speed,
            files_done: info.files_done,
            files_total: info.files_total,
            source_kind: info.source_kind.clone(),
            token: info.token,
            token_from: info.token_from.clone(),
            token_refused: info.token_refused,
            verified: info.verified,
            note: info.note,
            model: self.config.model.clone(),
        }
    }

    /// Send where the model is to every client listening (nobody listening is fine).
    pub fn publish(&self) {
        let _order = self.publishing.lock().unwrap();
        let mut progress = self.progress();
        progress.seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.events.send(progress);
    }

    pub fn sandbox_label(&self) -> String {
        match (self.config.sandbox, self.config.network) {
            (true, false) => "commands sandboxed, no network",
            (true, true) => "commands sandboxed, network open",
            (false, _) => "commands NOT sandboxed (each one asks first)",
        }
        .to_string()
    }
}

/// Start the model thread. The model loads in the background; the server answers meanwhile.
pub fn start(config: Config, roots: Roots) -> Result<Arc<Shared>> {
    let (tx, rx) = mpsc::channel();
    let state = if config.script.is_some() && !config.script_after_download { "scripted" } else if config.no_model { "none" } else { "loading" };
    let shared = Arc::new(Shared {
        config,
        roots,
        started: Instant::now(),
        model_state: Mutex::new(state.to_string()),
        download: crate::download::Download::default(),
        events: tokio::sync::broadcast::channel(256).0,
        seq: AtomicU64::new(0),
        publishing: Mutex::new(()),
        busy: AtomicBool::new(false),
        busy_project: Mutex::new(String::new()),
        queued: AtomicU32::new(0),
        context_tokens: AtomicU64::new(0),
        memory_active: AtomicU64::new(0),
        memory_peak: AtomicU64::new(0),
        model_about: Mutex::new((String::new(), 0, String::new())),
        speed: Mutex::new((0.0, 0.0)),
        approvals: Mutex::new(HashMap::new()),
        next_approval: AtomicI64::new(1),
        jobs: Mutex::new(tx),
    });
    let for_thread = shared.clone();
    std::thread::Builder::new().name("model".into()).spawn(move || Worker::new(for_thread).run(rx))?;
    Ok(shared)
}

/// Where a turn's events go: to the client, and into the conversation's log.
struct Sink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    /// A second connection to the project's store, for the log.
    log: Store,
    session: i64,
    thinking: String,
    text: String,
}

impl Sink {
    fn handle(&mut self, what: What) {
        match &what {
            // Pieces are streamed; the log gets the whole message when it ends.
            What::Thinking(t) => self.thinking.push_str(t),
            What::Text(t) => self.text.push_str(t),
            What::EndMessage => self.flush(),
            What::Done | What::Session(_) => {}
            other => {
                self.flush();
                let _ = self.log.log_event(self.session, &other.clone().into());
            }
        }
        // A client that went away stops the model: nobody is reading.
        if self.tx.send(what.into()).is_err() {
            agent::interrupt();
        }
    }

    fn flush(&mut self) {
        for (text, thinking) in [(std::mem::take(&mut self.thinking), true), (std::mem::take(&mut self.text), false)] {
            if !text.trim().is_empty() {
                let what = if thinking { What::Thinking(text) } else { What::Text(text.trim().to_string()) };
                let _ = self.log.log_event(self.session, &what.into());
            }
        }
    }
}

struct Worker {
    shared: Arc<Shared>,
    brain: Option<Brain>,
    global: Rc<Store>,
    /// The conversation in memory: its project folder and the agent holding its cache.
    current: Option<(PathBuf, Agent)>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        let global = Rc::new(Store::open(&shared.config.home).expect("opening the server's store"));
        Self { shared, brain: None, global, current: None }
    }

    fn set_state(&self, state: &str) {
        self.shared.set_model_state(state);
    }

    /// Load the model. One that is in no store and can be downloaded is "missing" (and, with
    /// `download`, downloaded: the download thread asks for this again when it is done).
    fn load(&mut self, download: bool) {
        let config = &self.shared.config;
        if let Some(script) = config.script.as_ref().filter(|_| !config.script_after_download) {
            match std::fs::read_to_string(script).map_err(anyhow::Error::from).and_then(|json| Brain::script(&json)) {
                Ok(brain) => self.brain = Some(brain),
                Err(e) => self.set_state(&format!("failed: cannot read the script {}: {e:#}", script.display())),
            }
            return;
        }
        if config.no_model {
            return;
        }
        if crate::inkling::ollama::manifest_path(&config.model).is_none() && crate::download::source_for(&config.model).is_some() {
            eprintln!("cwbcode: the model {} is in neither Ollama's store nor {}", config.model, crate::inkling::ollama::own_store().display());
            self.set_state("missing");
            if download {
                eprintln!("cwbcode: downloading it (start with --no-download-model not to)");
                if let Err(e) = crate::download::start(&self.shared) {
                    eprintln!("cwbcode: cannot download it: {e:#}");
                }
            } else {
                eprintln!("cwbcode: not downloading it (--no-download-model): run `make download`, or download it from the web client");
            }
            return;
        }
        self.set_state("loading");
        if let (Some(script), true) = (&config.script, config.script_after_download) {
            // (Tests: a moment in "loading", as a real model takes, then the script.)
            std::thread::sleep(Duration::from_millis(1500));
            match std::fs::read_to_string(script).map_err(anyhow::Error::from).and_then(|json| Brain::script(&json)) {
                Ok(brain) => {
                    self.brain = Some(brain);
                    self.set_state("ready");
                }
                Err(e) => self.set_state(&format!("failed: cannot read the script {}: {e:#}", script.display())),
            }
            return;
        }
        let t = Instant::now();
        eprintln!("cwbcode: loading {} ...", config.model);
        match Engine::load(&config.model, Sampler { temp: config.temp, top_k: config.top_k }) {
            Ok(engine) => {
                eprintln!("cwbcode: model loaded in {:.1}s{}", t.elapsed().as_secs_f64(), if engine.model.draft.is_some() { ", draft network on" } else { "" });
                *self.shared.model_about.lock().unwrap() = (engine.location.clone(), engine.bytes, engine.about.clone());
                self.brain = Some(Brain::Model(Rc::new(engine)));
                self.set_state("ready");
            }
            Err(e) => {
                eprintln!("cwbcode: the model did not load: {e:#}");
                self.set_state(&format!("failed: {e:#}"));
            }
        }
        self.measure();
    }

    fn measure(&self) {
        if matches!(self.brain, Some(Brain::Model(_))) {
            self.shared.memory_active.store(mlx_rs::memory::active_memory().unwrap_or(0) as u64, Ordering::Relaxed);
            self.shared.memory_peak.store(mlx_rs::memory::peak_memory().unwrap_or(0) as u64, Ordering::Relaxed);
        }
        self.shared.context_tokens.store(self.current.as_ref().map_or(0, |(_, a)| a.tokens()) as u64, Ordering::Relaxed);
    }

    fn run(mut self, jobs: mpsc::Receiver<Job>) {
        self.load(self.shared.config.download_model);
        for job in jobs {
            self.shared.queued.fetch_sub(1, Ordering::Relaxed);
            // (Not "busy": the page would show "working" instead of "loading".)
            if let Job::Load = job {
                self.load(false);
                continue;
            }
            self.shared.busy.store(true, Ordering::Relaxed);
            agent::clear_interrupt();
            match job {
                Job::Chat { request, events } => {
                    *self.shared.busy_project.lock().unwrap() = request.project.clone();
                    if let Err(e) = self.chat(request, &events) {
                        let _ = events.send(What::Error(format!("{e:#}")).into());
                    }
                    let _ = events.send(What::Done.into());
                }
                Job::Call { request, reply } => {
                    *self.shared.busy_project.lock().unwrap() = request.project.clone();
                    let response = self.call(&request).unwrap_or_else(|e| Response::error(format!("{e:#}")));
                    let _ = reply.send(response);
                }
                Job::Load => unreachable!("handled above"),
            }
            self.measure();
            self.shared.busy_project.lock().unwrap().clear();
            self.shared.busy.store(false, Ordering::Relaxed);
        }
    }

    fn brain(&self) -> Result<&Brain> {
        match &self.brain {
            Some(brain) => Ok(brain),
            None => {
                let state = self.shared.model_state.lock().unwrap().clone();
                match state.as_str() {
                    "missing" => bail!("the model is not downloaded yet: download it from the chat view or the dashboard (about 20 GB)"),
                    "downloading" | "verifying" => bail!("the model is still downloading; it loads by itself when it is done"),
                    _ => bail!("no model is loaded ({state})"),
                }
            }
        }
    }

    fn engine(&self) -> Option<&Engine> {
        self.brain.as_ref().and_then(|b| b.engine())
    }

    fn policy(&self, project: &Path, mode: Mode) -> Result<Policy> {
        let mut policy = Policy::new(mode, project, &self.shared.config.home)?;
        policy.sandbox &= self.shared.config.sandbox;
        policy.network = self.shared.config.network;
        Ok(policy)
    }

    /// Make the conversation in memory the one asked for (`session` 0: a new one).
    fn agent(&mut self, project: &Path, session: i64, mode: &str, think: &str) -> Result<&mut Agent> {
        let reuse = matches!(&self.current, Some((path, agent)) if path == project && session != 0 && agent.session == session);
        if !reuse {
            // The cache of the conversation in memory is given up before the next one is built.
            self.current = None;
            let store = Store::open_project(project)?;
            if session != 0 && !store.has_session(session)? {
                bail!("this project has no conversation #{session}");
            }
            let config = &self.shared.config;
            let effort = prompt::effort_level(&config.think).unwrap_or("0.2");
            let opts = Options { effort: effort.to_string(), max_tokens: config.max_tokens, max_steps: config.max_steps, context_limit: config.context_limit };
            let policy = self.policy(project, Mode::Ask)?;
            let agent = Agent::new(self.brain()?.clone(), store, self.global.clone(), policy, opts, (session != 0).then_some(session))?;
            self.global.touch_project(&project.to_string_lossy())?;
            self.current = Some((project.to_path_buf(), agent));
        }
        let agent = &mut self.current.as_mut().expect("just set").1;
        // The mode and the thinking effort are part of the prompt: a change reads it again.
        if !mode.is_empty() {
            let mode = Mode::parse(mode).with_context(|| format!("unknown mode {mode:?}: use ask, auto or read-only"))?;
            if mode != agent.policy.mode {
                agent.policy.mode = mode;
                agent.rebuild();
            }
        }
        if !think.is_empty() {
            let effort = prompt::effort_level(think).with_context(|| format!("unknown thinking level {think:?}"))?;
            if effort != agent.opts.effort {
                agent.opts.effort = effort.to_string();
                agent.rebuild();
            }
        }
        Ok(agent)
    }

    fn chat(&mut self, request: Request, tx: &tokio::sync::mpsc::UnboundedSender<Event>) -> Result<()> {
        let project = self.shared.roots.resolve(&request.project)?;
        let shared = self.shared.clone();
        let agent = self.agent(&project, request.session, &request.mode, &request.think)?;
        let session = agent.session;
        let _ = tx.send(What::Session(session).into());
        let sink = Rc::new(RefCell::new(Sink { tx: tx.clone(), log: Store::open_project(&project)?, session, thinking: String::new(), text: String::new() }));

        // Images sent with the message are stored in the project's store first.
        let mut images = Vec::new();
        for file in &request.files {
            images.push(store_image(&agent.store, file)?);
        }
        let shown = if images.is_empty() { request.text.clone() } else { format!("{}\n[{}]", request.text, images.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")) };
        sink.borrow().log.log_event(session, &What::User(shown).into())?;

        // An approval goes to the client as an event; the model waits here for the answer.
        let (for_asker, asker_shared) = (sink.clone(), shared.clone());
        agent.policy.asker = Some(Box::new(move |what, offer_always| {
            let id = asker_shared.next_approval.fetch_add(1, Ordering::Relaxed);
            let (answer_tx, answer_rx) = mpsc::channel();
            asker_shared.approvals.lock().unwrap().insert(id, answer_tx);
            for_asker.borrow_mut().handle(What::Approval { id, what: what.to_string(), offer_always });
            let asked = Instant::now();
            let answer = loop {
                match answer_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(answer) => break Some(answer),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break None,
                    // Nobody will answer if the client is gone, the user interrupted, or it took too long.
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if for_asker.borrow().tx.is_closed() {
                            agent::interrupt();
                        }
                        if agent::interrupted() || asked.elapsed() > APPROVAL_TIMEOUT {
                            break None;
                        }
                    }
                }
            };
            asker_shared.approvals.lock().unwrap().remove(&id);
            match answer {
                Some(answer) => {
                    for_asker.borrow_mut().handle(What::Answered(answer.clone()));
                    Answer::parse(&answer, offer_always)
                }
                None => {
                    for_asker.borrow_mut().handle(What::Answered("(no answer)".into()));
                    Answer::No("the user did not answer, so this was not approved".into())
                }
            }
        }));

        let for_ui = sink.clone();
        let mut ui = Ui::new(move |what| for_ui.borrow_mut().handle(what));
        // A command of the user's own (`/name ...`) is sent as the text its file holds.
        let text = expand_command(&project, &shared.config.home, &request.text);
        let result = agent.send(&text, images, &mut ui);
        agent.policy.asker = None;
        drop(ui);
        let tokens = agent.tokens();
        let mut sink = sink.borrow_mut();
        sink.flush();
        match result {
            Ok(usage) => sink.handle(What::Usage(Usage {
                context_tokens: tokens as u64,
                generated: usage.generated as u64,
                tokens_per_second: {
                    // The last turn's speeds, for the dashboard (a turn that wrote nothing
                    // leaves the ones before).
                    let writing = if usage.gen_seconds > 0.0 { usage.generated as f64 / usage.gen_seconds } else { 0.0 };
                    let reading = if usage.read_seconds > 0.0 { usage.read as f64 / usage.read_seconds } else { 0.0 };
                    if writing > 0.0 {
                        *shared.speed.lock().unwrap() = (writing, reading);
                    }
                    writing
                },
                tool_calls: usage.tool_calls as u32,
                seconds: usage.seconds,
                steps: usage.steps as u32,
            })),
            Err(e) => sink.handle(What::Error(format!("{e:#}"))),
        }
        Ok(())
    }

    /// The calls that need the model or must not run beside a turn.
    fn call(&mut self, request: &Request) -> Result<Response> {
        let project = self.shared.roots.resolve(&request.project)?;
        match request.method {
            Method::KbAdd => {
                let store = Store::open_project(&project)?;
                let mut docs = Vec::new();
                if !request.text.is_empty() {
                    let title = if request.name.is_empty() { request.text.lines().next().unwrap_or("note").chars().take(60).collect() } else { request.name.clone() };
                    let id = kb::save(&store, self.engine(), "text", &title, "", &request.text, None)?;
                    docs.push(doc_of(&store, id)?);
                }
                for file in &request.files {
                    let title = if request.name.is_empty() || request.files.len() > 1 { file.name.clone() } else { request.name.clone() };
                    let id = save_upload(&store, self.engine(), file, &title)?;
                    docs.push(doc_of(&store, id)?);
                }
                if docs.is_empty() {
                    bail!("nothing to save: give a text or a file");
                }
                Ok(Response { docs, ..Response::default() })
            }
            Method::KbSearch => {
                let Some(store) = Store::open_project_existing(&project)? else { return Ok(Response::default()) };
                let limit = if request.limit == 0 { 5 } else { request.limit.min(50) } as usize;
                let hits = kb::search(&store, self.engine(), &request.text, limit)?;
                Ok(Response { hits: hits.iter().map(hit_of).collect(), ..Response::default() })
            }
            Method::Compact => {
                let agent = self.agent(&project, request.session, "", "")?;
                let before = agent.messages.len();
                agent.compact(&mut Ui::muted())?;
                let were = if before == 1 { "1 message was".to_string() } else { format!("{before} messages were") };
                Ok(Response::text(format!("{were} replaced by a summary. The full transcript is kept in the log.")))
            }
            Method::Undo => {
                let store = Store::open_project_existing(&project)?.with_context(|| format!("this project has no conversation #{}", request.session))?;
                if !store.has_session(request.session)? {
                    bail!("this project has no conversation #{}", request.session);
                }
                let restored = tools::undo(&self.policy(&project, Mode::Auto)?, &store, request.session)?;
                if restored.is_empty() {
                    return Ok(Response::text("There are no file changes to undo in this conversation."));
                }
                let text = restored.join("\n");
                store.log_event(request.session, &What::Note(format!("undo:\n{text}")).into())?;
                // The model is told, so that it does not go on from files that are gone.
                let note = prompt::Message::User { text: format!("[The user undid your file changes of the last turn. These files are back as they were before it:\n{text}]"), images: vec![] };
                match &mut self.current {
                    Some((path, agent)) if *path == project && agent.session == request.session => {
                        agent.store.append_message(agent.session, &note)?;
                        agent.messages.push(note);
                    }
                    _ => store.append_message(request.session, &note)?,
                }
                Ok(Response::text(text))
            }
            other => bail!("{other:?} is not a call for the model thread"),
        }
    }
}

/// Store an image that came with a message, after checking that it is one.
pub fn store_image(store: &Store, file: &Upload) -> Result<ImageRef> {
    let format = image::guess_format(&file.data).with_context(|| format!("{} is not an image cwbcode can read (PNG, JPEG, WebP, GIF, BMP)", file.name))?;
    image::load_from_memory_with_format(&file.data, format).with_context(|| format!("{} could not be decoded as an image", file.name))?;
    let name = Path::new(&file.name).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "image".into());
    // The image encoder reads a file and goes by its extension.
    let name = if Path::new(&name).extension().is_some() { name } else { format!("{name}.{}", format.extensions_str().first().copied().unwrap_or("png")) };
    Ok(ImageRef { blob: store.put_blob(&name, format.to_mime_type(), &file.data)?, name })
}

/// Save an uploaded file in the knowledge base: text is indexed, an image is stored with a
/// description by the model, anything else is stored as it is.
pub fn save_upload(store: &Store, engine: Option<&Engine>, file: &Upload, title: &str) -> Result<i64> {
    if file.data.len() > cwbcode_proto::MAX_UPLOAD_BYTES {
        bail!("{} is {} MB; files up to {} MB can be saved", file.name, file.data.len() >> 20, cwbcode_proto::MAX_UPLOAD_BYTES >> 20);
    }
    if image::guess_format(&file.data).is_ok() {
        let image = store_image(store, file)?;
        let description = match engine {
            Some(engine) => agent::describe_image(engine, &store.blob_file(image.blob)?)?,
            None => format!("{title} (an image; no model was loaded to describe it)"),
        };
        return kb::save(store, engine, "image", title, &file.name, &description, Some(image.blob));
    }
    let mime = if std::str::from_utf8(&file.data).is_ok() { "text/plain" } else { "application/octet-stream" };
    let blob = store.put_blob(&file.name, mime, &file.data)?;
    match std::str::from_utf8(&file.data) {
        Ok(text) => kb::save(store, engine, "file", title, &file.name, text, Some(blob)),
        Err(_) => kb::save(store, engine, "file", title, &file.name, &format!("{title} ({} bytes; not text, stored as it is)", file.data.len()), Some(blob)),
    }
}

pub fn doc_of(store: &Store, id: i64) -> Result<Doc> {
    let d = store.doc(id)?;
    Ok(Doc { id: d.id, kind: d.kind, title: d.title, source: d.source, text: d.text, created: d.created, has_file: d.blob.is_some() })
}

fn hit_of(hit: &kb::Hit) -> Hit {
    Hit {
        doc: hit.chunk.doc,
        kind: hit.chunk.kind.clone(),
        title: hit.chunk.title.clone(),
        snippet: hit.chunk.text.chars().take(600).collect(),
        bm25_rank: hit.bm25_rank.unwrap_or(0) as u32,
        semantic_rank: hit.semantic_rank.unwrap_or(0) as u32,
        score: hit.score,
    }
}
/// The user's own commands: a message `/name the rest` is replaced by the text of
/// `commands/name.md`, looked for in the project's `.cwbcode` folder and then in the
/// server's. `$ARGUMENTS` in the text stands for the rest of the message (which is added
/// at the end where the text does not name it). A message that names no such file is sent
/// as it is. The agent cannot write these files: they are among cwbcode's own records.
pub fn expand_command(project: &Path, home: &Path, message: &str) -> String {
    let Some(rest) = message.strip_prefix('/') else { return message.to_string() };
    let (name, arguments) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return message.to_string();
    }
    let found = [project.join(".cwbcode"), home.to_path_buf()].iter().find_map(|folder| std::fs::read_to_string(folder.join("commands").join(format!("{name}.md"))).ok());
    match found {
        Some(text) if text.contains("$ARGUMENTS") => text.replace("$ARGUMENTS", arguments.trim()),
        Some(text) if arguments.trim().is_empty() => text,
        Some(text) => format!("{}\n\n{}", text.trim_end(), arguments.trim()),
        None => message.to_string(),
    }
}


#[cfg(test)]
mod command_tests {
    use super::expand_command;

    #[test]
    fn a_users_command_is_sent_as_the_text_of_its_file() {
        let base = std::env::temp_dir().join(format!("cwbcode-commands-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (project, home) = (base.join("proj"), base.join("home"));
        std::fs::create_dir_all(project.join(".cwbcode/commands")).unwrap();
        std::fs::create_dir_all(home.join("commands")).unwrap();
        std::fs::write(project.join(".cwbcode/commands/review.md"), "Review $ARGUMENTS for bugs. List them.").unwrap();
        std::fs::write(home.join("commands/review.md"), "the server's own review").unwrap();
        std::fs::write(home.join("commands/ship-it.md"), "Run the tests, then summarise.\n").unwrap();
        let go = |message: &str| expand_command(&project, &home, message);
        // The project's command comes before the server's of the same name.
        assert_eq!(go("/review src/lib.rs"), "Review src/lib.rs for bugs. List them.");
        assert_eq!(go("/ship-it"), "Run the tests, then summarise.\n");
        assert_eq!(go("/ship-it but skip the slow ones"), "Run the tests, then summarise.\n\nbut skip the slow ones");
        // Anything else is sent as it was typed, and a name cannot walk out of the folder.
        for plain in ["hello", "/unknown thing", "/", "/../../etc/passwd", "/review.md", "a /review"] {
            assert_eq!(go(plain), plain);
        }
    }
}
