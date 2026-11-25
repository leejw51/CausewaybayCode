//! The interactive prompt, its slash commands, and a turn of the conversation.

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result, bail};
use cwbcode_proto::{Hit, Method, Request, Upload, What};
use rustyline::error::ReadlineError;
use tokio::signal::unix::{Signal, SignalKind, signal};

use crate::term::{Term, todo_lines};
use crate::{Cli, Session, read_upload};

const HELP: &str = "\
Type a message and press Enter. End a line with \\ to continue on the next. Ctrl-C stops the
model or a running command; Ctrl-D leaves.

Conversation
  /clear                 start a new conversation
  /compact               replace the model's context by a summary (the log keeps everything)
  /save [name]           name this conversation (every conversation is saved as you go)
  /sessions              list this project's conversations
  /resume <id|name>      continue one
  /run [command]         run the project's code (the command, or one guessed from the folder)
  /undo                  take back the file changes of the last turn that made any
  /changes               every file version saved in this conversation
  /todos                 the model's task list
  /status                backend, model, context used, protocol

Attach to your next message
  /image <file>          show an image
  /file <file>           include a text file

Knowledge base (the project's; SQLite, BM25 + semantic search)
  /remember <text>       save a note
  /add <file> [title]    save a file: text is indexed, images are described, others are stored
  /kb                    list the entries
  /search <query>        hybrid search
  /load <id>             put an entry (text, file or image) into your next message
  /export <id> <path>    write an entry's stored file to disk
  /forget <id>           delete an entry

Tools and safety
  /tools                 the tools the agent has made (/tools <name> shows the source)
  /tools rm <name>       delete a made tool
  /mode ask|auto|read-only   ask before edits and commands (default) / do not ask / no changes
  /think <level>         none, minimal, low, medium, high, max
  !<command>             run a shell command yourself, here
  /help   /exit";

/// What the client keeps between messages.
pub struct State {
    /// The conversation (0: a new one starts with the next message).
    pub session: i64,
    pub mode: String,
    pub think: String,
    /// Waiting to go with the next message.
    pub images: Vec<Upload>,
    pub texts: Vec<String>,
    show_thinking: bool,
    verbose: bool,
}

impl State {
    pub fn new(cli: &Cli) -> Self {
        let mode = if cli.auto { "auto" } else if cli.read_only { "read-only" } else { "ask" };
        Self { session: 0, mode: mode.to_string(), think: cli.think.clone().unwrap_or_default(), images: Vec::new(), texts: Vec::new(), show_thinking: !cli.hide_thinking, verbose: cli.verbose }
    }
}

/// A path as typed or dragged into the terminal: quotes and backslash-escaped spaces removed,
/// `~/` expanded.
fn unquote(path: &str) -> String {
    let path = path.trim();
    let mut out = path.replace("\\ ", " ");
    for quote in ['"', '\''] {
        if path.len() >= 2 && path.starts_with(quote) && path.ends_with(quote) {
            out = path[1..path.len() - 1].to_string();
        }
    }
    match out.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => out,
    }
}

pub fn format_hits(hits: &[Hit], snippet_chars: usize) -> String {
    if hits.is_empty() {
        return "No matches in the knowledge base.".to_string();
    }
    let mut out = String::new();
    for hit in hits {
        let how = match (hit.bm25_rank, hit.semantic_rank) {
            (0, 0) => String::new(),
            (b, 0) => format!("bm25 #{b}"),
            (0, s) => format!("semantic #{s}"),
            (b, s) => format!("bm25 #{b}, semantic #{s}"),
        };
        let snippet: String = hit.snippet.chars().take(snippet_chars).collect();
        let more = if hit.snippet.chars().count() > snippet_chars { " ..." } else { "" };
        out.push_str(&format!("[#{} {} \"{}\" · {how}]\n{snippet}{more}\n\n", hit.doc, hit.kind, hit.title));
    }
    out.trim_end().to_string()
}

/// What mode the backend is in, and how to change it.
pub fn remote_lines(s: &cwbcode_proto::Status) -> String {
    match s.auth.as_str() {
        "wallet" => format!(
            "remote mode: on\nwallet      {}{}\nlistening   {}\nlogin       the wallet's recovery phrase (web), `cwbcodeclient login` (terminal, other machines)\nturn off    {}",
            s.wallet,
            if s.wallet_source == "settings" { String::new() } else { format!("  (set in {})", s.wallet_source) },
            s.bind,
            if s.wallet_source == "settings" { "cwbcodeclient remote off".to_string() } else { format!("remove CWBCODE_WALLET from {} and restart the backend", s.wallet_source) },
        ),
        "open" => format!("remote mode: off (local mode)\nlistening   {}\nlogin       none\nturn on     cwbcodeclient remote on <wallet address>   (or the web client's dashboard)", s.bind),
        _ => format!("remote mode: off, but the backend listens on {} and asks for its token", s.bind),
    }
}

pub async fn status(session: &Session) -> Result<String> {
    let s = session.client.call(&Request::new(Method::Status)).await?.status.context("the server sent no status")?;
    let gb = |bytes: u64| bytes as f64 / 1e9;
    Ok(format!(
        "backend   cwbcode {} at {} over {}{}\nlogin     {}\nmodel     {} ({}){}\ncontext   {} tokens in memory (summarized from {})\nmemory    {:.1} GB in use, peak {:.1} GB\nsafety    {}\nproject   {}\nstore     {} · <project>/.cwbcode\nuptime    {}m\n",
        s.version,
        session.client.base(),
        s.protocol,
        match &session.client.fallback {
            Some(why) => format!(" (HTTP/3 not available: {why})"),
            None => String::new(),
        },
        match s.auth.as_str() {
            "open" => "none: local mode".to_string(),
            "wallet" => format!("wallet {}", s.wallet),
            _ => "the backend's token".to_string(),
        },
        s.model,
        s.model_state,
        if s.busy { format!(" · busy with {}{}", s.busy_project, if s.queued > 0 { format!(", {} waiting", s.queued) } else { String::new() }) } else { String::new() },
        s.context_tokens,
        s.context_limit,
        gb(s.memory_active),
        gb(s.memory_peak),
        s.sandbox,
        session.project,
        s.home,
        s.uptime_seconds / 60,
    ))
}

pub async fn sessions(session: &Session) -> Result<String> {
    let sessions = session.client.call(&session.request(Method::Sessions)).await?.sessions;
    if sessions.is_empty() {
        return Ok("No conversations in this project yet.\n".into());
    }
    Ok(sessions.iter().map(|s| format!("#{:<4} {}  {:>3} msgs  {}— {}\n", s.id, s.updated.get(..16).unwrap_or(&s.updated), s.messages, if s.name.is_empty() { String::new() } else { format!("[{}] ", s.name) }, s.first)).collect())
}

pub async fn find_session(session: &Session, key: &str) -> Result<i64> {
    let sessions = session.client.call(&session.request(Method::Sessions)).await?.sessions;
    let key = key.trim_start_matches('#');
    sessions.iter().find(|s| s.id.to_string() == key || (!s.name.is_empty() && s.name == key)).map(|s| s.id).with_context(|| format!("this project has no conversation {key:?} (see /sessions)"))
}

pub async fn tools(session: &Session, source_of: Option<&str>) -> Result<String> {
    let tools = session.client.call(&Request::new(Method::Tools)).await?.tools;
    if let Some(name) = source_of {
        return Ok(format!("{}\n", tools.iter().find(|t| t.name == name).with_context(|| format!("there is no tool {name}"))?.source));
    }
    if tools.is_empty() {
        return Ok("The agent has not made any tools yet. Ask it for something repetitive, or say \"make a tool that ...\".\n".into());
    }
    Ok(tools.iter().map(|t| format!("{}: {}\n    arguments: {}\n", t.name, t.description, t.parameters)).collect())
}

pub async fn docs(session: &Session) -> Result<String> {
    let docs = session.client.call(&session.request(Method::KbList)).await?.docs;
    if docs.is_empty() {
        return Ok("The knowledge base is empty. Save something with /remember <text> or /add <file>.\n".into());
    }
    let mut out = String::new();
    for doc in &docs {
        let preview: String = doc.text.chars().take(70).collect::<String>().replace('\n', " ");
        out.push_str(&format!("#{:<4} {:<5} {}  {}  — {preview}\n", doc.id, doc.kind, doc.created.get(..16).unwrap_or(&doc.created), doc.title));
    }
    out.push_str(&format!("{} entries\n", docs.len()));
    Ok(out)
}

pub async fn export(session: &Session, id: i64, path: &Path) -> Result<String> {
    let got = session.client.call(&session.request(Method::KbGet).id(id)).await?;
    let doc = got.docs.first().context("the server sent no entry")?;
    let (bytes, name) = if doc.has_file { (got.data, got.text.clone()) } else { (doc.text.clone().into_bytes(), doc.title.clone()) };
    // A folder as the target keeps the entry's own name.
    let target = if path.is_dir() { path.join(Path::new(&name).file_name().unwrap_or_default()) } else { path.to_path_buf() };
    if target.exists() {
        bail!("{} exists already; choose another path", target.display());
    }
    std::fs::write(&target, &bytes).with_context(|| format!("cannot write {}", target.display()))?;
    Ok(format!("Wrote #{id} \"{}\" to {} ({} bytes).", doc.title, target.display(), bytes.len()))
}

/// One turn: send the message, show what happens, answer approvals. `interrupts` is Ctrl-C
/// (None when nobody is at the keyboard: approvals are then answered with no).
async fn turn(session: &Session, state: &mut State, term: &mut Term, text: &str, mut interrupts: Option<&mut Signal>) -> Result<()> {
    let mut request = session.request(Method::Chat).session(state.session).text(text);
    request.mode = state.mode.clone();
    request.think = state.think.clone();
    request.files = std::mem::take(&mut state.images);
    let mut events = session.client.chat(&request).await?;
    let mut failed = None;
    loop {
        let event = match &mut interrupts {
            Some(signal) => {
                tokio::select! {
                    event = events.next() => event,
                    _ = signal.recv() => {
                        // The server stops the model; the rest of the turn still arrives.
                        let _ = session.client.call(&Request::new(Method::Interrupt)).await;
                        continue;
                    }
                }
            }
            None => events.next().await,
        };
        let Some(event) = event? else { break };
        match &event.what {
            What::Session(id) => state.session = *id,
            What::Approval { id, what, offer_always } => {
                let answer = match &mut interrupts {
                    Some(signal) => {
                        let typed = tokio::task::block_in_place(|| term.ask(what, *offer_always));
                        // Ctrl-C at the question is a no, and stops the turn.
                        if futures::FutureExt::now_or_never(signal.recv()).is_some() {
                            let _ = session.client.call(&Request::new(Method::Interrupt)).await;
                            "n".to_string()
                        } else {
                            typed
                        }
                    }
                    None => "nobody is at the keyboard to approve this (the client ran without --auto)".to_string(),
                };
                // The model may have stopped waiting (interrupted, or timed out): not an error here.
                let _ = session.client.call(&Request::new(Method::Approve).id(*id).text(&answer)).await;
            }
            What::Error(message) => {
                failed = Some(message.clone());
                term.show(&event.what);
            }
            What::Done => break,
            other => term.show(other),
        }
    }
    term.settle();
    match failed {
        // Shown already; for a one-shot run it is also the exit status.
        Some(_) if interrupts.is_none() => bail!("the turn ended with an error"),
        _ => Ok(()),
    }
}

/// Answer one prompt and exit (`-p`, or a piped prompt).
pub async fn once(session: &Session, state: &mut State, prompt: &str) -> Result<()> {
    let quiet = !state.verbose && !std::io::stderr().is_terminal();
    let mut term = Term::new(state.show_thinking, quiet);
    turn(session, state, &mut term, prompt, None).await
}

/// The end of a conversation, as a reminder of where it stood.
async fn show_tail(session: &Session, id: i64) -> Result<()> {
    let transcript = session.client.call(&session.request(Method::Transcript).session(id)).await?;
    let lines: Vec<(&str, &str)> = transcript
        .events
        .iter()
        .filter_map(|e| match &e.what {
            What::User(text) => Some(("you", text.as_str())),
            What::Text(text) => Some(("cwbcode", text.as_str())),
            _ => None,
        })
        .collect();
    for (who, text) in lines.iter().rev().take(2).rev() {
        let short: String = text.chars().take(300).collect();
        println!("\x1b[2m{who}: {}{}\x1b[0m", short.replace('\n', " "), if text.chars().count() > 300 { " ..." } else { "" });
    }
    Ok(())
}

enum Flow {
    Continue,
    Exit,
}

async fn command(session: &Session, state: &mut State, line: &str) -> Result<Flow> {
    let (name, rest) = line.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((line, ""));
    let need = |what: &str| -> Result<&str> { if rest.is_empty() { bail!("usage: {name} {what}") } else { Ok(rest) } };
    let in_conversation = |state: &State| -> Result<i64> { if state.session == 0 { bail!("this conversation has not started yet") } else { Ok(state.session) } };
    let number = |text: &str, usage: &str| -> Result<i64> { text.trim().trim_start_matches('#').parse().with_context(|| format!("usage: {usage}")) };
    let client = &session.client;
    match name {
        "/help" | "/?" => println!("{HELP}"),
        "/exit" | "/quit" | "/q" => return Ok(Flow::Exit),
        "/clear" | "/new" => {
            state.session = 0;
            state.images.clear();
            state.texts.clear();
            println!("New conversation.");
        }
        "/compact" => println!("{}", client.call(&session.request(Method::Compact).session(in_conversation(state)?)).await?.text),
        "/status" => {
            print!("{}", status(session).await?);
            println!("session   {}\nmode      {}{}", if state.session == 0 { "new".to_string() } else { format!("#{}", state.session) }, state.mode, if state.think.is_empty() { String::new() } else { format!(" · thinking {}", state.think) });
        }
        "/save" => {
            let id = in_conversation(state)?;
            let name = if rest.is_empty() { format!("session-{id}") } else { rest.to_string() };
            client.call(&session.request(Method::NameSession).session(id).name(&name)).await?;
            println!("This conversation is saved as \"{name}\" (#{id}). Continue it later with /resume {name} or cwbcodeclient --resume {name}.");
        }
        "/sessions" => print!("{}", sessions(session).await?),
        "/resume" => {
            state.session = find_session(session, need("<id|name>")?).await?;
            println!("Continuing conversation #{}.", state.session);
            show_tail(session, state.session).await?;
        }
        // Run the project's code (a command given, or one guessed from the folder).
        "/run" => println!("{}", client.call(&session.request(Method::Run).text(rest)).await?.text),
        "/undo" => println!("{}", client.call(&session.request(Method::Undo).session(in_conversation(state)?)).await?.text),
        "/changes" => {
            let changes = client.call(&session.request(Method::Changes).session(in_conversation(state)?)).await?.changes;
            if changes.is_empty() {
                println!("No file was read or changed in this conversation yet.");
            }
            for c in changes {
                let size = |n: i64| if n < 0 { "—".to_string() } else { format!("{n} B") };
                println!("#{:<4} {}  {:<6} {}  {} → {}{}", c.id, c.created.get(11..19).unwrap_or(""), c.action, c.path, size(c.bytes_before), size(c.bytes_after), if c.undone { "  (undone)" } else { "" });
            }
        }
        "/todos" => {
            let lines = todo_lines(&client.call(&session.request(Method::Transcript).session(in_conversation(state)?)).await?.text);
            println!("{}", if lines.is_empty() { "The model has no task list in this conversation.".to_string() } else { lines.join("\n") });
        }
        "/image" => {
            let image = read_upload(Path::new(&unquote(need("<file>")?)))?;
            println!("{} will be shown with your next message.", image.name);
            state.images.push(image);
        }
        "/file" => {
            let path = unquote(need("<file>")?);
            let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {path} as text"))?;
            println!("{path} ({} lines) will be included in your next message.", text.lines().count());
            state.texts.push(format!("File {path}:\n```\n{}\n```", text.chars().take(40_000).collect::<String>()));
        }
        "/remember" => {
            let docs = client.call(&session.request(Method::KbAdd).text(need("<text>")?)).await?.docs;
            println!("Saved as #{}.", docs.first().map_or(0, |d| d.id));
        }
        "/add" => {
            let rest = need("<file> [title]")?;
            // The whole rest is the path if such a file exists; otherwise the last words are the title.
            let (path, title) = if Path::new(&unquote(rest)).is_file() {
                (unquote(rest), "")
            } else {
                let (first, title) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                (unquote(first), title.trim())
            };
            let mut request = session.request(Method::KbAdd).name(title);
            request.files.push(read_upload(Path::new(&path))?);
            for doc in client.call(&request).await?.docs {
                println!("Saved {path} as #{} ({}).", doc.id, doc.kind);
            }
        }
        "/kb" => print!("{}", docs(session).await?),
        "/search" => println!("{}", format_hits(&client.call(&session.request(Method::KbSearch).text(need("<query>")?)).await?.hits, 300)),
        "/load" => {
            let got = client.call(&session.request(Method::KbGet).id(number(need("<id>")?, "/load <id>")?)).await?;
            let doc = got.docs.first().context("the server sent no entry")?;
            if doc.kind == "image" && doc.has_file {
                println!("The image #{} \"{}\" will be shown with your next message.", doc.id, doc.title);
                state.images.push(Upload { name: got.text.clone(), data: got.data });
            } else {
                println!("#{} \"{}\" ({} characters) will be included in your next message.", doc.id, doc.title, doc.text.chars().count());
                state.texts.push(format!("From the knowledge base, entry #{} \"{}\":\n```\n{}\n```", doc.id, doc.title, doc.text.chars().take(40_000).collect::<String>()));
            }
        }
        "/export" => {
            let (id, path) = need("<id> <path>")?.split_once(char::is_whitespace).context("usage: /export <id> <path>")?;
            println!("{}", export(session, number(id, "/export <id> <path>")?, Path::new(&unquote(path))).await?);
        }
        "/forget" => {
            let id = number(need("<id>")?, "/forget <id>")?;
            client.call(&session.request(Method::KbDelete).id(id)).await?;
            println!("Deleted #{id}.");
        }
        "/tools" => match rest.split_once(char::is_whitespace) {
            Some(("rm", tool)) => {
                client.call(&Request::new(Method::ToolDelete).name(tool.trim())).await?;
                println!("Deleted the tool {}.", tool.trim());
            }
            _ => print!("{}", tools(session, (!rest.is_empty()).then_some(rest)).await?),
        },
        "/mode" => {
            if !rest.is_empty() {
                if !["ask", "auto", "read-only", "plan"].contains(&rest) {
                    bail!("usage: /mode ask|auto|read-only|plan");
                }
                state.mode = rest.to_string();
            }
            println!("{} mode.", state.mode);
        }
        "/think" => {
            if !rest.is_empty() {
                if !["none", "minimal", "low", "medium", "high", "max"].contains(&rest) {
                    bail!("usage: /think none|minimal|low|medium|high|max");
                }
                state.think = rest.to_string();
            }
            println!("Thinking: {}.", if state.think.is_empty() { "the server's default" } else { &state.think });
        }
        _ => println!("Unknown command {name}. /help lists the commands."),
    }
    Ok(Flow::Continue)
}

pub async fn run(session: &Session, mut state: State, first: &str) -> Result<()> {
    let s = session.client.call(&Request::new(Method::Status)).await?.status.context("the server sent no status")?;
    // Registering the project also checks that the server may work in this folder.
    session.client.call(&Request::new(Method::OpenProject).text(&session.project)).await?;
    println!("\x1b[1mCC\x1b[0m · Causewaybay Code {} · {} ({}) · {}", s.version, s.model, s.model_state, s.protocol);
    println!("{} · {} mode · {}", session.project, state.mode, s.sandbox);
    println!("\x1b[2m/help for commands · Ctrl-C stops · Ctrl-D leaves\x1b[0m");
    if let Some(why) = &session.client.fallback {
        println!("\x1b[2mHTTP/3 is not available ({why}); using HTTP/2.\x1b[0m");
    }
    if state.session != 0 {
        println!("Continuing conversation #{}.", state.session);
        show_tail(session, state.session).await?;
    }
    let mut editor = rustyline::DefaultEditor::new()?;
    let history = std::env::var("HOME").map(|h| Path::new(&h).join(".cwbcode")).unwrap_or_default().join("client_history");
    let _ = editor.load_history(&history);
    // After the editor, which installs a handler of its own: Ctrl-C while the model works.
    let mut interrupts = signal(SignalKind::interrupt())?;
    let mut term = Term::new(state.show_thinking, false);
    let mut queued = (!first.is_empty()).then(|| first.to_string());

    loop {
        let line = match queued.take() {
            Some(line) => {
                println!("\x1b[1m›\x1b[0m {line}");
                line
            }
            None => {
                let mut text = String::new();
                loop {
                    let prompt = if !text.is_empty() { "\x1b[2m…\x1b[0m " } else if state.images.is_empty() && state.texts.is_empty() { "\n\x1b[1m›\x1b[0m " } else { "\n\x1b[1m+›\x1b[0m " };
                    match tokio::task::block_in_place(|| editor.readline(prompt)) {
                        Ok(line) => match line.strip_suffix('\\') {
                            Some(more) => {
                                text.push_str(more);
                                text.push('\n');
                            }
                            None => {
                                text.push_str(&line);
                                break;
                            }
                        },
                        Err(ReadlineError::Interrupted) => {
                            text.clear();
                            break;
                        }
                        Err(ReadlineError::Eof) => {
                            let _ = editor.save_history(&history);
                            return Ok(());
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                text
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _ = editor.add_history_entry(line);
        let _ = editor.save_history(&history);
        // A Ctrl-C from before this message does not stop it.
        while futures::FutureExt::now_or_never(interrupts.recv()).is_some() {}

        if let Some(shell) = line.strip_prefix('!') {
            if let Err(e) = std::process::Command::new("/bin/zsh").arg("-c").arg(shell).status() {
                term.error(&format!("{e}"));
            }
            continue;
        }
        let first_word = line.split_whitespace().next().unwrap_or("");
        if line.starts_with('/') && !line[1..].starts_with(|c: char| c == '/' || c.is_whitespace()) && !Path::new(first_word).exists() {
            match command(session, &mut state, line).await {
                Ok(Flow::Exit) => {
                    let _ = editor.save_history(&history);
                    return Ok(());
                }
                Ok(Flow::Continue) => {}
                Err(e) => term.error(&format!("{e:#}")),
            }
            continue;
        }
        let mut text = state.texts.join("\n\n");
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(line);
        state.texts.clear();
        println!();
        if let Err(e) = turn(session, &mut state, &mut term, &text, Some(&mut interrupts)).await {
            term.error(&format!("{e:#}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_and_dragged_paths_are_read() {
        assert_eq!(unquote("  \"/a b/c.png\" "), "/a b/c.png");
        assert_eq!(unquote("'/a b/c.png'"), "/a b/c.png");
        assert_eq!(unquote("/a\\ b/c.png"), "/a b/c.png");
        assert_eq!(unquote("plain.txt"), "plain.txt");
        assert!(unquote("~/x.txt").ends_with("/x.txt") && !unquote("~/x.txt").starts_with('~'));
    }

    #[test]
    fn hits_say_which_ranking_found_them() {
        let hit = |b, s| Hit { doc: 3, kind: "text".into(), title: "Deploy".into(), snippet: "We deploy on Thursdays.".into(), bm25_rank: b, semantic_rank: s, score: 0.03 };
        assert_eq!(format_hits(&[hit(1, 2)], 100), "[#3 text \"Deploy\" · bm25 #1, semantic #2]\nWe deploy on Thursdays.");
        assert!(format_hits(&[hit(0, 1)], 9).ends_with("· semantic #1]\nWe deploy ..."));
        assert_eq!(format_hits(&[], 10), "No matches in the knowledge base.");
    }
}
