//! `cairn serve` — local HTTP server for the desktop GUI alpha.
//!
//! Listed in brief §13.3 as a management command (not a core verb).
//! Wraps `cairn-desktop`'s axum router with a fixture-backed
//! repository. Vault-binding into a real `.cairn/` directory is a
//! separate issue — until then, the server serves the alpha fixture
//! and the GUI displays a banner.

use std::net::SocketAddr;
use std::process::ExitCode;

use anyhow::Context;
use cairn_desktop::{fixture::DesktopFixture, repository::DesktopRepository, server::router};
use clap::ArgMatches;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};

/// Build the clap subcommand definition.
#[must_use]
pub fn subcommand() -> clap::Command {
    clap::Command::new("serve")
        .about("Run the desktop GUI backend (HTTP server on localhost).")
        .long_about(
            "Starts the cairn-desktop axum server on a localhost port. \
             Used as a sidecar by Cairn.app; can also be run standalone \
             for debugging. Port 0 binds an ephemeral port and prints \
             the bound address to stdout as the first line.",
        )
        .arg(
            clap::Arg::new("port")
                .long("port")
                .value_name("PORT")
                .value_parser(clap::value_parser!(u16))
                .default_value("4000")
                .help("TCP port (0 = ephemeral)"),
        )
        .arg(
            clap::Arg::new("host")
                .long("host")
                .value_name("HOST")
                .default_value("127.0.0.1")
                .help("Bind address"),
        )
        // NB: `--vault` is supplied by the top-level `cairn` command as a
        // global arg (see command.rs); we do not redeclare it here. clap
        // panics on first access if the same arg name appears with a
        // different `value_parser` at both levels.
}

/// Entry point. Returns an `ExitCode` so `main` can propagate.
#[must_use]
pub fn run(matches: &ArgMatches) -> ExitCode {
    let host: String = matches
        .get_one::<String>("host")
        .expect("invariant: --host has a default_value")
        .clone();
    let port: u16 = *matches
        .get_one::<u16>("port")
        .expect("invariant: --port has a default_value");
    // `--vault` is a global arg declared in command.rs as String; pick it
    // up here so the alpha can warn the user that fixture data is served
    // regardless of the vault path (real-vault binding is a follow-up).
    let vault: Option<String> = matches.get_one::<String>("vault").cloned();
    if let Some(ref v) = vault {
        eprintln!(
            "cairn serve: --vault {v} accepted but ignored (alpha serves \
             fixture data; vault binding lands in a follow-up issue)"
        );
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("cairn serve: failed to build tokio runtime: {err}");
            return ExitCode::from(69); // EX_UNAVAILABLE
        }
    };

    match runtime.block_on(serve(host, port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cairn serve: {err:#}");
            ExitCode::from(69)
        }
    }
}

/// Binds the listener, emits the sidecar discovery line, and runs until SIGTERM/SIGINT.
async fn serve(host: String, port: u16) -> anyhow::Result<()> {
    use std::io::Write as _;
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "warn,cairn_desktop=info,cairn_cli=info".to_string()),
        )
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let fixture = DesktopFixture::load_default().context("loading desktop alpha fixture")?;
    let app = router(DesktopRepository::from_fixture(fixture));
    let addr: SocketAddr = format!("{host}:{port}").parse().context("bind addr")?;
    let listener = TcpListener::bind(addr).await.map_err(|err| {
        anyhow::anyhow!(
            "bind {host}:{port} failed: {err}. \
             Is another Cairn instance already running, or another process \
             using this port? Try `lsof -iTCP:{port} -sTCP:LISTEN` to find it."
        )
    })?;
    let actual = listener.local_addr().context("local_addr")?;

    // Emit one tracing line so the log file is non-empty on a healthy boot
    // (operator-visible proof that the sidecar reached the serve loop).
    tracing::info!(addr = %actual, "cairn-desktop ready (alpha fixture)");

    // First line of stdout: the bound address. Electron sidecar parses
    // this. Match the existing dev-server output verbatim:
    println!("cairn-desktop listening on http://{actual}");
    std::io::stdout().flush().ok();

    let mut term = signal(SignalKind::terminate()).context("SIGTERM handler")?;
    let mut intr = signal(SignalKind::interrupt()).context("SIGINT handler")?;

    let shutdown = async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = intr.recv() => {}
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("axum serve")?;
    Ok(())
}
