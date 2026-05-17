//! `cairn admin archive-session --session <ID>`.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::{Ed25519Signature, MemoryRecord, ScopeTuple};
use clap::ArgMatches;
use serde::Serialize;

use super::cold_session::{ColdSessionBundle, write_bundle};

const ARCHIVED_STUB_BODY: &str =
    "[cold archived: body rehydrates with `cairn retrieve --session --rehydrate`]";

#[derive(Debug, Serialize)]
struct ArchiveSessionReceipt {
    session_id: String,
    records_archived: usize,
    bundle_path: String,
}

/// Run `cairn admin archive-session`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let session_id = sub
        .get_one::<String>("session")
        .expect("invariant: clap requires --session")
        .clone();
    let json = sub.get_flag("json");

    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        eprintln!("cairn admin archive-session: could not build tokio runtime");
        return ExitCode::FAILURE;
    };

    match rt.block_on(run_async(vault_root, session_id)) {
        Ok(receipt) => {
            if json {
                match serde_json::to_string_pretty(&receipt) {
                    Ok(output) => println!("{output}"),
                    Err(error) => {
                        eprintln!("cairn admin archive-session: render json: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                println!(
                    "cairn admin archive-session: archived {} records for {} to {}",
                    receipt.records_archived, receipt.session_id, receipt.bundle_path
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cairn admin archive-session: {error:#}");
            ExitCode::from(74)
        }
    }
}

async fn run_async(vault_root: &Path, session_id: String) -> Result<ArchiveSessionReceipt> {
    let store = cairn_store_sqlite::open(vault_root.join(".cairn/cairn.db"))
        .await
        .context("open memory store")?;
    let mut records = store
        .list_active_stored(&ListArgs {
            scope: Some(ScopeTuple {
                session_id: Some(session_id.clone()),
                ..ScopeTuple::default()
            }),
            limit: 1000,
            ..ListArgs::default()
        })
        .await
        .map_err(|error| anyhow::anyhow!("list session records: {error}"))?
        .into_iter()
        .map(|stored| stored.record)
        .filter(|record| record.scope.session_id.as_deref() == Some(session_id.as_str()))
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.updated_at
            .as_str()
            .cmp(right.updated_at.as_str())
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });

    let bundle =
        ColdSessionBundle::new(session_id.clone(), current_unix_ms_u128(), records.clone());
    let bundle_path = write_bundle(vault_root, &bundle)?;

    write_hot_stubs(
        &store,
        &records,
        bundle_path.file_name().and_then(|name| name.to_str()),
    )
    .await?;
    drain_async_lock_releases().await;

    Ok(ArchiveSessionReceipt {
        session_id,
        records_archived: records.len(),
        bundle_path: bundle_path.display().to_string(),
    })
}

fn archived_stub(record: &MemoryRecord, bundle_name: Option<&str>) -> MemoryRecord {
    let mut stub = record.clone();
    ARCHIVED_STUB_BODY.clone_into(&mut stub.body);
    stub.signature = Ed25519Signature::flush_mutated_sentinel();
    if let Some(trace) = stub
        .extra_frontmatter
        .get_mut("trace")
        .and_then(serde_json::Value::as_object_mut)
    {
        trace.remove("capture_event_id");
        trace.remove("sequence");
    }
    stub.extra_frontmatter.insert(
        "cold_archive".to_owned(),
        serde_json::json!({
            "source_record_id": record.id.as_str(),
            "bundle": bundle_name,
        }),
    );
    stub
}

async fn write_hot_stubs(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    records: &[MemoryRecord],
    bundle_name: Option<&str>,
) -> Result<()> {
    for record in records {
        let stub = archived_stub(record, bundle_name);
        store
            .tombstone(&record.id, TombstoneReason::Expire)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "tombstone archived record {} for target {}: {error}",
                    record.id.as_str(),
                    record.target_id.as_str()
                )
            })?;
        store.upsert(&stub).await.map_err(|error| {
            anyhow::anyhow!(
                "write archived hot stub for target {}: {error}",
                record.target_id.as_str()
            )
        })?;
    }
    Ok(())
}

fn current_unix_ms_u128() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

async fn drain_async_lock_releases() {
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
}
