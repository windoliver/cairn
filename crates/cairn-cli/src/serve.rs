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

/// Generate a 32-byte (256-bit) URL-safe random token from the OS
/// CSPRNG (`rand_core::OsRng` → `getrandom`). Encoded as 43 base64url
/// characters (no padding). One token per `cairn serve` launch.
///
/// Using a real CSPRNG (not a seeded LCG) is load-bearing: the
/// previous time/pid-seeded generator collapsed to 64 possible
/// outputs because the LCG's low 6 bits depend only on the previous
/// low 6 bits, leaving 99.97% of the seed entropy unused.
fn generate_token() -> String {
    use rand_core::{OsRng, RngCore as _};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64url_no_pad(&bytes)
}

fn base64url_no_pad(input: &[u8]) -> String {
    const ALPHA: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b0 = input[i];
        let b1 = input[i + 1];
        let b2 = input[i + 2];
        out.push(ALPHA[(b0 >> 2) as usize] as char);
        out.push(ALPHA[((b0 & 0x03) << 4 | b1 >> 4) as usize] as char);
        out.push(ALPHA[((b1 & 0x0F) << 2 | b2 >> 6) as usize] as char);
        out.push(ALPHA[(b2 & 0x3F) as usize] as char);
        i += 3;
    }
    let remaining = input.len() - i;
    if remaining == 1 {
        let b0 = input[i];
        out.push(ALPHA[(b0 >> 2) as usize] as char);
        out.push(ALPHA[((b0 & 0x03) << 4) as usize] as char);
    } else if remaining == 2 {
        let b0 = input[i];
        let b1 = input[i + 1];
        out.push(ALPHA[(b0 >> 2) as usize] as char);
        out.push(ALPHA[((b0 & 0x03) << 4 | b1 >> 4) as usize] as char);
        out.push(ALPHA[((b1 & 0x0F) << 2) as usize] as char);
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
        () = ctrl_c => {}
        () = terminate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_token_high_entropy() {
        // 1000 tokens must all be distinct AND span the full alphabet.
        // The previous LCG implementation collapsed to 64 outputs;
        // this test would have caught that immediately.
        let mut seen = HashSet::new();
        let mut chars = HashSet::new();
        for _ in 0..1000 {
            let t = generate_token();
            assert_eq!(t.len(), 43, "expected base64url(32 bytes) = 43 chars");
            for c in t.chars() {
                chars.insert(c);
            }
            assert!(seen.insert(t), "duplicate token within 1000 samples");
        }
        // base64url alphabet is 64 chars; 1000 × 43 = 43k samples should
        // exercise at least 50 of them with overwhelming probability.
        assert!(
            chars.len() >= 50,
            "alphabet coverage suspiciously low: {} chars",
            chars.len()
        );
    }
}
