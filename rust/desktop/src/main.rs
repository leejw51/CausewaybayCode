//! Causewaybay Code as a Mac app: a window on the web client, and the backend beside it.
//!
//! On start the window shows a page of its own (splash/) while the app finds the backend:
//! one already answering on the app's port (`make start` serves it there too) is used as it
//! is; otherwise the app starts the backend that ships inside it (`cwbcode`, beside this
//! program; MLX's `mlx.metallib` is in the app's Resources) and stops it again on quit. Once
//! the backend answers, the window goes to its page with the backend's token in the address
//! (the page keeps it and takes it out), as the `open` link `make url` prints does.
//!
//! The window reaches the backend over plain HTTP on 127.0.0.1 (`cwbcode serve --http-port`):
//! a web view will not take the backend's own certificate. That port answers this machine
//! only, for requests addressed to 127.0.0.1 or localhost, and asks for the token like the
//! HTTPS one. The page gets no commands of the app's.
//!
//! Whatever answers on the port is given the token only once it has shown that it holds it:
//! the app asks `GET /app-proof?nonce=N` and checks the answer, HMAC-SHA256 keyed with the
//! token (which the app reads from the backend's folder) of a label, the port and the nonce.
//! Another program on the port cannot answer, and is not given the token; the starting page
//! says so.
//!
//! The backend the app starts goes with it: on quit, on a terminate signal, and also when the
//! app is killed or crashes (the backend runs with --exit-with-stdin, and the app holds the
//! other end of its standard input). One app at a time per backend folder ($CWBCODE_HOME/
//! app.lock). If the backend stops while the window is open, the window goes back to the
//! starting page and says why; a backend the app did not start is waited for to come back.
//!
//!   CWBCODE_HOME       the backend's folder [~/.cwbcode]
//!   CWBCODE_APP_PORT   the app's port [7480]
//!   CWBCODE_APP_ARGS   more arguments for a backend the app starts (e.g. `--no-model`)

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hmac::Mac;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewWindow};

const DEFAULT_PORT: u16 = 7480;
/// How long a backend may take to answer: it starts listening before the model has loaded.
const START_SECONDS: u64 = 120;
/// How long the PATH a terminal would have may take to find out.
const LOGIN_PATH_SECONDS: u64 = 3;
/// The most of an answer read from whatever is on the port.
const MAX_ANSWER: usize = 8 << 10;

fn home() -> PathBuf {
    match std::env::var_os("CWBCODE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cwbcode"),
    }
}

fn port() -> u16 {
    std::env::var("CWBCODE_APP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// What the app says on its terminal (if it has one), for whoever runs it from there.
fn note(text: &str) {
    eprintln!("cwbcode-desktop: {text}");
}

fn token() -> String {
    std::fs::read_to_string(home().join("token")).unwrap_or_default().trim().to_string()
}

fn random_hex(bytes: usize) -> Option<String> {
    let mut buffer = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut buffer).ok()?;
    Some(buffer.iter().map(|b| format!("{b:02x}")).collect())
}

/// What the backend answers to `/app-proof` (see cwbcode's api::app_proof).
fn expected_proof(token: &str, port: u16, nonce: &str) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("cwbcode app proof\n{port}\n{nonce}").as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Who is on the app's port.
#[derive(Debug)]
enum Found {
    /// Nobody: nothing listens there.
    Nobody,
    /// This backend: it showed that it holds the token.
    Ours,
    /// Something that could not show it holds the token (why).
    Other(String),
}

/// Ask whoever is on the port to show that it holds the backend's token. Nothing of the
/// token is sent: a fresh nonce goes out, and its HMAC must come back.
fn found(port: u16) -> Found {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else { return Found::Nobody };
    let token = token();
    if token.is_empty() {
        return Found::Other(format!("there is no token in {} to check it with", home().join("token").display()));
    }
    let Some(nonce) = random_hex(24) else { return Found::Other("cannot read /dev/urandom".into()) };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if stream.write_all(format!("GET /app-proof?nonce={nonce} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).is_err() {
        return Found::Other("it did not take a request".into());
    }
    // The whole answer (Connection: close), up to a limit.
    let mut answer = Vec::new();
    let mut chunk = [0u8; 2048];
    while answer.len() < MAX_ANSWER {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => answer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let answer = String::from_utf8_lossy(&answer);
    let Some((head, body)) = answer.split_once("\r\n\r\n") else {
        return Found::Other(if answer.is_empty() { "it did not answer".into() } else { "it did not answer in HTTP".into() });
    };
    let status = head.lines().next().unwrap_or("");
    if !status.starts_with("HTTP/1.1 200") {
        return Found::Other(format!("it answered the proof with \"{}\" (an older Causewaybay Code backend, or another program)", status.trim()));
    }
    if same(body.trim().as_bytes(), expected_proof(&token, port, &nonce).as_bytes()) {
        Found::Ours
    } else {
        Found::Other("its proof does not match this backend's token (another program, or a backend with another folder)".into())
    }
}

/// Is this PATH the minimal one launchd gives (only the system's folders)?
fn minimal_path(path: &str) -> bool {
    path.split(':').filter(|p| !p.is_empty()).all(|p| matches!(p, "/usr/bin" | "/bin" | "/usr/sbin" | "/sbin"))
}

/// The PATH a terminal would have. Opened from the Finder the app gets a minimal one, and the
/// agent's commands would not find cargo, Homebrew's tools, and the like. It takes an
/// interactive login shell (~/.cargo/bin is often added in .zshrc, not .zprofile), which
/// prints it with printenv (a list with colons, also from fish); what its startup files print
/// is passed over by a marker. Asked only when the app was not started from a terminal (its
/// parent is launchd) or the PATH it has is the minimal one, and given a few seconds: the
/// shell runs in a session of its own and is killed with whatever it started if it takes
/// longer (startup files that wait for something, or leave a program holding its output).
fn login_path() -> Option<String> {
    const MARK: &str = "__cwbcode_path__=";
    let inherited = std::env::var("PATH").unwrap_or_default();
    if unsafe { libc::getppid() } != 1 && !minimal_path(&inherited) {
        return None;
    }
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/')).unwrap_or_else(|| "/bin/zsh".to_string());
    let mut command = Command::new(&shell);
    command.args(["-i", "-l", "-c", &format!("printf '\\n{MARK}'; /usr/bin/printenv PATH")]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    // A session of its own: no terminal to stop it for reading or writing, and one group to kill.
    unsafe {
        command.pre_exec(|| {
            unblock_signals();
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().ok()?;
    let pid = child.id() as i32;
    let stdout = child.stdout.take()?;
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(LOGIN_PATH_SECONDS);
    let mut path = None;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match lines.recv_timeout(left) {
            Ok(line) => {
                if let Some(found) = line.strip_prefix(MARK) {
                    path = Some(found.trim().to_string());
                    break;
                }
            }
            Err(_) => break,
        }
    }
    // The shell ends right after printing; one that does not is killed, with its group.
    let ending = Instant::now();
    while child.try_wait().ok().flatten().is_none() {
        if path.is_none() || ending.elapsed() > Duration::from_millis(500) {
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match path {
        Some(path) if path.starts_with('/') => Some(path),
        _ => {
            note(&format!("could not learn the login shell's PATH ({shell}, {LOGIN_PATH_SECONDS} s); the backend gets {inherited}"));
            None
        }
    }
}

/// Start the backend that ships beside this program. Its output goes to `app-backend.log` in
/// its folder; its standard input is a pipe this app holds, so that it ends with the app.
fn start_backend(port: u16) -> Result<Child, String> {
    let program = std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("cwbcode");
    if !program.exists() {
        return Err(format!("the backend is not in the app ({} is missing)", program.display()));
    }
    let home = home();
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let log_path = home.join("app-backend.log");
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).map_err(|e| format!("cannot write {}: {e}", log_path.display()))?;
    let mut command = Command::new(&program);
    command.arg("serve").arg("--http-port").arg(port.to_string()).arg("--exit-with-stdin");
    // (A wallet in this file turns remote mode on, as with `make start`.)
    let env_file = home.join("app.env");
    if env_file.exists() {
        command.arg("--env-file").arg(&env_file);
    } else {
        command.arg("--env-file").arg("/dev/null");
    }
    if let Ok(more) = std::env::var("CWBCODE_APP_ARGS") {
        command.args(more.split_whitespace());
    }
    if let Some(path) = login_path() {
        command.env("PATH", path);
    }
    let user_home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
    command
        .env("CWBCODE_HOME", &home)
        .current_dir(user_home)
        .stdin(Stdio::piped())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    // It must take the signals the app blocks for itself (a terminate signal stops it).
    unsafe {
        command.pre_exec(|| {
            unblock_signals();
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| format!("cannot start {}: {e}", program.display()))?;
    note(&format!("started the backend (pid {}; log {})", child.id(), log_path.display()));
    Ok(child)
}

/// The backend this app started, if it did (stopped again on quit).
struct Started(Mutex<Option<Child>>);

/// Stop the backend this app started. One that has already ended is only reaped (its pid may
/// be another program's by now).
fn stop(mut child: Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    // Asked to stop, it interrupts what is running and ends its commands; then it is made to.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    drop(child.stdin.take());
    let asked = Instant::now();
    while asked.elapsed() < Duration::from_secs(10) {
        if let Ok(Some(_)) = child.try_wait() {
            note("the backend stopped");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    note("the backend was made to stop");
}

/// Has the backend this app started ended? Then it is taken out (and reaped), with how.
fn ended(handle: &AppHandle) -> Option<std::process::ExitStatus> {
    let state = handle.state::<Started>();
    let mut started = state.0.lock().unwrap();
    let status = started.as_mut()?.try_wait().ok().flatten()?;
    started.take();
    Some(status)
}

fn stop_started(handle: &AppHandle) {
    let child = handle.state::<Started>().0.lock().unwrap().take();
    if let Some(child) = child {
        stop(child);
    }
}

/// The last lines of the log of a backend the app started.
fn log_tail() -> String {
    let log = home().join("app-backend.log");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    format!("{}\n\n(log: {})", lines[lines.len().saturating_sub(6)..].join("\n"), log.display())
}

/// Everything but letters and digits as %XX, for the starting page's address.
fn escape(text: &str) -> String {
    text.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The window, and its starting page (the app's own).
struct Window {
    window: WebviewWindow,
    splash: Url,
}

impl Window {
    /// Show the starting page with a message on it: `bad` (something is wrong) or `wait`.
    fn show(&self, kind: &str, text: &str) {
        note(text);
        let mut url = self.splash.clone();
        url.set_fragment(Some(&format!("{kind}={}", escape(text))));
        self.go(url);
    }

    fn open(&self, port: u16) {
        // The token is the backend's, readable by this user only; it has just shown it holds it.
        let address = format!("http://127.0.0.1:{port}/#token={}", token());
        match address.parse() {
            Ok(url) => {
                note(&format!("the backend on 127.0.0.1:{port} showed it holds the token: opening its page"));
                self.go(url);
            }
            Err(e) => self.show("bad", &format!("Bad address: {e}")),
        }
    }

    /// Go there, and say on the terminal where the window is a moment later (without the
    /// address's fragment, which may carry the token).
    fn go(&self, url: Url) {
        if let Err(e) = self.window.navigate(url) {
            note(&format!("cannot change the window's page: {e}"));
        }
        let window = self.window.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            if let Ok(url) = window.url() {
                note(&format!("the window shows {}", without_fragment(&url)));
            }
        });
    }
}

fn without_fragment(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

fn refused(port: u16, why: &str) -> String {
    format!(
        "Something else answers on 127.0.0.1:{port}, and could not show that it holds this backend's token: {why}.\n\nThe token was not given to it. If it is a Causewaybay Code backend started with make start from an older version, restart it from this one; otherwise stop the program on that port, or give the app another port (CWBCODE_APP_PORT)."
    )
}

/// Find the backend (or start it), open its page, and keep watching it.
fn connect(window: Window, handle: AppHandle) {
    let port = port();
    // One backend at a time: two copies of the model do not fit in memory.
    match found(port) {
        Found::Ours => {}
        Found::Other(why) => return window.show("bad", &refused(port, &why)),
        Found::Nobody => match start_backend(port) {
            Ok(child) => *handle.state::<Started>().0.lock().unwrap() = Some(child),
            Err(e) => return window.show("bad", &format!("Could not start the backend: {e}")),
        },
    }
    let mut connected = false;
    let mut began = Instant::now();
    let mut strikes = 0;
    loop {
        let now = found(port);
        // The backend this app started has ended: reaped, whatever answers now.
        if let Some(status) = ended(&handle) {
            if !matches!(now, Found::Ours) {
                let what = if connected {
                    format!("The backend stopped ({status}). Quit the app and open it again to start it again.")
                } else {
                    format!("The backend could not start ({status}). If another one is already running (make start) without the app's port, stop it (make stop) or start it again from this version.")
                };
                return window.show("bad", &format!("{what}\n\n{}", log_tail()));
            }
            note(&format!("the backend this app started ended ({status}); another one answers on 127.0.0.1:{port}"));
        }
        match now {
            Found::Ours => {
                strikes = 0;
                if !connected {
                    window.open(port);
                    connected = true;
                }
            }
            Found::Nobody if !connected => {
                strikes = 0;
                if began.elapsed() > Duration::from_secs(START_SECONDS) {
                    stop_started(&handle);
                    return window.show("bad", &format!("The backend did not answer on 127.0.0.1:{port} within {START_SECONDS} s. See {}.", home().join("app-backend.log").display()));
                }
            }
            Found::Other(why) if !connected => {
                // Twice in a row, in case the port changed hands just then.
                strikes += 1;
                if strikes >= 2 {
                    // A backend this app started that cannot have the port is of no use.
                    stop_started(&handle);
                    return window.show("bad", &refused(port, &why));
                }
            }
            lost => {
                // Connected, and the backend no longer shows itself: three looks in a row.
                strikes += 1;
                if strikes >= 3 {
                    connected = false;
                    strikes = 0;
                    began = Instant::now();
                    if let Found::Other(why) = &lost {
                        return window.show("bad", &refused(port, why));
                    }
                    // Waited for: one started with make start may come back, and one this app
                    // started (still running) may answer again; it is stopped only on quit.
                    let ours = handle.state::<Started>().0.lock().unwrap().is_some();
                    window.show("wait", &if ours {
                        format!("The backend on 127.0.0.1:{port} stopped answering, though it is still running (log: {}). Waiting for it", home().join("app-backend.log").display())
                    } else {
                        format!("The backend on 127.0.0.1:{port} stopped answering. Waiting for it to come back (make start)")
                    });
                    loop {
                        std::thread::sleep(Duration::from_secs(1));
                        if let Some(status) = ended(&handle) {
                            return window.show("bad", &format!("The backend stopped ({status}). Quit the app and open it again to start it again.\n\n{}", log_tail()));
                        }
                        match found(port) {
                            Found::Ours => break,
                            Found::Other(why) => return window.show("bad", &refused(port, &why)),
                            Found::Nobody => {}
                        }
                    }
                    continue;
                }
            }
        }
        std::thread::sleep(if connected { Duration::from_secs(2) } else { Duration::from_millis(250) });
    }
}

/// The signals that end the app cleanly (stopping the backend it started): they are taken
/// from every thread's mask before any thread starts, and waited for by one thread.
/// Programs the app starts would inherit that mask (and ignore a terminate signal), so they
/// clear it before they begin (`unblock_signals`, between fork and exec).
fn signals() -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::sigaddset(&mut set, signal);
        }
        set
    }
}

/// In a child, between fork and exec: no signal blocked (async-signal-safe calls only).
fn unblock_signals() {
    unsafe {
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::pthread_sigmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
    }
}

/// One app per backend folder: two would each start a backend. The lock is the kernel's
/// and goes with the process, however it ends.
fn single_instance() -> Result<std::fs::File, String> {
    let home = home();
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let path = home.join("app.lock");
    // (std opens it close-on-exec: the backend does not inherit the lock.)
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(format!("Causewaybay Code is already open for {} ({} is held)", home.display(), path.display()));
    }
    Ok(file)
}

fn main() {
    let set = signals();
    unsafe {
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    let lock = match single_instance() {
        Ok(lock) => lock,
        Err(e) => {
            note(&e);
            // (Opened from the Finder, macOS brings the open one forward by itself.)
            std::process::exit(0);
        }
    };
    tauri::Builder::default()
        .manage(Started(Mutex::new(None)))
        .setup(move |app| {
            let window = app.get_webview_window("main").expect("the main window");
            let splash = window.url().expect("the starting page's address");
            note(&format!("starting page: {}", without_fragment(&splash)));
            let handle = app.handle().clone();
            let signalled = handle.clone();
            std::thread::spawn(move || {
                let mut signal = 0;
                if unsafe { libc::sigwait(&set, &mut signal) } == 0 {
                    note(&format!("signal {signal}: quitting"));
                    signalled.exit(0);
                }
            });
            std::thread::spawn(move || connect(Window { window, splash }, handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the app")
        .run(move |handle, event| {
            if let RunEvent::Exit = event {
                stop_started(handle);
                let _ = &lock;
            }
        });
}
