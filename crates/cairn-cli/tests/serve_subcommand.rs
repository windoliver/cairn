//! Integration test for `cairn serve`. Spawns the binary on an
//! ephemeral port, polls /health, then SIGTERM and asserts a clean
//! exit.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn serve_binds_responds_and_shuts_down() {
    let bin = env!("CARGO_BIN_EXE_cairn");
    let mut child = Command::new(bin)
        .args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn serve");

    // Parse the first stdout line for the bound address.
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).expect("read first line");

    let prefix = "cairn-desktop listening on http://";
    let addr = first_line
        .trim()
        .strip_prefix(prefix)
        .unwrap_or_else(|| {
            panic!("unexpected first line: {first_line:?}");
        });

    // Poll /health for up to 5 s.
    let url = format!("http://{addr}/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ok = false;
    while Instant::now() < deadline {
        if let Ok(resp) = ureq::get(&url).call() {
            if resp.status() == 200 {
                ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ok, "GET {url} did not return 200 within 5s");

    // Graceful shutdown via SIGTERM — use /bin/kill to avoid unsafe.
    let pid = child.id().to_string();
    let kill_status = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("invoke kill(1)");
    assert!(kill_status.success(), "kill -TERM {pid} failed: {kill_status:?}");

    let status = child.wait().expect("wait child");
    assert!(status.success(), "child did not exit cleanly: {status:?}");
}
