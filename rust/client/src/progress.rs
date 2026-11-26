//! The model on its way, in the terminal: when the backend's model is not ready yet, the
//! client follows the backend's events stream (the same messages every client gets) and
//! draws one line, redrawn ten times a second, as scripts/model.py draws its own: a spinner,
//! a bar, the percent, the bytes, the speed and the time left; then the stage it is at.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use cwbcode_proto::Progress;
use cwbcodeclient::net::Client;

const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const PARTS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
/// Said once when no Hugging Face token was found.
pub const TOKEN_TIP: &str = "Tip: a free Hugging Face token makes this faster: run `hf auth login` (or set HF_TOKEN), then restart the backend";

fn human(bytes: f64) -> String {
    if bytes >= 1e9 { format!("{:.1} GB", bytes / 1e9) } else { format!("{:.0} MB", bytes / 1e6) }
}

/// "1:02:03", "12:34".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Where the download comes from, said with the token's place (never the token).
pub fn source(p: &Progress) -> String {
    if p.source_kind.is_empty() {
        return String::new();
    }
    if p.token {
        format!("from Hugging Face, with your token ({}): faster downloads", p.token_from)
    } else if p.token_refused {
        "from Hugging Face: your token was refused; downloading without it (check it with hf auth whoami)".to_string()
    } else {
        "from Hugging Face, without a token (none found, none needed)".to_string()
    }
}

/// The line for a message: `frame` turns the spinner, `width` is the bar's cells.
pub fn line(p: &Progress, frame: usize, width: usize) -> String {
    let spin = SPIN[frame % SPIN.len()];
    match p.state.as_str() {
        "downloading" if p.total == 0 => format!("{spin} heating the forge: reading the checkpoint ({})", source(p)),
        "downloading" => {
            let cells = p.fraction() * width as f64;
            let full = cells as usize;
            let mut bar: String = std::iter::repeat_n('█', full.min(width)).collect();
            if full < width {
                bar.push(PARTS[((cells - full as f64) * 8.0) as usize]);
                bar.extend(std::iter::repeat_n(' ', width - full - 1));
            }
            let left = if p.speed > 1.0 && p.total > p.done { format!("{} left", clock((p.total - p.done) as f64 / p.speed)) } else { "--:-- left".to_string() };
            let files = if p.files_total > 0 { format!("  {} of {} files", p.files_done, p.files_total) } else { String::new() };
            format!("{spin} the model \x1b[36m▕{bar}▏\x1b[0m {:5.1}%  {} / {}  {}/s  {left}{files}  {}", p.fraction() * 100.0, human(p.done as f64), human(p.total as f64), human(p.speed), source(p))
        }
        "verifying" => format!("{spin} tempering: checking every file against Ollama's registry"),
        "loading" => format!("{spin} waking it up: loading the model into memory"),
        "missing" if !p.error.is_empty() => format!("✗ the download stopped: {}", p.error),
        "missing" => "the model is not downloaded".to_string(),
        "failed" => format!("✗ the model did not load: {}", p.error),
        other => format!("✔ the model is {other}"),
    }
}

/// Is the model there to answer, by the backend's Status.model_state?
pub fn ready(state: &str) -> bool {
    matches!(state, "ready" | "scripted" | "none")
}

/// Follow the events stream until the model is ready, drawing the line on standard error (a
/// plain line on each change of stage, and every ten seconds, where it is not a terminal).
pub async fn follow(client: &Client) -> Result<()> {
    let mut events = client.progress().await?;
    let tty = std::io::stderr().is_terminal();
    let mut last: Option<Progress> = None;
    let (mut frame, mut plain_at, mut tipped) = (0usize, Instant::now() - Duration::from_secs(60), false);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    eprintln!("The model is not ready yet: following it (Ctrl-C stops waiting; chat works once it is ready).");
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let result: Result<()> = loop {
        tokio::select! {
            next = events.next_progress() => match next? {
                Some(p) => {
                    let changed = last.as_ref().is_none_or(|l| l.state != p.state);
                    if !tty && (changed || plain_at.elapsed() >= Duration::from_secs(10)) {
                        plain_at = Instant::now();
                        eprintln!("{}", line(&p, 0, 30).replace("\x1b[36m", "").replace("\x1b[0m", ""));
                    }
                    if !tipped && p.state == "downloading" && !p.source_kind.is_empty() && !p.token {
                        tipped = true;
                        if tty { eprint!("\r\x1b[2K"); }
                        eprintln!("{TOKEN_TIP}");
                    }
                    let settled = p.settled();
                    let stopped = matches!(p.state.as_str(), "failed") || (p.state == "missing" && !p.error.is_empty());
                    last = Some(p);
                    if settled || stopped {
                        break Ok(());
                    }
                }
                None => break Ok(()),
            },
            _ = tick.tick() => {}
            _ = interrupt.recv() => {
                if tty { eprint!("\r\x1b[2K"); }
                eprintln!("(stopped waiting; the model is still on its way)");
                return Ok(());
            }
        }
        if tty {
            if let Some(p) = &last {
                frame += 1;
                let cols: usize = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()).unwrap_or(130);
                let width = cols.saturating_sub(100).clamp(10, 30);
                eprint!("\r\x1b[2K{}", line(p, frame, width));
                let _ = std::io::stderr().flush();
            }
        }
    };
    if tty {
        eprint!("\r\x1b[2K");
    }
    result?;
    match last {
        Some(p) if p.settled() => {
            eprintln!("✔ the model is {}", p.state);
            Ok(())
        }
        Some(p) => bail!("{}", line(&p, 0, 30)),
        None => bail!("the backend's events stream ended at once"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_says_how_far_and_where_from_never_the_token() {
        let mut p = Progress { state: "downloading".into(), done: 7_500_000_000, total: 20_200_000_000, speed: 42.7e6, files_done: 468, files_total: 1250, source_kind: "huggingface".into(), token: true, token_from: "~/.cache/huggingface/token".into(), ..Progress::default() };
        let l = line(&p, 3, 20);
        assert!(l.starts_with('⠸') && l.contains(" 37.1%") && l.contains("7.5 GB / 20.2 GB") && l.contains("43 MB/s") && l.contains("4:57 left") && l.contains("468 of 1250 files"), "{l}");
        assert!(l.contains("with your token (~/.cache/huggingface/token): faster downloads"), "{l}");
        // The bar is as wide as asked: 20 cells between its ends.
        let bar = l.split('▕').nth(1).unwrap().split('▏').next().unwrap();
        assert_eq!(bar.chars().count(), 20, "{bar:?}");
        p.token = false;
        assert!(line(&p, 0, 20).contains("without a token (none found, none needed)"));
        p.token_refused = true;
        assert!(line(&p, 0, 20).contains("your token was refused; downloading without it"));
        for (state, says) in [("verifying", "tempering"), ("loading", "waking it up"), ("ready", "✔")] {
            assert!(line(&Progress { state: state.into(), ..Progress::default() }, 0, 20).contains(says), "{state}");
        }
        assert!(line(&Progress { state: "failed".into(), error: "no memory".into(), ..Progress::default() }, 0, 20).contains("no memory"));
        assert_eq!(clock(3723.0), "1:02:03");
        assert!(ready("ready") && ready("scripted") && !ready("downloading"));
    }
}
