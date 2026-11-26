//! What keeps a local agent safe to run:
//!   - no tool reaches the network, and commands run under macOS's sandbox (`sandbox-exec`)
//!     with the network denied and writes allowed only in the workspace and temporary folders;
//!   - files are written only inside the workspace (paths are resolved through symlinks
//!     first), unless the user approves one outside;
//!   - credentials (~/.ssh, ~/.aws, keychains, ...) are not readable by any tool;
//!   - in the default mode, every edit and command is shown and approved first.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Ask before every edit and command.
    Ask,
    /// Edits in the workspace and sandboxed commands run without asking.
    Auto,
    /// Nothing is written and nothing is run.
    ReadOnly,
    /// As read-only, and the agent is asked for a plan to approve before any change.
    Plan,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ask" => Self::Ask,
            "auto" => Self::Auto,
            "read-only" | "readonly" => Self::ReadOnly,
            "plan" => Self::Plan,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::ReadOnly => "read-only",
            Self::Plan => "plan",
        }
    }
}

/// The kinds of action a user can approve for the rest of a session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    Edit,
    Command,
    MakeTool,
}

pub struct Policy {
    pub mode: Mode,
    /// The folder the agent works in (canonical).
    pub workspace: PathBuf,
    /// cwbcode's own folder: compiled tools live here.
    pub home: PathBuf,
    /// Run commands under sandbox-exec.
    pub sandbox: bool,
    /// Let sandboxed commands use the network.
    pub network: bool,
    /// How the user is asked to approve something (None: nobody is there to ask). It is given
    /// what would happen and whether "always" may be offered.
    pub asker: Option<Asker>,
    always: HashSet<Action>,
}

pub type Asker = Box<dyn FnMut(&str, bool) -> Answer>;

pub enum Answer {
    Yes,
    /// Yes, and to every later action of this kind in the session.
    Always,
    /// No, with what the model is told.
    No(String),
}

impl Answer {
    /// What a person typed in answer: only an explicit yes approves.
    pub fn parse(typed: &str, offer_always: bool) -> Self {
        match typed.trim() {
            "y" | "Y" | "yes" => Self::Yes,
            "a" | "A" | "always" if offer_always => Self::Always,
            "" | "n" | "N" | "no" => Self::No("the user did not approve this. Do not retry it; ask what they would like instead.".into()),
            other => Self::No(format!("the user did not approve this and said: {other}")),
        }
    }
}

/// Folders under $HOME that no tool may read: keys, tokens and password stores.
const SECRET_DIRS: [&str; 12] = [
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".config/gh", ".config/gcloud", ".azure", ".password-store", "Library/Keychains", "Library/Cookies",
    "Library/Application Support/com.apple.TCC",
];
const SECRET_FILES: [&str; 4] = [".netrc", ".npmrc", ".pypirc", ".git-credentials"];
/// Folders under $HOME that build tools write to (package caches), writable in the sandbox.
const CACHE_DIRS: [&str; 8] = [".cargo/registry", ".cargo/git", ".cache", "Library/Caches", ".npm", ".rustup/tmp", ".rustup/downloads", "go/pkg/mod"];
const CACHE_FILES: [&str; 2] = [".cargo/.package-cache", ".cargo/.global-cache"];

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/var/empty".into()))
}

/// Resolve `.` and `..` without touching the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The real location of a path that may not exist yet: its deepest existing ancestor with
/// symlinks resolved, plus the rest. A symlink whose target does not exist (yet) is followed
/// too: writing "through" it would create that target, wherever it points.
fn real(path: &Path) -> PathBuf {
    real_within(path, 0)
}

fn real_within(path: &Path, depth: usize) -> PathBuf {
    let path = lexical(path);
    let mut existing = path.as_path();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = existing.canonicalize() {
            return rest.iter().rev().fold(canon, |p, part| p.join(part));
        }
        // Not resolvable: a link to nowhere, or simply not there.
        if depth < 32 && existing.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
            if let (Ok(target), Some(parent)) = (std::fs::read_link(existing), existing.parent()) {
                let pointed = rest.iter().rev().fold(parent.join(target), |p, part| p.join(part));
                return real_within(&pointed, depth + 1);
            }
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path,
        }
    }
}

/// Write a file without following a symlink at its last step: the path was resolved and
/// checked, and must not have become a link to somewhere else since.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| anyhow::anyhow!("cannot write {}: {e}{}", path.display(), if path.is_symlink() { " (it is a symbolic link)" } else { "" }))?;
    file.write_all(bytes)?;
    Ok(())
}

impl Policy {
    pub fn new(mode: Mode, workspace: &Path, home: &Path) -> Result<Self> {
        let workspace = workspace.canonicalize()?;
        std::fs::create_dir_all(home)?;
        Ok(Self {
            mode,
            workspace,
            home: home.canonicalize()?,
            sandbox: Path::new("/usr/bin/sandbox-exec").exists(),
            network: false,
            asker: None,
            always: HashSet::new(),
        })
    }

    /// A path as the tools take it: `~/`, relative to the workspace, or absolute; with
    /// symlinks resolved.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let path = path.trim();
        let expanded = if path == "~" {
            home_dir()
        } else if let Some(rest) = path.strip_prefix("~/") {
            home_dir().join(rest)
        } else if path.is_empty() {
            self.workspace.clone()
        } else {
            self.workspace.join(path)
        };
        real(&expanded)
    }

    pub fn in_workspace(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace)
    }

    /// A path shown to the user and the model: relative inside the workspace.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.workspace) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => path.display().to_string(),
        }
    }

    /// The server's own secrets: its database, its token, its TLS key, and its certificate
    /// authority's key (devices trust what that signs).
    fn own_secrets(&self) -> Vec<PathBuf> {
        ["cwbcode.db", "cwbcode.db-wal", "cwbcode.db-shm", "token", "key.pem", "ca-key.pem", "sessions"].iter().map(|f| self.home.join(f)).collect()
    }

    /// The project's own `.cwbcode` folder: cwbcode's records, which tools neither read nor write.
    pub fn records(&self) -> PathBuf {
        self.workspace.join(crate::store::DIR)
    }

    /// Is this (resolved) path one that no tool may read or write: credentials, the
    /// server's secrets, cwbcode's records?
    pub fn off_limits(&self, path: &Path) -> bool {
        self.secret(path)
    }

    fn secret(&self, path: &Path) -> bool {
        let home = real(&home_dir());
        // cwbcode's records anywhere: this project's, a nested project's, the server's own.
        let records = path.components().any(|c| c.as_os_str() == crate::store::DIR) && !path.starts_with(self.home.join("tools"));
        SECRET_DIRS.iter().any(|d| path.starts_with(home.join(d))) || SECRET_FILES.iter().any(|f| path == home.join(f)) || self.own_secrets().iter().any(|f| path == f) || records
    }

    /// May a tool read this file or folder?
    pub fn check_read(&self, path: &str) -> Result<PathBuf> {
        let resolved = self.resolve(path);
        if self.secret(&resolved) {
            bail!("{} is off limits: cwbcode does not read credentials or its own records", resolved.display());
        }
        Ok(resolved)
    }

    /// May a tool write this file? Asks the user when the mode or the place requires it.
    pub fn check_write(&mut self, path: &str, what: &str) -> Result<PathBuf> {
        let resolved = self.resolve(path);
        if self.secret(&resolved) {
            bail!("{} is off limits: cwbcode does not touch credentials or its own records", resolved.display());
        }
        if self.mode == Mode::ReadOnly {
            bail!("read-only mode: nothing is written. (The user can switch with /mode ask.)");
        }
        if self.mode == Mode::Plan {
            bail!("plan mode: nothing is written until the user approves the plan and switches mode. Present the plan instead.");
        }
        if !self.in_workspace(&resolved) {
            // Outside the workspace is never allowed silently, whatever the mode.
            self.ask_once(&format!("{what}\n  This is OUTSIDE the workspace ({}).", self.workspace.display()))?;
        } else if self.mode == Mode::Ask {
            self.ask(Action::Edit, what)?;
        }
        Ok(resolved)
    }

    /// May this action go ahead? In ask mode the user is shown `what`; "always" is remembered
    /// for the kind of action until the session ends.
    pub fn ask(&mut self, action: Action, what: &str) -> Result<()> {
        // Without the sandbox a command can do anything: it is never run unasked, in any mode.
        let unguarded = !self.sandbox && matches!(action, Action::Command | Action::MakeTool);
        match self.mode {
            Mode::ReadOnly => bail!("read-only mode: nothing is written or run. (The user can switch with /mode ask.)"),
            Mode::Plan => bail!("plan mode: nothing is written or run until the user approves the plan and switches mode. Present the plan instead."),
            Mode::Auto if !unguarded => return Ok(()),
            _ => {}
        }
        if unguarded {
            return self.ask_once(&format!("{what}\n  The sandbox is OFF: this runs with your full access."));
        }
        if self.always.contains(&action) {
            return Ok(());
        }
        match self.prompt(what, true)? {
            Answer::Yes => Ok(()),
            Answer::Always => {
                self.always.insert(action);
                Ok(())
            }
            Answer::No(why) => bail!("{why}"),
        }
    }

    fn ask_once(&mut self, what: &str) -> Result<()> {
        match self.prompt(what, false)? {
            Answer::Yes | Answer::Always => Ok(()),
            Answer::No(why) => bail!("{why}"),
        }
    }

    fn prompt(&mut self, what: &str, offer_always: bool) -> Result<Answer> {
        let Some(asker) = &mut self.asker else {
            return Ok(Answer::No("not allowed: nobody is there to approve it (auto mode allows edits in the workspace and sandboxed commands; outside the workspace or without the sandbox, a person must approve)".into()));
        };
        let answer = asker(what, offer_always);
        if crate::agent::interrupted() {
            return Ok(Answer::No("the user did not approve this".into()));
        }
        Ok(answer)
    }

    /// The sandbox profile for commands: everything may be read except credentials, nothing
    /// may be written except the workspace, temporary folders and package caches, and the
    /// network is closed.
    pub fn profile(&self) -> String {
        let quote = |p: &Path| format!("\"{}\"", p.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
        let home = real(&home_dir());
        let mut sb = String::from("(version 1)\n(allow default)\n");
        if !self.network {
            sb.push_str("(deny network*)\n");
        }
        // No handing work to programs outside the sandbox (`open`, AppleScript).
        sb.push_str("(deny lsopen)\n(deny appleevent-send)\n");
        sb.push_str("(deny file-write*)\n(allow file-write*\n");
        let tmp = real(&std::env::temp_dir());
        for dir in [self.workspace.clone(), self.home.join("tools"), PathBuf::from("/private/tmp"), PathBuf::from("/private/var/folders"), tmp] {
            sb.push_str(&format!("  (subpath {})\n", quote(&dir)));
        }
        for dir in CACHE_DIRS {
            sb.push_str(&format!("  (subpath {})\n", quote(&home.join(dir))));
        }
        for file in CACHE_FILES {
            sb.push_str(&format!("  (literal {})\n", quote(&home.join(file))));
        }
        sb.push_str("  (literal \"/dev/null\") (literal \"/dev/stdout\") (literal \"/dev/stderr\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\")\n  (regex #\"^/dev/ttys[0-9]+$\") (regex #\"^/dev/fd/\"))\n");
        sb.push_str("(deny file-read* file-write*\n");
        for dir in SECRET_DIRS {
            sb.push_str(&format!("  (subpath {})\n", quote(&home.join(dir))));
        }
        for file in SECRET_FILES {
            sb.push_str(&format!("  (literal {})\n", quote(&home.join(file))));
        }
        for file in self.own_secrets() {
            sb.push_str(&format!("  (literal {})\n", quote(&file)));
        }
        sb.push_str(")\n");
        // cwbcode's records anywhere (this project's, a nested project's, the server's): their
        // contents cannot be read or written. That they exist can be seen, so that `ls -la`
        // in a project works as usual.
        sb.push_str("(deny file-read-data file-write* (regex #\"/\\.cwbcode(/|$)\"))\n");
        // ... except the tools the agent made, which are compiled and run from there.
        // (Named as the denial names it: a rule for the exact operation outranks one for `file-read*`.)
        sb.push_str(&format!("(allow file-read-data file-read* file-write* (subpath {}))\n", quote(&self.home.join("tools"))));
        sb
    }

    /// A command for `program`, inside the sandbox when it is on.
    pub fn command(&self, program: &Path, args: &[&str]) -> Command {
        let mut cmd = if self.sandbox {
            let mut c = Command::new("/usr/bin/sandbox-exec");
            c.arg("-p").arg(self.profile()).arg(program);
            c
        } else {
            Command::new(program)
        };
        cmd.args(args).current_dir(&self.workspace);
        cmd
    }

    pub fn describe(&self) -> String {
        let sandbox = match (self.sandbox, self.network) {
            (true, false) => "commands sandboxed, no network",
            (true, true) => "commands sandboxed, network open",
            (false, _) => "commands NOT sandboxed (each one asks first)",
        };
        format!("{} mode · {sandbox}", self.mode.label())
    }
}

pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub interrupted: bool,
}

/// Run a command to its end, feeding it `stdin`, for at most `timeout`; `stop` is polled and
/// ends it early (Ctrl-C).
pub fn run(mut cmd: Command, stdin: Option<&[u8]>, timeout: Duration, stop: &dyn Fn() -> bool) -> Result<Output> {
    use std::os::unix::process::CommandExt;
    // A process group of its own, so the command and whatever it started end together.
    cmd.process_group(0);
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    fn read(pipe: Option<impl std::io::Read + Send + 'static>) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    }
    let out = read(child.stdout.take());
    let err = read(child.stderr.take());
    let start = Instant::now();
    let (mut timed_out, mut interrupted) = (false, false);
    let group = -(child.id() as i32);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status.code();
        }
        timed_out = start.elapsed() > timeout;
        interrupted = stop();
        if timed_out || interrupted {
            // SAFETY: signals the process group created above.
            unsafe { libc::kill(group, libc::SIGKILL) };
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // A background process the command left behind may hold the pipes open: do not wait for it.
    let text = |rx: std::sync::mpsc::Receiver<Vec<u8>>| String::from_utf8_lossy(&rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()).into_owned();
    Ok(Output { status, stdout: text(out), stderr: text(err), timed_out, interrupted })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: Mode) -> (Policy, PathBuf) {
        let base = std::env::temp_dir().join(format!("cwbcode-policy-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws/src")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        let p = Policy::new(mode, &base.join("ws"), &base.join("home")).unwrap();
        (p, base.canonicalize().unwrap())
    }

    #[test]
    fn paths_resolve_inside_the_workspace_and_escapes_are_seen() {
        let (p, base) = policy(Mode::Auto);
        assert_eq!(p.resolve("src/a.rs"), base.join("ws/src/a.rs"));
        assert_eq!(p.resolve(""), base.join("ws"));
        assert!(p.in_workspace(&p.resolve("new/dir/file.txt")));
        assert!(!p.in_workspace(&p.resolve("../outside/x")));
        assert!(!p.in_workspace(&p.resolve("src/../../outside/x")));
        assert!(!p.in_workspace(&p.resolve("/etc/hosts")));
        // A symlink out of the workspace is followed before the check.
        std::os::unix::fs::symlink(base.join("outside"), base.join("ws/link")).unwrap();
        assert_eq!(p.resolve("link/x.txt"), base.join("outside/x.txt"));
        assert!(!p.in_workspace(&p.resolve("link/x.txt")));
        assert_eq!(p.display(&p.resolve("src/a.rs")), "src/a.rs");
    }

    #[test]
    fn a_link_to_a_file_that_does_not_exist_yet_is_followed_before_the_check() {
        let (mut p, base) = policy(Mode::Auto);
        // In the workspace, links to places outside it where nothing exists yet.
        std::os::unix::fs::symlink(base.join("outside/planted.txt"), base.join("ws/dangling")).unwrap();
        std::os::unix::fs::symlink("../outside/relative.txt", base.join("ws/relative")).unwrap();
        std::os::unix::fs::symlink(base.join("ws/dangling"), base.join("ws/chain")).unwrap();
        std::os::unix::fs::symlink("loop-b", base.join("ws/loop-a")).unwrap();
        std::os::unix::fs::symlink("loop-a", base.join("ws/loop-b")).unwrap();
        assert_eq!(p.resolve("dangling"), base.join("outside/planted.txt"));
        assert_eq!(p.resolve("relative"), base.join("outside/relative.txt"));
        assert_eq!(p.resolve("chain"), base.join("outside/planted.txt"));
        for link in ["dangling", "relative", "chain", "dangling/sub/file.txt"] {
            assert!(!p.in_workspace(&p.resolve(link)), "{link}");
            assert!(p.check_write(link, "write").is_err(), "{link} was writable");
        }
        // A link to a credential that is not there yet is still a credential.
        std::os::unix::fs::symlink(home_dir().join(".ssh/authorized_keys_cwbcode_test"), base.join("ws/keys")).unwrap();
        assert!(p.check_write("keys", "write").unwrap_err().to_string().contains("off limits"));
        assert!(p.check_read("keys").is_err());
        // Links in a circle resolve to something (and do not hang); a dangling link inside
        // the workspace to inside the workspace is fine.
        let _ = p.resolve("loop-a");
        std::os::unix::fs::symlink("src/later.rs", base.join("ws/inside")).unwrap();
        assert_eq!(p.check_write("inside", "write").unwrap(), base.join("ws/src/later.rs"));
        // The write itself does not go through a link that appears at the last moment.
        let target = base.join("outside/swapped.txt");
        std::os::unix::fs::symlink(&target, base.join("ws/swapped")).unwrap();
        assert!(write_file(&base.join("ws/swapped"), b"x").is_err());
        assert!(!target.exists());
        write_file(&base.join("ws/plain.txt"), b"ok").unwrap();
        assert_eq!(std::fs::read(base.join("ws/plain.txt")).unwrap(), b"ok");
    }

    #[test]
    fn writes_follow_the_mode_and_never_leave_the_workspace_unasked() {
        let (mut p, _) = policy(Mode::Auto);
        assert!(p.check_write("src/a.rs", "write").is_ok());
        // Outside: needs a person, and there is none.
        assert!(p.check_write("../outside/x", "write").is_err());
        assert!(p.check_write("link-less/../../outside/x", "write").is_err());
        p.mode = Mode::Ask;
        assert!(p.check_write("src/a.rs", "write").unwrap_err().to_string().contains("nobody is there"));
        assert!(p.ask(Action::Command, "ls").is_err());
        p.mode = Mode::ReadOnly;
        assert!(p.check_write("src/a.rs", "write").unwrap_err().to_string().contains("read-only"));
        assert!(p.ask(Action::Command, "ls").is_err());
    }

    #[test]
    fn a_person_is_asked_and_only_an_explicit_yes_approves() {
        let (mut p, _) = policy(Mode::Ask);
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let answers = std::rc::Rc::new(std::cell::RefCell::new(vec!["a", "", "do it in tmp", "y"]));
        let (log, queue) = (asked.clone(), answers.clone());
        p.asker = Some(Box::new(move |what, always| {
            log.borrow_mut().push(what.to_string());
            Answer::parse(queue.borrow_mut().pop().unwrap(), always)
        }));
        assert!(p.ask(Action::Command, "Run: ls").is_ok());
        assert_eq!(p.ask(Action::Command, "Run: pwd").unwrap_err().to_string(), "the user did not approve this and said: do it in tmp");
        assert!(p.ask(Action::Command, "Run: w").is_err());
        // "always" covers the kind of action it was given for, and only that.
        assert!(p.ask(Action::Edit, "Edit a").is_ok());
        assert!(p.ask(Action::Edit, "Edit b").is_ok());
        assert_eq!(*asked.borrow(), ["Run: ls", "Run: pwd", "Run: w", "Edit a"]);
        assert!(answers.borrow().is_empty());
    }

    #[test]
    fn the_projects_own_records_and_the_servers_secrets_are_off_limits() {
        let (mut p, _) = policy(Mode::Auto);
        assert!(p.check_read(".cwbcode/cwbcode.db").is_err());
        assert!(p.check_write(".cwbcode/x", "write").is_err());
        assert!(p.check_write("src/.cwbcode", "write").is_err());
        assert!(p.check_write("src/cwbcode.txt", "write").is_ok());
        for secret in ["token", "key.pem", "ca-key.pem", "cwbcode.db"] {
            assert!(p.check_read(&p.home.join(secret).to_string_lossy()).is_err(), "{secret}");
        }
        let profile = p.profile();
        assert!(profile.contains("(deny file-read-data file-write* (regex #\"/\\.cwbcode(/|$)\"))"));
        // A project nested in this one keeps its records to itself too.
        assert!(p.check_read("sub/.cwbcode/cwbcode.db").is_err());
        assert!(p.check_write("sub/deeper/.cwbcode/x", "write").is_err());
        if p.sandbox {
            std::fs::create_dir_all(p.workspace.join("sub/.cwbcode")).unwrap();
            std::fs::write(p.workspace.join("sub/.cwbcode/inner.txt"), "private").unwrap();
            let sh = |script: &str| run(p.command(Path::new("/bin/sh"), &["-c", script]), None, Duration::from_secs(20), &|| false).unwrap();
            assert_ne!(sh("cat sub/.cwbcode/inner.txt").status, Some(0));
            assert_ne!(sh("echo x > sub/.cwbcode/new.txt").status, Some(0));
            assert_eq!(sh("echo ok > sub/fine.txt && cat sub/fine.txt").stdout.trim(), "ok");
            // The folder can be seen (a plain listing of the project works), not looked into.
            let listing = sh("ls -la sub");
            assert!(listing.status == Some(0) && listing.stdout.contains(".cwbcode"), "{} {}", listing.stdout, listing.stderr);
            assert_ne!(sh("ls sub/.cwbcode").status, Some(0));
        }
        assert!(profile.contains(&format!("(literal \"{}\")", p.home.join("token").display())));
    }

    #[test]
    fn without_the_sandbox_a_command_always_needs_a_person() {
        let (mut p, _) = policy(Mode::Auto);
        p.sandbox = true;
        assert!(p.ask(Action::Command, "ls").is_ok());
        assert!(p.ask(Action::MakeTool, "tool").is_ok());
        p.sandbox = false;
        // Auto mode, but nobody to ask: refused.
        assert!(p.ask(Action::Command, "ls").is_err());
        assert!(p.ask(Action::MakeTool, "tool").is_err());
        assert!(p.ask(Action::Edit, "edit").is_ok());
        assert!(p.describe().contains("NOT sandboxed"));
    }

    #[test]
    fn credentials_are_off_limits() {
        let (mut p, _) = policy(Mode::Auto);
        assert!(p.check_read("~/.ssh/id_ed25519").is_err());
        assert!(p.check_read("~/.aws/credentials").is_err());
        assert!(p.check_read("~/.netrc").is_err());
        assert!(p.check_write("~/.ssh/authorized_keys", "write").is_err());
        assert!(p.check_read("/etc/hosts").is_ok());
        assert!(p.check_read("src/a.rs").is_ok());
    }

    #[test]
    fn the_sandbox_stops_writes_outside_and_the_network() {
        let (p, base) = policy(Mode::Auto);
        if !p.sandbox {
            return;
        }
        let sh = |script: &str| run(p.command(Path::new("/bin/sh"), &["-c", script]), None, Duration::from_secs(20), &|| false).unwrap();
        let inside = sh("echo hi > made.txt && cat made.txt");
        assert_eq!((inside.status, inside.stdout.trim()), (Some(0), "hi"));
        // (The test's own folders are temporary ones, which the sandbox lets commands write.)
        let target = home_dir().join(format!(".cwbcode-test-escape-{}.txt", std::process::id()));
        let outside = sh(&format!("echo hi > '{}'", target.display()));
        assert_ne!(outside.status, Some(0));
        assert!(!target.exists());
        let _ = base;
        let secrets = sh("ls ~/.ssh");
        assert_ne!(secrets.status, Some(0));
        let net = sh("/usr/bin/curl -s -m 5 -o /dev/null http://1.1.1.1");
        assert_ne!(net.status, Some(0));
        // Nor can it hand a file to a program outside the sandbox.
        let handed = sh("echo 'touch escaped-by-open' > job.command; chmod +x job.command; /usr/bin/open job.command; sleep 1; ls escaped-by-open");
        assert_ne!(handed.status, Some(0), "{} {}", handed.stdout, handed.stderr);
    }

    #[test]
    fn a_command_is_fed_timed_out_and_stopped() {
        let fed = run(Command::new("/bin/cat"), Some(b"abc"), Duration::from_secs(5), &|| false).unwrap();
        assert_eq!(fed.stdout, "abc");
        let mut slow = Command::new("/bin/sleep");
        slow.arg("5");
        let t = Instant::now();
        let out = run(slow, None, Duration::from_millis(100), &|| false).unwrap();
        assert!(out.timed_out && t.elapsed() < Duration::from_secs(3));
        let mut slow = Command::new("/bin/sleep");
        slow.arg("5");
        assert!(run(slow, None, Duration::from_secs(5), &|| true).unwrap().interrupted);
    }
}
