#![allow(missing_docs)]

use cairn_core::pipeline::skillify::SkillifyGateStatus;
use cairn_workflows::skillify::materialize::{AuthoredSkillBundle, materialize_bundle};
use cairn_workflows::skillify::packer::{SkillPackBuilder, unpack_archive};
use serde_json::json;
use tempfile::TempDir;

fn authored(slug: &str) -> AuthoredSkillBundle {
    AuthoredSkillBundle {
        lane: format!("test.{slug}"),
        slug: slug.to_owned(),
        skill_markdown: format!(
            "---\nname: {slug}\nlane: test.{slug}\ntriggers:\n  - {slug}\nuses: scripts/{slug}.sh\nfiles_to: wiki/test/\n---\nSkill."
        ),
        script: format!("#!/usr/bin/env bash\necho {slug}\n"),
        unit_tests: json!({"cases": []}),
        integration_tests: json!({"cases": []}),
        llm_evals: json!({"rubric": []}),
        resolver_triggers: json!([slug]),
        resolver_eval: json!({"intents": []}),
        smoke: json!({"cases": []}),
        filing_rules: json!({"files_to": "wiki/test/"}),
    }
}

fn setup_candidate(temp: &TempDir, candidate_id: &str, slug: &str) {
    let a = authored(slug);
    materialize_bundle(
        temp.path(),
        candidate_id,
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();

    // Write a passing gate report so the packer accepts it.
    let root = temp
        .path()
        .join(".cairn/evolution/skillify")
        .join(candidate_id);
    let report = cairn_core::pipeline::skillify::SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: cairn_core::pipeline::skillify::SkillArtifactKind::required()
            .iter()
            .map(|kind| cairn_core::pipeline::skillify::SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Passed,
                message: None,
            })
            .collect(),
    };
    std::fs::write(
        root.join("gate-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();

    // Round 7: pack-build now requires a parseable skill-spec.draft.json
    // whose lane/slug match the bundle. Write one for each setup candidate.
    let spec = cairn_core::pipeline::skillify::SkillSpecDraft {
        lane: format!("test.{slug}"),
        slug: slug.to_owned(),
        decision_tree: serde_json::json!({"root": "x"}),
        triggers: vec![slug.to_owned()],
        success_criteria: vec!["passes".to_owned()],
        source_refs: vec!["01HQZX9F5N0000000000000001".to_owned()],
        requires: vec![],
        provides: vec![format!("test.{slug}")],
    };
    std::fs::write(
        root.join("skill-spec.draft.json"),
        serde_json::to_vec_pretty(&spec).unwrap(),
    )
    .unwrap();
}

#[test]
fn pack_and_unpack_round_trip() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_alpha", "alpha");
    setup_candidate(&temp, "skc_beta", "beta");

    let archive = SkillPackBuilder::new("test-pack", "0.1.0", ">=0.1.0", "Test pack")
        .add_candidate("skc_alpha")
        .add_candidate("skc_beta")
        .build(temp.path())
        .unwrap();

    assert!(archive.archive_path.exists());
    assert_eq!(archive.manifest.skills.len(), 2);
    assert!(archive.manifest.pack_id.starts_with("skp_"));

    // Unpack into a fresh vault.
    let install_temp = TempDir::new().unwrap();
    unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap();

    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_alpha/manifest.json")
            .exists()
    );
    assert!(
        install_temp
            .path()
            .join(".cairn/evolution/skillify/skc_beta/manifest.json")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn pack_preserves_script_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_exec", "exec");

    // Mark the source script executable.
    let src_script = temp
        .path()
        .join(".cairn/evolution/skillify/skc_exec/bundle/scripts/exec.sh");
    std::fs::set_permissions(&src_script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let archive = SkillPackBuilder::new("exec-pack", "0.1.0", ">=0.1.0", "Exec pack")
        .add_candidate("skc_exec")
        .build(temp.path())
        .unwrap();

    let install_temp = TempDir::new().unwrap();
    unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap();

    let installed_script = install_temp
        .path()
        .join(".cairn/evolution/skillify/skc_exec/bundle/scripts/exec.sh");
    let mode = std::fs::metadata(&installed_script)
        .unwrap()
        .permissions()
        .mode();
    assert!(
        mode & 0o100 != 0,
        "installed script should retain owner-execute bit, got mode {mode:o}"
    );
}

#[test]
fn pack_rejects_candidate_with_failing_gates() {
    let temp = TempDir::new().unwrap();
    let a = authored("gamma");
    materialize_bundle(
        temp.path(),
        "skc_gamma",
        &a,
        &["01HQZX9F5N0000000000000001".to_owned()],
    )
    .unwrap();
    // Gate report is blocked (default from materialize_bundle) → packer should reject.

    let err = SkillPackBuilder::new("fail-pack", "0.1.0", ">=0.1.0", "Fail")
        .add_candidate("skc_gamma")
        .build(temp.path())
        .unwrap_err();

    assert!(err.to_string().contains("gate"));
}

#[test]
fn unpack_rejects_incompatible_version() {
    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_delta", "delta");

    let archive = SkillPackBuilder::new("version-pack", "0.1.0", ">=99.0.0", "Future pack")
        .add_candidate("skc_delta")
        .build(temp.path())
        .unwrap();

    let install_temp = TempDir::new().unwrap();
    let err = unpack_archive(&archive.archive_path, install_temp.path(), "0.1.0").unwrap_err();
    assert!(err.to_string().contains("Cairn"));
}

// NOTE: path-traversal entries (`../escape`) cannot be constructed via
// `tar::Builder::append_data` — the library rejects them at write time. The
// runtime check in `extract_archive_safely` still rejects them on the read
// side (defense-in-depth against hand-crafted tar bytes); the symlink test
// below exercises the same code path through a different unsafe entry type.

#[cfg(unix)]
#[test]
fn unpack_rejects_archive_with_symlink_entry() {
    use std::io::Write as _;
    let temp = TempDir::new().unwrap();
    let archive_path = temp.path().join("symlink.cairnpack");
    let file = std::fs::File::create(&archive_path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);

    let manifest_json = b"{\"pack_id\":\"skp_x\",\"name\":\"x\",\"version\":\"0.1.0\",\"cairn_compat\":\">=0.1.0\",\"description\":\"x\",\"skills\":[],\"requires\":[],\"provides\":[],\"content_sha256\":\"\"}";
    let mut h = tar::Header::new_gnu();
    h.set_size(manifest_json.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, "manifest.json", &manifest_json[..])
        .unwrap();

    // Hostile entry: symlink.
    let mut h2 = tar::Header::new_gnu();
    h2.set_entry_type(tar::EntryType::Symlink);
    h2.set_size(0);
    h2.set_link_name("/etc/passwd").unwrap();
    h2.set_mode(0o644);
    h2.set_cksum();
    tar.append_data(&mut h2, "evil-link", std::io::empty())
        .unwrap();

    let inner = tar.into_inner().unwrap();
    inner.finish().unwrap().flush().unwrap();

    let install = TempDir::new().unwrap();
    let err = unpack_archive(&archive_path, install.path(), "0.1.0").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("regular file") || msg.contains("Symlink") || msg.contains("integrity"),
        "expected symlink rejection, got: {msg}"
    );
}

#[cfg(unix)]
#[test]
fn install_lock_is_held_through_transaction_lifetime() {
    // Round-18 regression: the install lock must be held until the
    // InstallTransaction is consumed (commit/rollback), not just until
    // unpack_archive_staged returns. Otherwise a second install can
    // sneak in between staged swap and rollback.
    use cairn_workflows::skillify::packer::unpack_archive_staged;
    use std::sync::{Arc, Barrier};

    let temp = TempDir::new().unwrap();
    setup_candidate(&temp, "skc_lock_test", "lock-test");
    let archive = SkillPackBuilder::new("lock-pack", "0.1.0", ">=0.1.0", "lock test")
        .add_candidate("skc_lock_test")
        .build(temp.path())
        .unwrap();

    // First install: stage but don't commit yet (hold the transaction).
    let install_temp = TempDir::new().unwrap();
    let (manifest1, transaction1) =
        unpack_archive_staged(&archive.archive_path, install_temp.path(), "0.1.0").unwrap();

    // Spawn a second install on the SAME vault while we hold transaction1.
    // It must block (the lock is held by transaction1) — we time it out
    // after 200ms and check it hasn't completed yet.
    let archive_path = archive.archive_path.clone();
    let vault_path = install_temp.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(2));
    let barrier_clone = barrier.clone();
    let handle = std::thread::spawn(move || {
        barrier_clone.wait();
        unpack_archive_staged(&archive_path, &vault_path, "0.1.0")
    });

    // Let the second-install thread reach the lock acquisition.
    barrier.wait();
    // Give it a generous head start to attempt the lock.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !handle.is_finished(),
        "second install completed before first transaction was consumed — lock not held"
    );

    // Roll back the first install; the second should now proceed.
    let _ = manifest1;
    transaction1.rollback();

    // Second install should complete within a few seconds.
    let start = std::time::Instant::now();
    let result = handle.join().expect("second install thread");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "second install took too long"
    );
    let (_, tx2) = result.expect("second install succeeded after rollback released lock");
    tx2.commit();
}
