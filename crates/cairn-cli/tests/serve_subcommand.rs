//! Integration test for `cairn serve`. Spawns the binary on an
//! ephemeral port, polls /health, then SIGTERM and asserts a clean
//! exit. Also covers the bearer-token auth gate on non-/health routes.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn_serve(extra_env: &[(&str, &str)]) -> (Child, String, String) {
    let bin = env!("CARGO_BIN_EXE_cairn");
    let mut cmd = Command::new(bin);
    cmd.args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn cairn serve");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let mut line1 = String::new();
    reader.read_line(&mut line1).expect("read addr line");
    let addr = line1
        .trim()
        .strip_prefix("cairn-desktop listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line: {line1:?}"))
        .to_owned();

    let mut line2 = String::new();
    reader.read_line(&mut line2).expect("read token line");
    let token = line2
        .trim()
        .strip_prefix("cairn-desktop token ")
        .unwrap_or_else(|| panic!("unexpected second line: {line2:?}"))
        .to_owned();
    (child, addr, token)
}

fn sigterm(pid: u32) {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("invoke kill(1)");
    assert!(status.success(), "kill -TERM {pid} failed: {status:?}");
}

#[test]
fn serve_binds_responds_and_shuts_down() {
    let (mut child, addr, _token) = spawn_serve(&[]);

    // Poll /health for up to 5 s. Health is unauthenticated.
    let url = format!("http://{addr}/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ok = false;
    while Instant::now() < deadline {
        if let Ok(resp) = ureq::get(&url).call()
            && resp.status() == 200
        {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ok, "GET {url} did not return 200 within 5s");

    sigterm(child.id());
    let status = child.wait().expect("wait child");
    assert!(status.success(), "child did not exit cleanly: {status:?}");
}

#[test]
fn serve_bearer_token_gates_api_routes() {
    let (mut child, addr, token) = spawn_serve(&[]);
    assert_ne!(token, "<none>", "default launch must auto-generate a token");

    // /health is always open.
    let health = ureq::get(&format!("http://{addr}/health"))
        .call()
        .expect("/health");
    assert_eq!(health.status(), 200);

    // /api/v1/vault without a token must reject.
    let no_token = ureq::get(&format!("http://{addr}/api/v1/vault")).call();
    match no_token {
        Err(ureq::Error::Status(401, _)) => {}
        other => panic!("expected 401 without token, got {other:?}"),
    }

    // With the correct token: 200.
    let ok = ureq::get(&format!("http://{addr}/api/v1/vault"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .expect("/api/v1/vault with token");
    assert_eq!(ok.status(), 200);

    // Wrong token: 401.
    let wrong = ureq::get(&format!("http://{addr}/api/v1/vault"))
        .set("Authorization", "Bearer wrong-token")
        .call();
    match wrong {
        Err(ureq::Error::Status(401, _)) => {}
        other => panic!("expected 401 with wrong token, got {other:?}"),
    }

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn serve_cors_preflight_for_authenticated_origin() {
    // OPTIONS preflight from a renderer origin must succeed and echo
    // Access-Control-Allow-Methods. Without allow_methods on the
    // restricted CORS layer, browsers would refuse the follow-up
    // authenticated GET/POST.
    let (mut child, addr, _token) = spawn_serve(&[]);

    let resp = ureq::request("OPTIONS", &format!("http://{addr}/api/v1/vault"))
        .set("Origin", "null")
        .set("Access-Control-Request-Method", "GET")
        .set("Access-Control-Request-Headers", "authorization,content-type")
        .call()
        .expect("OPTIONS preflight");
    assert!(
        (200..400).contains(&resp.status()),
        "preflight status {}",
        resp.status()
    );
    let allow_methods = resp
        .header("access-control-allow-methods")
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(
        allow_methods.contains("GET") && allow_methods.contains("POST"),
        "Access-Control-Allow-Methods missing GET/POST: {allow_methods:?}"
    );

    sigterm(child.id());
    let _ = child.wait();
}

#[test]
fn serve_token_disabled_when_env_is_empty() {
    let (mut child, addr, token) = spawn_serve(&[("CAIRN_DESKTOP_TOKEN", "")]);
    assert_eq!(token, "<none>", "empty env disables auth");

    // Without a token, /api/v1/vault must succeed.
    let resp = ureq::get(&format!("http://{addr}/api/v1/vault"))
        .call()
        .expect("/api/v1/vault open");
    assert_eq!(resp.status(), 200);

    sigterm(child.id());
    let _ = child.wait();
}
