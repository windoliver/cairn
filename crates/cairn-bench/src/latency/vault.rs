//! [`SeededVault`] — bootstrap a tempdir vault for latency benches.
//!
//! Uses subprocess invocations of the `cairn` CLI so this helper stays
//! fully within `cairn-bench`'s own dependency boundary (no `cairn-cli`
//! or `cairn-test-fixtures` runtime dep required).

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

/// A bootstrapped, seeded tempdir vault.
///
/// The vault is kept alive for the lifetime of this struct — dropping it
/// removes the tempdir.
pub struct SeededVault {
    /// The temporary directory backing the vault. Kept alive so the vault is
    /// not deleted until the bench iteration is complete.
    pub dir: TempDir,
    /// Absolute path to the vault root (same as `dir.path()`).
    pub path: PathBuf,
    /// Record id captured from the first `cairn ingest` call.
    first_record_id: String,
}

impl SeededVault {
    /// Bootstrap a new vault using the `cairn` binary at `cairn_bin`,
    /// seed identity + 20 small records, and return the ready helper.
    ///
    /// Pass `env!("CARGO_BIN_EXE_cairn")` from a bench or integration test
    /// target (Cargo sets this env var for bench/test targets only).
    ///
    /// # Errors
    ///
    /// Returns an error if the binary cannot be found, if bootstrap fails,
    /// or if the ingest response cannot be parsed.
    pub fn new(cairn_bin: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let cairn_bin = cairn_bin.into();
        let dir = tempfile::tempdir()?;
        let path = dir.path().to_path_buf();

        // 1. Bootstrap the vault layout.
        bootstrap_vault(&cairn_bin, &path)?;

        // 2. Ingest the identity seed and capture the first record id.
        let first_record_id = ingest_one(&cairn_bin, &path, "reference", "identity seed")?;

        // 3. Ingest 20 small records so search benches have data to hit.
        for i in 0..20_u32 {
            ingest_one(&cairn_bin, &path, "reference", &format!("Lorem ipsum {i}"))?;
        }

        Ok(Self {
            dir,
            path,
            first_record_id,
        })
    }

    /// The record id of the first record ingested into this vault.
    ///
    /// Use this as the target for `cairn retrieve <id>` bench iterations.
    #[must_use]
    pub fn first_record_id(&self) -> &str {
        &self.first_record_id
    }

    /// Write a minimal JSONL trace file containing a four-event turn
    /// (`UserPromptSubmit` → `PreToolUse` → `PostToolUse` → `Stop`) and return the
    /// path to the file.
    ///
    /// The payload files are also written under `sources/hook/` so
    /// `capture_trace` can resolve them.
    ///
    /// # Errors
    ///
    /// Returns an error if the file or directory cannot be created.
    pub fn write_trace_jsonl(&self, name: &str) -> anyhow::Result<PathBuf> {
        let sources_dir = self.path.join("sources").join("hook");
        std::fs::create_dir_all(&sources_dir)?;

        let inputs: &[(&str, &str, Option<&str>, &str, &str)] = &[
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "UserPromptSubmit",
                None,
                "2026-05-15T00:00:01Z",
                "bench prompt",
            ),
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "PreToolUse",
                Some("tool-call-1"),
                "2026-05-15T00:00:02Z",
                r#"{"tool":"Read","input":{"file_path":"README.md"}}"#,
            ),
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAC",
                "PostToolUse",
                Some("tool-call-1"),
                "2026-05-15T00:00:03Z",
                "tool returned contents",
            ),
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAD",
                "Stop",
                None,
                "2026-05-15T00:00:04Z",
                "session ended",
            ),
        ];

        let trace_path = self.path.join(format!("{name}.jsonl"));
        let mut events_out = String::new();

        for &(event_id, hook_name, tool_id, captured_at, body) in inputs {
            let file_name = format!("{event_id}.txt");
            std::fs::write(sources_dir.join(&file_name), body)?;
            let hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
            let event = serde_json::json!({
                "event_id": event_id,
                "sensor_id": "snr:local:hook:cc-session:v1",
                "capture_mode": "auto",
                "actor_chain": [{
                    "role": "author",
                    "identity": "snr:local:hook:cc-session:v1",
                    "at": captured_at
                }],
                "refs": {
                    "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "turn_id": "turn-1",
                    "tool_id": tool_id
                },
                "payload_hash": hash,
                "payload_ref": format!("sources/hook/{file_name}"),
                "captured_at": captured_at,
                "payload": {
                    "source_family": "hook",
                    "hook_name": hook_name
                },
                "source_family": "hook"
            });
            events_out.push_str(&serde_json::to_string(&event)?);
            events_out.push('\n');
        }

        std::fs::write(&trace_path, &events_out)?;
        Ok(trace_path)
    }

    /// Write a minimal JSONL trace file that uses a different session id
    /// (so a second `capture_trace` invocation creates a fresh turn group).
    ///
    /// # Errors
    ///
    /// Returns an error if the file or directory cannot be created.
    pub fn write_enqueue_trace_jsonl(&self) -> anyhow::Result<PathBuf> {
        let sources_dir = self.path.join("sources").join("hook");
        std::fs::create_dir_all(&sources_dir)?;

        // Use different ULIDs and session to avoid collision with the first trace.
        let inputs: &[(&str, &str, Option<&str>, &str, &str)] = &[
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAE",
                "UserPromptSubmit",
                None,
                "2026-05-15T01:00:01Z",
                "enqueue bench prompt",
            ),
            (
                "01ARZ3NDEKTSV4RRFFQ69G5FAF",
                "Stop",
                None,
                "2026-05-15T01:00:02Z",
                "enqueue session ended",
            ),
        ];

        let trace_path = self.path.join("bench_enqueue.jsonl");
        let mut events_out = String::new();

        for &(event_id, hook_name, tool_id, captured_at, body) in inputs {
            let file_name = format!("{event_id}.txt");
            std::fs::write(sources_dir.join(&file_name), body)?;
            let hash = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
            let event = serde_json::json!({
                "event_id": event_id,
                "sensor_id": "snr:local:hook:cc-session:v1",
                "capture_mode": "auto",
                "actor_chain": [{
                    "role": "author",
                    "identity": "snr:local:hook:cc-session:v1",
                    "at": captured_at
                }],
                "refs": {
                    "session_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
                    "turn_id": "turn-2",
                    "tool_id": tool_id
                },
                "payload_hash": hash,
                "payload_ref": format!("sources/hook/{file_name}"),
                "captured_at": captured_at,
                "payload": {
                    "source_family": "hook",
                    "hook_name": hook_name
                },
                "source_family": "hook"
            });
            events_out.push_str(&serde_json::to_string(&event)?);
            events_out.push('\n');
        }

        std::fs::write(&trace_path, &events_out)?;
        Ok(trace_path)
    }
}

// ── subprocess helpers ────────────────────────────────────────────────────────

/// Bootstrap the vault at `path` using `cairn bootstrap --vault-path`.
fn bootstrap_vault(cairn_bin: &Path, path: &Path) -> anyhow::Result<()> {
    let out = Command::new(cairn_bin)
        .args(["bootstrap", "--vault-path", &path.to_string_lossy()])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "cairn bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Ingest one record and return its `data.record_id` string.
fn ingest_one(cairn_bin: &Path, vault: &Path, kind: &str, body: &str) -> anyhow::Result<String> {
    let out = Command::new(cairn_bin)
        .current_dir(vault)
        .env_remove("CAIRN_VAULT")
        .env_remove("CAIRN_REGISTRY")
        .args(["ingest", "--kind", kind, "--body", body, "--json"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "cairn ingest failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let id = v["data"]["record_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("ingest response missing data.record_id: {v}"))?
        .to_owned();
    Ok(id)
}
