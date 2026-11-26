//! `cwbcodeclient`: Causewaybay Code in the terminal. It talks to the `cwbcode` backend
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
#[command(name = "cwbcodeclient", version, about = "Causewaybay Code in the terminal: a client of the cwbcode backend, working on the current folder", subcommand_precedence_over_arg = true)]
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

    /// A token for the backend [default: none in local mode; otherwise the session saved by
    /// `cwbcodeclient login`, or this machine's ~/.cwbcode/token].
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
    /// Sign in to a backend that belongs to a wallet, with the wallet's recovery phrase. The
    /// phrase is typed here (or read from CWBCODE_PHRASE) and never leaves this machine: only
    /// a signature is sent. The session is saved for later runs.
    Login {
        /// Which account of the phrase (0: the first).
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
    /// End the saved session.
    Logout,
    /// Remote mode: show it, or turn it on (the backend then listens on every interface and
    /// the wallet's owner signs in) or off (this machine only, with its token). Takes effect at
    /// once and is remembered in ~/.cwbcode/settings.jsonl.
    Remote {
        #[command(subcommand)]
        action: Option<RemoteCmd>,
    },
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
enum RemoteCmd {
    /// Turn remote mode on for a wallet.
    On {
        /// The wallet's public address (0x and 40 hex digits). Without one, the address is
        /// worked out from the recovery phrase you type (or CWBCODE_PHRASE); the phrase
        /// itself goes nowhere.
        address: Option<String>,
        /// Which account of the phrase (0: the first).
        #[arg(long, default_value_t = 0)]
        index: u32,
    },
    /// Turn remote mode off: this machine only, with its token.
    Off,
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

/// Where the session for a backend is kept between runs.
fn session_file(cli: &Cli) -> PathBuf {
    let name: String = cli.server.trim_start_matches("https://").chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    home(cli).join("client").join(format!("{name}.session"))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?.write_all(text.as_bytes())?;
    Ok(())
}

/// Connect, without credentials yet, and ask the backend how it wants clients to sign in.
async fn reach(cli: &Cli) -> Result<(Client, cwbcode_proto::Status)> {
    let home = home(cli);
    let trust = if cli.insecure {
        Trust::Insecure
    } else {
        let path = cli.cert.clone().unwrap_or_else(|| home.join("cert.pem"));
        let pem = std::fs::read(&path).with_context(|| format!("no certificate at {}: is the backend running (make start)? For a backend elsewhere pass --cert with its cert.pem, or --insecure", path.display()))?;
        Trust::from_pem(&pem).with_context(|| format!("{} is not a certificate", path.display()))?
    };
    let client = Client::connect(&cli.server, "", trust, !cli.h2).await?;
    let hello = client.call(&Request::new(Method::Hello)).await?.status.context("the backend did not say how to sign in")?;
    Ok((client, hello))
}

async fn connect(cli: &Cli) -> Result<Session> {
    let (client, hello) = reach(cli).await?;
    let saved = || std::fs::read_to_string(session_file(cli)).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let own = || std::fs::read_to_string(home(cli).join("token")).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    match hello.auth.as_str() {
        "wallet" => {
            // A session from `login`; on the backend's own machine, its token works too.
            let mut candidates: Vec<String> = cli.token.clone().into_iter().chain(saved()).chain(own()).collect();
            candidates.dedup();
            let mut signed_in = false;
            for token in candidates {
                client.set_token(&token);
                if client.call(&Request::new(Method::Status)).await.is_ok() {
                    signed_in = true;
                    break;
                }
            }
            if !signed_in {
                bail!("this backend belongs to wallet {}: sign in first with `cwbcodeclient login`", hello.wallet);
            }
        }
        // Local mode, or a backend that asks for its token: this machine's token file.
        _ => {
            let token = cli.token.clone().or_else(own).with_context(|| format!("no token: pass --token (the backend prints it with `cwbcode url`), or run on its machine (read {})", home(cli).join("token").display()))?;
            client.set_token(&token);
        }
    }
    let project = match &cli.dir {
        Some(dir) if dir.starts_with('/') || dir.starts_with('~') => dir.clone(),
        Some(dir) => std::env::current_dir()?.join(dir).to_string_lossy().into_owned(),
        None => current_folder()?,
    };
    Ok(Session { client, project })
}

/// Sign in with the wallet's recovery phrase and save the session.
async fn login(cli: &Cli, index: u32) -> Result<()> {
    use cwbcode_proto::wallet;
    let (client, hello) = reach(cli).await?;
    if hello.auth != "wallet" {
        println!("{}", if hello.auth == "open" { "This backend is in local mode: there is no wallet to sign in with; the client uses this machine's token file." } else { "This backend signs in with its token, not a wallet (pass --token)." });
        return Ok(());
    }
    let phrase = match std::env::var("CWBCODE_PHRASE") {
        Ok(phrase) if !phrase.trim().is_empty() => phrase,
        _ => {
            eprintln!("This backend belongs to wallet {}.", hello.wallet);
            rpassword::prompt_password("Recovery phrase (not shown, not sent): ").context("cannot read the phrase from this terminal (set CWBCODE_PHRASE instead)")?
        }
    };
    let key = wallet::key_from_phrase(&phrase, index).map_err(|e| anyhow::anyhow!("{e}"))?;
    drop(phrase);
    let address = wallet::address(&key).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !wallet::same_address(&address, &hello.wallet) {
        bail!("that phrase is wallet {address} (account {index}); this backend belongs to {}. Another account of the same phrase? Try --index.", hello.wallet.to_lowercase());
    }
    let challenge = client.call(&Request::new(Method::Challenge)).await?.text;
    // Sign only what is a sign-in for this wallet at this server.
    if wallet::challenge_field(&challenge, "wallet").is_none_or(|w| !wallet::same_address(w, &address)) || wallet::challenge_field(&challenge, "server") != Some(hello.fingerprint.as_str()) || !challenge.starts_with("Causewaybay Code sign-in") {
        bail!("the backend sent something that is not a sign-in request for this wallet; nothing was signed");
    }
    let signature = wallet::sign(&key, &challenge).map_err(|e| anyhow::anyhow!("{e}"))?;
    let session = client.call(&Request::new(Method::Login).text(&challenge).name(&signature)).await?.text;
    write_private(&session_file(cli), &session)?;
    println!("Signed in as {address}. The session is saved in {}.", session_file(cli).display());
    Ok(())
}

async fn logout(cli: &Cli) -> Result<()> {
    let file = session_file(cli);
    if let Ok(token) = std::fs::read_to_string(&file) {
        let (client, _) = reach(cli).await?;
        client.set_token(token.trim());
        let _ = client.call(&Request::new(Method::Logout)).await;
        std::fs::remove_file(&file)?;
        println!("Signed out.");
    } else {
        println!("There is no saved session for {}.", cli.server);
    }
    Ok(())
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
        Cmd::Login { .. } | Cmd::Logout => unreachable!("handled before connecting"),
        Cmd::Remote { action } => {
            use cwbcode_proto::wallet;
            let value = match action {
                None => {
                    let hello = session.client.call(&Request::new(Method::Hello)).await?.status.context("the backend sent no status")?;
                    println!("{}", repl::remote_lines(&hello));
                    return Ok(());
                }
                Some(RemoteCmd::Off) => String::new(),
                Some(RemoteCmd::On { address: Some(address), .. }) => address.clone(),
                Some(RemoteCmd::On { address: None, index }) => {
                    let phrase = match std::env::var("CWBCODE_PHRASE") {
                        Ok(phrase) if !phrase.trim().is_empty() => phrase,
                        _ => rpassword::prompt_password("Recovery phrase of the wallet (not shown, not sent; only its address is used): ").context("cannot read the phrase from this terminal (give the address instead)")?,
                    };
                    let key = wallet::key_from_phrase(&phrase, *index).map_err(|e| anyhow::anyhow!("{e}"))?;
                    wallet::address(&key).map_err(|e| anyhow::anyhow!("{e}"))?
                }
            };
            let response = session.client.call(&Request::new(Method::Setup).text(&value)).await?;
            println!("{}", response.text);
            if let Some(status) = &response.status {
                println!("{}", repl::remote_lines(status));
            }
        }
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
    match &cli.command {
        Some(Cmd::Login { index }) => return login(&cli, *index).await,
        Some(Cmd::Logout) => return logout(&cli).await,
        _ => {}
    }
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
        assert!(matches!(parse(&["login"]).command, Some(Cmd::Login { index: 0 })));
        assert!(matches!(parse(&["--server", "https://mac.local:7443", "login", "--index", "2"]).command, Some(Cmd::Login { index: 2 })));
        assert!(matches!(parse(&["logout"]).command, Some(Cmd::Logout)));
        assert!(matches!(parse(&["remote"]).command, Some(Cmd::Remote { action: None })));
        assert!(matches!(parse(&["remote", "off"]).command, Some(Cmd::Remote { action: Some(RemoteCmd::Off) })));
        assert!(matches!(parse(&["remote", "on", "0xabc"]).command, Some(Cmd::Remote { action: Some(RemoteCmd::On { address: Some(_), index: 0 }) })));
        assert!(matches!(parse(&["remote", "on", "--index", "3"]).command, Some(Cmd::Remote { action: Some(RemoteCmd::On { address: None, index: 3 }) })));
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
