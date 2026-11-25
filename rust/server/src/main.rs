//! `cwbcode`: the backend of Causeway Bay Code. It holds the model (Inkling 2 Mini on Apple
//! MLX), runs the agent and its tools in any project folder under the home folder, keeps
//! everything in SQLite (~/.cwbcode for itself, <project>/.cwbcode for each project), and
//! serves the terminal client (`cwbcodeclient`) and the web client over HTTP/3, or HTTP/2
//! where QUIC is not available, with a self-signed certificate.
//!
//!   cwbcode serve                  start the server (127.0.0.1:7443)
//!   cwbcode serve --bind 0.0.0.0:7443   reachable from other machines (with the token)
//!   cwbcode url                    where the web client is, and the token to give it

mod agent;
mod api;
mod dyntool;
mod inkling;
mod kb;
mod llm;
mod net;
mod projects;
mod prompt;
mod sandbox;
mod store;
mod tools;
mod ui;
mod worker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

pub const DEFAULT_PORT: u16 = 7443;

#[derive(Parser)]
#[command(name = "cwbcode", version, about = "Causeway Bay Code backend: a local AI coding agent (Inkling 2 Mini on Apple MLX) served over HTTP/3 and HTTP/2")]
struct Cli {
    /// Where the server keeps its database, certificate, token and compiled tools [default: ~/.cwbcode].
    #[arg(long, value_name = "DIR", env = "CWBCODE_HOME", global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,

    #[command(flatten)]
    serve: Serve,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the server (the default).
    Serve(Serve),
    /// Print the token clients must present.
    Token,
    /// Print the web client's address, the token and the certificate's fingerprint.
    Url(Serve),
}

#[derive(Args, Clone)]
struct Serve {
    /// The address to listen on (TCP for HTTP/2, UDP for HTTP/3). 0.0.0.0 makes the server
    /// reachable from other machines.
    #[arg(long, default_value = "127.0.0.1:7443", env = "CWBCODE_BIND")]
    bind: SocketAddr,

    /// Do not offer HTTP/3: clients use HTTP/2.
    #[arg(long)]
    no_h3: bool,

    /// The model, by its name in Ollama's store.
    #[arg(long, default_value = llm::DEFAULT_MODEL, env = "CWBCODE_MODEL")]
    model: String,

    /// Start without loading a model: projects, conversations and keyword search work, chat does not.
    #[arg(long)]
    no_model: bool,

    /// Take the model's turns from a JSON script instead of a model (for tests).
    #[arg(long, value_name = "FILE")]
    script: Option<PathBuf>,

    /// Run commands without the macOS sandbox (each one then asks first, in every mode).
    #[arg(long)]
    no_sandbox: bool,

    /// Let sandboxed commands use the network (e.g. to fetch dependencies).
    #[arg(long)]
    allow_network: bool,

    /// How much the model thinks before it acts, unless a client asks otherwise.
    #[arg(long, default_value = "low", value_name = "LEVEL")]
    think: String,

    /// Sampling temperature (0 is greedy).
    #[arg(long, default_value_t = 0.3)]
    temp: f32,

    #[arg(long, default_value_t = 20)]
    top_k: i32,

    /// Most tokens the model writes in one step.
    #[arg(long, default_value_t = 8192)]
    max_tokens: usize,

    /// Most steps (model turns) for one message.
    #[arg(long, default_value_t = 60)]
    max_steps: usize,

    /// Tokens of context from which a conversation is summarized. Memory grows with it:
    /// about 27 GB at the peak for 24k tokens, 31 GB for 48k.
    #[arg(long, default_value_t = 24_000, value_name = "TOKENS")]
    context: usize,
}

/// The addresses to show for a bind address, each with what it is: the address itself, or,
/// when it is 0.0.0.0, this machine's on the local network and on Tailscale.
fn addresses(bind: SocketAddr) -> Vec<(&'static str, String)> {
    if !bind.ip().is_unspecified() {
        return vec![("web", format!("https://{bind}"))];
    }
    let port = bind.port();
    let mut out = vec![("web", format!("https://localhost:{port}"))];
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("192.0.2.1:9").is_ok() {
            if let Ok(local) = socket.local_addr() {
                out.push(("lan", format!("https://{}:{port}", local.ip())));
            }
        }
    }
    for host in net::tailscale() {
        out.push(("tailscale", format!("https://{host}:{port}")));
    }
    out.dedup_by(|a, b| a.1 == b.1);
    out
}

fn print_access(home: &std::path::Path, serve: &Serve) -> Result<()> {
    let identity = net::identity(home)?;
    let token = net::token(home)?;
    for (what, address) in addresses(serve.bind) {
        println!("{what:<9} {address}");
    }
    println!("{:<9} {token}", "token");
    println!("{:<9} {} (sha256 {})", "cert", home.join("cert.pem").display(), identity.fingerprint);
    Ok(())
}

async fn serve(home: PathBuf, options: Serve) -> Result<()> {
    prompt::effort_level(&options.think).with_context(|| format!("--think {}: use none, minimal, low, medium, high or max", options.think))?;
    std::fs::create_dir_all(&home).with_context(|| format!("cannot create {}", home.display()))?;
    let home = home.canonicalize()?;
    let identity = net::identity(&home)?;
    let token = net::token(&home)?;
    let roots = projects::Roots::from_env(&home)?;
    let config = worker::Config {
        home: home.clone(),
        model: options.model.clone(),
        temp: options.temp,
        top_k: options.top_k,
        think: options.think.clone(),
        max_tokens: options.max_tokens,
        max_steps: options.max_steps,
        context_limit: options.context,
        sandbox: !options.no_sandbox && std::path::Path::new("/usr/bin/sandbox-exec").exists(),
        network: options.allow_network,
        script: options.script.clone(),
        no_model: options.no_model,
        h3: !options.no_h3,
    };

    // Bind both ports before anything slow, so that a port in use fails at once.
    let endpoint = if options.no_h3 { None } else { Some(net::bind_udp(options.bind, &identity)?) };
    let shared = worker::start(config, roots)?;
    let app = api::App { shared: shared.clone(), token: Arc::new(token), h3_port: endpoint.as_ref().map(|_| options.bind.port()) };
    let router = api::router(app);

    eprintln!("cwbcode {} · {}", env!("CARGO_PKG_VERSION"), shared.sandbox_label());
    for (what, address) in addresses(options.bind) {
        eprintln!("cwbcode: listening on {address} ({what}; HTTP/2{})", if endpoint.is_some() { " and HTTP/3" } else { "" });
    }
    eprintln!("cwbcode: `cwbcode url` prints the token; home {}", home.display());
    if options.bind.ip().is_unspecified() {
        eprintln!("cwbcode: reachable from other machines. Anyone with the token can run commands here as you: keep it private.");
    }

    if let Some(endpoint) = endpoint {
        tokio::spawn(net::serve_udp(endpoint, router.clone()));
    }
    tokio::select! {
        result = net::serve_tcp(options.bind, &identity, router) => result,
        _ = shutdown() => {
            eprintln!("cwbcode: stopping");
            // Stop what the model or a command is doing, and wait for it to be gone: a
            // command runs in a process group of its own and would otherwise outlive us.
            let waited = std::time::Instant::now();
            while shared.busy.load(std::sync::atomic::Ordering::Relaxed) && waited.elapsed() < std::time::Duration::from_secs(8) {
                agent::interrupt();
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Ok(())
        }
    }
}

/// Resolves on Ctrl-C or a terminate signal (what a supervisor sends).
async fn shutdown() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("installing the signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

fn main() {
    let cli = Cli::parse();
    let home = cli.home.clone().unwrap_or_else(store::default_home);
    let result = match cli.command {
        Some(Cmd::Token) => std::fs::create_dir_all(&home).map_err(Into::into).and_then(|_| net::token(&home)).map(|t| println!("{t}")),
        Some(Cmd::Url(options)) => std::fs::create_dir_all(&home).map_err(Into::into).and_then(|_| print_access(&home, &options)),
        Some(Cmd::Serve(options)) => run(home, options),
        None => run(home, cli.serve),
    };
    if let Err(e) = result {
        eprintln!("cwbcode: {e:#}");
        std::process::exit(1);
    }
}

fn run(home: PathBuf, options: Serve) -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(serve(home, options))
}
