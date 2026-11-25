//! Running a project's code at the user's request (the Run button, `/run`): the command is
//! the one given, or one guessed from what the folder holds. It runs as the agent's commands
//! do, in the project and in the sandbox, and what it printed is the answer.

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::sandbox::{Policy, run};

/// Longest a run may take, and the most of its output that is kept.
const RUN_SECONDS: u64 = 120;
const RUN_CHARS: usize = 40_000;

/// The command that runs what is in `project`: a build file decides first; otherwise the
/// source file touched last, by its kind. None when nothing there can be run.
pub fn guess(project: &Path) -> Option<String> {
    let has = |name: &str| project.join(name).is_file();
    if has("Cargo.toml") {
        return Some("cargo run".into());
    }
    if has("Makefile") && std::fs::read_to_string(project.join("Makefile")).is_ok_and(|text| text.lines().any(|line| line.starts_with("run:"))) {
        return Some("make run".into());
    }
    if has("go.mod") {
        return Some("go run .".into());
    }
    if has("package.json") {
        let package: serde_json::Value = std::fs::read_to_string(project.join("package.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        if package["scripts"]["start"].is_string() {
            return Some("npm start".into());
        }
    }
    // The source file changed last, among those that can be run by themselves.
    let mut newest: Option<(std::time::SystemTime, String)> = None;
    for entry in std::fs::read_dir(project).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = name.rsplit_once('.').map(|(_, kind)| kind).unwrap_or("");
        if name.starts_with('.') || !["rs", "py", "go", "js", "mjs", "c", "cpp", "sh", "rb", "lua", "swift"].contains(&kind) {
            continue;
        }
        let Ok(changed) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if newest.as_ref().is_none_or(|(when, _)| changed > *when) {
            newest = Some((changed, name));
        }
    }
    let (_, file) = newest?;
    let (stem, kind) = file.rsplit_once('.')?;
    let quoted = format!("'{}'", file.replace('\'', "'\\''"));
    // What is compiled goes to the temporary folder, not into the project.
    let built = format!("\"$TMPDIR/cwbcode-run-{}\"", stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>());
    Some(match kind {
        "rs" => format!("rustc -O {quoted} -o {built} && {built}"),
        "c" => format!("cc -O2 {quoted} -o {built} && {built}"),
        "cpp" => format!("c++ -O2 -std=c++17 {quoted} -o {built} && {built}"),
        "py" => format!("python3 {quoted}"),
        "go" => format!("go run {quoted}"),
        "js" | "mjs" => format!("node {quoted}"),
        "sh" => format!("sh {quoted}"),
        "rb" => format!("ruby {quoted}"),
        "lua" => format!("lua {quoted}"),
        "swift" => format!("swift {quoted}"),
        _ => return None,
    })
}

/// Run `command` (or the one guessed) in the project; the answer starts with the command.
pub fn run_project(policy: &Policy, command: &str) -> Result<String> {
    let command = match command.trim() {
        "" => match guess(&policy.workspace) {
            Some(command) => command,
            None => bail!("there is nothing here to run yet (no Cargo.toml, Makefile with a run target, go.mod, package.json with a start script, or source file). Give a command to run."),
        },
        given => given.to_string(),
    };
    let out = run(policy.command(Path::new("/bin/zsh"), &["-c", &command]), None, Duration::from_secs(RUN_SECONDS), &|| false)?;
    let mut text = format!("$ {command}\n{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&out.stderr);
    }
    if text.chars().count() > RUN_CHARS {
        let kept: String = text.chars().take(RUN_CHARS).collect();
        text = format!("{kept}\n[... the rest of the output is not shown]");
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&match (out.timed_out, out.status) {
        (true, _) => format!("[stopped after {RUN_SECONDS} seconds]"),
        (_, Some(0)) => "[done]".to_string(),
        (_, Some(code)) => format!("[exit status {code}]"),
        (_, None) => "[ended by a signal]".to_string(),
    });
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_is_guessed_from_what_the_folder_holds_and_run_in_it() {
        let base = std::env::temp_dir().join(format!("cwbcode-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        assert_eq!(guess(&ws), None);
        let policy = Policy::new(crate::sandbox::Mode::Auto, &ws, &base.join("home")).unwrap();
        assert!(run_project(&policy, "").unwrap_err().to_string().contains("nothing here to run"));

        // A source file by itself: the one changed last, by its kind.
        std::fs::write(ws.join("hello.sh"), "echo hello from $(basename \"$PWD\")\necho careful >&2\n").unwrap();
        assert_eq!(guess(&ws).unwrap(), "sh 'hello.sh'");
        assert_eq!(run_project(&policy, "").unwrap(), "$ sh 'hello.sh'\nhello from ws\ncareful\n[done]");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ws.join("it's.py"), "print(6 * 7)\n").unwrap();
        assert_eq!(guess(&ws).unwrap(), "python3 'it'\\''s.py'");
        std::fs::write(ws.join("notes.md"), "not code").unwrap();
        assert_eq!(guess(&ws).unwrap(), "python3 'it'\\''s.py'");
        // What is compiled is put in the temporary folder.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ws.join("q sort.rs"), "fn main() {}").unwrap();
        assert_eq!(guess(&ws).unwrap(), "rustc -O 'q sort.rs' -o \"$TMPDIR/cwbcode-run-qsort\" && \"$TMPDIR/cwbcode-run-qsort\"");
        // A build file decides before any source file.
        std::fs::write(ws.join("Makefile"), "build:\n\ttrue\nrun:\n\t@echo made\n").unwrap();
        assert_eq!(guess(&ws).unwrap(), "make run");
        std::fs::write(ws.join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(guess(&ws).unwrap(), "cargo run");

        // A command that is given is the one run; how it ended is said.
        assert_eq!(run_project(&policy, "echo given; exit 3").unwrap(), "$ echo given; exit 3\ngiven\n[exit status 3]");
        // It runs in the sandbox: nothing is written outside the project.
        // (the temporary folder is open to commands, so the home folder is what is tried).
        let outside = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(format!(".cwbcode-runner-test-{}", std::process::id()));
        let wrote = run_project(&policy, &format!("echo x > '{}'", outside.display())).unwrap();
        let written = outside.exists();
        let _ = std::fs::remove_file(&outside);
        assert!(!written && wrote.contains("[exit status"), "{wrote}");
    }
}
