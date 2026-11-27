//! The backend, tested from outside: these start the real `cwbcode` program (built by
//! `cargo build`, with a scripted model instead of the 20 GB one) in a scratch home folder,
//! and talk to it with this crate's client over HTTP/3 and HTTP/2.
//!
//! Nothing here touches the real ~/.cwbcode: the server is given its own HOME.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use cwbcode_proto::{Event, Method, Request, Response, Upload, What};
use cwbcodeclient::net::{Client, Events, Trust};

struct Server {
    child: Child,
    port: u16,
    args: Vec<String>,
    /// The scratch home folder; projects go under it.
    home: PathBuf,
    url: String,
    token: String,
    trust: Trust,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn backend_binary() -> PathBuf {
    // <target>/<profile>/cwbcode, where <target> is CARGO_TARGET_DIR or the workspace's own.
    // (Not found from where this test program is: newer Cargos keep test programs elsewhere
    // than target/<profile>/deps.)
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target"));
    let binary = target.join(if cfg!(debug_assertions) { "debug" } else { "release" }).join("cwbcode");
    assert!(binary.is_file(), "{} is not built: run `cargo build{}` first", binary.display(), if cfg!(debug_assertions) { "" } else { " --release" });
    binary
}

fn free_port() -> u16 {
    // Each test in this process gets a port of its own (two tests asking the system for "any
    // free port" at the same moment can be given the same one), free for both TCP and UDP.
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    // Below the range the system hands out to clients (49152 and up).
    let base = 21000 + (std::process::id() % 250) as u16 * 100;
    loop {
        // Two at a time: HTTPS on one (TCP), HTTP/3 on the next (UDP).
        let port = base + 2 * NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() && std::net::UdpSocket::bind(("127.0.0.1", cwbcode_proto::quic_port(port))).is_ok() {
            return port;
        }
    }
}

impl Server {
    /// Start a backend whose model follows `script` (JSON turns), with extra arguments.
    fn start(name: &str, script: &str, args: &[&str]) -> Self {
        // For the plain HTTPS client some tests use beside this crate's own.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let home = std::env::temp_dir().join(format!("cwbcode-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("proj")).unwrap();
        let home = home.canonicalize().unwrap();
        std::fs::write(home.join("script.json"), script).unwrap();
        // An argument "file:PATH=content" is a file in the home folder, there before it starts.
        for (path, content) in args.iter().filter_map(|a| a.strip_prefix("file:")?.split_once('=')) {
            let path = home.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        let args: Vec<String> = args.iter().filter(|a| !a.starts_with("file:")).map(|a| a.to_string()).collect();
        // A port can be taken between finding it free and the backend binding it: try another.
        let mut attempt = 0;
        let mut server = loop {
            attempt += 1;
            let port = free_port();
            let child = Self::spawn(&home, port, &args);
            let mut server = Self { child, port, args: args.clone(), home: home.clone(), url: format!("https://127.0.0.1:{port}"), token: String::new(), trust: Trust::Insecure };
            let start = Instant::now();
            let up = loop {
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() && home.join(".cwbcode/token").exists() {
                    break true;
                }
                if server.child.try_wait().unwrap().is_some() {
                    break false;
                }
                assert!(start.elapsed() < Duration::from_secs(20), "the backend did not start");
                std::thread::sleep(Duration::from_millis(50));
            };
            if up {
                break server;
            }
            assert!(attempt < 5, "the backend exited at once, five times: {}", std::fs::read_to_string(home.join("server.log")).unwrap_or_default());
        };
        server.token = std::fs::read_to_string(home.join(".cwbcode/token")).unwrap();
        server.trust = Trust::from_pem(&std::fs::read(home.join(".cwbcode/cert.pem")).unwrap()).unwrap();
        server
    }

    fn spawn(home: &Path, port: u16, args: &[String]) -> Child {
        let real_home = std::env::var("HOME").unwrap();
        // "no-script": a backend that looks for the real model (the download's test).
        let script: Vec<std::ffi::OsString> = if args.iter().any(|a| a == "no-script") { vec![] } else { vec!["--script".into(), home.join("script.json").into()] };
        Command::new(backend_binary())
            // (No .env of the developer's is to decide how a test's backend behaves.)
            .args(["serve", "--env-file", "/dev/null"])
            .args(script)
            // "movable": the backend chooses its address by its mode (this machine only, or
            // every interface in remote mode) instead of being told one.
            .args(if args.iter().any(|a| a == "movable") { vec!["--port".to_string(), port.to_string()] } else { vec!["--bind".to_string(), format!("127.0.0.1:{port}")] })
            // An argument "env:NAME=value" is given to the backend as an environment variable.
            .args(args.iter().filter(|a| !a.starts_with("env:") && *a != "movable" && *a != "no-script"))
            .env_remove("CWBCODE_WALLET")
            // (Nor the developer's Hugging Face token, nor how they like their downloads.)
            .env_remove("HF_TOKEN")
            .env_remove("HUGGING_FACE_HUB_TOKEN")
            .env_remove("HF_TOKEN_PATH")
            .env_remove("HF_HOME")
            .env_remove("CWBCODE_DOWNLOAD_MODEL")
            .envs(args.iter().filter_map(|a| a.strip_prefix("env:")?.split_once('=')))
            .env("HOME", home)
            .env("CWBCODE_HOME", home.join(".cwbcode"))
            // The Rust toolchain (for tools made at runtime) lives in the real home folder.
            .env("RUSTUP_HOME", std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{real_home}/.rustup")))
            .env("CARGO_HOME", std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{real_home}/.cargo")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Kept, so that a backend that stops says why.
            .stderr(std::fs::OpenOptions::new().create(true).append(true).open(home.join("server.log")).map(Stdio::from).unwrap_or_else(|_| Stdio::null()))
            .spawn()
            .unwrap()
    }

    /// Stop the backend and start it again on the same port, with the same home.
    fn restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.child = Self::spawn(&self.home, self.port, &self.args);
        let start = Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err() {
            assert!(start.elapsed() < Duration::from_secs(20), "the backend did not come back");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn project(&self) -> String {
        self.home.join("proj").to_string_lossy().into_owned()
    }

    async fn client(&self, h3: bool) -> Client {
        Client::connect(&self.url, &self.token, self.trust.clone(), h3).await.unwrap()
    }
}

fn chat(server: &Server, session: i64, text: &str, mode: &str) -> Request {
    let mut request = Request::new(Method::Chat).project(&server.project()).session(session).text(text);
    request.mode = mode.to_string();
    request
}

/// Read events until `stop` says so (or the stream ends), with a time limit.
async fn read_until(events: &mut Events, mut stop: impl FnMut(&What) -> bool) -> Vec<What> {
    let mut seen = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(60), events.next()).await.expect("no event for 60 seconds").unwrap();
        match next {
            Some(Event { what, .. }) => {
                let done = stop(&what);
                seen.push(what);
                if done {
                    return seen;
                }
            }
            None => return seen,
        }
    }
}

async fn read_all(events: &mut Events) -> Vec<What> {
    read_until(events, |_| false).await
}

/// The answer text of a turn, pieces joined.
fn text_of(events: &[What]) -> String {
    events.iter().filter_map(|w| if let What::Text(t) = w { Some(t.as_str()) } else { None }).collect()
}

fn session_of(events: &[What]) -> i64 {
    events.iter().find_map(|w| if let What::Session(id) = w { Some(*id) } else { None }).expect("the stream names its conversation")
}

async fn wait_idle(client: &Client) {
    let start = Instant::now();
    loop {
        let status = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
        if !status.busy && status.queued == 0 {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "the model thread stayed busy");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const HELLO: &str = r#"[{"thinking": "Say hello.", "text": "Hello from the script, in several pieces."}]"#;

#[tokio::test(flavor = "multi_thread")]
async fn http3_is_used_and_http2_when_quic_is_off() {
    let twice = format!("[{0},{0}]", HELLO.trim_start_matches('[').trim_end_matches(']'));
    let server = Server::start("h3", &twice, &[]);
    let client = server.client(true).await;
    assert_eq!(client.protocol(), "h3");
    assert!(client.fallback.is_none());
    let status = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
    // The server says what it saw.
    assert_eq!((status.protocol.as_str(), status.h3, status.model_state.as_str()), ("h3", true, "scripted"));
    // No model files behind a script, and no speed before the first answer.
    assert_eq!((status.model_path.as_str(), status.model_bytes, status.model_info.as_str(), status.gen_tps), ("", 0, "", 0.0));
    // A client told not to use QUIC talks HTTP/2 to the same server.
    let over_tcp = server.client(false).await;
    let (response, arrived_by) = over_tcp.call_raw(&Request::new(Method::Status)).await.unwrap();
    assert_eq!((over_tcp.protocol(), arrived_by.as_str(), response.status.unwrap().protocol.as_str()), ("h2", "h2", "h2"));
    // A chat streams over both.
    for client in [&client, &over_tcp] {
        let events = read_all(&mut client.chat(&chat(&server, 0, "hi", "")).await.unwrap()).await;
        assert_eq!(text_of(&events), "Hello from the script, in several pieces.", "{}", client.protocol());
        assert!(events.iter().filter(|w| matches!(w, What::Text(_))).count() > 3, "the answer arrived in pieces");
        assert_eq!(events.last(), Some(&What::Done));
    }
    // The last turn's writing speed is reported once there was one.
    wait_idle(&client).await;
    let after = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
    assert!(after.gen_tps > 0.0, "no writing speed after an answer: {}", after.gen_tps);

    // A server without HTTP/3: the client tries QUIC, gets no answer, and falls back.
    let tcp_only = Server::start("h2only", HELLO, &["--no-h3"]);
    let client = tcp_only.client(true).await;
    assert_eq!(client.protocol(), "h2");
    assert!(client.fallback.is_some());
    let status = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
    assert_eq!((status.protocol.as_str(), status.h3), ("h2", false));
    assert_eq!(text_of(&read_all(&mut client.chat(&chat(&tcp_only, 0, "hi", "")).await.unwrap()).await), "Hello from the script, in several pieces.");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_and_the_certificate_are_required() {
    // A backend that asks for its token (as one listening beyond this machine without a wallet does).
    let server = Server::start("auth", HELLO, &["--require-token"]);
    for h3 in [true, false] {
        let wrong = Client::connect(&server.url, "not-the-token", server.trust.clone(), h3).await.unwrap();
        let refused = wrong.call(&Request::new(Method::Status)).await.unwrap_err().to_string();
        assert!(refused.contains("refused the token"), "{refused}");
        assert!(wrong.chat(&chat(&server, 0, "hi", "")).await.is_err());
        let none = Client::connect(&server.url, "", server.trust.clone(), h3).await.unwrap();
        assert!(none.call(&Request::new(Method::Projects)).await.is_err());
    }
    // Another certificate than the server's is not trusted, over either protocol.
    let other = Server::start("auth-other", HELLO, &["--require-token"]);
    let mismatched = Client::connect(&server.url, &server.token, other.trust.clone(), true).await.unwrap();
    assert_eq!(mismatched.protocol(), "h2", "QUIC with the wrong certificate must not connect");
    assert!(mismatched.call(&Request::new(Method::Status)).await.is_err());
    // The web page itself needs no token; the API behind it does.
    let page = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap().get(&server.url).send().await.unwrap();
    assert!(page.status().is_success());
    // HTTP/3 is not advertised to browsers (they would try it and refuse the certificate).
    assert_eq!(page.headers().get("alt-svc").unwrap().to_str().unwrap(), "clear");
    // Asked to (--advertise-h3), it is: on the UDP port after the HTTPS one, for an hour.
    let advertising = Server::start("auth-h3", HELLO, &["--advertise-h3"]);
    let page = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap().get(&advertising.url).send().await.unwrap();
    assert_eq!(page.headers().get("alt-svc").unwrap().to_str().unwrap(), format!("h3=\":{}\"; ma=3600", cwbcode_proto::quic_port(advertising.port)));
    // Not without HTTP/3 itself.
    let without = Server::start("auth-no-h3", HELLO, &["--advertise-h3", "--no-h3"]);
    let page = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap().get(&without.url).send().await.unwrap();
    assert_eq!(page.headers().get("alt-svc").unwrap().to_str().unwrap(), "clear");
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_are_folders_under_home_with_their_own_store() {
    let twice = format!("[{0},{0}]", HELLO.trim_start_matches('[').trim_end_matches(']'));
    let server = Server::start("projects", &twice, &[]);
    let client = server.client(true).await;
    // A folder that does not exist is not a project to talk in ...
    for outside in ["/etc", "/", "/tmp", "~", "~/.cwbcode", "~/missing", "relative/path"] {
        assert!(client.call(&Request::new(Method::Sessions).project(outside)).await.is_err(), "{outside}");
        let mut stream = client.chat(&Request::new(Method::Chat).project(outside).text("hi")).await.unwrap();
        assert!(read_all(&mut stream).await.iter().any(|w| matches!(w, What::Error(_))), "{outside}");
    }
    assert!(!server.home.join("missing").exists());
    // ... and where the backend may not work, none is opened or made.
    for outside in ["/etc", "/", "/tmp", "/tmp/cwbcode-it-made-outside", "~", "~/.cwbcode", "~/.cwbcode/new", "~/.ssh/new", "~/proj/../../escaped", "relative/path"] {
        assert!(client.call(&Request::new(Method::OpenProject).text(outside)).await.is_err(), "{outside} was accepted as a project");
    }
    assert!(!Path::new("/tmp/cwbcode-it-made-outside").exists() && !server.home.join(".ssh").exists() && !server.home.parent().unwrap().join("escaped").exists());
    // Opening a folder under home that does not exist makes it, with the folders above it.
    let made = client.call(&Request::new(Method::OpenProject).text("~/brand/new/project")).await.unwrap();
    assert!(made.text.starts_with("created ") && made.text.ends_with("/brand/new/project"), "{}", made.text);
    assert!(server.home.join("brand/new/project/.cwbcode/cwbcode.db").exists());
    assert_eq!(client.call(&Request::new(Method::OpenProject).text("~/brand/new/project")).await.unwrap().text, "", "opening it again makes nothing");
    let opened = client.call(&Request::new(Method::OpenProject).text("~/proj")).await.unwrap();
    assert_eq!(opened.projects[0].path, server.project());
    assert_eq!(opened.projects[0].name, "proj");
    // Asking about a folder leaves no records in it.
    std::fs::create_dir_all(server.home.join("untouched")).unwrap();
    let untouched = server.home.join("untouched").to_string_lossy().into_owned();
    assert!(client.call(&Request::new(Method::Sessions).project(&untouched)).await.unwrap().sessions.is_empty());
    assert!(client.call(&Request::new(Method::KbList).project(&untouched)).await.unwrap().docs.is_empty());
    assert!(client.call(&Request::new(Method::KbSearch).project(&untouched).text("anything")).await.unwrap().hits.is_empty());
    assert!(client.call(&Request::new(Method::Undo).project(&untouched).session(1)).await.is_err());
    assert!(!server.home.join("untouched/.cwbcode").exists());
    // The project's records are in its own folder, kept out of git.
    assert!(server.home.join("proj/.cwbcode/cwbcode.db").exists());
    assert_eq!(std::fs::read_to_string(server.home.join("proj/.cwbcode/.gitignore")).unwrap().lines().last(), Some("*"));
    let events = read_all(&mut client.chat(&chat(&server, 0, "hello there", "")).await.unwrap()).await;
    let session = session_of(&events);
    let listed = client.call(&Request::new(Method::Projects)).await.unwrap().projects;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().find(|p| p.name == "proj").unwrap().sessions, 1);
    // The user's own "run it": a command given, or one guessed from the folder, in the project.
    std::fs::write(server.home.join("proj/hello.sh"), "echo run from $(basename \"$PWD\")\n").unwrap();
    let ran = client.call(&Request::new(Method::Run).project("~/proj")).await.unwrap().text;
    assert_eq!(ran, "$ sh 'hello.sh'\nrun from proj\n[done]");
    let ran = client.call(&Request::new(Method::Run).project("~/proj").text("echo given; exit 2")).await.unwrap().text;
    assert_eq!(ran, "$ echo given; exit 2\ngiven\n[exit status 2]");
    assert!(client.call(&Request::new(Method::Run).project("/etc").text("id")).await.is_err(), "a command was run outside home");
    // Clients are told the home folder, to show paths under it as ~/...
    let told = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().user_home;
    assert_eq!(std::fs::canonicalize(&told).unwrap(), std::fs::canonicalize(&server.home).unwrap());
    // A project is taken off the list and nothing else: its folder, its files and its own
    // records stay, and opening it again brings it back with its conversations.
    client.call(&Request::new(Method::RemoveProject).project("~/proj")).await.unwrap();
    let listed = client.call(&Request::new(Method::Projects)).await.unwrap().projects;
    assert!(listed.iter().all(|p| p.name != "proj"), "{listed:?}");
    assert!(server.home.join("proj/.cwbcode/cwbcode.db").exists(), "removing a project from the list deleted its records");
    for outside in ["/etc", "~/.ssh", "~/proj/../../escaped"] {
        assert!(client.call(&Request::new(Method::RemoveProject).project(outside)).await.is_err(), "{outside}");
    }
    client.call(&Request::new(Method::OpenProject).text("~/proj")).await.unwrap();
    let listed = client.call(&Request::new(Method::Projects)).await.unwrap().projects;
    assert_eq!(listed.iter().find(|p| p.name == "proj").unwrap().sessions, 1, "its conversation did not come back");
    client.call(&Request::new(Method::NameSession).project(&server.project()).session(session).name("greeting")).await.unwrap();
    let sessions = client.call(&Request::new(Method::Sessions).project(&server.project())).await.unwrap().sessions;
    assert_eq!((sessions[0].id, sessions[0].name.as_str(), sessions[0].first.as_str()), (session, "greeting", "hello there"));
    assert!(client.call(&Request::new(Method::Transcript).project(&server.project()).session(999)).await.is_err());
    // A conversation is removed for good, with its log; one that is not there is an error, and
    // the project's files stay.
    assert!(client.call(&Request::new(Method::RemoveSession).project(&server.project()).session(999)).await.is_err());
    let said = client.call(&Request::new(Method::RemoveSession).project(&server.project()).session(session)).await.unwrap().text;
    assert!(said.contains(&format!("#{session}")) && said.contains("removed"), "{said}");
    assert!(client.call(&Request::new(Method::Sessions).project(&server.project())).await.unwrap().sessions.is_empty());
    assert!(client.call(&Request::new(Method::Transcript).project(&server.project()).session(session)).await.is_err());
    assert!(server.home.join("proj").is_dir());
    // The next message starts a new conversation, with another number.
    let events = read_all(&mut client.chat(&chat(&server, 0, "hello again", "")).await.unwrap()).await;
    assert_ne!(session_of(&events), session);
}

const EDITS: &str = r#"[
  {"thinking": "I will write the file.", "calls": [{"name": "write_file", "args": {"path": "notes.txt", "content": "first version\n"}}]},
  {"text": "Written."},
  {"calls": [{"name": "edit_file", "args": {"path": "notes.txt", "old_string": "first", "new_string": "second"}},
             {"name": "write_file", "args": {"path": "extra/new.txt", "content": "new file"}},
             {"name": "todo_write", "args": {"todos": [{"content": "edit the notes", "status": "completed"}, {"content": "tell the user", "status": "in_progress"}]}}]},
  {"text": "Edited."},
  {"calls": [{"name": "bash", "args": {"command": "echo ran > ran.txt; cat notes.txt"}}]},
  {"text": "Ran."}
]"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_asks_runs_tools_logs_everything_and_can_be_undone() {
    let server = Server::start("turn", EDITS, &[]);
    let client = server.client(true).await;
    let proj = server.home.join("proj");

    // Ask mode: the write waits for the user's answer.
    let mut stream = client.chat(&chat(&server, 0, "write the notes", "ask")).await.unwrap();
    let before = read_until(&mut stream, |w| matches!(w, What::Approval { .. })).await;
    let session = session_of(&before);
    let Some(What::Approval { id, what, offer_always }) = before.last().cloned() else { panic!("no approval was asked: {before:?}") };
    assert!(what.starts_with("Create notes.txt") && what.contains("+ first version") && offer_always, "{what}");
    assert!(!proj.join("notes.txt").exists(), "the file was written before the answer");
    // Other calls are answered while the model waits.
    let status = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
    assert!(status.busy && status.busy_project == server.project());
    client.call(&Request::new(Method::Approve).id(id).text("y")).await.unwrap();
    // An answer is taken once.
    assert!(client.call(&Request::new(Method::Approve).id(id).text("y")).await.is_err());
    let rest = read_all(&mut stream).await;
    assert_eq!(std::fs::read_to_string(proj.join("notes.txt")).unwrap(), "first version\n");
    assert!(rest.iter().any(|w| matches!(w, What::ToolEnd { ok: true, text, .. } if text.starts_with("Created notes.txt"))));
    assert_eq!(text_of(&rest), "Written.");
    assert!(matches!(rest[rest.len() - 2], What::Usage(ref u) if u.tool_calls == 1 && u.steps == 2));

    // Auto mode: an edit, a new file and the task list, without asking.
    let second = read_all(&mut client.chat(&chat(&server, session, "now edit them", "auto")).await.unwrap()).await;
    assert!(!second.iter().any(|w| matches!(w, What::Approval { .. })));
    assert_eq!(std::fs::read_to_string(proj.join("notes.txt")).unwrap(), "second version\n");
    assert_eq!(std::fs::read_to_string(proj.join("extra/new.txt")).unwrap(), "new file");
    assert!(second.iter().any(|w| matches!(w, What::Todos(t) if t.contains("tell the user"))));

    // Everything that happened is in the log, in order, whole.
    let transcript = client.call(&Request::new(Method::Transcript).project(&server.project()).session(session)).await.unwrap();
    let kinds: Vec<&str> = transcript
        .events
        .iter()
        .map(|e| match &e.what {
            What::User(_) => "user",
            What::Thinking(_) => "thinking",
            What::Text(_) => "text",
            What::ToolStart { .. } => "start",
            What::ToolEnd { .. } => "end",
            What::Approval { .. } => "approval",
            What::Answered(_) => "answered",
            What::Usage(_) => "usage",
            What::Todos(_) => "todos",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["user", "thinking", "start", "approval", "answered", "end", "text", "usage", "user", "start", "end", "start", "end", "start", "end", "todos", "text", "usage"]);
    assert!(transcript.events.iter().all(|e| !e.created.is_empty()));
    assert!(transcript.events.iter().any(|e| e.what == What::Thinking("I will write the file.".into())));
    assert!(transcript.text.contains("edit the notes"), "the task list comes with the transcript");

    // Every version of every file is stored.
    let changes = client.call(&Request::new(Method::Changes).project(&server.project()).session(session)).await.unwrap().changes;
    let summary: Vec<String> = changes.iter().map(|c| format!("{} {} {}>{}", c.action, c.path, c.bytes_before, c.bytes_after)).collect();
    assert_eq!(summary, ["create notes.txt -1>14", "edit notes.txt 14>15", "create extra/new.txt -1>8"]);

    // Undo takes back the last turn's changes, byte for byte, then the one before.
    let undone = client.call(&Request::new(Method::Undo).project(&server.project()).session(session)).await.unwrap().text;
    assert_eq!(undone, "removed extra/new.txt (it did not exist before)\nrestored notes.txt");
    assert_eq!(std::fs::read(proj.join("notes.txt")).unwrap(), b"first version\n");
    assert!(!proj.join("extra/new.txt").exists());
    client.call(&Request::new(Method::Undo).project(&server.project()).session(session)).await.unwrap();
    assert!(!proj.join("notes.txt").exists());
    assert!(client.call(&Request::new(Method::Undo).project(&server.project()).session(session)).await.unwrap().text.contains("no file changes"));

    // A command runs in the project folder, inside the sandbox.
    std::fs::write(proj.join("notes.txt"), "by hand\n").unwrap();
    let third = read_all(&mut client.chat(&chat(&server, session, "run it", "auto")).await.unwrap()).await;
    assert!(third.iter().any(|w| matches!(w, What::ToolEnd { ok: true, text, .. } if text.trim() == "by hand")), "{third:?}");
    assert_eq!(std::fs::read_to_string(proj.join("ran.txt")).unwrap(), "ran\n");
}

const NEEDS_APPROVAL: &str = r#"[
  {"calls": [{"name": "write_file", "args": {"path": "a.txt", "content": "a"}}]},
  {"text": "first done"},
  {"calls": [{"name": "write_file", "args": {"path": "b.txt", "content": "b"}}]},
  {"text": "second done"},
  {"calls": [{"name": "write_file", "args": {"path": "c.txt", "content": "c"}}]},
  {"text": "third done"},
  {"text": "fourth done"}
]"#;

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_approval_does_not_hold_the_model() {
    let server = Server::start("approval", NEEDS_APPROVAL, &[]);
    let proj = server.home.join("proj");
    for h3 in [true, false] {
        let client = server.client(h3).await;
        // The client goes away while the model waits for an answer.
        let mut stream = client.chat(&chat(&server, 0, "write", "ask")).await.unwrap();
        read_until(&mut stream, |w| matches!(w, What::Approval { .. })).await;
        drop(stream);
        drop(client);
        let client = server.client(h3).await;
        wait_idle(&client).await;
        // Nothing was written, and the next request is served.
        assert!(!proj.join("a.txt").exists() && !proj.join("b.txt").exists() && !proj.join("c.txt").exists(), "h3={h3}");
        let next = read_all(&mut client.chat(&chat(&server, 0, "again", "read-only")).await.unwrap()).await;
        assert!(text_of(&next).ends_with("done"), "h3={h3}: {next:?}");
    }
    // An interrupt answers a pending approval with no; "no" itself is passed to the model.
    let client = server.client(true).await;
    let mut stream = client.chat(&chat(&server, 0, "write", "ask")).await.unwrap();
    read_until(&mut stream, |w| matches!(w, What::Approval { .. })).await;
    assert_eq!(client.call(&Request::new(Method::Interrupt)).await.unwrap().text, "stopping");
    let rest = read_all(&mut stream).await;
    assert!(rest.iter().any(|w| matches!(w, What::ToolEnd { ok: false, .. })), "{rest:?}");
    assert!(rest.iter().any(|w| matches!(w, What::Warning(t) if t.contains("interrupted"))));
    assert!(!proj.join("c.txt").exists());
    wait_idle(&client).await;
    assert_eq!(client.call(&Request::new(Method::Interrupt)).await.unwrap().text, "nothing is running");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_knowledge_base_keeps_text_and_files_byte_for_byte() {
    let server = Server::start("kb", HELLO, &[]);
    let client = server.client(true).await;
    let project = server.project();
    let note = client.call(&Request::new(Method::KbAdd).project(&project).name("Deploy").text("We deploy on Thursdays; Minsu owns it.")).await.unwrap().docs;
    assert_eq!((note[0].kind.as_str(), note[0].title.as_str(), note[0].has_file), ("text", "Deploy", false));
    // A text file and a binary one, a megabyte of every byte value.
    let binary: Vec<u8> = (0..1_000_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let mut add = Request::new(Method::KbAdd).project(&project);
    add.files = vec![Upload { name: "guide.md".into(), data: "# Guide\nRestart the router when the wifi drops.\n".into() }, Upload { name: "dump.bin".into(), data: binary.clone() }];
    let added = client.call(&add).await.unwrap().docs;
    assert_eq!(added.iter().map(|d| d.title.as_str()).collect::<Vec<_>>(), ["guide.md", "dump.bin"]);
    assert_eq!(client.call(&Request::new(Method::KbList).project(&project)).await.unwrap().docs.len(), 3);
    // With no model loaded, search is by keywords, also when semantic search is asked for.
    for keywords_only in [true, false] {
        let mut search = Request::new(Method::KbSearch).project(&project).text("when do we deploy?");
        search.flag = keywords_only;
        let hits = client.call(&search).await.unwrap().hits;
        assert_eq!((hits.len(), hits[0].title.as_str(), hits[0].bm25_rank, hits[0].semantic_rank), (1, "Deploy", 1, 0));
    }
    let got = client.call(&Request::new(Method::KbGet).project(&project).id(added[1].id)).await.unwrap();
    assert_eq!((got.text.as_str(), got.data.len()), ("dump.bin", binary.len()));
    assert!(got.data == binary, "the stored file differs from what was sent");
    let text = client.call(&Request::new(Method::KbGet).project(&project).id(added[0].id)).await.unwrap();
    assert!(text.docs[0].text.contains("Restart the router"));
    client.call(&Request::new(Method::KbDelete).project(&project).id(note[0].id)).await.unwrap();
    assert!(client.call(&Request::new(Method::KbGet).project(&project).id(note[0].id)).await.is_err());
    // Something that is not an image cannot be sent as one.
    let mut bad = chat(&server, 0, "look", "");
    bad.files = vec![Upload { name: "fake.png".into(), data: b"not an image".to_vec() }];
    assert!(read_all(&mut client.chat(&bad).await.unwrap()).await.iter().any(|w| matches!(w, What::Error(e) if e.contains("not an image"))));
}

const TOOLMAKER: &str = r#"[
  {"calls": [{"name": "make_tool", "args": {"tool": "shout", "description": "Uppercase a text", "parameters": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
     "source": "fn run(args: &Json) -> Result<String, String> { Ok(args.str(\"text\")?.to_uppercase()) }"}}]},
  {"calls": [{"name": "run_tool", "args": {"tool": "shout", "input": {"text": "made at runtime"}}}]},
  {"text": "Done."},
  {"text": "A summary: the user asked for a tool named shout; it was made and works."},
  {"calls": [{"name": "shout", "args": {"text": "by its own name"}}]},
  {"text": "Again."}
]"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_is_made_kept_for_every_project_and_the_log_outlives_a_summary() {
    let server = Server::start("tools", TOOLMAKER, &[]);
    let client = server.client(true).await;
    let made = read_all(&mut client.chat(&chat(&server, 0, "make a tool", "auto")).await.unwrap()).await;
    let session = session_of(&made);
    assert!(made.iter().any(|w| matches!(w, What::ToolEnd { ok: true, text, .. } if text == "MADE AT RUNTIME")), "{made:?}");
    let tools = client.call(&Request::new(Method::Tools)).await.unwrap().tools;
    assert_eq!((tools.len(), tools[0].name.as_str()), (1, "shout"));
    assert!(tools[0].source.contains("to_uppercase"));
    // The tool belongs to the server, not the project: its source is in the server's store.
    assert!(!server.home.join("proj/.cwbcode/tools").exists());
    assert!(std::fs::read_dir(server.home.join(".cwbcode/tools")).unwrap().count() >= 1);

    // Summarizing replaces the model's context, not the log.
    let before = client.call(&Request::new(Method::Transcript).project(&server.project()).session(session)).await.unwrap().events;
    let said = client.call(&Request::new(Method::Compact).project(&server.project()).session(session)).await.unwrap().text;
    assert!(said.contains("replaced by a summary"), "{said}");
    let after = client.call(&Request::new(Method::Transcript).project(&server.project()).session(session)).await.unwrap().events;
    assert_eq!(after, before);

    // In another project the tool is there, under its own name.
    std::fs::create_dir_all(server.home.join("other")).unwrap();
    let other = Request::new(Method::Chat).project(&server.home.join("other").to_string_lossy()).text("use it");
    let used = read_all(&mut client.chat(&Request { mode: "auto".into(), ..other }).await.unwrap()).await;
    assert!(used.iter().any(|w| matches!(w, What::ToolEnd { ok: true, text, .. } if text == "BY ITS OWN NAME")), "{used:?}");
    client.call(&Request::new(Method::ToolDelete).name("shout")).await.unwrap();
    assert!(client.call(&Request::new(Method::Tools)).await.unwrap().tools.is_empty());
    assert!(client.call(&Request::new(Method::ToolDelete).name("shout")).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_refused_without_harm() {
    let server = Server::start("bad", HELLO, &[]);
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
    let post = |path: &'static str, body: Vec<u8>| http.post(format!("{}{path}", server.url)).bearer_auth(&server.token).body(body).send();
    assert_eq!(post("/api/call", b"garbage".to_vec()).await.unwrap().status(), 400);
    assert_eq!(post("/api/chat", Request::new(Method::Status).encode()).await.unwrap().status(), 400);
    assert_eq!(post("/api/call", vec![0; 60 << 20]).await.unwrap().status(), 413);
    // A chat sent to the call route is an error in the answer, not a hang.
    let answer = Response::decode(&post("/api/call", Request::new(Method::Chat).encode()).await.unwrap().bytes().await.unwrap()).unwrap();
    assert!(answer.error.contains("stream"));
    // The server still works.
    let client = server.client(true).await;
    assert!(client.call(&Request::new(Method::Status)).await.is_ok());
    let _ = Path::new(".");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_goes_on_after_the_backend_restarts() {
    let twice = format!("[{0},{0}]", HELLO.trim_start_matches('[').trim_end_matches(']'));
    let mut server = Server::start("restart", &twice, &[]);
    for h3 in [true, false] {
        let client = server.client(h3).await;
        let before = read_all(&mut client.chat(&chat(&server, 0, "hi", "")).await.unwrap()).await;
        let session = session_of(&before);
        server.restart();
        // The same client, without connecting again by hand; what was saved is still there.
        let transcript = client.call(&Request::new(Method::Transcript).project(&server.project()).session(session)).await.unwrap();
        assert!(transcript.events.iter().any(|e| e.what == What::User("hi".into())), "h3={h3}");
        assert_eq!(client.protocol(), if h3 { "h3" } else { "h2" });
        assert!(client.call(&Request::new(Method::Status)).await.is_ok());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_the_token_is_turned_away_before_its_body_is_read() {
    let server = Server::start("preauth", HELLO, &["--require-token"]);
    // 30 MB that would be buffered if the body were read first; over HTTP/3 and HTTP/2.
    let mut big = Request::new(Method::KbAdd).project(&server.project());
    big.files = vec![Upload { name: "big.bin".into(), data: vec![7; 30 << 20] }];
    for h3 in [true, false] {
        let stranger = Client::connect(&server.url, "not-the-token", server.trust.clone(), h3).await.unwrap();
        let started = Instant::now();
        for _ in 0..4 {
            let refused = stranger.call(&big).await.unwrap_err().to_string();
            assert!(refused.contains("refused the token") || refused.contains("broke") || refused.contains("reach"), "h3={h3}: {refused}");
        }
        assert!(started.elapsed() < Duration::from_secs(20), "h3={h3}: refusing took {:?}", started.elapsed());
    }
    // Nothing of it was stored, and the owner is served as before, also with a large upload.
    let owner = server.client(true).await;
    assert!(owner.call(&Request::new(Method::KbList).project(&server.project())).await.unwrap().docs.is_empty());
    assert!(!server.home.join("proj/.cwbcode").exists());
    let saved = owner.call(&big).await.unwrap().docs;
    assert_eq!(saved[0].title, "big.bin");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_apps_plain_http_is_on_this_machine_only_and_asks_for_the_token() {
    // The desktop app's window reaches the backend over plain HTTP on 127.0.0.1.
    let port = free_port().to_string();
    let server = Server::start("http", HELLO, &["--http-port", &port]);
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let page = http.get(&base).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("<html"), "not the web client");
    // The API asks for the token there too, and takes it.
    let call = |token: Option<&str>, request: Request| {
        let mut post = http.post(format!("{base}/api/call")).body(request.encode());
        if let Some(token) = token {
            post = post.bearer_auth(token);
        }
        post.send()
    };
    assert_eq!(call(None, Request::new(Method::Status)).await.unwrap().status(), 401);
    assert_eq!(call(Some("a-guess"), Request::new(Method::Status)).await.unwrap().status(), 401);
    let answer = call(Some(&server.token), Request::new(Method::Status)).await.unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(Response::decode(&answer.bytes().await.unwrap()).unwrap().status.unwrap().auth, "open");
    // Another site's page in the same browser is turned away, token or not.
    let foreign = http.post(format!("{base}/api/call")).bearer_auth(&server.token).header("origin", "http://evil.example").body(Request::new(Method::Status).encode()).send().await.unwrap();
    assert_eq!(foreign.status(), 401);
    // Not on the machine's other addresses.
    if let Some(ip) = lan_address() {
        assert!(std::net::TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, port.parse().unwrap()), Duration::from_secs(1)).is_err(), "plain HTTP answered on {ip}");
    }
    // Requests addressed to another name are refused, page and API alike: a site whose name
    // a rebinding DNS server points at 127.0.0.1 is its own origin, and would pass the
    // same-origin check. localhost at the port is taken.
    for host in ["evil.example", &format!("evil.example:{port}"), "127.0.0.1", "127.0.0.1:1"] {
        assert_eq!(http.get(&base).header("host", host).send().await.unwrap().status(), 421, "host {host}");
        let rebound = http.post(format!("{base}/api/call")).bearer_auth(&server.token).header("host", host).body(Request::new(Method::Status).encode()).send().await.unwrap();
        assert_eq!(rebound.status(), 421, "host {host}");
    }
    assert_eq!(http.get(&base).header("host", format!("localhost:{port}")).send().await.unwrap().status(), 200);
}

/// HMAC-SHA256 (RFC 2104), by hand, in hex.
fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backend_shows_the_desktop_app_it_holds_the_token_and_nothing_more() {
    // Before it gives a page on its port the token, the app asks whoever answers there for
    // an HMAC of a nonce, keyed with the token and bound to that port.
    let port = free_port().to_string();
    let server = Server::start("proof", HELLO, &["--http-port", &port]);
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let nonce = "00112233445566778899aabbccddeeff0123456789abcdef";
    let answer = http.get(format!("{base}/app-proof?nonce={nonce}")).send().await.unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(answer.headers()["cache-control"], "no-store");
    let proof = answer.text().await.unwrap();
    assert_eq!(proof, hmac_sha256(server.token.as_bytes(), format!("cwbcode app proof\n{port}\n{nonce}").as_bytes()));
    // Only that: not the token, not a proof for another port or with another key.
    assert!(!proof.contains(&server.token));
    assert_ne!(proof, hmac_sha256(server.token.as_bytes(), format!("cwbcode app proof\n{}\n{nonce}", server.port).as_bytes()));
    assert_ne!(proof, hmac_sha256(b"", format!("cwbcode app proof\n{port}\n{nonce}").as_bytes()));
    // A different nonce, a different answer; a nonce that is not 32 to 128 hex digits, none.
    let other = http.get(format!("{base}/app-proof?nonce={}", "ab".repeat(16))).send().await.unwrap().text().await.unwrap();
    assert_eq!(other, hmac_sha256(server.token.as_bytes(), format!("cwbcode app proof\n{port}\n{}", "ab".repeat(16)).as_bytes()));
    for bad in ["", "?nonce=", "?nonce=abc", "?nonce=zz112233445566778899aabbccddeeff", &format!("?nonce={}", "a".repeat(130))] {
        assert_eq!(http.get(format!("{base}/app-proof{bad}")).send().await.unwrap().status(), 400, "{bad}");
    }
    // Addressed to another name, no proof either.
    assert_eq!(http.get(format!("{base}/app-proof?nonce={nonce}")).header("host", "evil.example").send().await.unwrap().status(), 421);
    // And not on the HTTPS port at all: whatever held the app's port could pass the question
    // on to it.
    let https = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
    let elsewhere = https.get(format!("{}/app-proof?nonce={nonce}", server.url)).send().await.unwrap().text().await.unwrap();
    assert!(!elsewhere.contains(&proof) && elsewhere.len() != 64, "the HTTPS port gave a proof");
}

#[test]
fn with_exit_with_stdin_the_backend_goes_when_its_starter_does() {
    // The desktop app holds the other end of the backend's standard input: when the app ends
    // (quit, crash, kill -9), the pipe closes and the backend stops.
    let home = std::env::temp_dir().join(format!("cwbcode-it-stdin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("script.json"), HELLO).unwrap();
    let port = free_port();
    let mut child = Command::new(backend_binary())
        .args(["serve", "--env-file", "/dev/null", "--no-h3", "--exit-with-stdin", "--bind", &format!("127.0.0.1:{port}"), "--script"])
        .arg(home.join("script.json"))
        .env_remove("CWBCODE_WALLET")
        .env("HOME", &home)
        .env("CWBCODE_HOME", home.join(".cwbcode"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(child.try_wait().unwrap().is_none(), "the backend exited at once");
        assert!(start.elapsed() < Duration::from_secs(20), "the backend did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Still there while the pipe is open.
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.try_wait().unwrap().is_none());
    drop(child.stdin.take());
    let closed = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if closed.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("the backend did not stop when its standard input ended");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_port_held_by_something_else_is_a_warning_not_the_end() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    let server = Server::start("httpheld", HELLO, &["--http-port", &port]);
    assert_eq!(server.client(false).await.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().auth, "open");
    let log = std::fs::read_to_string(server.home.join("server.log")).unwrap();
    assert!(log.contains("warning: cannot listen on 127.0.0.1:") && log.contains("(--http-port)"), "{log}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_page_forbids_framing_and_foreign_scripts_and_the_stores_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start("headers", HELLO, &[]);
    // A device that trusts this machine's certificate authority, and nothing else, reaches the
    // page with no warning, and can download the authority from it.
    let ca = std::fs::read(server.home.join(".cwbcode/ca.pem")).unwrap();
    let trusting = reqwest::Client::builder().tls_built_in_root_certs(false).add_root_certificate(reqwest::Certificate::from_pem(&ca).unwrap()).build().unwrap();
    let base = format!("https://localhost:{}", server.port);
    assert_eq!(trusting.get(&base).send().await.unwrap().status(), 200);
    let download = trusting.get(format!("{base}/ca.crt")).send().await.unwrap();
    assert_eq!(download.headers()["content-type"], "application/x-x509-ca-cert");
    assert_eq!(download.bytes().await.unwrap().as_ref(), ca.as_slice());
    assert_eq!(std::fs::metadata(server.home.join(".cwbcode/ca-key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
    // One that trusts nothing is refused.
    assert!(reqwest::Client::builder().tls_built_in_root_certs(false).build().unwrap().get(&base).send().await.is_err());
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
    let page = http.get(&server.url).send().await.unwrap();
    let header = |name: &str| page.headers().get(name).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    assert_eq!(header("x-frame-options"), "DENY");
    assert_eq!(header("x-content-type-options"), "nosniff");
    let csp = header("content-security-policy");
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
    // The server's folder and a project's records are for their owner only.
    let client = server.client(true).await;
    read_all(&mut client.chat(&chat(&server, 0, "hi", "")).await.unwrap()).await;
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(server.home.join(".cwbcode")), 0o700);
    assert_eq!(mode(server.home.join("proj/.cwbcode")), 0o700);
    assert_eq!(mode(server.home.join(".cwbcode/token")), 0o600);
    assert_eq!(mode(server.home.join(".cwbcode/key.pem")), 0o600);
}

const LINKS: &str = r#"[
  {"calls": [{"name": "bash", "args": {"command": "ln -s ../planted-by-link.txt innocent.txt; ln -s \"$HOME/.cwbcode/token\" stolen; echo linked"}},
             {"name": "write_file", "args": {"path": "innocent.txt", "content": "escaped"}},
             {"name": "read_file", "args": {"path": "stolen"}},
             {"name": "grep", "args": {"pattern": ".", "path": "~", "glob": ".cwbcode/*"}}]},
  {"text": "Tried."}
]"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_steered_model_cannot_write_through_a_link_or_read_the_token() {
    let server = Server::start("links", LINKS, &[]);
    let client = server.client(true).await;
    let mut stream = client.chat(&chat(&server, 0, "go", "auto")).await.unwrap();
    // Even in auto mode, a write that would land outside the project stops to ask, and says where.
    let mut events = read_until(&mut stream, |w| matches!(w, What::Approval { .. })).await;
    let Some(What::Approval { id, what, offer_always }) = events.last().cloned() else { panic!("the write through the link did not ask: {events:?}") };
    assert!(what.contains("OUTSIDE the workspace") && !offer_always, "{what}");
    assert!(!server.home.join("planted-by-link.txt").exists());
    client.call(&Request::new(Method::Approve).id(id).text("n")).await.unwrap();
    events.extend(read_all(&mut stream).await);
    let results: Vec<(bool, String)> = events.iter().filter_map(|w| if let What::ToolEnd { ok, text, .. } = w { Some((*ok, text.clone())) } else { None }).collect();
    assert_eq!(results.len(), 4, "{events:?}");
    assert!(results[0].0 && results[0].1.contains("linked"), "{:?}", results[0]);
    // The write through the link to a file outside the project is refused, and nothing is made.
    assert!(!results[1].0, "{:?}", results[1]);
    assert!(!server.home.join("planted-by-link.txt").exists());
    // The token is not read through a link, nor found by searching the home folder.
    assert!(!results[2].0 && results[2].1.contains("off limits"), "{:?}", results[2]);
    assert!(!results[3].1.contains(&server.token), "the search printed the token");
    assert!(!events.iter().any(|w| matches!(w, What::ToolEnd { text, .. } | What::Text(text) if text.contains(&server.token))));
}

const TEST_PHRASE: &str = "test test test test test test test test test test test junk";
const TEST_WALLET: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

#[tokio::test(flavor = "multi_thread")]
async fn local_mode_takes_the_token_file_and_refuses_other_sites_pages() {
    // No wallet, this machine only.
    let server = Server::start("open", HELLO, &[]);
    for h3 in [true, false] {
        let client = Client::connect(&server.url, "", server.trust.clone(), h3).await.unwrap();
        let hello = client.call(&Request::new(Method::Hello)).await.unwrap().status.unwrap();
        assert_eq!((hello.auth.as_str(), hello.wallet.as_str()), ("open", ""));
        assert_eq!(hello.fingerprint.len(), 64);
        // Without the token nothing but the greeting: any program on this machine could ask,
        // and nobody may turn remote mode on with a wallet of their own.
        assert!(client.call(&Request::new(Method::Status)).await.is_err(), "h3={h3}: answered without the token");
        assert!(client.call(&Request::new(Method::Setup).text(TEST_WALLET)).await.is_err(), "h3={h3}: remote mode was set up without the token");
        // With the token file's token, everything works.
        client.set_token(&server.token);
        assert_eq!(client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().auth, "open");
        let events = read_all(&mut client.chat(&chat(&server, 0, "hi", "")).await.unwrap()).await;
        assert!(events.iter().any(|w| matches!(w, What::Text(_))), "h3={h3}: {events:?}");
        assert!(client.call(&Request::new(Method::Challenge)).await.is_err(), "there is nothing to sign in to");
    }
    // A page of another website, open in the same browser, is turned away: with its Origin,
    // without one but marked cross-site, and for the sign-in calls too.
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
    // (With the token: the page is turned away for where it comes from, not for lacking it.)
    let post = |header: (&'static str, &'static str), request: Request| http.post(format!("{}/api/call", server.url)).bearer_auth(&server.token).header(header.0, header.1).body(request.encode()).send();
    for foreign in [("origin", "https://evil.example"), ("origin", "null"), ("sec-fetch-site", "cross-site")] {
        assert_eq!(post(foreign, Request::new(Method::Projects)).await.unwrap().status(), 401, "{foreign:?}");
        assert_eq!(post(foreign, Request::new(Method::Hello)).await.unwrap().status(), 401, "{foreign:?}");
        let chat = http.post(format!("{}/api/chat", server.url)).bearer_auth(&server.token).header(foreign.0, foreign.1).body(chat(&server, 0, "hi", "auto").encode()).send().await.unwrap();
        assert_eq!(chat.status(), 401, "{foreign:?}");
    }
    // The backend's own page is not.
    let own = format!("https://127.0.0.1:{}", server.port);
    let from_own_page = http.post(format!("{}/api/call", server.url)).bearer_auth(&server.token).header("origin", own).body(Request::new(Method::Projects).encode()).send().await.unwrap();
    assert_eq!(from_own_page.status(), 200);
}

/// The one-time code in the `open` link `cwbcode url` prints for this server's home (or None,
/// when it prints no such link). `bind`: as `--bind`, or the server's port in its own mode.
fn link_code(server: &Server, bind: Option<&str>) -> Option<String> {
    let mut command = Command::new(backend_binary());
    command.args(["url", "--env-file", "/dev/null"]);
    match bind {
        Some(bind) => command.args(["--bind", bind]),
        None => command.args(["--port", &server.port.to_string()]),
    };
    let out = command.env_remove("CWBCODE_WALLET").env("HOME", &server.home).env("CWBCODE_HOME", server.home.join(".cwbcode")).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "cwbcode url failed: {}", String::from_utf8_lossy(&out.stderr));
    // The token itself is never in the link (the browser's history would keep it).
    assert!(!printed.contains(server.token.trim()), "the token was printed: {printed}");
    let link = printed.lines().find_map(|line| line.strip_prefix("open"))?.split_whitespace().next()?.to_string();
    Some(link.split_once("/#code=").unwrap_or_else(|| panic!("not a code link: {link}")).1.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_links_code_gives_the_token_once_from_this_machine_and_expires() {
    let server = Server::start("code", HELLO, &["movable"]);
    async fn redeem(client: &Client, code: &str) -> anyhow::Result<String> {
        Ok(client.call(&Request::new(Method::Redeem).text(code)).await?.text)
    }
    for h3 in [true, false] {
        let client = Client::connect(&server.url, "", server.trust.clone(), h3).await.unwrap();
        let code = link_code(&server, None).expect("local mode prints the open link");
        // The code gets the backend's token, once.
        assert_eq!(redeem(&client, &code).await.unwrap(), server.token.trim(), "h3={h3}");
        let again = redeem(&client, &code).await.unwrap_err().to_string();
        assert!(again.contains("used already"), "h3={h3}: {again}");
        assert!(redeem(&client, "0123456789abcdef0123456789abcdef").await.is_err());
        // A code past its time does not work. (Its file holds when it ends.)
        let code = link_code(&server, None).unwrap();
        let codes = server.home.join(".cwbcode/codes");
        for entry in std::fs::read_dir(&codes).unwrap().flatten() {
            std::fs::write(entry.path(), "1").unwrap();
        }
        assert!(redeem(&client, &code).await.is_err(), "h3={h3}: a code that has ended worked");
    }
    // Another website's page cannot use a code either.
    let code = link_code(&server, None).unwrap();
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
    let foreign = http.post(format!("{}/api/call", server.url)).header("origin", "https://evil.example").body(Request::new(Method::Redeem).text(&code).encode()).send().await.unwrap();
    assert_eq!(foreign.status(), 401);

    // From another machine a code is refused, even where the backend listens for one (remote
    // mode: every interface), and is not used up by the attempt.
    let wallet_env = format!("env:CWBCODE_WALLET={TEST_WALLET}");
    let remote = Server::start("code-remote", HELLO, &["movable", &wallet_env]);
    // (A code for this home, as `cwbcode url` for local mode issues it.)
    let code = link_code(&remote, Some(&format!("127.0.0.1:{}", remote.port))).unwrap();
    if let Some(ip) = lan_address() {
        for h3 in [true, false] {
            let outside = Client::connect(&format!("https://{ip}:{}", remote.port), "", remote.trust.clone(), h3).await.unwrap();
            let refused = redeem(&outside, &code).await.unwrap_err().to_string();
            assert!(refused.contains("only on the machine"), "h3={h3}: {refused}");
        }
    }
    let here = Client::connect(&remote.url, "", remote.trust.clone(), true).await.unwrap();
    assert_eq!(redeem(&here, &code).await.unwrap(), remote.token.trim());
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_wallet_one_signs_in_with_its_recovery_phrase() {
    use cwbcode_proto::wallet;
    let wallet_env = format!("env:CWBCODE_WALLET={TEST_WALLET}");
    let server = Server::start("wallet", HELLO, &[&wallet_env]);
    for h3 in [true, false] {
        let client = Client::connect(&server.url, "", server.trust.clone(), h3).await.unwrap();
        let hello = client.call(&Request::new(Method::Hello)).await.unwrap().status.unwrap();
        assert_eq!(hello.auth, "wallet");
        assert!(wallet::same_address(&hello.wallet, TEST_WALLET));
        // Without signing in: nothing but the sign-in calls.
        let refused = client.call(&Request::new(Method::Status)).await.unwrap_err().to_string();
        assert!(refused.contains("not signed in"), "{refused}");
        assert!(client.chat(&chat(&server, 0, "hi", "")).await.is_err());

        // Another wallet's signature (the same phrase's second account) is refused, and says why.
        let challenge = client.call(&Request::new(Method::Challenge)).await.unwrap().text;
        let other = wallet::key_from_phrase(TEST_PHRASE, 1).unwrap();
        let wrong = client.call(&Request::new(Method::Login).text(&challenge).name(&wallet::sign(&other, &challenge).unwrap())).await.unwrap_err().to_string();
        assert!(wrong.contains("belongs to"), "{wrong}");

        // The wallet's own signature of a fresh challenge signs in.
        let challenge = client.call(&Request::new(Method::Challenge)).await.unwrap().text;
        assert_eq!(wallet::challenge_field(&challenge, "server"), Some(hello.fingerprint.as_str()));
        let key = wallet::key_from_phrase(TEST_PHRASE, 0).unwrap();
        let signature = wallet::sign(&key, &challenge).unwrap();
        let session = client.call(&Request::new(Method::Login).text(&challenge).name(&signature)).await.unwrap().text;
        assert_eq!(session.len(), 64);
        // The same signature cannot be used again.
        assert!(client.call(&Request::new(Method::Login).text(&challenge).name(&signature)).await.is_err());

        client.set_token(&session);
        let status = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
        assert_eq!(status.auth, "wallet");
        if h3 {
            let events = read_all(&mut client.chat(&chat(&server, 0, "hi", "")).await.unwrap()).await;
            assert_eq!(text_of(&events), "Hello from the script, in several pieces.");
        }
        // Signing out ends the session.
        client.call(&Request::new(Method::Logout)).await.unwrap();
        assert!(client.call(&Request::new(Method::Status)).await.is_err());
        // The backend's own page can make the sign-in calls (a browser names the host in the
        // request's address over HTTP/2, not in a Host header); another site's page cannot.
        let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
        let hello = |origin: String| http.post(format!("{}/api/call", server.url)).header("origin", origin).body(Request::new(Method::Hello).encode()).send();
        assert_eq!(hello(format!("https://127.0.0.1:{}", server.port)).await.unwrap().status(), 200);
        assert_eq!(hello("https://evil.example".to_string()).await.unwrap().status(), 401);
        // On the backend's own machine its token file still lets the terminal client in.
        client.set_token(&server.token);
        assert!(client.call(&Request::new(Method::Status)).await.is_ok());
        client.set_token("a-guess");
        assert!(client.call(&Request::new(Method::Status)).await.is_err());
    }
}

/// What the backend tells anyone: how to sign in, and where it listens.
async fn hello(client: &Client) -> anyhow::Result<cwbcode_proto::Status> {
    Ok(client.call(&Request::new(Method::Hello)).await?.status.unwrap())
}

/// An address of this machine that other machines reach it by, if it has one.
fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_mode_is_set_up_from_local_mode_applied_at_once_and_remembered() {
    use cwbcode_proto::wallet;
    let mut server = Server::start("remote", HELLO, &["movable"]);
    let local = format!("127.0.0.1:{}", server.port);
    let everywhere = format!("0.0.0.0:{}", server.port);
    let settings = server.home.join(".cwbcode/settings.jsonl");

    // Local mode: this machine only, the token file's token, nothing set up.
    let client = Client::connect(&server.url, &server.token, server.trust.clone(), true).await.unwrap();
    let before = hello(&client).await.unwrap();
    assert_eq!((before.auth.as_str(), before.bind.as_str(), before.wallet_source.as_str()), ("open", local.as_str(), ""));
    if let Some(ip) = lan_address() {
        let from_outside = Client::connect(&format!("https://{ip}:{}", server.port), "", server.trust.clone(), false).await.unwrap();
        assert!(hello(&from_outside).await.is_err(), "local mode answered on {ip}");
    }
    // Something that is not an address sets nothing up.
    assert!(client.call(&Request::new(Method::Setup).text("0x1234")).await.is_err());
    assert!(!settings.exists());

    // Remote mode on: said in the answer, and in force at once.
    let on = client.call(&Request::new(Method::Setup).text(TEST_WALLET)).await.unwrap();
    assert!(on.text.contains("Remote mode is on"), "{}", on.text);
    let status = on.status.unwrap();
    assert_eq!((status.auth.as_str(), status.bind.as_str(), status.wallet_source.as_str()), ("wallet", everywhere.as_str(), "settings"));
    assert!(wallet::same_address(&status.wallet, TEST_WALLET));
    let line: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&settings).unwrap().lines().last().unwrap()).unwrap();
    assert_eq!((line["set"].as_str(), line["value"].as_str()), (Some("wallet"), Some(TEST_WALLET)));
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The backend now answers on the machine's other addresses, and asks who is there.
    let sign_in = |client: Client| async move {
        let challenge = client.call(&Request::new(Method::Challenge)).await.unwrap().text;
        let key = wallet::key_from_phrase(TEST_PHRASE, 0).unwrap();
        let session = client.call(&Request::new(Method::Login).text(&challenge).name(&wallet::sign(&key, &challenge).unwrap())).await.unwrap().text;
        client.set_token(&session);
        client
    };
    if let Some(ip) = lan_address() {
        for h3 in [true, false] {
            // The listeners move a moment after the answer; on a busy machine (CI) that can take
            // longer than the pause above, so ask again until the new address answers.
            let mut tries = 0;
            let remote = loop {
                match Client::connect(&format!("https://{ip}:{}", server.port), "", server.trust.clone(), h3).await {
                    Ok(client) if hello(&client).await.is_ok() => break client,
                    _ if tries < 60 => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    other => panic!("h3={h3}: {ip} did not answer after remote mode was turned on: {:?}", other.err()),
                }
            };
            assert_eq!(hello(&remote).await.unwrap().auth, "wallet", "h3={h3}");
            assert!(remote.call(&Request::new(Method::Status)).await.is_err(), "h3={h3}: no login was asked for");
            // The backend's own token is not a way in from another address.
            remote.set_token(&server.token);
            assert!(remote.call(&Request::new(Method::Status)).await.is_err(), "h3={h3}: the token file worked from {ip}");
            remote.set_token("");
            let remote = sign_in(remote).await;
            assert_eq!(remote.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().bind, everywhere);
        }
    }
    // On this machine the same client goes on (it reconnects), with the token file or a session.
    client.set_token(&server.token);
    assert_eq!(client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().auth, "wallet");

    // A restart reads the setting: still remote mode.
    server.restart();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let again = Client::connect(&server.url, &server.token, server.trust.clone(), true).await.unwrap();
    let after = hello(&again).await.unwrap();
    assert_eq!((after.auth.as_str(), after.bind.as_str(), after.wallet_source.as_str()), ("wallet", everywhere.as_str(), "settings"));

    // Off: by someone signed in. Local mode again, at once.
    let stranger = Client::connect(&server.url, "", server.trust.clone(), true).await.unwrap();
    assert!(stranger.call(&Request::new(Method::Setup).text("")).await.is_err(), "remote mode was turned off without signing in");
    let owner = sign_in(Client::connect(&server.url, "", server.trust.clone(), true).await.unwrap()).await;
    let off = owner.call(&Request::new(Method::Setup).text("")).await.unwrap();
    assert!(off.text.contains("Remote mode is off"), "{}", off.text);
    // (It moves back to this machine's address: waited for, not a fixed pause, which a busy
    // machine can outlast.)
    let start = Instant::now();
    let (fresh, finally) = loop {
        if let Ok(fresh) = Client::connect(&server.url, "", server.trust.clone(), true).await {
            if let Ok(said) = hello(&fresh).await {
                if said.auth == "open" {
                    break (fresh, said);
                }
            }
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the backend did not come back in local mode");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!((finally.auth.as_str(), finally.bind.as_str(), finally.wallet_source.as_str()), ("open", local.as_str(), ""));
    assert!(fresh.call(&Request::new(Method::Status)).await.is_err(), "local mode answered without the token");
    fresh.set_token(&server.token);
    assert!(fresh.call(&Request::new(Method::Status)).await.is_ok());
    if let Some(ip) = lan_address() {
        let from_outside = Client::connect(&format!("https://{ip}:{}", server.port), "", server.trust.clone(), false).await.unwrap();
        assert!(hello(&from_outside).await.is_err(), "still answering on {ip} after remote mode was turned off");
    }
    assert_eq!(std::fs::read_to_string(&settings).unwrap().lines().count(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wallet_fixed_in_the_environment_is_not_changed_by_setting_up() {
    let wallet_env = format!("env:CWBCODE_WALLET={TEST_WALLET}");
    let server = Server::start("fixed", HELLO, &[&wallet_env]);
    let client = Client::connect(&server.url, &server.token, server.trust.clone(), true).await.unwrap();
    let hello = client.call(&Request::new(Method::Hello)).await.unwrap().status.unwrap();
    assert_eq!((hello.auth.as_str(), hello.wallet_source.as_str()), ("wallet", "the environment"));
    for value in ["", "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"] {
        let refused = client.call(&Request::new(Method::Setup).text(value)).await.unwrap_err().to_string();
        assert!(refused.contains("the environment"), "{refused}");
    }
    assert!(!server.home.join(".cwbcode/settings.jsonl").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_client_is_served_whole_over_http3_as_over_http2() {
    let server = Server::start("pages", HELLO, &[]);
    let (quic, tcp) = (server.client(true).await, server.client(false).await);
    assert_eq!((quic.protocol(), tcp.protocol()), ("h3", "h2"));
    let (status, page) = tcp.get("/").await.unwrap();
    assert_eq!(status, 200);
    let page = String::from_utf8(page).unwrap();
    // The page and everything it names: scripts, the WebAssembly, the stylesheet.
    let mut paths = vec!["/".to_string(), "/some/route/of/the/app".to_string()];
    for part in page.split(['"', '\'']) {
        if part.starts_with('/') && (part.ends_with(".js") || part.ends_with(".wasm") || part.ends_with(".css")) {
            paths.push(part.to_string());
        }
    }
    paths.dedup();
    let mut total = 0;
    for path in &paths {
        let over_tcp = tcp.get(path).await.unwrap();
        // Several at once, as a browser asks.
        let (a, b, c) = tokio::join!(quic.get(path), quic.get(path), quic.get(path));
        for over_quic in [a.unwrap(), b.unwrap(), c.unwrap()] {
            assert_eq!(over_quic.0, 200, "{path} over HTTP/3");
            assert!(over_quic.1 == over_tcp.1, "{path}: {} bytes over HTTP/3, {} over HTTP/2", over_quic.1.len(), over_tcp.1.len());
        }
        total += over_tcp.1.len();
    }
    assert!(paths.len() >= 2 && total > 0, "{paths:?}");
    // It can be installed: a manifest that names icons the backend serves as pictures.
    let (status, manifest) = tcp.get("/manifest.webmanifest").await.unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!((status, manifest["display"].as_str(), manifest["start_url"].as_str()), (200, Some("standalone"), Some("/")));
    let mut icons: Vec<String> = manifest["icons"].as_array().unwrap().iter().map(|icon| icon["src"].as_str().unwrap().to_string()).collect();
    // (Also as the page names it, with a version to make a device ask again.)
    icons.extend(["/apple-touch-icon.png".to_string(), "/apple-touch-icon.png?v=3".to_string(), "/favicon-32.png".to_string()]);
    for icon in &icons {
        let (status, bytes) = quic.get(icon).await.unwrap();
        assert!(status == 200 && bytes.starts_with(b"\x89PNG"), "{icon} is not a picture");
    }
    assert!(page.contains(r#"rel="manifest""#) && page.contains(r#"rel="apple-touch-icon""#) && page.contains("apple-mobile-web-app-capable"));
    // The icon an iPad asks for by another name is there too; a file that does not exist is
    // not answered with the page.
    let (status, bytes) = tcp.get("/apple-touch-icon-precomposed.png").await.unwrap();
    assert!(status == 200 && bytes.starts_with(b"\x89PNG"));
    for missing in ["/no-such-icon.png", "/nothing.js", "/fonts/none.woff2"] {
        assert_eq!(tcp.get(missing).await.unwrap().0, 404, "{missing}");
    }
    // ... and a service worker, which is never kept by the browser itself (it asks each time).
    let worker = tcp.raw_get("/sw.js").await.unwrap();
    assert_eq!((worker.0, worker.1.as_str(), worker.2.as_str()), (200, "text/javascript", "no-cache"));
    assert!(String::from_utf8(worker.3).unwrap().contains("addEventListener('fetch'") && page.contains("serviceWorker.register('/sw.js')"));
    // Nothing answers QUIC on the HTTPS port itself: a browser that tries HTTP/3 there (having
    // been told of it by an earlier version) is turned away at once and stays on HTTP/2.
    assert!(std::net::UdpSocket::bind(("127.0.0.1", server.port)).is_ok(), "something listens on UDP {}", server.port);
    assert!(std::net::UdpSocket::bind(("127.0.0.1", cwbcode_proto::quic_port(server.port))).is_err(), "HTTP/3 is not on the port after");
}

// ---------- the model's download, from a Hugging Face and a registry of the test's own ----------

/// What the fake Hugging Face, its "CDN" and the fake Ollama registry were asked: each
/// request's host, path and Authorization header.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, String, Option<String>)>>>;

/// A safetensors file: a JSON header (as Hugging Face writes it, with spaces) and the data.
fn safetensors(tensors: &[(&str, &str, &[u64], &[u8])]) -> Vec<u8> {
    let mut entries = vec![r#""__metadata__": {"format": "pt"}"#.to_string()];
    let mut data = Vec::new();
    for (name, dtype, shape, bytes) in tensors {
        let shape: Vec<String> = shape.iter().map(|d| d.to_string()).collect();
        entries.push(format!(r#""{name}": {{"dtype": "{dtype}", "shape": [{}], "data_offsets": [{}, {}]}}"#, shape.join(", "), data.len(), data.len() + bytes.len()));
        data.extend_from_slice(bytes);
    }
    let head = format!("{{{}}}", entries.join(", "));
    let mut out = (head.len() as u64).to_le_bytes().to_vec();
    out.extend(head.as_bytes());
    out.extend(data);
    out
}

/// A tensor file as Ollama has it, written out by hand: compact JSON, metadata first, padded
/// with spaces to a multiple of eight, then the bytes.
fn ollama_blob(meta: Option<(&str, &str)>, tensors: &[(&str, &str, &[u64], &[u8])]) -> Vec<u8> {
    let mut entries = Vec::new();
    if let Some((group, quant)) = meta {
        entries.push(format!(r#""__metadata__":{{"group_size":"{group}","quant_type":"{quant}"}}"#));
    }
    let mut data = Vec::new();
    for (name, dtype, shape, bytes) in tensors {
        let shape: Vec<String> = shape.iter().map(|d| d.to_string()).collect();
        entries.push(format!(r#""{name}":{{"dtype":"{dtype}","shape":[{}],"data_offsets":[{},{}]}}"#, shape.join(","), data.len(), data.len() + bytes.len()));
        data.extend_from_slice(bytes);
    }
    let mut head = format!("{{{}}}", entries.join(",")).into_bytes();
    while head.len() % 8 != 0 {
        head.push(b' ');
    }
    let mut out = (head.len() as u64).to_le_bytes().to_vec();
    out.extend(head);
    out.extend(data);
    out
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{}", sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// A small HTTP/1.1 server on 127.0.0.1: `answer(path, headers)` gives the status, headers
/// and body; a body is sent at about `rate` bytes a second (0: at once).
fn http_server(seen: Seen, rate: usize, answer: impl Fn(&str, &std::collections::HashMap<String, String>) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync + 'static) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let answer = std::sync::Arc::new(answer);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (seen, answer) = (seen.clone(), answer.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut headers = std::collections::HashMap::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                seen.lock().unwrap().push((headers.get("host").cloned().unwrap_or_default(), path.clone(), headers.get("authorization").cloned()));
                let (status, extra, body) = answer(&path, &headers);
                let mut out = stream;
                let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in extra {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                if out.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in body.chunks(if rate == 0 { body.len().max(1) } else { 32 << 10 }) {
                    if out.write_all(chunk).is_err() {
                        return;
                    }
                    if rate > 0 {
                        std::thread::sleep(Duration::from_secs_f64(chunk.len() as f64 / rate as f64));
                    }
                }
            });
        }
    });
    port
}

/// Bytes [a, b] of `data` for a Range header, as a 206 (or the whole as a 200).
fn ranged(data: &[u8], range: Option<&String>) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let Some((a, b)) = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split_once('-')) else { return (200, vec![], data.to_vec()) };
    let a: usize = a.parse().unwrap();
    let b: usize = b.parse::<usize>().unwrap().min(data.len() - 1);
    (206, vec![("Content-Range".into(), format!("bytes {a}-{b}/{}", data.len()))], data[a..=b].to_vec())
}

/// The fake checkpoint, served as Hugging Face serves one, and Ollama's registry for it.
struct Fake {
    hub: u16,
    cdn: u16,
    seen: Seen,
    /// The tensor files as Ollama's registry has them.
    expected: [(&'static str, Vec<u8>); 4],
    config: &'static str,
}

/// A checkpoint of a large plain tensor (its bytes sent at `rate` bytes a second, so that the
/// download can be watched and stopped half way), an NVFP4 expert bank and an MXFP8 one with
/// their scales, and the draft network's file; a hub, a "CDN" and a registry for it.
fn fake_hub(rate: usize) -> Fake {
    // The checkpoint: a large plain tensor (sent slowly, so that the download can be watched
    // and stopped half way), an NVFP4 expert bank and an MXFP8 one with their scales, and the
    // draft network's file.
    let big: Vec<u8> = (0..(6u32 << 20)).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let (w13, w13s, w2, w2s, fc) = ([7u8; 128], [9u8; 16], [3u8; 16], [5u8; 2], [1u8, 2, 3, 4, 5, 6, 7, 8]);
    let shard = safetensors(&[
        ("model.llm.big.weight", "BF16", &[1536, 2048], &big),
        ("model.llm.layers.0.mlp.experts.w13_weight", "U8", &[2, 4, 16], &w13),
        ("model.llm.layers.0.mlp.experts.w13_weight_scale", "F8_E4M3", &[2, 4, 2], &w13s),
        ("model.llm.layers.0.mlp.experts.w2_weight", "F8_E4M3", &[2, 8], &w2),
        ("model.llm.layers.0.mlp.experts.w2_weight_scale", "U8", &[2, 1], &w2s),
    ]);
    let mtp = safetensors(&[("fc.weight", "BF16", &[4], &fc)]);
    // What Ollama's registry has for them.
    let expected = [
        ("model.llm.big.weight", ollama_blob(None, &[("model.llm.big.weight", "BF16", &[1536, 2048], &big)])),
        ("model.llm.layers.0.mlp.experts.w13_weight", ollama_blob(Some(("16", "nvfp4")), &[("model.llm.layers.0.mlp.experts.w13_weight", "U32", &[2, 4, 4], &w13), ("model.llm.layers.0.mlp.experts.w13_weight_scale", "U8", &[2, 4, 2], &w13s)])),
        ("model.llm.layers.0.mlp.experts.w2_weight", ollama_blob(Some(("32", "mxfp8")), &[("model.llm.layers.0.mlp.experts.w2_weight", "U32", &[2, 2], &w2), ("model.llm.layers.0.mlp.experts.w2_weight_scale", "U8", &[2, 1], &w2s)])),
        ("draft.fc.weight", ollama_blob(None, &[("draft.fc.weight", "BF16", &[4], &fc)])),
    ];
    let layers: Vec<String> = expected.iter().map(|(name, blob)| format!(r#"{{"mediaType":"application/vnd.ollama.image.tensor","digest":"{}","size":{},"name":"{name}"}}"#, sha256(blob), blob.len())).collect();
    let registry = format!(r#"{{"schemaVersion":2,"layers":[{}]}}"#, layers.join(","));
    let config = r#"{"architectures": ["FakeForTest"], "mtp_config": {"hidden_size": 4, "rms_norm_eps": 1e-06}}"#;
    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    files.insert("model.safetensors.index.json".into(), br#"{"metadata": {"total_size": 1}, "weight_map": {"model.llm.big.weight": "model-00001-of-00001.safetensors"}}"#.to_vec());
    files.insert("model-00001-of-00001.safetensors".into(), shard);
    files.insert("mtp.safetensors".into(), mtp);
    for name in ["generation_config.json", "hf_quant_config.json", "processor_config.json", "special_tokens_map.json", "tokenizer.json", "tokenizer_config.json"] {
        files.insert(name.into(), format!(r#"{{"file": "{name}"}}"#).into_bytes());
    }
    files.insert("config.json".into(), config.as_bytes().to_vec());
    let files = std::sync::Arc::new(files);

    // The "CDN" (on another origin: localhost, not 127.0.0.1) has the bytes, slowly.
    let seen: Seen = Default::default();
    let cdn_files = files.clone();
    let cdn = http_server(seen.clone(), rate, move |path, headers| {
        let file = path.trim_start_matches("/xet/").split('?').next().unwrap_or("");
        match cdn_files.get(file) {
            Some(data) => ranged(data, headers.get("range")),
            None => (404, vec![], b"no such file".to_vec()),
        }
    });
    // The hub sends a file's bytes to the CDN, its small JSON files through a redirect of its
    // own (as huggingface.co does), and has the registry's manifest too.
    let hub_files = files.clone();
    const REPO: &str = "/thinkingmachines/Inkling-2-Mini/resolve/c9d028302cd0f035fdf8e7e91f089a4ce94d2195/";
    let hub = http_server(seen.clone(), 0, move |path, headers| {
        // A token Hugging Face does not take.
        if path.starts_with(REPO) && headers.get("authorization").map(String::as_str) == Some("Bearer hf_refused") {
            return (401, vec![], b"invalid token".to_vec());
        }
        if path == "/v2/library/inkling-2-mini/manifests/26b-a5b-nvfp4" {
            return (200, vec![("Content-Type".into(), "application/json".into())], registry.clone().into_bytes());
        }
        if let Some(file) = path.strip_prefix("/api/resolve-cache/") {
            return match hub_files.get(file) {
                Some(data) => ranged(data, headers.get("range")),
                None => (404, vec![], vec![]),
            };
        }
        match path.strip_prefix(REPO) {
            Some(file) if file.ends_with(".json") => (307, vec![("Location".into(), format!("/api/resolve-cache/{file}"))], vec![]),
            Some(file) => (302, vec![("Location".into(), format!("http://localhost:{cdn}/xet/{file}?Signature=abc"))], vec![]),
            None => (404, vec![], vec![]),
        }
    });

    Fake { hub, cdn, seen, expected, config }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backend_downloads_the_model_itself_shows_how_far_it_is_and_picks_up_where_it_stopped() {
    let Fake { hub, cdn, seen, expected, config } = fake_hub(2 << 20);
    let base = std::env::temp_dir().join(format!("cwbcode-it-download-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let models = base.join("models");
    let env = |k: &str, v: &str| format!("env:{k}={v}");
    let args = [
        "no-script".to_string(),
        // (Asked for below: the download waits for the call, as when started so.)
        "--no-download-model".to_string(),
        env("CWBCODE_MODELS", &models.display().to_string()),
        env("OLLAMA_MODELS", &base.join("no-ollama").display().to_string()),
        env("HF_HOME", &base.join("no-hf").display().to_string()),
        env("HF_TOKEN", "hf_test_token"),
        env("CWBCODE_HF_ENDPOINT", &format!("http://127.0.0.1:{hub}")),
        env("CWBCODE_OLLAMA_REGISTRY", &format!("http://127.0.0.1:{hub}")),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut server = Server::start("download", "[]", &args);
    let client = server.client(false).await;
    let status = || async { client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap() };

    // Not in any store: "missing", and nothing is fetched until asked.
    let start = Instant::now();
    while status().await.model_state != "missing" {
        assert!(start.elapsed() < Duration::from_secs(20), "the backend did not find the model missing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen.lock().unwrap().is_empty());
    // Asking needs the token, as every call does.
    let stranger = Client::connect(&server.url, "not-the-token", server.trust.clone(), false).await.unwrap();
    assert!(stranger.call(&Request::new(Method::DownloadModel)).await.is_err());
    assert_eq!(status().await.model_state, "missing");

    // Asked: it downloads, and says how far it is; stopped half way.
    client.call(&Request::new(Method::DownloadModel)).await.unwrap();
    let state_file = models.join(".download-inkling-2-mini-26b-a5b-nvfp4.json");
    let start = Instant::now();
    let mut first = None;
    loop {
        let s = status().await;
        assert_eq!(s.model_state, "downloading", "{s:?}");
        assert_eq!(s.download_source, "Hugging Face, with your token (env HF_TOKEN): faster downloads");
        assert!(s.download_token_used && s.download_token_from == "env HF_TOKEN" && !s.download_token_refused && s.download_source_kind == "huggingface", "{s:?}");
        // As 0.1.2's clients read it: where it was found, never the token itself.
        assert_eq!(s.download_token, "env HF_TOKEN");
        assert!(!format!("{s:?}").contains("hf_test_token"), "the token itself reached a client");
        assert!(s.download_done <= s.download_total);
        if s.download_total > 0 && s.download_done > 0 {
            first.get_or_insert(s.download_done);
        }
        let finished = std::fs::read_to_string(&state_file).ok().map_or(0, |t| t.matches("sha256:").count());
        if finished == 3 && s.download_done > first.unwrap_or(u64::MAX) && s.download_speed > 0.0 && s.download_done < s.download_total / 4 * 3 {
            assert_eq!(s.download_total, (6 << 20) + 128 + 16 + 16 + 2 + 8);
            assert!(s.download_note.ends_with("of 4 files"), "{}", s.download_note);
            assert_eq!((s.download_files_done, s.download_files_total), (3, 4));
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "no progress to see: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    server.restart();
    let client = server.client(false).await;
    let status = || async { client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap() };

    // Started again (by the call, as the page's button does), it goes on from there.
    let start = Instant::now();
    while status().await.model_state != "missing" {
        assert!(start.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    client.call(&Request::new(Method::DownloadModel)).await.unwrap();
    assert!(client.call(&Request::new(Method::DownloadModel)).await.is_err(), "a second download at once");
    let start = Instant::now();
    let mut seen_progress = Vec::new();
    let last = loop {
        let s = status().await;
        if s.model_state == "downloading" || s.model_state == "verifying" {
            // (Until the checkpoint's headers are read the total is not known: 0.)
            if s.download_total > 0 {
                seen_progress.push(s.download_done);
            }
        } else {
            break s;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "the download did not end: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let log = std::fs::read_to_string(server.home.join("server.log")).unwrap();
    assert!(log.contains("resuming: 3 of 4 tensor files"), "{log}");
    // Bytes done never went back, and started with what was there (the three small files).
    assert!(seen_progress.windows(2).all(|w| w[0] <= w[1]) && seen_progress.first().is_some_and(|&d| d >= 128 + 16 + 16 + 2 + 8), "{seen_progress:?}");
    // Done, checked against the registry, and then loaded at once, without a restart (this
    // fake model is refused by the loader: that is how far a test without 20 GB can go).
    assert_eq!(last.download_error, "", "{last:?}");
    assert_eq!(last.download_note, "verified: all 4 tensor files match Ollama's registry (SHA-256)");
    assert_eq!(last.download_done, last.download_total);
    assert!(last.download_verified);
    let start = Instant::now();
    let mut state = last.model_state.clone();
    while state == "loading" {
        assert!(start.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(50)).await;
        state = status().await.model_state;
    }
    assert!(state.starts_with("failed:") && state.contains("FakeForTest"), "{state}");
    assert!(log.contains("download: verified: all 4 tensor files match"), "{log}");

    // The store: Ollama's layout, every tensor file Ollama's byte for byte, and no leftovers.
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(models.join("manifests/registry.ollama.ai/library/inkling-2-mini/26b-a5b-nvfp4")).unwrap()).unwrap();
    let blob = |name: &str| {
        let layer = manifest["layers"].as_array().unwrap().iter().find(|l| l["name"] == name).unwrap_or_else(|| panic!("no {name} in the manifest"));
        std::fs::read(models.join("blobs").join(layer["digest"].as_str().unwrap().replace(':', "-"))).unwrap()
    };
    for (name, bytes) in &expected {
        assert!(blob(name) == *bytes, "{name} is not Ollama's");
    }
    assert_eq!(blob("config.json"), config.as_bytes());
    assert_eq!(blob("draft/config.json"), br#"{"hidden_size": 4, "rms_norm_eps": 1e-06, "architectures": ["Inkling2ForConditionalGenerationMTP"]}"#);
    assert_eq!(manifest["layers"].as_array().unwrap().len(), 4 + 7 + 1);
    assert!(!state_file.exists());
    assert!(!std::fs::read_dir(models.join("blobs")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".partial-")));

    // The token went to the hub, and never to the CDN (nor to the registry).
    let seen = seen.lock().unwrap();
    let cdn_host = format!("localhost:{cdn}");
    assert!(seen.iter().any(|(host, _, _)| *host == cdn_host), "the CDN was not used");
    assert!(seen.iter().filter(|(host, path, _)| *host != cdn_host && path.contains("/resolve/")).all(|(_, _, auth)| auth.as_deref() == Some("Bearer hf_test_token")));
    for (host, path, auth) in seen.iter() {
        if *host == cdn_host || path.starts_with("/v2/") {
            assert_eq!(auth, &None, "the token was sent to {host}{path}");
        }
    }
    drop(seen);
    let _ = std::fs::remove_dir_all(&base);
}

/// The states a list of messages went through, each once, in order.
fn stages(messages: &[(Instant, cwbcode_proto::Progress)]) -> Vec<String> {
    let mut out: Vec<String> = messages.iter().map(|(_, p)| p.state.clone()).collect();
    out.dedup();
    out
}

/// Read an events stream to its end (the model ready), with the time each message came.
async fn follow(mut events: Events) -> Vec<(Instant, cwbcode_proto::Progress)> {
    let mut got = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), events.next_progress()).await.expect("no message for 30 seconds").unwrap() {
            Some(p) => got.push((Instant::now(), p)),
            None => return got,
        }
    }
}

/// The first message of a stream.
async fn follow_first(client: &Client) -> cwbcode_proto::Progress {
    let mut events = client.progress().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), events.next_progress()).await.unwrap().unwrap().unwrap()
}

/// Messages until the model failed to load (a stream for a failed model does not end).
async fn follow_until_stopped(client: &Client) -> Vec<(Instant, cwbcode_proto::Progress)> {
    let mut events = client.progress().await.unwrap();
    let mut got = Vec::new();
    loop {
        let p = tokio::time::timeout(Duration::from_secs(30), events.next_progress()).await.expect("no message for 30 seconds").unwrap().expect("the stream ended");
        let failed = p.state == "failed";
        got.push((Instant::now(), p));
        if failed {
            return got;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_client_is_told_how_far_the_download_is_from_one_stream_to_the_end() {
    let Fake { hub, cdn, seen, .. } = fake_hub(2 << 20);
    let base = std::env::temp_dir().join(format!("cwbcode-it-fanout-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let env = |k: &str, v: &str| format!("env:{k}={v}");
    let http_port = free_port();
    let args = [
        // The script stands in for the model once it is downloaded: the stream goes to "ready".
        "--script-after-download".to_string(),
        "--no-download-model".to_string(),
        "--http-port".to_string(),
        http_port.to_string(),
        // The token `hf auth login` saves, in the (scratch) home folder.
        "file:.cache/huggingface/token=hf_file_token\n".to_string(),
        env("CWBCODE_MODELS", &base.join("models").display().to_string()),
        env("OLLAMA_MODELS", &base.join("no-ollama").display().to_string()),
        env("CWBCODE_HF_ENDPOINT", &format!("http://127.0.0.1:{hub}")),
        env("CWBCODE_OLLAMA_REGISTRY", &format!("http://127.0.0.1:{hub}")),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let server = Server::start("fanout", HELLO, &args);
    let (one, two) = (server.client(true).await, server.client(false).await);
    assert_eq!((one.protocol(), two.protocol()), ("h3", "h2"));
    let start = Instant::now();
    while one.call(&Request::new(Method::Status)).await.unwrap().status.unwrap().model_state != "missing" {
        assert!(start.elapsed() < Duration::from_secs(20), "the backend did not find the model missing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The stream is asked for as any call is: not without the token, nor from another site.
    for h3 in [false, true] {
        let stranger = Client::connect(&server.url, "not-the-token", server.trust.clone(), h3).await.unwrap();
        let refused = stranger.progress().await.err().expect("a stream without the token").to_string();
        assert!(refused.contains("refused the token"), "h3={h3}: {refused}");
    }
    let http = reqwest::Client::new();
    let plain = format!("http://127.0.0.1:{http_port}{}", cwbcode_proto::EVENTS_PATH);
    assert_eq!(http.post(&plain).send().await.unwrap().status(), 401);
    assert_eq!(http.post(&plain).bearer_auth(&server.token).header("origin", "http://evil.example").send().await.unwrap().status(), 401);

    // Three listeners at once: over HTTP/3, HTTP/2, and the desktop app's plain HTTP.
    let (a, b) = (one.progress().await.unwrap(), two.progress().await.unwrap());
    let third = http.post(&plain).bearer_auth(&server.token).send().await.unwrap();
    assert_eq!(third.status(), 200);
    let third = tokio::spawn(async move {
        use futures::StreamExt;
        let mut body = third.bytes_stream();
        let mut frames = cwbcode_proto::Frames::new();
        let mut got = Vec::new();
        while let Some(chunk) = body.next().await {
            frames.push(&chunk.unwrap());
            while let Some(p) = frames.next_progress().unwrap() {
                got.push(p);
            }
        }
        got
    });
    let (a, b) = (tokio::spawn(follow(a)), tokio::spawn(follow(b)));
    // (Each has had its first message before the download starts.)
    tokio::time::sleep(Duration::from_millis(300)).await;
    one.call(&Request::new(Method::DownloadModel)).await.unwrap();
    let (a, b, c) = (a.await.unwrap(), b.await.unwrap(), third.await.unwrap());

    // Every one was told the same, message for message, from "missing" to "ready" (then the
    // stream ended), through every stage.
    let same = |got: &[cwbcode_proto::Progress]| got.iter().map(|p| (p.seq, p.state.clone(), p.done, p.files_done)).collect::<Vec<_>>();
    let (pa, pb): (Vec<_>, Vec<_>) = (a.iter().map(|(_, p)| p.clone()).collect(), b.iter().map(|(_, p)| p.clone()).collect());
    assert_eq!(same(&pa), same(&pb));
    assert_eq!(same(&pa), same(&c));
    assert_eq!(stages(&a), ["missing", "downloading", "verifying", "loading", "ready"]);
    assert!(pa.windows(2).all(|w| w[0].seq < w[1].seq && w[0].done <= w[1].done), "{pa:?}");
    // While it downloads, a message at least every half second (and some to spare).
    let downloading: Vec<&(Instant, cwbcode_proto::Progress)> = a.iter().filter(|(_, p)| p.state == "downloading" && p.total > 0).collect();
    assert!(downloading.len() >= 4, "{} messages while downloading", downloading.len());
    let gap = downloading.windows(2).map(|w| w[1].0 - w[0].0).max().unwrap();
    assert!(gap < Duration::from_millis(900), "{gap:?} without a message while downloading");
    assert!(downloading.iter().any(|(_, p)| p.speed > 0.0 && p.done < p.total));
    // With the token `hf auth login` saved, and where it is: said, never the token itself.
    let d = &downloading.last().unwrap().1;
    assert!(d.token && d.token_from == "~/.cache/huggingface/token" && !d.token_refused && d.source_kind == "huggingface" && d.files_total == 4, "{d:?}");
    assert!(!format!("{pa:?}").contains("hf_file_token"), "the token itself went to a client");
    let end = pa.last().unwrap();
    assert!(end.verified && end.done == end.total && end.state == "ready", "{end:?}");
    // The token went to the hub, never to the CDN.
    let seen = seen.lock().unwrap().clone();
    let cdn_host = format!("localhost:{cdn}");
    assert!(seen.iter().any(|(_, path, auth)| path.contains("/resolve/") && auth.as_deref() == Some("Bearer hf_file_token")));
    assert!(seen.iter().filter(|(host, ..)| *host == cdn_host).all(|(_, _, auth)| auth.is_none()));
    // Ready: the model answers, and a stream asked for now ends at once.
    let mut events = one.chat(&chat(&server, 0, "hi", "auto")).await.unwrap();
    assert!(text_of(&read_all(&mut events).await).contains("Hello from the script"));
    let after = follow(two.progress().await.unwrap()).await;
    assert_eq!(stages(&after), ["ready"]);
    let log = std::fs::read_to_string(server.home.join("server.log")).unwrap();
    assert!(log.contains("with your Hugging Face token, from ~/.cache/huggingface/token"), "{log}");
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_model_is_downloaded_by_default_a_refused_token_is_dropped_and_the_opt_out_waits() {
    let Fake { hub, seen, .. } = fake_hub(8 << 20);
    let base = std::env::temp_dir().join(format!("cwbcode-it-default-models-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let env = |k: &str, v: &str| format!("env:{k}={v}");
    let common = |models: &str| {
        vec![
            "no-script".to_string(),
            env("CWBCODE_MODELS", &base.join(models).display().to_string()),
            env("OLLAMA_MODELS", &base.join("no-ollama").display().to_string()),
            env("CWBCODE_HF_ENDPOINT", &format!("http://127.0.0.1:{hub}")),
            env("CWBCODE_OLLAMA_REGISTRY", &format!("http://127.0.0.1:{hub}")),
        ]
    };

    // --no-download-model (or CWBCODE_DOWNLOAD_MODEL=0): missing, and nothing is fetched.
    for opt_out in ["--no-download-model".to_string(), env("CWBCODE_DOWNLOAD_MODEL", "0")] {
        let mut args = common("opt-out");
        args.push(opt_out.clone());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let server = Server::start("optout", "[]", &args);
        let client = server.client(false).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let s = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
        assert_eq!((s.model_state.as_str(), s.download_source_kind.as_str()), ("missing", ""), "{opt_out}");
        assert!(seen.lock().unwrap().is_empty(), "{opt_out}: something was fetched");
        let first = follow_first(&client).await;
        assert_eq!((first.state.as_str(), first.error.as_str()), ("missing", ""));
    }

    // No flag at all: it downloads at once. With a token Hugging Face refuses, it goes on
    // without it, and says so; then the loader refuses this fake model, as far as a test
    // without 20 GB goes.
    let mut args = common("default");
    args.push(env("HF_TOKEN", "hf_refused"));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let server = Server::start("default-download", "[]", &args);
    let client = server.client(true).await;
    let got = follow_until_stopped(&client).await;
    let states = stages(&got);
    assert!(states.ends_with(&["verifying".to_string(), "loading".to_string(), "failed".to_string()]), "{states:?}");
    assert!(states.iter().any(|s| s == "downloading"), "{states:?}");
    assert!(got.last().unwrap().1.error.contains("FakeForTest"), "{:?}", got.last());
    let refused = got.iter().map(|(_, p)| p).find(|p| p.token_refused).expect("the refusal was not told");
    assert!(!refused.token && refused.source_kind == "huggingface");
    let s = client.call(&Request::new(Method::Status)).await.unwrap().status.unwrap();
    assert!(s.download_token_refused && !s.download_token_used && s.download_token == "refused" && s.download_source.contains("your token was refused"), "{s:?}");
    let log = std::fs::read_to_string(server.home.join("server.log")).unwrap();
    assert!(log.contains("your token was refused") && log.contains("downloading without it"), "{log}");
    // The refused token was tried on the hub, and once refused never sent again.
    let seen = seen.lock().unwrap().clone();
    let first_without = seen.iter().position(|(_, path, auth)| path.contains("/resolve/") && auth.is_none()).unwrap();
    assert!(seen[first_without..].iter().all(|(_, _, auth)| auth.is_none()));
    assert!(seen[..first_without].iter().any(|(_, _, auth)| auth.as_deref() == Some("Bearer hf_refused")));
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mac_apps_own_page_may_call_the_desktop_port_and_nothing_else_may() {
    // The Mac app's page is its own (tauri://localhost), and calls the backend's desktop port
    // across origins; the token is asked for as ever, and no other origin, nor the HTTPS port,
    // is opened to it.
    let port = free_port().to_string();
    let server = Server::start("app-origin", HELLO, &["--http-port", &port]);
    let http = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for origin in ["tauri://localhost", "http://tauri.localhost"] {
        // The preflight a web view sends before a POST with the Authorization header.
        let preflight = http.request(reqwest::Method::OPTIONS, format!("{base}/api/call")).header("origin", origin).header("access-control-request-method", "POST").header("access-control-request-headers", "authorization, content-type").send().await.unwrap();
        assert_eq!(preflight.status(), 204, "{origin}");
        let h = preflight.headers();
        assert_eq!(h["access-control-allow-origin"], origin);
        assert!(h["access-control-allow-headers"].to_str().unwrap().contains("authorization"));
        assert!(h["access-control-allow-methods"].to_str().unwrap().contains("POST"));
        // With the token: answered, and the answer may be read by that page.
        let call = |token: Option<&str>| {
            let mut post = http.post(format!("{base}/api/call")).header("origin", origin).header("sec-fetch-site", "cross-site").body(Request::new(Method::Status).encode());
            if let Some(token) = token {
                post = post.bearer_auth(token);
            }
            post.send()
        };
        let answer = call(Some(&server.token)).await.unwrap();
        assert_eq!(answer.status(), 200, "{origin}");
        assert_eq!(answer.headers()["access-control-allow-origin"], origin);
        assert!(Response::decode(&answer.bytes().await.unwrap()).unwrap().status.is_some());
        // Without it, or with a wrong one: refused, as for any page.
        assert_eq!(call(None).await.unwrap().status(), 401);
        assert_eq!(call(Some("a-guess")).await.unwrap().status(), 401);
        // The events stream too (the page follows the model's download there).
        let events = http.post(format!("{base}{}", cwbcode_proto::EVENTS_PATH)).header("origin", origin).bearer_auth(&server.token).send().await.unwrap();
        assert_eq!(events.status(), 200);
        assert_eq!(events.headers()["access-control-allow-origin"], origin);
        // The HTTPS port is not opened to it: that port is for the backend's own pages.
        let https = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
        let over_https = https.post(format!("{}/api/call", server.url)).header("origin", origin).bearer_auth(&server.token).body(Request::new(Method::Status).encode()).send().await.unwrap();
        assert_eq!(over_https.status(), 401, "{origin} over HTTPS");
        assert!(over_https.headers().get("access-control-allow-origin").is_none());
    }
    // Any other origin: no preflight answer that lets it in, and its calls are refused.
    for origin in ["http://evil.example", "tauri://evil", "http://localhost:3000", "null"] {
        let preflight = http.request(reqwest::Method::OPTIONS, format!("{base}/api/call")).header("origin", origin).header("access-control-request-method", "POST").send().await.unwrap();
        assert!(preflight.headers().get("access-control-allow-origin").is_none(), "{origin}");
        let call = http.post(format!("{base}/api/call")).header("origin", origin).bearer_auth(&server.token).body(Request::new(Method::Status).encode()).send().await.unwrap();
        assert_eq!(call.status(), 401, "{origin}");
        assert!(call.headers().get("access-control-allow-origin").is_none(), "{origin}");
    }
    // A name a rebinding DNS server points at 127.0.0.1 is still turned away, app origin or not.
    let rebound = http.post(format!("{base}/api/call")).header("host", "evil.example").header("origin", "tauri://localhost").bearer_auth(&server.token).body(Request::new(Method::Status).encode()).send().await.unwrap();
    assert_eq!(rebound.status(), 421);
}
