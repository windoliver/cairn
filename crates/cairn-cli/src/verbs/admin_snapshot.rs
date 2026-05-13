//! `cairn admin snapshot --backup <PATH> [--json]`

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use cairn_core::domain::{
    BackupRegistryEntry, ConsentKind, ConsentPayload, MemoryRecord, Rfc3339Timestamp,
    ShreddedBackupEntry, SourceId, TargetId,
};
use clap::ArgMatches;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Serialize)]
struct SnapshotReceipt {
    backup_id: String,
    backup_path: String,
    registry_dir: String,
    registry_entry: String,
}

#[derive(Debug)]
pub(crate) struct RegistryEntryFile {
    pub(crate) path: PathBuf,
    pub(crate) entry: BackupRegistryEntry,
}

#[derive(Debug, Clone)]
struct SourceRewrite {
    source_hash: String,
    source_ids: BTreeSet<SourceId>,
}

/// Run `cairn admin snapshot`.
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let backup_path = PathBuf::from(
        sub.get_one::<String>("backup")
            .expect("invariant: clap requires --backup"),
    );
    let json = sub.get_flag("json");

    match validate_non_overlapping_paths("vault", vault_root, "backup", &backup_path)
        .and_then(|()| create_snapshot(vault_root, &backup_path))
    {
        Ok(receipt) => {
            if json {
                match serde_json::to_string_pretty(&receipt) {
                    Ok(output) => println!("{output}"),
                    Err(error) => {
                        eprintln!("cairn admin snapshot: failed to render json — {error}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                println!(
                    "cairn admin snapshot: prepared backup at {}",
                    backup_path.display()
                );
            }

            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cairn admin snapshot: {error:#}");
            ExitCode::from(74)
        }
    }
}

fn create_snapshot(vault_root: &Path, backup_path: &Path) -> Result<SnapshotReceipt> {
    fs::create_dir_all(backup_registry_dir(vault_root))
        .with_context(|| format!("create {}", backup_registry_dir(vault_root).display()))?;

    materialize_backup_artifact(vault_root, backup_path)
        .with_context(|| format!("materialize backup at {}", backup_path.display()))?;

    let backup_id = format!("bkp_{}", timestamp_millis());
    let registry_path = backup_registry_dir(vault_root).join(format!("{backup_id}.json"));
    let entry = BackupRegistryEntry {
        backup_id: backup_id.clone(),
        created_at: now_timestamp()?,
        artifact_path: backup_path.display().to_string(),
        target_ids_included: collect_target_ids(&vault_root.join(".cairn/cairn.db"))
            .with_context(|| format!("collect target ids from {}", vault_root.display()))?,
    };
    write_registry_entry(&registry_path, &entry)
        .with_context(|| format!("write registry entry {}", registry_path.display()))?;

    Ok(SnapshotReceipt {
        backup_id,
        backup_path: backup_path.display().to_string(),
        registry_dir: backup_registry_dir(vault_root).display().to_string(),
        registry_entry: registry_path.display().to_string(),
    })
}

pub(crate) fn materialize_backup_artifact(source_root: &Path, backup_path: &Path) -> Result<()> {
    fs::create_dir_all(backup_path)
        .with_context(|| format!("create backup root {}", backup_path.display()))?;
    clear_materialized_subtrees(backup_path)?;

    copy_sqlite_database(
        &source_root.join(".cairn/cairn.db"),
        &backup_path.join(".cairn/cairn.db"),
    )?;
    copy_optional_tree(&source_root.join("raw"), &backup_path.join("raw"))?;
    copy_optional_tree(&source_root.join("wiki"), &backup_path.join("wiki"))?;
    copy_optional_tree(&source_root.join("sources"), &backup_path.join("sources"))?;

    Ok(())
}

pub(crate) fn replay_current_forgets(live_vault_root: &Path, restored_root: &Path) -> Result<()> {
    let live_db = live_vault_root.join(".cairn/cairn.db");
    let forget_hashes = current_record_forget_hashes(&live_db)?;
    if forget_hashes.is_empty() {
        return Ok(());
    }

    let restored_db = restored_root.join(".cairn/cairn.db");
    let targets = collect_target_ids(&restored_db)?;
    if targets.is_empty() {
        return Ok(());
    }

    let targets_to_purge: Vec<TargetId> = targets
        .into_iter()
        .filter(|target| forget_hashes.contains(&target_id_hash(target.as_str())))
        .collect();
    if targets_to_purge.is_empty() {
        return Ok(());
    }

    let source_rewrites = collect_source_rewrites(&restored_db, &targets_to_purge)?;
    purge_targets(&restored_db, &targets_to_purge)?;
    rewrite_sources(restored_root, &source_rewrites, "restore-replay")?;
    Ok(())
}

pub(crate) fn rewrite_registered_backups(
    vault_root: &Path,
    targets: &[TargetId],
    forget_operation_id: &str,
) -> Result<()> {
    if targets.is_empty() {
        return Ok(());
    }

    let target_set: BTreeSet<String> = targets
        .iter()
        .map(|target| target.as_str().to_owned())
        .collect();
    for registry_entry in load_registry_entries(vault_root)? {
        let retained: Vec<TargetId> = registry_entry
            .entry
            .target_ids_included
            .iter()
            .filter(|target| !target_set.contains(target.as_str()))
            .cloned()
            .collect();
        if retained.len() == registry_entry.entry.target_ids_included.len() {
            continue;
        }

        rewrite_registered_backup(
            vault_root,
            &registry_entry,
            targets,
            retained,
            forget_operation_id,
        )?;
    }

    Ok(())
}

pub(crate) fn load_registry_entries(vault_root: &Path) -> Result<Vec<RegistryEntryFile>> {
    let registry_dir = backup_registry_dir(vault_root);
    if !registry_dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in
        fs::read_dir(&registry_dir).with_context(|| format!("read {}", registry_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
            continue;
        }

        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let entry: BackupRegistryEntry =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        entries.push(RegistryEntryFile { path, entry });
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn rewrite_registered_backup(
    vault_root: &Path,
    registry_entry: &RegistryEntryFile,
    targets: &[TargetId],
    retained_targets: Vec<TargetId>,
    forget_operation_id: &str,
) -> Result<()> {
    let artifact_path = PathBuf::from(&registry_entry.entry.artifact_path);
    validate_backup_root(&artifact_path)?;
    let parent = artifact_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "backup artifact {} has no parent directory",
            artifact_path.display()
        )
    })?;
    let artifact_name = artifact_path.file_name().ok_or_else(|| {
        anyhow::anyhow!(
            "backup artifact {} has no file name",
            artifact_path.display()
        )
    })?;
    let rewrite_path = parent.join(format!(
        ".{}.rewrite-{}",
        artifact_name.to_string_lossy(),
        timestamp_millis()
    ));

    materialize_backup_artifact(&artifact_path, &rewrite_path)?;
    let rewrite_db = rewrite_path.join(".cairn/cairn.db");
    let source_rewrites = collect_source_rewrites(&rewrite_db, targets)?;
    purge_targets(&rewrite_db, targets)?;
    rewrite_sources(&rewrite_path, &source_rewrites, forget_operation_id)?;

    let shredded_dir = backup_registry_dir(vault_root).join("shredded");
    fs::create_dir_all(&shredded_dir)
        .with_context(|| format!("create {}", shredded_dir.display()))?;
    let shredded_path = shredded_dir.join(format!(
        "{}-{}",
        registry_entry.entry.backup_id,
        timestamp_millis()
    ));

    copy_tree(&artifact_path, &shredded_path)?;
    remove_path_if_exists(&artifact_path)?;
    fs::rename(&rewrite_path, &artifact_path).with_context(|| {
        format!(
            "replace backup artifact {} with rewritten copy",
            artifact_path.display()
        )
    })?;

    let updated_entry = BackupRegistryEntry {
        target_ids_included: retained_targets,
        ..registry_entry.entry.clone()
    };
    write_registry_entry(&registry_entry.path, &updated_entry)?;

    let shredded = ShreddedBackupEntry::new(
        updated_entry.backup_id.clone(),
        shredded_path.display().to_string(),
        forget_operation_id.to_owned(),
        now_timestamp()?,
    );
    append_shredded_log(vault_root, &shredded)?;
    Ok(())
}

fn append_shredded_log(vault_root: &Path, entry: &ShreddedBackupEntry) -> Result<()> {
    let log_path = backup_registry_dir(vault_root).join("shredded.log");
    let payload = serde_json::to_string(entry).context("serialize shredded backup entry")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    writeln!(file, "{payload}").with_context(|| format!("append {}", log_path.display()))?;
    Ok(())
}

fn backup_registry_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(".cairn").join("backups")
}

fn write_registry_entry(path: &Path, entry: &BackupRegistryEntry) -> Result<()> {
    let payload = serde_json::to_vec_pretty(entry).context("serialize backup registry entry")?;
    fs::write(path, payload).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn clear_materialized_subtrees(root: &Path) -> Result<()> {
    remove_path_if_exists(&root.join(".cairn"))?;
    remove_path_if_exists(&root.join("raw"))?;
    remove_path_if_exists(&root.join("wiki"))?;
    remove_path_if_exists(&root.join("sources"))?;
    Ok(())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    } else {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

fn copy_optional_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    copy_tree(src, dst)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(src).with_context(|| format!("stat {}", src.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refusing to copy symlinked path {}", src.display());
    }

    fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("type {}", src_path.display()))?;
        if file_type.is_dir() {
            copy_tree(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!("copy {} -> {}", src_path.display(), dst_path.display())
            })?;
        } else {
            anyhow::bail!("unsupported artifact entry {}", src_path.display());
        }
    }

    Ok(())
}

fn copy_sqlite_database(src_db: &Path, dst_db: &Path) -> Result<()> {
    ensure_vec0_registered();
    let parent = dst_db.parent().ok_or_else(|| {
        anyhow::anyhow!("database path {} has no parent directory", dst_db.display())
    })?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    remove_path_if_exists(dst_db)?;

    if !src_db.exists() {
        Connection::open(dst_db)
            .with_context(|| format!("create empty database {}", dst_db.display()))?;
        return Ok(());
    }

    let conn = Connection::open(src_db)
        .with_context(|| format!("open source database {}", src_db.display()))?;
    let sql = format!("VACUUM INTO {};", sqlite_string_literal(dst_db));
    conn.execute_batch(&sql).with_context(|| {
        format!(
            "copy sqlite database {} -> {}",
            src_db.display(),
            dst_db.display()
        )
    })?;
    Ok(())
}

fn sqlite_string_literal(path: &Path) -> String {
    let escaped = path.display().to_string().replace('\'', "''");
    format!("'{escaped}'")
}

fn current_record_forget_hashes(db_path: &Path) -> Result<BTreeSet<String>> {
    if !db_path.exists() {
        return Ok(BTreeSet::new());
    }

    ensure_vec0_registered();
    let conn = Connection::open(db_path)
        .with_context(|| format!("open consent database {}", db_path.display()))?;
    if !table_exists(&conn, "consent_journal")? {
        return Ok(BTreeSet::new());
    }

    let events =
        cairn_store_sqlite::consent::read_since_rowid(&conn, 0).context("read consent journal")?;
    let mut hashes = BTreeSet::new();
    for (_, event) in events {
        if event.kind != ConsentKind::ForgetIntent {
            continue;
        }
        if let ConsentPayload::IntentReceipt {
            target_id_hash,
            reason_code,
            ..
        } = &event.payload
            && reason_code == "record_forget"
        {
            hashes.insert(target_id_hash.clone());
        }
    }

    Ok(hashes)
}

fn collect_target_ids(db_path: &Path) -> Result<Vec<TargetId>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    ensure_vec0_registered();
    let conn = Connection::open(db_path)
        .with_context(|| format!("open database {}", db_path.display()))?;
    if !table_exists(&conn, "records")? {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare("SELECT DISTINCT target_id FROM records ORDER BY target_id ASC")
        .context("prepare target id query")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut targets = Vec::new();
    for row in rows {
        let target = row?;
        targets.push(
            TargetId::parse(target.clone()).with_context(|| {
                format!("parse target id `{target}` from {}", db_path.display())
            })?,
        );
    }
    Ok(targets)
}

fn collect_source_rewrites(db_path: &Path, targets: &[TargetId]) -> Result<Vec<SourceRewrite>> {
    if targets.is_empty() || !db_path.exists() {
        return Ok(Vec::new());
    }

    ensure_vec0_registered();
    let conn = Connection::open(db_path)
        .with_context(|| format!("open database {}", db_path.display()))?;
    if !table_exists(&conn, "records")? {
        return Ok(Vec::new());
    }

    let mut groups = BTreeMap::<String, SourceRewrite>::new();
    let mut stmt = conn
        .prepare("SELECT record_json FROM records WHERE target_id = ?1")
        .context("prepare record provenance query")?;
    for target in targets {
        let rows = stmt.query_map([target.as_str()], |row| row.get::<_, String>(0))?;
        for row in rows {
            let record_json = row?;
            let record: MemoryRecord =
                serde_json::from_str(&record_json).context("deserialize record_json")?;
            let entry = groups
                .entry(record.provenance.source_hash.clone())
                .or_insert_with(|| SourceRewrite {
                    source_hash: record.provenance.source_hash.clone(),
                    source_ids: BTreeSet::new(),
                });
            entry
                .source_ids
                .extend(record.provenance.source_ids.iter().cloned());
        }
    }

    Ok(groups.into_values().collect())
}

fn purge_targets(db_path: &Path, targets: &[TargetId]) -> Result<()> {
    if targets.is_empty() || !db_path.exists() {
        return Ok(());
    }

    ensure_vec0_registered();
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("open database {}", db_path.display()))?;
    if !table_exists(&conn, "records")? {
        return Ok(());
    }

    let tx = conn.transaction().context("start purge transaction")?;
    for target in targets {
        tx.execute(
            "DELETE FROM edges \
              WHERE src IN (SELECT record_id FROM records WHERE target_id = ?1) \
                 OR dst IN (SELECT record_id FROM records WHERE target_id = ?1)",
            [target.as_str()],
        )
        .with_context(|| format!("purge edges for {}", target.as_str()))?;
        tx.execute(
            "DELETE FROM records WHERE target_id = ?1",
            [target.as_str()],
        )
        .with_context(|| format!("purge records for {}", target.as_str()))?;
    }
    tx.commit().context("commit purge transaction")?;
    Ok(())
}

fn rewrite_sources(root: &Path, rewrites: &[SourceRewrite], operation_id: &str) -> Result<()> {
    for rewrite in rewrites {
        for source_id in &rewrite.source_ids {
            rewrite_source_redaction_marker(root, source_id, &rewrite.source_hash, operation_id)?;
        }
    }
    Ok(())
}

fn rewrite_source_redaction_marker(
    root: &Path,
    source_id: &SourceId,
    source_hash: &str,
    operation_id: &str,
) -> Result<()> {
    let path = root.join(source_id.as_str());
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("source path {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let marker = format!(
        "cairn:redacted-source:v1\nsource_hash={source_hash}\noperation_id={operation_id}\n"
    );
    fs::write(&path, marker).with_context(|| format!("rewrite {}", path.display()))?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |row| row.get(0),
        )
        .context("query sqlite_master")?;
    Ok(exists == 1)
}

fn target_id_hash(target_id: &str) -> String {
    use sha2::{Digest, Sha256};

    format!("sha256:{:x}", Sha256::digest(target_id.as_bytes()))
}

pub(crate) fn validate_backup_root(path: &Path) -> Result<()> {
    if !path.is_dir() {
        anyhow::bail!(
            "backup root {} does not exist or is not a directory",
            path.display()
        );
    }

    let db_path = path.join(".cairn/cairn.db");
    if !db_path.is_file() {
        anyhow::bail!(
            "backup root {} is missing required database {}",
            path.display(),
            db_path.display()
        );
    }

    Ok(())
}

pub(crate) fn validate_non_overlapping_paths(
    left_label: &str,
    left: &Path,
    right_label: &str,
    right: &Path,
) -> Result<()> {
    let left = normalize_absolute_path(left)?;
    let right = normalize_absolute_path(right)?;
    if left == right || left.starts_with(&right) || right.starts_with(&left) {
        anyhow::bail!(
            "{} path {} overlaps {} path {}",
            left_label,
            left.display(),
            right_label,
            right.display()
        );
    }
    Ok(())
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve current working directory")?
            .join(path)
    };

    let lexical = normalize_path_components(&absolute);
    let mut suffix = Vec::<OsString>::new();
    let mut cursor = lexical.as_path();
    while !cursor.exists() {
        let name = cursor.file_name().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot resolve non-existent path component for {}",
                lexical.display()
            )
        })?;
        suffix.push(name.to_os_string());
        cursor = cursor.parent().ok_or_else(|| {
            anyhow::anyhow!("cannot resolve parent directory for {}", lexical.display())
        })?;
    }

    let mut normalized = cursor
        .canonicalize()
        .with_context(|| format!("canonicalize {}", cursor.display()))?;
    for part in suffix.iter().rev() {
        normalized.push(part);
    }

    Ok(normalized)
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut anchored = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
                anchored = true;
            }
            Component::RootDir => {
                normalized.push(component.as_os_str());
                anchored = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !anchored {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn ensure_vec0_registered() {
    cairn_store_sqlite::vec_ext::register_vec0();
}

fn now_timestamp() -> Result<Rfc3339Timestamp> {
    Rfc3339Timestamp::parse(cairn_core::time::now_rfc3339_seconds())
        .context("parse current timestamp")
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}
