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
    // target/<profile>/deps/backend-xxxx -> target/<profile>/cwbcode
    let exe = std::env::current_exe().unwrap();
    let binary = exe.parent().unwrap().parent().unwrap().join("cwbcode");
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
        let port = base + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok() {
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
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
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
        Command::new(backend_binary())
            .args(["serve", "--bind", &format!("127.0.0.1:{port}"), "--script"])
            .arg(home.join("script.json"))
            .args(args)
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
    let server = Server::start("auth", HELLO, &[]);
    for h3 in [true, false] {
        let wrong = Client::connect(&server.url, "not-the-token", server.trust.clone(), h3).await.unwrap();
        let refused = wrong.call(&Request::new(Method::Status)).await.unwrap_err().to_string();
        assert!(refused.contains("refused the token"), "{refused}");
        assert!(wrong.chat(&chat(&server, 0, "hi", "")).await.is_err());
        let none = Client::connect(&server.url, "", server.trust.clone(), h3).await.unwrap();
        assert!(none.call(&Request::new(Method::Projects)).await.is_err());
    }
    // Another certificate than the server's is not trusted, over either protocol.
    let other = Server::start("auth-other", HELLO, &[]);
    let mismatched = Client::connect(&server.url, &server.token, other.trust.clone(), true).await.unwrap();
    assert_eq!(mismatched.protocol(), "h2", "QUIC with the wrong certificate must not connect");
    assert!(mismatched.call(&Request::new(Method::Status)).await.is_err());
    // The web page itself needs no token; the API behind it does.
    let page = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap().get(&server.url).send().await.unwrap();
    assert!(page.status().is_success());
    assert!(page.headers().get("alt-svc").unwrap().to_str().unwrap().starts_with("h3=\":"));
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_are_folders_under_home_with_their_own_store() {
    let server = Server::start("projects", HELLO, &[]);
    let client = server.client(true).await;
    for outside in ["/etc", "/", "/tmp", "~", "~/.cwbcode", "~/missing", "relative/path"] {
        let refused = client.call(&Request::new(Method::OpenProject).text(outside)).await;
        assert!(refused.is_err(), "{outside} was accepted as a project");
        assert!(client.call(&Request::new(Method::Sessions).project(outside)).await.is_err(), "{outside}");
        let mut stream = client.chat(&Request::new(Method::Chat).project(outside).text("hi")).await.unwrap();
        assert!(read_all(&mut stream).await.iter().any(|w| matches!(w, What::Error(_))), "{outside}");
    }
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
    assert_eq!((listed.len(), listed[0].sessions), (1, 1));
    client.call(&Request::new(Method::NameSession).project(&server.project()).session(session).name("greeting")).await.unwrap();
    let sessions = client.call(&Request::new(Method::Sessions).project(&server.project())).await.unwrap().sessions;
    assert_eq!((sessions[0].id, sessions[0].name.as_str(), sessions[0].first.as_str()), (session, "greeting", "hello there"));
    assert!(client.call(&Request::new(Method::Transcript).project(&server.project()).session(999)).await.is_err());
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
    let server = Server::start("preauth", HELLO, &[]);
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
async fn the_page_forbids_framing_and_foreign_scripts_and_the_stores_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let server = Server::start("headers", HELLO, &[]);
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
