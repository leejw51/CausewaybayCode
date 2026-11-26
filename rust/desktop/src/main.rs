//! Causewaybay Code as a Mac app: a window on the web client, and the backend beside it.
//!
//! On start the window shows a page of its own (splash/) while the app finds the backend:
//! one already answering on the app's port (`make start` serves it there too) is used as it
//! is; otherwise the app starts the backend that ships inside it (`cwbcode`, beside this
//! program, with MLX's `mlx.metallib`) and stops it again on quit. Once the backend answers,
//! the window goes to its page with the backend's token in the address (the page keeps it and
//! takes it out), as the `open` link `make url` prints does.
//!
//! The window reaches the backend over plain HTTP on 127.0.0.1 (`cwbcode serve --http-port`):
//! a web view will not take the backend's own certificate. That port answers this machine
//! only, and asks for the token like the HTTPS one. The page gets no commands of the app's.
//!
//!   CWBCODE_HOME       the backend's folder [~/.cwbcode]
//!   CWBCODE_APP_PORT   the app's port [7480]
//!   CWBCODE_APP_ARGS   more arguments for a backend the app starts (e.g. `--no-model`)

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{Manager, RunEvent, WebviewWindow};

const DEFAULT_PORT: u16 = 7480;
/// How long a backend may take to answer: it starts listening before the model has loaded.
const START_SECONDS: u64 = 120;

fn home() -> PathBuf {
    match std::env::var_os("CWBCODE_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cwbcode"),
    }
}

fn port() -> u16 {
    std::env::var("CWBCODE_APP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// Does a cwbcode backend answer on the app's port? (Its page, over plain HTTP.)
fn answers(port: u16) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else { return false };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream.write_all(format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 512];
    let n = stream.read(&mut head).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]);
    head.starts_with("HTTP/1.1 200") && head.to_ascii_lowercase().contains("x-cwbcode-protocol")
}

/// The PATH a terminal would have. Opened from the Finder the app gets a minimal one, and the
/// agent's commands would not find cargo, Homebrew's tools, and the like. It takes an
/// interactive login shell (~/.cargo/bin is often added in .zshrc, not .zprofile); what its
/// startup files print is passed over by a marker.
fn login_path() -> Option<String> {
    const MARK: &str = "__cwbcode_path__=";
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/')).unwrap_or_else(|| "/bin/zsh".to_string());
    let out = Command::new(shell).args(["-i", "-l", "-c", &format!("printf '\\n{MARK}%s\\n' \"$PATH\"")]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let path = text.lines().rev().find_map(|line| line.strip_prefix(MARK))?.trim().to_string();
    path.contains('/').then_some(path)
}

/// Start the backend that ships beside this program. Its output goes to `app-backend.log` in
/// its folder.
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
    command.arg("serve").arg("--http-port").arg(port.to_string());
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
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    command.spawn().map_err(|e| format!("cannot start {}: {e}", program.display()))
}

/// Say on the starting page what went wrong (it has no commands; this writes into it).
fn tell(window: &WebviewWindow, text: &str) {
    let quoted = format!("{text:?}");
    let _ = window.eval(&format!("(() => {{ const s = document.getElementById('state'); if (s) {{ s.className = 'bad'; s.textContent = {quoted}; }} }})()"));
}

/// The backend this app started, if it did (stopped again on quit).
struct Started(Mutex<Option<Child>>);

fn stop(child: &mut Child) {
    // Asked to stop, it interrupts what is running and ends its commands; then it is made to.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let asked = Instant::now();
    while asked.elapsed() < Duration::from_secs(10) {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn main() {
    tauri::Builder::default()
        .manage(Started(Mutex::new(None)))
        .setup(|app| {
            let window = app.get_webview_window("main").expect("the main window");
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let port = port();
                // One backend at a time: two copies of the model do not fit in memory.
                if !answers(port) {
                    match start_backend(port) {
                        Ok(child) => *handle.state::<Started>().0.lock().unwrap() = Some(child),
                        Err(e) => return tell(&window, &format!("Could not start the backend: {e}")),
                    }
                }
                let began = Instant::now();
                while !answers(port) {
                    let exited = handle.state::<Started>().0.lock().unwrap().as_mut().and_then(|c| c.try_wait().ok().flatten());
                    if let Some(status) = exited {
                        let log = home().join("app-backend.log");
                        let tail: String = std::fs::read_to_string(&log).unwrap_or_default().lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                        return tell(&window, &format!(
                            "The backend stopped ({status}). If one is already running (make start) without the app's port, stop it (make stop) or start it again from this version.\n\n{tail}\n\n(log: {})",
                            log.display()
                        ));
                    }
                    if began.elapsed() > Duration::from_secs(START_SECONDS) {
                        return tell(&window, &format!("The backend did not answer on 127.0.0.1:{port} within {START_SECONDS} s. See {}.", home().join("app-backend.log").display()));
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                // The token is the backend's, readable by this user only.
                let token = std::fs::read_to_string(home().join("token")).unwrap_or_default().trim().to_string();
                let address = if token.is_empty() { format!("http://127.0.0.1:{port}/") } else { format!("http://127.0.0.1:{port}/#token={token}") };
                match address.parse() {
                    Ok(url) => {
                        let _ = window.navigate(url);
                    }
                    Err(e) => tell(&window, &format!("Bad address: {e}")),
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building the app")
        .run(|handle, event| {
            if let RunEvent::Exit = event {
                if let Some(mut child) = handle.state::<Started>().0.lock().unwrap().take() {
                    stop(&mut child);
                }
            }
        });
}
