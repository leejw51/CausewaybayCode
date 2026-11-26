//! `cwbcode`: the backend of Causewaybay Code. It holds the model (Inkling 2 Mini on Apple
//! MLX), runs the agent and its tools in any project folder under the home folder, keeps
//! everything in SQLite (~/.cwbcode for itself, <project>/.cwbcode for each project), and
//! serves the terminal client (`cwbcodeclient`) and the web client over HTTP/3, or HTTP/2
//! where QUIC is not available, with a self-signed certificate.
//!
//!   cwbcode serve                  start the server: on 127.0.0.1:7443, or on 0.0.0.0:7443
//!                                  (reachable from other machines, with the token) when
//!                                  CWBCODE_WALLET is set in .env
//!   cwbcode serve --bind ADDR      listen there, whatever .env says
//!   cwbcode url                    where the web client is, and how to get in (in local
//!                                  mode a link with a one-time code for the token)

mod agent;
mod api;
mod auth;
mod control;
mod dyntool;
mod env;
mod inkling;
mod kb;
mod llm;
mod net;
mod projects;
mod prompt;
mod runner;
mod sandbox;
mod settings;
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
#[command(name = "cwbcode", version, about = "Causewaybay Code backend: a local AI coding agent (Inkling 2 Mini on Apple MLX) served over HTTP/3 and HTTP/2")]
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
    /// Print the web client's address, how to get in (in local mode a link with a one-time
    /// code), and the certificates' fingerprints. Writes no certificate.
    Url(Serve),
}

#[derive(Args, Clone)]
struct Serve {
    /// The address to listen on (TCP for HTTP/2, UDP for HTTP/3). By default: 0.0.0.0:7443
    /// (every interface, reachable from other machines) when CWBCODE_WALLET is set, in the
    /// environment or the .env file; 127.0.0.1:7443 (this machine only) when it is not.
    #[arg(long, env = "CWBCODE_BIND")]
    bind: Option<SocketAddr>,

    /// The port to listen on when --bind is not given.
    #[arg(long, default_value_t = DEFAULT_PORT, env = "CWBCODE_PORT")]
    port: u16,

    /// The .env file to read CWBCODE_WALLET from [default: .env in the current folder].
    #[arg(long, value_name = "FILE")]
    env_file: Option<PathBuf>,

    /// Take the backend's token from any address, not only from this machine, when there is
    /// no wallet. (Local mode asks for the token anyway, from this machine only.)
    #[arg(long)]
    require_token: bool,

    /// Do not offer HTTP/3: clients use HTTP/2.
    #[arg(long)]
    no_h3: bool,

    /// Also serve plain HTTP on 127.0.0.1 at this port, for the desktop app's window (a web
    /// view that will not take the backend's certificate). This machine only, whatever the
    /// mode; the token or a session is asked for as over HTTPS.
    #[arg(long, env = "CWBCODE_HTTP_PORT")]
    http_port: Option<u16>,

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

/// The wallet (if set, and where) and the address to listen on.
fn exposure(options: &Serve, home: &std::path::Path) -> Result<(Option<env::Wallet>, SocketAddr)> {
    let wallet = env::wallet(std::env::var(env::WALLET).ok(), options.env_file.as_deref(), home)?;
    let bind = env::bind(options.bind, &wallet, options.port);
    Ok((wallet, bind))
}

/// The lines that say who can reach the backend and why.
fn exposure_lines(options: &Serve, home: &std::path::Path, wallet: &Option<env::Wallet>, bind: SocketAddr) -> Vec<String> {
    let scope = if bind.ip().is_unspecified() { "every interface: reachable from other machines" } else if bind.ip().is_loopback() { "this machine only" } else { "that address only" };
    let why = if options.bind.is_some() { "as asked with --bind" } else if wallet.is_some() { "remote mode: a wallet is set" } else { "local mode: no wallet is set" };
    vec![
        match wallet {
            Some(w) if w.fixed() => format!("{:<9} {}  (CWBCODE_WALLET from {})", "wallet", w.address, w.source),
            Some(w) => format!("{:<9} {}  (set up; in {})", "wallet", w.address, home.join(settings::FILE).display()),
            None => format!("{:<9} none  (not set up, and no CWBCODE_WALLET in the environment or {})", "wallet", options.env_file.as_ref().map_or(".env".to_string(), |p| p.display().to_string())),
        },
        format!("{:<9} {bind}  ({scope}, {why})", "bind"),
        match control::mode(wallet, bind, options.require_token) {
            auth::Mode::Wallet(_) => format!("{:<9} required: sign in with the wallet's recovery phrase (only a signature leaves the browser; the phrase is not stored)", "login"),
            auth::Mode::Open => format!("{:<9} the backend's token, from this machine only: open the `open` link below (its one-time code gets the page the token), or the terminal client reads it itself", "login"),
            auth::Mode::Token => format!("{:<9} the backend's token", "login"),
        },
    ]
}

fn print_access(home: &std::path::Path, serve: &Serve) -> Result<()> {
    let (wallet, bind) = exposure(serve, home)?;
    // Only read: a backend may be running on these files, and only it changes its certificate,
    // when it starts. (`make start` runs this before and after starting it.)
    let shown = net::current(home)?;
    let token = net::token(home)?;
    for line in exposure_lines(serve, home, &wallet, bind) {
        println!("{line}");
    }
    for (what, address) in addresses(bind) {
        println!("{what:<9} {address}");
    }
    // The token matters where it is the way in; with a wallet it works from this machine only.
    match control::mode(&wallet, bind, serve.require_token) {
        auth::Mode::Token => println!("{:<9} {token}", "token"),
        auth::Mode::Wallet(_) => println!("{:<9} {} (for clients on this machine; others sign in with the wallet)", "token", home.join("token").display()),
        // Local mode: a link with a one-time code, which the page exchanges for the token and
        // then takes out of the address. The token itself is not put in the link: the
        // browser's history would keep it. (Printed here only, never into the log.)
        auth::Mode::Open => {
            if let Some((_, web)) = addresses(bind).first() {
                let code = auth::issue_code(home)?;
                println!("{:<9} {web}/#code={code}  (works once, within {} minutes)", "open", auth::CODE_SECONDS / 60);
            }
        }
    }
    let cert = home.join("cert.pem");
    match &shown.fingerprint {
        Some(fingerprint) if shown.reissue => println!("{:<9} {} (sha256 {fingerprint}; made again when the backend next starts)", "cert", cert.display()),
        Some(fingerprint) => println!("{:<9} {} (sha256 {fingerprint})", "cert", cert.display()),
        None => println!("{:<9} {} (made when the backend first starts)", "cert", cert.display()),
    }
    // Installed and trusted once on a phone, tablet or another computer, it makes every
    // certificate this backend makes trusted there: no warning, and the page installs as an app.
    let download = addresses(bind).into_iter().find(|(what, _)| *what == "lan" || *what == "tailscale").or_else(|| addresses(bind).into_iter().next()).map(|(_, web)| format!("{web}{}", api::CA_PATH)).unwrap_or_default();
    let ca = home.join("ca.pem");
    match &shown.ca_fingerprint {
        Some(fingerprint) if shown.replace_ca => println!("{:<9} {} (sha256 {fingerprint}): made before authorities were limited to this machine's names, so it is replaced when the backend next starts; trust the new one then, from {download}", "ca", ca.display()),
        Some(fingerprint) => println!("{:<9} {} (sha256 {fingerprint}): trust it once on each device, from {download}", "ca", ca.display()),
        None => println!("{:<9} {} (made when the backend first starts; then trust it once on each device, from {download})", "ca", ca.display()),
    }
    Ok(())
}

async fn serve(home: PathBuf, options: Serve) -> Result<()> {
    prompt::effort_level(&options.think).with_context(|| format!("--think {}: use none, minimal, low, medium, high or max", options.think))?;
    std::fs::create_dir_all(&home).with_context(|| format!("cannot create {}", home.display()))?;
    let home = home.canonicalize()?;
    let (wallet, bind) = exposure(&options, &home)?;
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
    let mut endpoint = if options.no_h3 { None } else { Some(net::bind_udp(bind, &identity)?) };
    let shared = worker::start(config, roots)?;
    let auth = Arc::new(auth::Auth::new(control::mode(&wallet, bind, options.require_token), token, identity.fingerprint.clone(), &home));
    let (rebind, mut moves) = tokio::sync::watch::channel(bind);
    let control = Arc::new(control::Control {
        home: home.clone(),
        asked: options.bind,
        port: options.port,
        require_token: options.require_token,
        fixed: wallet.as_ref().filter(|w| w.fixed()).map(|w| w.source.clone()),
        bind: std::sync::Mutex::new(bind),
        source: std::sync::Mutex::new(wallet.as_ref().map_or(String::new(), |w| w.source.clone())),
        rebind,
    });
    let app = api::App { shared: shared.clone(), auth, control, h3_port: endpoint.as_ref().map(|_| bind.port()), ca_pem: Arc::new(identity.ca_pem.clone()), ca_fingerprint: Arc::new(identity.ca_fingerprint.clone()) };
    let router = api::router(app.clone());
    let allowed: net::Allowed = Arc::new(move |headers, uri, peer| app.allowed(headers, uri, Some(peer)));

    eprintln!("cwbcode {} · {}", env!("CARGO_PKG_VERSION"), shared.sandbox_label());
    for line in exposure_lines(&options, &home, &wallet, bind) {
        eprintln!("cwbcode: {line}");
    }
    eprintln!("cwbcode: home {}", home.display());

    // The desktop app's window: plain HTTP, on this machine's loopback address only, and
    // never moved with remote mode. (Bound now, so a port in use is an error at startup.)
    if let Some(port) = options.http_port {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(address).await.with_context(|| format!("cannot listen on {address} (--http-port)"))?;
        eprintln!("cwbcode: listening on http://{address} (the desktop app; this machine only)");
        let router = router.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await {
                eprintln!("cwbcode: http://{address}: {e}");
            }
        });
    }

    // Serve at `bind` until told to stop, or to listen somewhere else (remote mode turned on
    // or off while running), and then serve there.
    let mut bind = bind;
    loop {
        let quic = match endpoint.take() {
            Some(endpoint) => Some(endpoint),
            None if options.no_h3 => None,
            None => Some(net::bind_udp(bind, &identity)?),
        };
        for (what, address) in addresses(bind) {
            eprintln!("cwbcode: listening on {address} ({what}; HTTP/2{})", if quic.is_some() { format!(", and HTTP/3 on UDP port {}", cwbcode_proto::quic_port(bind.port())) } else { String::new() });
        }
        if let Some(quic) = &quic {
            tokio::spawn(net::serve_udp(quic.clone(), router.clone(), allowed.clone(), api::UNSIGNED_BYTES));
        }
        let handle = axum_server::Handle::new();
        let tcp = net::serve_tcp(bind, &identity, router.clone(), handle.clone());
        tokio::pin!(tcp);
        tokio::select! {
            result = &mut tcp => return result,
            _ = shutdown() => {
                eprintln!("cwbcode: stopping");
                // Stop what the model or a command is doing, and wait for it to be gone: a
                // command runs in a process group of its own and would otherwise outlive us.
                let waited = std::time::Instant::now();
                while shared.busy.load(std::sync::atomic::Ordering::Relaxed) && waited.elapsed() < std::time::Duration::from_secs(8) {
                    agent::interrupt();
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                return Ok(());
            }
            _ = moves.changed() => {
                // Let the answer to the request that asked for this go out first.
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                handle.graceful_shutdown(Some(std::time::Duration::from_secs(3)));
                if let Some(quic) = quic {
                    quic.close(0u32.into(), b"listening elsewhere");
                    quic.wait_idle().await;
                }
                let _ = tcp.await;
                bind = *moves.borrow_and_update();
                eprintln!("cwbcode: remote mode {}: moving to {bind}", if bind.ip().is_loopback() { "off" } else { "on" });
            }
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
        Some(Cmd::Url(options)) => std::fs::create_dir_all(&home).map_err(anyhow::Error::from).and_then(|_| Ok(home.canonicalize()?)).and_then(|home| print_access(&home, &options)),
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
