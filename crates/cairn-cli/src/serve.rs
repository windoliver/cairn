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
use cairn_desktop::{
    fixture::DesktopFixture, repository::DesktopRepository, server::router_with_auth,
};
use clap::ArgMatches;
use tokio::net::TcpListener;

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
        .arg(
            clap::Arg::new("alpha-fixture")
                .long("alpha-fixture")
                .action(clap::ArgAction::SetTrue)
                .help(
                    "Acknowledge that the alpha serves canned fixture data \
                     regardless of --vault. Required when --vault is set, \
                     so callers cannot silently mislead users about which \
                     vault is open. Real-vault binding is a follow-up issue.",
                ),
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
    // `--vault` is a global arg declared in command.rs as String. The alpha
    // does NOT bind to it — fixture data is served regardless. Require an
    // explicit `--alpha-fixture` ack when `--vault` is passed so callers
    // cannot mislead end users about which vault is open. Real-vault
    // binding lands in a follow-up issue.
    let vault: Option<String> = matches.get_one::<String>("vault").cloned();
    let alpha_fixture = matches.get_flag("alpha-fixture");
    if let Some(ref v) = vault {
        if !alpha_fixture {
            eprintln!(
                "cairn serve: refusing to start. --vault {v} was supplied \
                 but the alpha serves canned fixture data regardless of the \
                 vault path. Re-run with --alpha-fixture to acknowledge \
                 that the renderer will NOT see the contents of this vault, \
                 or wait for the real-vault-binding follow-up issue."
            );
            return ExitCode::from(78); // EX_CONFIG
        }
        eprintln!(
            "cairn serve: --vault {v} accepted but ignored \
             (alpha fixture mode; --alpha-fixture acknowledged)"
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

    // Per-launch bearer token gates every non-/health API route. Token
    // is read from CAIRN_DESKTOP_TOKEN if set (so a parent process can
    // share its own value), otherwise generated fresh. Set to an empty
    // string to disable auth (dev/test only).
    let token = std::env::var("CAIRN_DESKTOP_TOKEN").unwrap_or_else(|_| generate_token());
    let auth_token = if token.is_empty() {
        None
    } else {
        Some(token.clone())
    };

    let fixture = DesktopFixture::load_default().context("loading desktop alpha fixture")?;
    let app = router_with_auth(DesktopRepository::from_fixture(fixture), auth_token);
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

    // First TWO lines of stdout:
    //   1. "cairn-desktop listening on http://HOST:PORT" — bound address
    //   2. "cairn-desktop token TOKEN" or "cairn-desktop token <none>"
    // The Electron sidecar parses both. Token never appears in logs
    // (which are stderr-only).
    println!("cairn-desktop listening on http://{actual}");
    if token.is_empty() {
        println!("cairn-desktop token <none>");
    } else {
        println!("cairn-desktop token {token}");
    }
    std::io::stdout().flush().ok();

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;
    Ok(())
}

/// Generate a 32-byte URL-safe random token for per-launch bearer auth.
fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // 192 bits of entropy: 12 u128 mixes of nanos + process id + addr
    // of a stack variable. Not cryptographic-grade but sufficient for a
    // loopback-bound, per-launch secret; the real defense is that the
    // token never leaves this machine and never reaches the log file.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let stack_var: u8 = 0;
    let addr = (&raw const stack_var) as u128;
    let mut state = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15_u128)
        .wrapping_add(pid)
        .wrapping_add(addr);
    let mut out = String::with_capacity(43);
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    for _ in 0..43 {
        state = state
            .wrapping_mul(0x2545_F491_4F6C_DD1D_u128)
            .wrapping_add(0xBF58_476D_1CE4_E5B9_u128);
        out.push(ALPHA[(state as usize) & 0x3F] as char);
    }
    out
}

/// Cross-platform shutdown signal. Resolves on Ctrl-C on every target;
/// on Unix it also resolves on SIGTERM (the signal launchd / Electron's
/// `kill()` send by default).
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("invariant: install ctrl_c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate())
            .expect("invariant: install SIGTERM handler");
        term.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
