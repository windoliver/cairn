//! Pack conformance suite, surfaced via `cairn plugins verify --pack <id>`.
//!
//! Tier 1 — manifest schema validity (Pass A + Pass B + path presence).
//! Tier 2 — install round-trip (install into tempdir, compare to embed).
//! Tier 3 — snapshot test (delegated to `tests/claude_code_pack_install.rs`).

use serde::Serialize;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::tempdir;

use crate::packs::install::{PackInstallOpts, install_pack_from_source};
use crate::packs::manifest::{Harness, PackError, PackManifest};
use crate::packs::source::{EmbeddedPackSource, FsPackSource, PackSource};

const SMOKE_SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SMOKE_OUTPUT_BYTES: usize = 1024 * 1024;

/// Tier of a conformance case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Manifest schema validity.
    One,
    /// Install round-trip.
    Two,
    /// Snapshot integrity (delegated; see Tier-3 invocation note).
    Three,
}

/// Outcome of a single case.
#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    /// Stable static id (`snake_case`; safe to embed in JSON wire output).
    pub id: &'static str,
    /// Case label (human-readable).
    pub name: String,
    /// Tier.
    pub tier: Tier,
    /// `Ok` if passed; `Err` otherwise.
    pub status: Result<(), String>,
}

/// Run the pack-verify suite for the bundled `cairn-claude-code` pack.
///
/// Tier-3 is delegated to the `claude_code_pack_install` integration test
/// — running it here would require regenerating snapshots; the
/// conformance suite only checks that the pack's emitted bytes are
/// deterministic across two installs (the round-trip case).
#[must_use]
pub fn run_pack_conformance(pack_id: &str) -> Vec<CaseOutcome> {
    let mut out = Vec::new();
    if pack_id != "cairn-claude-code" {
        out.push(CaseOutcome {
            id: "pack_unknown",
            name: format!("pack `{pack_id}` is bundled"),
            tier: Tier::One,
            status: Err(format!("unknown bundled pack `{pack_id}`")),
        });
        return out;
    }

    let dir = match crate::packs::bundled_pack_for(Harness::ClaudeCode) {
        Some(dir) => dir,
        None => {
            out.push(CaseOutcome {
                id: "pack_bundled",
                name: "pack has bundled source".to_owned(),
                tier: Tier::One,
                status: Err(PackError::ManifestInvalid {
                    reason: "no bundled pack available for harness `ClaudeCode`".to_owned(),
                }
                .to_string()),
            });
            return out;
        }
    };
    let source = EmbeddedPackSource::new("cairn-claude-code", dir);
    out.extend(run_pack_source_conformance(&source));
    out
}

/// Run the pack-verify suite for an author-provided pack directory.
#[must_use]
pub fn run_pack_path_conformance(path: &Path) -> Vec<CaseOutcome> {
    let source = FsPackSource::new(path.to_path_buf());
    run_pack_source_conformance(&source)
}

/// Run source-backed pack conformance checks shared by bundled and external packs.
#[must_use]
pub fn run_pack_source_conformance(source: &dyn PackSource) -> Vec<CaseOutcome> {
    let mut out = Vec::new();
    let manifest_bytes = match source.read_file("pack.json") {
        Ok(bytes) => bytes,
        Err(e) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Err(format!("{e:#}")),
            });
            return out;
        }
    };

    let manifest = match serde_json::from_slice::<PackManifest>(&manifest_bytes) {
        Ok(m) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Ok(()),
            });
            m
        }
        Err(e) => {
            out.push(CaseOutcome {
                id: "pack_json_parses",
                name: "pack.json parses".to_owned(),
                tier: Tier::One,
                status: Err(format!("{e:#}")),
            });
            return out;
        }
    };

    out.push(CaseOutcome {
        id: "pack_pass_a",
        name: "Pass A structural validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_a().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_pass_b",
        name: "Pass B cross-reference validation".to_owned(),
        tier: Tier::One,
        status: manifest.validate_pass_b().map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_paths_present",
        name: "all referenced paths present".to_owned(),
        tier: Tier::One,
        status: manifest
            .assert_all_paths_present(source)
            .map_err(|e| format!("{e:#}")),
    });
    out.extend(run_harness_static_checks(&manifest, source));
    out.push(CaseOutcome {
        id: "pack_install_round_trip",
        name: "install round-trip is idempotent".to_owned(),
        tier: Tier::Two,
        status: run_install_round_trip(source).map_err(|e| format!("{e:#}")),
    });
    out.push(CaseOutcome {
        id: "pack_smoke_script",
        name: "optional scaffold smoke script".to_owned(),
        tier: Tier::Two,
        status: run_smoke_script(source).map_err(|e| format!("{e:#}")),
    });
    out
}

fn run_smoke_script(source: &dyn PackSource) -> Result<(), PackError> {
    if !source.has_file("tests/smoke.sh") {
        return Ok(());
    }

    let manifest_bytes = source.read_file("pack.json")?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;
    let tmp = tempdir().map_err(PackError::Io)?;
    let opts = PackInstallOpts {
        harness: manifest.harness,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    install_pack_from_source(source, &opts)?;

    let smoke_bytes = source.read_file("tests/smoke.sh")?;
    let smoke_path = tmp.path().join("smoke.sh");
    std::fs::write(&smoke_path, smoke_bytes).map_err(PackError::Io)?;
    let output = run_bash_smoke_script(tmp.path())?;
    if !output.status.success() {
        return Err(PackError::ManifestInvalid {
            reason: format_smoke_diagnostic(
                &format!("tests/smoke.sh exited with {}", output.status),
                render_capped_output(&output.stdout),
                render_capped_output(&output.stderr),
            ),
        });
    }
    Ok(())
}

struct SmokeOutput {
    status: std::process::ExitStatus,
    stdout: CappedOutput,
    stderr: CappedOutput,
}

struct CappedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

fn run_bash_smoke_script(project_dir: &Path) -> Result<SmokeOutput, PackError> {
    let mut cmd = Command::new("bash");
    cmd.arg("smoke.sh")
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().map_err(PackError::Io)?;
    #[cfg(unix)]
    let child_pgid = i32::try_from(child.id()).ok();
    #[cfg(not(unix))]
    let child_pgid = None;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "tests/smoke.sh missing stdout pipe".to_owned(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PackError::ManifestInvalid {
            reason: "tests/smoke.sh missing stderr pipe".to_owned(),
        })?;

    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let started = Instant::now();
    let deadline = started + SMOKE_SCRIPT_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(PackError::Io)? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_child_process_group(child_pgid);
            let _ = child.kill();
            let _ = child.wait();
            let stdout = join_capped_reader_until(stdout_reader, deadline)?;
            let stderr = join_capped_reader_until(stderr_reader, deadline)?;
            return Err(PackError::ManifestInvalid {
                reason: format_smoke_diagnostic(
                    &format!(
                        "tests/smoke.sh timed out after {}s",
                        SMOKE_SCRIPT_TIMEOUT.as_secs()
                    ),
                    render_capped_output(&stdout),
                    render_capped_output(&stderr),
                ),
            });
        }
        thread::sleep(Duration::from_millis(10));
    };
    kill_child_process_group(child_pgid);

    Ok(SmokeOutput {
        status,
        stdout: join_capped_reader_until(stdout_reader, deadline)?,
        stderr: join_capped_reader_until(stderr_reader, deadline)?,
    })
}

#[cfg(unix)]
fn kill_child_process_group(child_pgid: Option<i32>) {
    if let Some(pgid) = child_pgid {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pgid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn kill_child_process_group(_child_pgid: Option<i32>) {}

fn read_capped(mut reader: impl Read) -> std::io::Result<CappedOutput> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buf = [0; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let remaining = MAX_SMOKE_OUTPUT_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let copied = remaining.min(n);
        bytes.extend_from_slice(&buf[..copied]);
        if copied < n {
            truncated = true;
        }
    }
    Ok(CappedOutput { bytes, truncated })
}

fn join_capped_reader_until(
    handle: thread::JoinHandle<std::io::Result<CappedOutput>>,
    deadline: Instant,
) -> Result<CappedOutput, PackError> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return Err(PackError::ManifestInvalid {
                reason: format_smoke_diagnostic(
                    &format!(
                        "tests/smoke.sh timed out after {}s",
                        SMOKE_SCRIPT_TIMEOUT.as_secs()
                    ),
                    "",
                    "",
                ),
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle
        .join()
        .map_err(|_| PackError::ManifestInvalid {
            reason: "tests/smoke.sh output reader panicked".to_owned(),
        })?
        .map_err(PackError::Io)
}

fn render_capped_output(output: &CappedOutput) -> String {
    let mut text = String::from_utf8_lossy(&output.bytes).to_string();
    if output.truncated {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("[truncated after {MAX_SMOKE_OUTPUT_BYTES} bytes]"));
    }
    text
}

fn format_smoke_diagnostic(
    header: &str,
    stdout: impl std::fmt::Display,
    stderr: impl std::fmt::Display,
) -> String {
    format!("{header}\nstdout:\n{stdout}\nstderr:\n{stderr}")
}

fn run_install_round_trip(source: &dyn PackSource) -> Result<(), PackError> {
    let manifest_bytes = source.read_file("pack.json")?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)?;
    let tmp = tempdir().map_err(PackError::Io)?;
    let opts = PackInstallOpts {
        harness: manifest.harness,
        project_dir: tmp.path().to_path_buf(),
        force: false,
    };
    let first = install_pack_from_source(source, &opts)?;
    if first.files_created.is_empty() {
        return Err(PackError::ManifestInvalid {
            reason: "first install created no files".to_owned(),
        });
    }
    let second = install_pack_from_source(source, &opts)?;
    if !second.files_created.is_empty() || !second.files_merged.is_empty() {
        return Err(PackError::ManifestInvalid {
            reason: format!(
                "round-trip not idempotent: created={} merged={}",
                second.files_created.len(),
                second.files_merged.len()
            ),
        });
    }
    Ok(())
}

/// Run harness-specific static checks against source-backed pack content.
#[must_use]
pub fn run_harness_static_checks(
    manifest: &PackManifest,
    source: &dyn PackSource,
) -> Vec<CaseOutcome> {
    match manifest.harness {
        Harness::ClaudeCode => vec![CaseOutcome {
            id: "pack_subagent_frontmatter",
            name: "subagent frontmatter tools match manifest".to_owned(),
            tier: Tier::One,
            status: manifest
                .assert_subagent_frontmatter_matches_manifest(source)
                .map_err(|e| format!("{e:#}")),
        }],
        Harness::Codex => vec![CaseOutcome {
            id: "pack_codex_static_files",
            name: "Codex manual and hooks files are valid".to_owned(),
            tier: Tier::One,
            status: assert_manual_and_hook_json(manifest, source, "AGENTS.md")
                .map_err(|e| format!("{e:#}")),
        }],
        Harness::Gemini => vec![CaseOutcome {
            id: "pack_gemini_static_files",
            name: "Gemini manual and hooks files are valid".to_owned(),
            tier: Tier::One,
            status: assert_manual_and_hook_json(manifest, source, "GEMINI.md")
                .map_err(|e| format!("{e:#}")),
        }],
    }
}

fn assert_manual_and_hook_json(
    manifest: &PackManifest,
    source: &dyn PackSource,
    expected_manual: &str,
) -> Result<(), PackError> {
    if manifest.manual_fragment != expected_manual {
        return Err(PackError::ManifestInvalid {
            reason: format!(
                "manual_fragment `{}` must be `{expected_manual}` for {:?}",
                manifest.manual_fragment, manifest.harness
            ),
        });
    }

    let manual_bytes = source.read_file(expected_manual)?;
    let manual = std::str::from_utf8(&manual_bytes).map_err(|e| PackError::ManifestInvalid {
        reason: format!("{expected_manual} is not UTF-8: {e}"),
    })?;
    let begin = format!("<!-- BEGIN CAIRN PACK {} -->", manifest.pack_id);
    let end = format!("<!-- END CAIRN PACK {} -->", manifest.pack_id);
    if !manual.contains(&begin) || !manual.contains(&end) {
        return Err(PackError::ManifestInvalid {
            reason: format!("{expected_manual} must contain guarded block `{begin}` ... `{end}`"),
        });
    }

    let hooks_bytes = source.read_file("hooks/hooks.json")?;
    let hook_payload: serde_json::Value = serde_json::from_slice(&hooks_bytes)?;
    manifest.validate_hook_payload_events(&hook_payload, "hooks/hooks.json")?;
    Ok(())
}

/// Pack ids the verify suite knows how to run.
#[must_use]
pub fn bundled_pack_ids() -> Vec<&'static str> {
    vec!["cairn-claude-code"]
}

/// Render outcomes as a quick human summary.
#[must_use]
pub fn render_outcomes(outcomes: &[CaseOutcome]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for o in outcomes {
        let status = match &o.status {
            Ok(()) => "OK".to_owned(),
            Err(reason) => format!("FAIL — {reason}"),
        };
        let _ = writeln!(s, "{:?} {:30} {}", o.tier, o.name, status);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::source::FsPackSource;

    fn write_codex_pack(root: &Path) {
        std::fs::create_dir(root.join("agents")).expect("create agents dir");
        std::fs::create_dir(root.join("commands")).expect("create commands dir");
        std::fs::create_dir(root.join("hooks")).expect("create hooks dir");

        std::fs::write(
            root.join("pack.json"),
            r#"{
  "schema": "cairn-pack/v1",
  "pack_id": "sample-pack",
  "name": "sample-pack",
  "version": "1.0.0",
  "harness": "codex",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Sample Codex harness pack for verification.",
  "requires_capabilities": [],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["status"]
    }
  ],
  "commands": [
    {
      "id": "cairn-context",
      "path": "commands/cairn-context.md",
      "kind": "verb-direct",
      "verb": "status"
    }
  ],
  "hooks": {
    "SessionStart": {
      "command": "cairn status --json"
    }
  },
  "manual_fragment": "AGENTS.md"
}
"#,
        )
        .expect("write pack.json");
        std::fs::write(
            root.join("agents/context-loader.md"),
            "# Context Loader\n\nLoads Cairn context.\n",
        )
        .expect("write subagent");
        std::fs::write(
            root.join("commands/cairn-context.md"),
            "# Cairn Context\n\nRun Cairn context retrieval.\n",
        )
        .expect("write command");
        std::fs::write(
            root.join("AGENTS.md"),
            "<!-- BEGIN CAIRN PACK sample-pack -->\nSample pack instructions.\n<!-- END CAIRN PACK sample-pack -->\n",
        )
        .expect("write AGENTS.md");
        std::fs::write(
            root.join("hooks/hooks.json"),
            r#"{"hooks":{"SessionStart":[{"command":"cairn status --json"}]}}"#,
        )
        .expect("write hooks.json");
    }

    fn case<'a>(outcomes: &'a [CaseOutcome], id: &str) -> &'a CaseOutcome {
        outcomes
            .iter()
            .find(|outcome| outcome.id == id)
            .unwrap_or_else(|| panic!("missing case {id}: {outcomes:#?}"))
    }

    #[test]
    fn bundled_pack_passes_full_conformance() {
        let outcomes = run_pack_conformance("cairn-claude-code");
        for o in &outcomes {
            assert!(
                o.status.is_ok(),
                "case `{}` (tier {:?}) failed: {:?}",
                o.name,
                o.tier,
                o.status
            );
        }
    }

    #[test]
    fn unknown_pack_returns_single_fail() {
        let outcomes = run_pack_conformance("does-not-exist");
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].status.is_err());
    }

    #[test]
    fn missing_smoke_script_is_optional_success() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_codex_pack(tmp.path());
        let source = FsPackSource::new(tmp.path().to_path_buf());

        let outcomes = run_pack_source_conformance(&source);

        assert!(case(&outcomes, "pack_smoke_script").status.is_ok());
    }

    #[test]
    fn failing_smoke_script_reports_stdout_and_stderr() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_codex_pack(tmp.path());
        std::fs::create_dir(tmp.path().join("tests")).expect("create tests dir");
        std::fs::write(
            tmp.path().join("tests/smoke.sh"),
            "#!/usr/bin/env bash\nprintf smoke-stdout\nprintf smoke-stderr >&2\nexit 7\n",
        )
        .expect("write smoke");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        let outcomes = run_pack_source_conformance(&source);

        let message = case(&outcomes, "pack_smoke_script")
            .status
            .as_ref()
            .expect_err("smoke must fail");
        assert!(
            message.contains("stdout:\nsmoke-stdout\nstderr:\nsmoke-stderr"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn codex_hook_payload_rejects_unknown_event() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_codex_pack(tmp.path());
        std::fs::write(
            tmp.path().join("hooks/hooks.json"),
            r#"{"hooks":{"DefinitelyNotAHook":[{"command":"cairn status --json"}]}}"#,
        )
        .expect("write hooks");
        let source = FsPackSource::new(tmp.path().to_path_buf());

        let outcomes = run_pack_source_conformance(&source);

        let message = case(&outcomes, "pack_codex_static_files")
            .status
            .as_ref()
            .expect_err("unknown hook payload event must fail");
        assert!(
            message.contains("unknown hook event `DefinitelyNotAHook`"),
            "unexpected message: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn smoke_script_cleans_up_background_processes_that_hold_pipes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("smoke.sh"),
            "#!/usr/bin/env bash\nsleep 5 &\nexit 0\n",
        )
        .expect("write smoke");

        let started = Instant::now();
        let output = run_bash_smoke_script(tmp.path()).expect("run smoke");

        assert!(output.status.success());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "smoke runner waited for a background descendant holding stdout/stderr pipes"
        );
    }
}
