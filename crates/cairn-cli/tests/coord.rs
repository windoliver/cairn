//! `cairn coord` extension surface tests.

use std::process::Command;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

#[test]
fn coord_next_is_not_registered_before_dispatch_exists() {
    let out = cairn_bin()
        .args(["coord", "next", "--json"])
        .output()
        .expect("spawn cairn");

    assert_eq!(
        out.status.code(),
        Some(64),
        "coord must not be parseable before dispatch exists; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"),
        "coord should be absent from the root CLI until dispatch exists; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn coord_signal_flags_are_not_parseable_before_dispatch_exists() {
    for args in [
        [
            "coord",
            "signal",
            "send",
            "--to",
            "agt:codex:worker:v1",
            "--kind",
            "info",
            "--payload-id",
            "01HQZK000000000000000PAY01",
            "--json",
        ]
        .as_slice(),
        [
            "coord",
            "signal",
            "recv",
            "--cursor",
            "sig:17",
            "--kind",
            "task_completed",
            "--json",
        ]
        .as_slice(),
    ] {
        let out = cairn_bin().args(args).output().expect("spawn cairn");
        assert_eq!(
            out.status.code(),
            Some(64),
            "coord command must not parse before dispatch exists; args={args:?}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("unrecognized subcommand"),
            "coord should be absent from the root CLI until dispatch exists; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
