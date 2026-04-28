//! Integration tests for `cairn lint --fix-folders` (issue #44).

use cairn_cli::verbs::lint::{FixFoldersResult, fix_folders_handler};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_test_fixtures::store::{FixtureStore, sample_record};

#[tokio::test]
async fn rebuilds_index_from_empty_markdown_tree() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    // Bootstrap-style minimal layout: just .cairn/ and raw/.
    std::fs::create_dir_all(vault.path().join(".cairn")).unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let result: FixFoldersResult = fix_folders_handler(&store, vault.path()).await.unwrap();

    assert!(
        !result.written.is_empty(),
        "expected at least one _index.md written"
    );
    let index = vault.path().join("raw/_index.md");
    assert!(index.exists(), "raw/_index.md not written");
    let content = std::fs::read_to_string(&index).unwrap();
    assert!(content.contains("kind: folder_index"));
}

#[tokio::test]
async fn idempotent_second_run_reports_unchanged() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let r1 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(!r1.written.is_empty());

    let r2 = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(r2.written.is_empty());
    assert!(r2.unchanged > 0);
}

#[tokio::test]
async fn bad_policy_yaml_taints_subtree_and_skips_indexing() {
    // Brief invariant 6 (fail-closed): a folder with an unparseable
    // _policy.yaml must skip its entire subtree, not silently fall back to
    // default policy. The sample record projects to `raw/<kind>_<id>.md`,
    // so a broken `raw/_policy.yaml` taints the parent of every record we
    // emit — no `_index.md` should be written, but the parse failure is
    // surfaced via `policy_errors`.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    std::fs::write(vault.path().join("raw/_policy.yaml"), "unknown_key: 42\n").unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert_eq!(result.policy_errors.len(), 1);
    assert!(result.policy_errors[0].path.ends_with("raw/_policy.yaml"));
    // Tainted prefix → record dropped → no index for `raw/`.
    assert!(
        result.written.is_empty(),
        "expected no _index.md when raw/ is tainted, got {:?}",
        result.written,
    );
    assert!(
        !vault.path().join("raw/_index.md").exists(),
        "raw/_index.md must not be written under a tainted policy",
    );
}

#[tokio::test]
async fn sibling_subtree_unaffected_by_broken_policy() {
    // A broken policy under `raw/broken/` must not taint `raw/` itself.
    // The sample record sits directly in `raw/`, outside the tainted
    // subtree, so its index is still written.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw/broken")).unwrap();
    std::fs::write(
        vault.path().join("raw/broken/_policy.yaml"),
        "unknown_key: 42\n",
    )
    .unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert_eq!(result.policy_errors.len(), 1);
    assert!(
        vault.path().join("raw/_index.md").exists(),
        "sibling subtree raw/ should still be indexed",
    );
}

#[tokio::test]
async fn corrupt_non_utf8_index_is_overwritten() {
    // A pre-existing `_index.md` that contains non-UTF-8 bytes (e.g. a
    // partial write left behind by a crash, or a binary file dropped by
    // accident) must NOT abort the run with an InvalidData error — the
    // whole point of `--fix-folders` is to recover state. We byte-compare
    // instead of `read_to_string`, so the corrupt file is simply marked
    // stale and overwritten atomically.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    // Plant invalid UTF-8 bytes at the destination.
    std::fs::write(
        vault.path().join("raw/_index.md"),
        [0xFF, 0xFE, 0xFD, 0xFC].as_slice(),
    )
    .unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();
    assert!(
        !result.written.is_empty(),
        "expected the corrupt index to be replaced",
    );
    let content = std::fs::read_to_string(vault.path().join("raw/_index.md")).unwrap();
    assert!(
        content.contains("kind: folder_index"),
        "corrupt content was not replaced: {content:?}"
    );
}

#[tokio::test]
async fn atomic_writes_overwrite_stale_and_leave_no_tmp() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    std::fs::create_dir_all(vault.path().join(".cairn")).unwrap();

    // Pre-place stale content; atomic rename must replace it.
    std::fs::write(vault.path().join("raw/_index.md"), "stale content\n").unwrap();

    let _ = fix_folders_handler(&store, vault.path()).await.unwrap();

    let content = std::fs::read_to_string(vault.path().join("raw/_index.md")).unwrap();
    assert!(
        content.contains("kind: folder_index"),
        "stale content not replaced: {content:?}"
    );

    // No leftover temp files in raw/. tempfile::Builder::suffix(".md.tmp")
    // produces names like `<random>.md.tmp` — match anything containing ".tmp".
    let leftovers: Vec<_> = std::fs::read_dir(vault.path().join("raw"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "tempfile leftovers: {leftovers:?}");
}

#[tokio::test]
#[cfg(unix)]
async fn symlinked_index_destination_is_rejected_even_when_unchanged() {
    // `_index.md` itself as a symlink — even one whose target's content
    // matches what we would write — must be rejected. Earlier the unchanged
    // fast-path read through the symlink with `read_to_string` and skipped
    // `write_once`, bypassing its symlink guard.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    let attacker = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    // Pre-populate the attacker target with arbitrary bytes.
    std::fs::write(attacker.path().join("decoy.md"), b"decoy\n").unwrap();
    // Plant a symlink at raw/_index.md pointing into the attacker tempdir.
    std::os::unix::fs::symlink(
        attacker.path().join("decoy.md"),
        vault.path().join("raw/_index.md"),
    )
    .unwrap();

    let result = fix_folders_handler(&store, vault.path()).await;

    assert!(result.is_err(), "symlinked _index.md must be rejected");
    // The attacker's file must remain untouched; we never wrote through.
    let decoy = std::fs::read_to_string(attacker.path().join("decoy.md")).unwrap();
    assert_eq!(decoy, "decoy\n", "attacker file was modified");
}

#[tokio::test]
#[cfg(unix)]
async fn write_through_symlinked_parent_is_rejected() {
    // The atomic-write path delegates to `vault::bootstrap::write_once`,
    // which lstat-checks the immediate parent. A symlinked `raw/` (the
    // parent of `raw/_index.md`) must be rejected, not silently written
    // through to the symlink target.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    let attacker = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(attacker.path(), vault.path().join("raw")).unwrap();

    let result = fix_folders_handler(&store, vault.path()).await;

    assert!(result.is_err(), "symlink-parent write should be rejected");
    assert!(
        !attacker.path().join("_index.md").exists(),
        "symlink target was written through",
    );
}

#[tokio::test]
#[cfg(unix)]
async fn write_through_symlinked_ancestor_is_rejected() {
    // `lstat` only refuses to follow the FINAL path component; intermediate
    // components are still resolved through symlinks. So a write at
    // `vault/raw/a/_index.md` could traverse a symlinked `raw` and land
    // outside the vault even with the immediate-parent guard. The fix walks
    // every ancestor between vault_root and the target.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    let attacker = tempfile::tempdir().unwrap();
    // `raw` is a symlink that resolves to a real directory.
    std::os::unix::fs::symlink(attacker.path(), vault.path().join("raw")).unwrap();
    // The symlink's target has a real `a/` subdirectory; `_index.md` would
    // be created at `attacker/a/_index.md` if validation only checked the
    // immediate parent (which lstats `vault/raw/a` and sees a real dir).
    std::fs::create_dir_all(attacker.path().join("a")).unwrap();

    let result = fix_folders_handler(&store, vault.path()).await;

    assert!(result.is_err(), "symlinked ancestor must be rejected");
    assert!(
        !attacker.path().join("a/_index.md").exists(),
        "write reached attacker subdirectory through symlinked ancestor",
    );
    assert!(
        !attacker.path().join("_index.md").exists(),
        "write reached attacker root through symlinked ancestor",
    );
}

#[tokio::test]
async fn symlinked_policy_yaml_taints_subtree() {
    // A `_policy.yaml` that is itself a symlink (or any non-regular file)
    // must be reported as a policy error and taint its containing folder.
    // `walkdir(follow_links=false)` returns symlinks as non-files, and the
    // earlier code skipped non-files silently — fail-OPEN, not fail-closed.
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    let real_target = vault.path().join("decoy.yaml");
    std::fs::write(&real_target, "anything\n").unwrap();
    std::os::unix::fs::symlink(&real_target, vault.path().join("raw/_policy.yaml")).unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();

    assert_eq!(
        result.policy_errors.len(),
        1,
        "symlinked _policy.yaml must produce a PolicyError"
    );
    assert!(
        result.policy_errors[0].path.ends_with("raw/_policy.yaml"),
        "unexpected policy error path: {:?}",
        result.policy_errors[0].path
    );
    assert!(
        result.written.is_empty(),
        "expected no _index.md when raw/ is tainted, got {:?}",
        result.written,
    );
    assert!(
        !vault.path().join("raw/_index.md").exists(),
        "raw/_index.md must not be written under a symlinked policy",
    );
}

#[tokio::test]
#[cfg(unix)]
async fn non_utf8_policy_yaml_taints_subtree_not_run() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();
    std::fs::create_dir_all(vault.path().join("raw/broken")).unwrap();

    // Write non-UTF-8 bytes — 0xFF, 0xFE are invalid as UTF-8 in this position.
    std::fs::write(
        vault.path().join("raw/broken/_policy.yaml"),
        [0xFF, 0xFE, 0xFD, 0xFC].as_slice(),
    )
    .unwrap();

    let result = fix_folders_handler(&store, vault.path()).await.unwrap();

    // Run did NOT abort. raw/_index.md still emitted (sibling subtree).
    assert!(vault.path().join("raw/_index.md").exists());
    // The broken policy is recorded as a policy error.
    assert_eq!(result.policy_errors.len(), 1);
    assert!(
        result.policy_errors[0]
            .path
            .ends_with("raw/broken/_policy.yaml")
    );
}

#[tokio::test]
async fn fixture_index_matches_snapshot() {
    let store = FixtureStore::default();
    store.upsert(sample_record()).await.unwrap();

    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(vault.path().join("raw")).unwrap();

    let _ = fix_folders_handler(&store, vault.path()).await.unwrap();
    let content = std::fs::read_to_string(vault.path().join("raw/_index.md")).unwrap();

    // Strip the timestamped `updated_at` line so the snapshot stays stable.
    let stable: String = content
        .lines()
        .filter(|l| !l.starts_with("updated_at:") && !l.contains("· updated "))
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!("raw_index_single_record", stable);
}
