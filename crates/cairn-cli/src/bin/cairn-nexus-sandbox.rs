//! Minimal bundled Nexus sandbox sidecar.
//!
//! This binary owns only the lifecycle health contract used by `cairn` in the
//! v0.2 sandbox profile. It does not implement Nexus search, indexing, or
//! federation protocols.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use cairn_cli::nexus::HttpEndpoint;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8765";
const DEFAULT_HEALTH_PATH: &str = "/health";

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if wants_help(&args) {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("cairn-nexus-sandbox {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args != ["sandbox", "serve"] {
        eprintln!("usage: cairn-nexus-sandbox sandbox serve");
        return ExitCode::from(64);
    }

    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("cairn-nexus-sandbox: {err}");
            ExitCode::from(69)
        }
    }
}

fn wants_help(args: &[String]) -> bool {
    args.is_empty()
        || args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
}

fn print_help() {
    println!(
        "cairn-nexus-sandbox {}\n\nUSAGE:\n    cairn-nexus-sandbox sandbox serve\n\nENV:\n    CAIRN_NEXUS_ENDPOINT      endpoint to bind, default {DEFAULT_ENDPOINT}\n    CAIRN_NEXUS_HEALTH_PATH   health path, default {DEFAULT_HEALTH_PATH}\n    CAIRN_VAULT_DIR           vault root passed by cairn\n    CAIRN_NEXUS_DATA_DIR      derived projection directory passed by cairn\n    CAIRN_SQLITE_DB           authoritative SQLite DB path passed by cairn",
        env!("CARGO_PKG_VERSION")
    );
}

fn serve() -> Result<(), String> {
    let endpoint_raw =
        std::env::var("CAIRN_NEXUS_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_owned());
    let health_path =
        std::env::var("CAIRN_NEXUS_HEALTH_PATH").unwrap_or_else(|_| DEFAULT_HEALTH_PATH.to_owned());
    validate_health_path(&health_path)?;
    let endpoint = HttpEndpoint::parse(&endpoint_raw)?;
    let listener = TcpListener::bind((endpoint.host.as_str(), endpoint.port))
        .map_err(|err| format!("binding {}:{}: {err}", endpoint.host, endpoint.port))?;

    for stream in listener.incoming() {
        let stream = stream.map_err(|err| format!("accepting connection: {err}"))?;
        handle_connection(stream, &health_path);
    }
    Ok(())
}

fn validate_health_path(health_path: &str) -> Result<(), String> {
    if !health_path.starts_with('/') {
        return Err("CAIRN_NEXUS_HEALTH_PATH must start with /".to_owned());
    }
    if health_path
        .bytes()
        .any(|byte| byte == b' ' || byte.is_ascii_control())
    {
        return Err("CAIRN_NEXUS_HEALTH_PATH must not contain spaces or controls".to_owned());
    }
    Ok(())
}

fn handle_connection(mut stream: TcpStream, health_path: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0_u8; 1024];
    let Ok(read) = stream.read(&mut buf) else {
        return;
    };
    let request = String::from_utf8_lossy(&buf[..read]);
    let expected_prefix = format!("GET {health_path} HTTP/");
    let response = if request.starts_with(&expected_prefix) {
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
    } else {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
    };
    let _ = stream.write_all(response);
}
