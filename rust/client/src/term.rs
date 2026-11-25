//! What the terminal shows of a turn: the answer on stdout; thinking, tool calls and notes on
//! stderr (so `cwbcodeclient -p ... > answer.txt` keeps only the answer).

use std::io::{IsTerminal, Write};

use cwbcode_proto::{Usage, What};

#[derive(PartialEq)]
enum Stream {
    None,
    Thinking,
    Text,
}

pub struct Term {
    pub show_thinking: bool,
    /// Nothing but the answer.
    pub quiet: bool,
    color: bool,
    stream: Stream,
    /// The answer's last character was a line end (or nothing was written yet).
    at_line_start: bool,
}

/// Text from the model, a file or a command, made safe to print: control characters (escape
/// sequences that could hide or rewrite what the terminal shows) become a visible mark.
pub fn printable(text: &str) -> String {
    text.chars().map(|c| if c == '\n' || c == '\t' || !c.is_control() { c } else { '\u{fffd}' }).collect()
}

pub fn kilo(n: u64) -> String {
    if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() }
}

pub fn usage_line(u: &Usage) -> String {
    let speed = if u.tokens_per_second > 0.0 { format!(" · {:.0} tok/s", u.tokens_per_second) } else { String::new() };
    let tools = if u.tool_calls > 0 { format!(" · {} tool call{}", u.tool_calls, if u.tool_calls == 1 { "" } else { "s" }) } else { String::new() };
    format!("[{} tokens in context · wrote {}{speed}{tools} · {:.1}s]", kilo(u.context_tokens), kilo(u.generated), u.seconds)
}

/// The task list (JSON [{content, status}]) as lines.
pub fn todo_lines(json: &str) -> Vec<String> {
    let todos: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    todos
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|t| {
                    let mark = match t["status"].as_str() {
                        Some("completed") => "☒",
                        Some("in_progress") => "◐",
                        _ => "☐",
                    };
                    format!("{mark} {}", t["content"].as_str().unwrap_or(""))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Term {
    pub fn new(show_thinking: bool, quiet: bool) -> Self {
        let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { show_thinking, quiet, color, stream: Stream::None, at_line_start: true }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }

    /// End whatever is streaming, leaving the cursor at the start of a line.
    pub fn settle(&mut self) {
        match self.stream {
            Stream::Thinking => eprintln!("{}", if self.color { "\x1b[0m" } else { "" }),
            Stream::Text => {
                if !self.at_line_start {
                    println!();
                    self.at_line_start = true;
                }
            }
            Stream::None => {}
        }
        self.stream = Stream::None;
    }

    pub fn show(&mut self, what: &What) {
        // Whatever arrives is shown as text, never obeyed as terminal control.
        let cleaned = match what {
            What::Thinking(t) => What::Thinking(printable(t)),
            What::Text(t) => What::Text(printable(t)),
            What::ToolStart { name, summary, args } => What::ToolStart { name: name.clone(), summary: printable(summary), args: args.clone() },
            What::ToolEnd { ok, text, images, seconds } => What::ToolEnd { ok: *ok, text: printable(text), images: *images, seconds: *seconds },
            What::Todos(t) => What::Todos(printable(t)),
            What::Note(t) => What::Note(printable(t)),
            What::Warning(t) => What::Warning(printable(t)),
            What::Error(t) => What::Error(printable(t)),
            other => other.clone(),
        };
        match &cleaned {
            What::Thinking(text) => {
                if !self.show_thinking || self.quiet {
                    return;
                }
                if self.stream != Stream::Thinking {
                    self.settle();
                    eprint!("{}", if self.color { "\x1b[2;3m" } else { "" });
                    self.stream = Stream::Thinking;
                }
                eprint!("{text}");
                let _ = std::io::stderr().flush();
            }
            What::Text(text) => {
                if self.stream != Stream::Text {
                    self.settle();
                    self.stream = Stream::Text;
                }
                // Leading blank lines of an answer are not shown.
                let text = if self.at_line_start { text.trim_start_matches('\n') } else { text };
                if text.is_empty() {
                    return;
                }
                print!("{text}");
                let _ = std::io::stdout().flush();
                self.at_line_start = text.ends_with('\n');
            }
            What::EndMessage => self.settle(),
            What::ToolStart { summary, .. } => {
                self.settle();
                if !self.quiet {
                    eprintln!("{} {}", self.paint("36", "⏺"), self.paint("1", summary));
                }
            }
            What::ToolEnd { ok, text, images, seconds } => {
                if self.quiet {
                    return;
                }
                let lines: Vec<&str> = text.lines().collect();
                let shown = if *ok { 4 } else { 8 };
                for line in lines.iter().take(shown) {
                    let line: String = line.chars().take(160).collect();
                    eprintln!("  {}", self.paint(if *ok { "2" } else { "31" }, &format!("⎿ {line}")));
                }
                let mut tail = Vec::new();
                if lines.len() > shown {
                    tail.push(format!("{} more lines", lines.len() - shown));
                }
                if *images > 0 {
                    tail.push(format!("{images} image{}", if *images == 1 { "" } else { "s" }));
                }
                if *seconds >= 1.0 {
                    tail.push(format!("{seconds:.1}s"));
                }
                if !tail.is_empty() {
                    eprintln!("  {}", self.paint("2", &format!("  ({})", tail.join(", "))));
                }
            }
            What::Todos(json) => {
                self.settle();
                if !self.quiet {
                    for line in todo_lines(json) {
                        eprintln!("  {}", self.paint("35", &line));
                    }
                }
            }
            What::Note(text) => self.note(text),
            What::Warning(text) => self.warn(text),
            What::Error(text) => self.error(text),
            What::Usage(usage) => self.note(&usage_line(usage)),
            What::Session(_) | What::User(_) | What::Approval { .. } | What::Answered(_) | What::Done => {}
        }
    }

    /// A line from cwbcode itself.
    pub fn note(&mut self, text: &str) {
        self.settle();
        if !self.quiet {
            eprintln!("{}", self.paint("2", text));
        }
    }

    pub fn warn(&mut self, text: &str) {
        self.settle();
        eprintln!("{}", self.paint("33", text));
    }

    pub fn error(&mut self, text: &str) {
        self.settle();
        eprintln!("{}", self.paint("31", text));
    }

    /// Ask the user to approve something; returns what they typed.
    pub fn ask(&mut self, what: &str, offer_always: bool) -> String {
        self.settle();
        let always = if offer_always { " / [a]lways this session" } else { "" };
        loop {
            // Keys typed before the question was shown are not an answer to it.
            // SAFETY: tcflush only discards the terminal's unread input.
            unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
            eprint!("\n{}\n  Allow? [y]es{always} / [n]o, or say what to do instead: ", self.paint("33", &format!("? {}", printable(what))));
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => return "n".to_string(),
                // Only something typed is an answer; an empty line asks again.
                Ok(_) if line.trim().is_empty() => continue,
                Ok(_) => return line.trim().to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usage_line_and_the_task_list_read_well() {
        let usage = Usage { context_tokens: 4100, generated: 265, tokens_per_second: 95.4, tool_calls: 3, seconds: 5.31, steps: 4 };
        assert_eq!(usage_line(&usage), "[4.1k tokens in context · wrote 265 · 95 tok/s · 3 tool calls · 5.3s]");
        assert_eq!(usage_line(&Usage { tool_calls: 1, ..Usage::default() }), "[0 tokens in context · wrote 0 · 1 tool call · 0.0s]");
        assert_eq!(todo_lines(r#"[{"content":"a","status":"completed"},{"content":"b","status":"in_progress"},{"content":"c","status":"pending"}]"#), ["☒ a", "◐ b", "☐ c"]);
        assert!(todo_lines("not json").is_empty());
        // An escape sequence that would erase the line above it is shown, not obeyed.
        assert_eq!(printable("rm -rf x\u{1b}[2K\u{1b}[1A\rls\tfine\n한글 \u{9b}31m"), "rm -rf x\u{fffd}[2K\u{fffd}[1A\u{fffd}ls\tfine\n한글 \u{fffd}31m");
    }
}
