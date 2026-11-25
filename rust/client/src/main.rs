//! `cwbcodeclient`: Causeway Bay Code in the terminal. It talks to the `cwbcode` backend
//! (HTTP/3, or HTTP/2 where QUIC is not available) and works on the current folder.
//!
//!   cwbcodeclient                          talk to the agent about this folder
//!   cwbcodeclient "fix the failing test"   start with a message
//!   cwbcodeclient -p "explain src/main.rs" answer once and exit (stdin is added to the prompt)
//!   cwbcodeclient -c                       continue this folder's last conversation
//!   cwbcodeclient kb add notes.md          the project's knowledge base (kb search, list, ...)
//!   cwbcodeclient status | projects | sessions | tools

mod repl;
mod term;

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use cwbcode_proto::{Method, Request, Upload};
use cwbcodeclient::net::{Client, Trust};

#[derive(Parser)]
#[command(name = "cwbcodeclient", version, about = "Causeway Bay Code in the terminal: a client of the cwbcode backend, working on the current folder", subcommand_precedence_over_arg = true)]
pub struct Cli {
    /// A first message. Without one, the client waits for you to type.
    prompt: Vec<String>,

    /// Answer the prompt (and stdin, if piped) once, print the answer and exit.
    #[arg(short, long)]
    print: bool,

    /// Continue the most recent conversation of this folder.
    #[arg(short = 'c', long = "continue")]
    resume_last: bool,

    /// Continue a conversation, by number or name (see `cwbcodeclient sessions`).
    #[arg(short, long, value_name = "ID|NAME")]
    resume: Option<String>,

    /// Show an image with the first message (repeatable).
    #[arg(short, long, value_name = "FILE")]
    image: Vec<PathBuf>,

    /// Allow edits in the project and sandboxed commands without asking.
    #[arg(long, conflicts_with = "read_only")]
    auto: bool,

    /// Read and search only: no edits, no commands.
    #[arg(long)]
    read_only: bool,

    /// How much the model thinks before it acts: none, minimal, low, medium, high, max.
    #[arg(long, value_name = "LEVEL")]
    think: Option<String>,

    /// Do not show the model's thinking.
    #[arg(long, global = true)]
    hide_thinking: bool,

    /// Show thinking and tool calls even when stderr is not a terminal.
    #[arg(short, long, global = true)]
    verbose: bool,

    /// The project folder, as the server knows it [default: the current folder].
    #[arg(short = 'C', long = "dir", value_name = "DIR", global = true)]
    dir: Option<String>,

    /// The backend's address.
    #[arg(long, default_value = "https://127.0.0.1:7443", env = "CWBCODE_SERVER", global = true)]
    server: String,

    /// The backend's token [default: read from ~/.cwbcode/token].
    #[arg(long, env = "CWBCODE_TOKEN", hide_env_values = true, global = true)]
    token: Option<String>,

    /// The backend's certificate, the only one trusted [default: ~/.cwbcode/cert.pem].
    #[arg(long, value_name = "PEM", env = "CWBCODE_CERT", global = true)]
    cert: Option<PathBuf>,

    /// Accept any certificate (for a backend on another machine whose certificate you have
    /// not copied): encrypted, but the server is not verified.
    #[arg(long, global = true)]
    insecure: bool,

    /// Use HTTP/2 without trying HTTP/3 first.
    #[arg(long, global = true)]
    h2: bool,

    /// Where the backend on this machine keeps its token and certificate [default: ~/.cwbcode].
    #[arg(long, value_name = "DIR", env = "CWBCODE_HOME", global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// The backend: model, memory, protocol in use.
    Status,
    /// The projects the backend has worked in.
    Projects,
    /// This project's conversations.
    Sessions,
    /// Take back the file changes of the last turn that made any (in the latest conversation,
    /// or the one given).
    Undo {
        #[arg(value_name = "ID|NAME")]
        session: Option<String>,
    },
    /// The project's knowledge base.
    Kb {
        #[command(subcommand)]
        action: KbCmd,
    },
    /// The tools the agent has made (--source NAME shows one's Rust source).
    Tools {
        #[arg(long, value_name = "NAME")]
        source: Option<String>,
        /// Delete a tool.
        #[arg(long, value_name = "NAME")]
        remove: Option<String>,
    },
}

#[derive(Subcommand)]
enum KbCmd {
    /// Save files (text, images, anything) or, with --text, a note.
    Add {
        files: Vec<PathBuf>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        title: Option<String>,
    },
    /// Hybrid search: BM25 and semantic together.
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 5)]
        limit: u32,
        /// Keywords only: answered at once, also while the model is busy.
        #[arg(long)]
        bm25: bool,
    },
    /// List the entries.
    List,
    /// Print an entry's text.
    Get { id: i64 },
    /// Write an entry's stored file (or its text) to a path.
    Export { id: i64, path: PathBuf },
    /// Delete an entry.
    Rm { id: i64 },
}

/// The connection and what every request needs.
pub struct Session {
    pub client: Client,
    /// The project folder, as sent to the server.
    pub project: String,
}

impl Session {
    pub fn request(&self, method: Method) -> Request {
        Request::new(method).project(&self.project)
    }
}

fn home(cli: &Cli) -> PathBuf {
    cli.home.clone().unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cwbcode"))
}

/// The current folder as the user reached it (through a link in the home folder, say), which
/// is how the server is asked for it.
fn current_folder() -> Result<String> {
    let real = std::env::current_dir()?;
    if let Ok(pwd) = std::env::var("PWD") {
        if Path::new(&pwd).canonicalize().ok().as_deref() == Some(real.canonicalize()?.as_path()) {
            return Ok(pwd);
        }
    }
    Ok(real.to_string_lossy().into_owned())
}

async fn connect(cli: &Cli) -> Result<Session> {
    let home = home(cli);
    let token = match &cli.token {
        Some(token) => token.clone(),
        None => std::fs::read_to_string(home.join("token")).map(|t| t.trim().to_string()).with_context(|| format!("no token: start the backend on this machine (make start), or pass --token (read {})", home.join("token").display()))?,
    };
    let trust = if cli.insecure {
        Trust::Insecure
    } else {
        let path = cli.cert.clone().unwrap_or_else(|| home.join("cert.pem"));
        let pem = std::fs::read(&path).with_context(|| format!("no certificate at {}: pass --cert with the backend's cert.pem, or --insecure", path.display()))?;
        Trust::from_pem(&pem).with_context(|| format!("{} is not a certificate", path.display()))?
    };
    let client = Client::connect(&cli.server, &token, trust, !cli.h2).await?;
    let project = match &cli.dir {
        Some(dir) if dir.starts_with('/') || dir.starts_with('~') => dir.clone(),
        Some(dir) => std::env::current_dir()?.join(dir).to_string_lossy().into_owned(),
        None => current_folder()?,
    };
    Ok(Session { client, project })
}

pub fn read_upload(path: &Path) -> Result<Upload> {
    let data = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    if data.len() > cwbcode_proto::MAX_UPLOAD_BYTES {
        bail!("{} is {} MB; files up to {} MB can be sent", path.display(), data.len() >> 20, cwbcode_proto::MAX_UPLOAD_BYTES >> 20);
    }
    Ok(Upload { name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into()), data })
}

async fn command(session: &Session, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Status => print!("{}", repl::status(session).await?),
        Cmd::Projects => {
            let projects = session.client.call(&Request::new(Method::Projects)).await?.projects;
            if projects.is_empty() {
                println!("The backend has not worked in any project yet.");
            }
            for p in projects {
                println!("{:<40} {:>3} conversations  {}", p.path, p.sessions, p.updated);
            }
        }
        Cmd::Sessions => print!("{}", repl::sessions(session).await?),
        Cmd::Undo { session: which } => {
            let id = match which {
                Some(key) => repl::find_session(session, key).await?,
                None => session.client.call(&session.request(Method::Sessions)).await?.sessions.first().map(|s| s.id).context("this project has no conversation yet")?,
            };
            println!("{}", session.client.call(&session.request(Method::Undo).session(id)).await?.text);
        }
        Cmd::Tools { source, remove } => {
            if let Some(name) = remove {
                session.client.call(&Request::new(Method::ToolDelete).name(name)).await?;
                println!("Deleted the tool {name}.");
            } else {
                print!("{}", repl::tools(session, source.as_deref()).await?);
            }
        }
        Cmd::Kb { action } => match action {
            KbCmd::Add { files, text, title } => {
                if text.is_none() && files.is_empty() {
                    bail!("give files to add, or --text \"...\"");
                }
                let mut request = session.request(Method::KbAdd).name(title.as_deref().unwrap_or("")).text(text.as_deref().unwrap_or(""));
                for file in files {
                    request.files.push(read_upload(file)?);
                }
                for doc in session.client.call(&request).await?.docs {
                    println!("#{} saved: {} ({})", doc.id, doc.title, doc.kind);
                }
            }
            KbCmd::Search { query, limit, bm25 } => {
                let mut request = session.request(Method::KbSearch).text(&query.join(" "));
                request.limit = *limit;
                request.flag = *bm25;
                println!("{}", repl::format_hits(&session.client.call(&request).await?.hits, 400));
            }
            KbCmd::List => print!("{}", repl::docs(session).await?),
            KbCmd::Get { id } => println!("{}", session.client.call(&session.request(Method::KbGet).id(*id)).await?.docs.first().map(|d| d.text.clone()).unwrap_or_default()),
            KbCmd::Export { id, path } => println!("{}", repl::export(session, *id, path).await?),
            KbCmd::Rm { id } => {
                session.client.call(&session.request(Method::KbDelete).id(*id)).await?;
                println!("Deleted #{id}.");
            }
        },
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<()> {
    let session = connect(&cli).await?;
    if let Some(cmd) = &cli.command {
        return command(&session, cmd).await;
    }
    if let Some(think) = &cli.think {
        if !["none", "minimal", "low", "medium", "high", "max"].contains(&think.as_str()) {
            bail!("--think {think}: use none, minimal, low, medium, high or max");
        }
    }
    // The prompt: the arguments, and what is piped in.
    let mut prompt = cli.prompt.join(" ");
    let piped = !std::io::stdin().is_terminal();
    if piped {
        // A pipe that stays open and silent would look like a hang: say what is awaited.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut input = String::new();
            let _ = tx.send(std::io::stdin().read_to_string(&mut input).map(|_| input));
        });
        let input = match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(read) => read?,
            Err(_) => {
                eprintln!("cwbcodeclient: reading the prompt from stdin (not a terminal) ... close it, or run with < /dev/null");
                rx.recv()??
            }
        };
        if !input.trim().is_empty() {
            prompt = if prompt.is_empty() { input } else { format!("{prompt}\n\n{input}") };
        }
    }
    let mut state = repl::State::new(&cli);
    if let Some(key) = &cli.resume {
        state.session = repl::find_session(&session, key).await?;
    } else if cli.resume_last {
        match session.client.call(&session.request(Method::Sessions)).await?.sessions.first() {
            Some(last) => state.session = last.id,
            None => eprintln!("No earlier conversation in this folder; starting a new one."),
        }
    }
    for path in &cli.image {
        state.images.push(read_upload(path)?);
    }
    if cli.print || piped {
        if prompt.trim().is_empty() {
            bail!("nothing to answer: give a prompt (cwbcodeclient -p \"...\") or pipe one in");
        }
        return repl::once(&session, &mut state, prompt.trim()).await;
    }
    repl::run(&session, state, prompt.trim()).await
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("starting the runtime");
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("\x1b[31mcwbcodeclient: {e:#}\x1b[0m");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("cwbcodeclient").chain(args.iter().copied())).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    #[test]
    fn a_subcommand_is_a_subcommand_wherever_the_options_are() {
        assert!(matches!(parse(&["sessions"]).command, Some(Cmd::Sessions)));
        // After a global option too: it must not be taken for a prompt.
        let cli = parse(&["-C", "/some/dir", "sessions"]);
        assert!(matches!(cli.command, Some(Cmd::Sessions)) && cli.prompt.is_empty() && cli.dir.as_deref() == Some("/some/dir"));
        assert!(matches!(parse(&["--h2", "status"]).command, Some(Cmd::Status)));
        assert!(matches!(parse(&["status", "--h2"]).command, Some(Cmd::Status)));
        let cli = parse(&["kb", "add", "a.md", "b.png", "--title", "Notes", "-C", "/p"]);
        assert!(matches!(&cli.command, Some(Cmd::Kb { action: KbCmd::Add { files, title, .. } }) if files.len() == 2 && title.as_deref() == Some("Notes")));
        assert_eq!(cli.dir.as_deref(), Some("/p"));
        assert!(matches!(parse(&["kb", "search", "how", "do", "we", "deploy", "-n", "3"]).command, Some(Cmd::Kb { action: KbCmd::Search { limit: 3, .. } })));
        assert!(matches!(parse(&["tools", "--source", "shout"]).command, Some(Cmd::Tools { source: Some(_), remove: None })));
        assert!(matches!(parse(&["undo"]).command, Some(Cmd::Undo { session: None })));
        assert!(matches!(parse(&["-C", "/p", "undo", "demo"]).command, Some(Cmd::Undo { session: Some(_) })));
    }

    #[test]
    fn anything_else_is_the_prompt() {
        let cli = parse(&["fix", "the", "failing", "test"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.prompt.join(" "), "fix the failing test");
        let cli = parse(&["--auto", "-p", "explain src/main.rs"]);
        assert!(cli.auto && cli.print && cli.prompt == ["explain src/main.rs"]);
        let cli = parse(&["-c", "-i", "a.png", "-i", "b.png", "what are these?"]);
        assert!(cli.resume_last && cli.image.len() == 2 && cli.prompt == ["what are these?"]);
        assert!(Cli::try_parse_from(["cwbcodeclient", "--auto", "--read-only"]).is_err());
        assert!(parse(&[]).command.is_none());
    }
}
