use std::sync::Arc;

use anyhow::Result;
use cairn_core::contract::memory_store::{Edge, EdgeKind, MemoryStore, TombstoneReason};
use cairn_core::domain::flush_plan::{
    FlushPlan, PatchTarget, PlannedMutation, ReplaceOccurrence, StrReplace,
};
use cairn_core::domain::{BodyHash, RecordId, Rfc3339Timestamp, Session};
use cairn_store_sqlite::{SqliteMemoryStore, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionPatchDocument {
    title: String,
    channel: Option<String>,
    priority: Option<String>,
    tags: Vec<String>,
}

impl From<&Session> for SessionPatchDocument {
    fn from(session: &Session) -> Self {
        Self {
            title: session.title.clone(),
            channel: session.channel.clone(),
            priority: session.priority.clone(),
            tags: session.tags.clone(),
        }
    }
}

pub(crate) async fn apply_real_plan(
    _store: &Arc<dyn MemoryStore>,
    sqlite: &Arc<SqliteMemoryStore>,
    plan: &FlushPlan,
) -> Result<()> {
    let plan = plan.clone();
    sqlite
        .with_tx(move |tx| {
            for mutation in &plan.mutations {
                check_drift(tx, &plan, mutation)?;
                apply_mutation(tx, &plan.operation_id.0, mutation)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

/// Drift guard (issue #289 review round 1): before mutating, verify the live
/// record body hash matches the plan's recorded pre-state in
/// `target_hashes`. If the plan author did not record a hash for the
/// target, the check is skipped (consistent with stub-planner output that
/// leaves the map empty). Aborts the WAL tx on mismatch so partial mutations
/// roll back.
fn check_drift(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    plan: &FlushPlan,
    mutation: &PlannedMutation,
) -> Result<(), StoreError> {
    let target = match mutation {
        PlannedMutation::Patch {
            target: PatchTarget::Record(target),
            ..
        } => target,
        PlannedMutation::Rename { record_id, .. } => record_id,
        // Session metadata patches do not have a body to hash; rename/patch
        // for sessions is covered by `session.ended_at IS NULL` in the
        // tx-level helpers, not by `target_hashes`.
        _ => return Ok(()),
    };
    let Some(expected) = plan.target_hash(target) else {
        return Ok(());
    };
    let Some(stored) = tx.get_active_by_target(target)? else {
        return Err(StoreError::PatchTargetMissing {
            target_id: target.as_str().to_owned(),
        });
    };
    let actual = BodyHash::compute(&stored.record.body);
    if actual.as_str() != expected {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply drift: target `{}` body hash `{}` does not match plan \
                 pre-state hash `{}`; live record changed between plan creation and apply",
                target.as_str(),
                actual.as_str(),
                expected,
            ),
        });
    }
    Ok(())
}

fn apply_mutation(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    operation_id: &str,
    mutation: &PlannedMutation,
) -> Result<(), StoreError> {
    match mutation {
        PlannedMutation::Patch {
            target: PatchTarget::Record(target),
            str_replace,
        } => apply_record_patch(tx, operation_id, target, str_replace),
        PlannedMutation::Patch {
            target: PatchTarget::Session(session_id),
            str_replace,
        } => apply_session_patch(tx, session_id, str_replace),
        PlannedMutation::Rename { record_id, new_id } => apply_rename(tx, record_id, new_id),
        other => Err(StoreError::Invariant {
            what: format!("flush apply does not yet support mutation kind `{other:?}`"),
        }),
    }
}

fn apply_record_patch(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    operation_id: &str,
    target: &cairn_core::domain::TargetId,
    replacements: &[StrReplace],
) -> Result<(), StoreError> {
    let Some(mut stored) = tx.get_active_by_target(target)? else {
        return Err(StoreError::PatchTargetMissing {
            target_id: target.as_str().to_owned(),
        });
    };
    let prior_hash = BodyHash::compute(&stored.record.body);
    let target_label = format!("record:{}", target.as_str());
    stored.record.body = apply_str_replacements(&stored.record.body, replacements, &target_label)?;
    stored.record.updated_at = current_timestamp()?;
    append_patch_audit(&mut stored.record, operation_id, prior_hash.as_str())?;
    let _ = tx.upsert(&stored.record)?;
    Ok(())
}

fn apply_session_patch(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    session_id: &cairn_core::domain::SessionId,
    replacements: &[StrReplace],
) -> Result<(), StoreError> {
    let mut session = tx.get_live_session(session_id)?;
    let target_label = format!("session:{}", session_id.as_str());
    let doc_json = serde_json::to_string(&SessionPatchDocument::from(&session))?;
    let patched_json = apply_str_replacements(&doc_json, replacements, &target_label)?;
    let patched: SessionPatchDocument =
        serde_json::from_str(&patched_json).map_err(|e| StoreError::Invariant {
            what: format!(
                "session patch produced invalid metadata document for `{}`: {e}",
                session_id.as_str()
            ),
        })?;
    session.title = patched.title;
    session.channel = patched.channel;
    session.priority = patched.priority;
    session.tags = patched.tags;
    tx.update_session_metadata(&session)?;
    Ok(())
}

fn apply_rename(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    source_target: &cairn_core::domain::TargetId,
    new_target: &cairn_core::domain::TargetId,
) -> Result<(), StoreError> {
    let Some(source) = tx.get_active_by_target(source_target)? else {
        return Err(StoreError::NotFound {
            id: source_target.as_str().to_owned(),
        });
    };
    if tx.get_active_by_target(new_target)?.is_some() {
        return Err(StoreError::RenameTargetConflict {
            target_id: new_target.as_str().to_owned(),
        });
    }

    let mut renamed = source.record.clone();
    renamed.id =
        RecordId::parse(new_target.as_str().to_owned()).map_err(|e| StoreError::Invariant {
            what: format!(
                "rename destination `{}` cannot be re-used as record_id: {e}",
                new_target.as_str()
            ),
        })?;
    renamed.target_id = new_target.clone();
    renamed.updated_at = current_timestamp()?;
    let new_row = tx.upsert(&renamed)?;

    let new_edge = Edge {
        src: new_row.record_id.clone(),
        dst: source.record.id.clone(),
        kind: EdgeKind::Updates,
        weight: None,
    };
    tx.rewrite_non_updates_edges(&source.record.id, &new_row.record_id)?;
    tx.put_edge(&new_edge)?;
    tx.tombstone(&source.record.id, TombstoneReason::Update)?;
    Ok(())
}

fn current_timestamp() -> Result<Rfc3339Timestamp, StoreError> {
    Rfc3339Timestamp::parse(cairn_core::time::now_rfc3339_seconds()).map_err(|e| {
        StoreError::Invariant {
            what: format!("generated timestamp failed RFC-3339 validation: {e}"),
        }
    })
}

fn append_patch_audit(
    record: &mut cairn_core::domain::MemoryRecord,
    operation_id: &str,
    old_body_hash: &str,
) -> Result<(), StoreError> {
    let applied_at = current_timestamp()?.to_string();
    let entry = json!({
        "operation_id": operation_id,
        "old_body_hash": old_body_hash,
        "applied_at": applied_at,
    });
    match record.extra_frontmatter.get_mut("flush_patch_history") {
        Some(value) => {
            let Some(items) = value.as_array_mut() else {
                return Err(StoreError::Invariant {
                    what: "extra_frontmatter.flush_patch_history must be an array".into(),
                });
            };
            items.push(entry);
        }
        None => {
            record.extra_frontmatter.insert(
                "flush_patch_history".into(),
                serde_json::Value::Array(vec![entry]),
            );
        }
    }
    Ok(())
}

fn apply_str_replacements(
    input: &str,
    replacements: &[StrReplace],
    target_label: &str,
) -> Result<String, StoreError> {
    let mut current = input.to_owned();
    for replacement in replacements {
        if replacement.old.is_empty() {
            return Err(StoreError::Invariant {
                what: format!("patch replacement for {target_label} cannot use an empty `old`"),
            });
        }
        current = apply_one_replacement(&current, replacement, target_label)?;
    }
    Ok(current)
}

fn apply_one_replacement(
    input: &str,
    replacement: &StrReplace,
    target_label: &str,
) -> Result<String, StoreError> {
    match replacement.occurrence {
        ReplaceOccurrence::First => {
            if !input.contains(&replacement.old) {
                return Err(patch_substring_missing(
                    target_label,
                    &replacement.old,
                    "first",
                ));
            }
            Ok(input.replacen(&replacement.old, &replacement.new, 1))
        }
        ReplaceOccurrence::All => {
            if !input.contains(&replacement.old) {
                return Err(patch_substring_missing(
                    target_label,
                    &replacement.old,
                    "all",
                ));
            }
            Ok(input.replace(&replacement.old, &replacement.new))
        }
        ReplaceOccurrence::Nth(index) => replace_nth(input, replacement, target_label, index),
    }
}

fn replace_nth(
    input: &str,
    replacement: &StrReplace,
    target_label: &str,
    index: usize,
) -> Result<String, StoreError> {
    let Some((start, matched)) = input.match_indices(&replacement.old).nth(index) else {
        return Err(patch_substring_missing(
            target_label,
            &replacement.old,
            &format!("nth({index})"),
        ));
    };
    let end = start + matched.len();
    let mut out =
        String::with_capacity(input.len() + replacement.new.len().saturating_sub(matched.len()));
    out.push_str(&input[..start]);
    out.push_str(&replacement.new);
    out.push_str(&input[end..]);
    Ok(out)
}

fn patch_substring_missing(target_label: &str, needle: &str, occurrence: &str) -> StoreError {
    StoreError::PatchSubstringMissing {
        target: target_label.to_owned(),
        needle: needle.to_owned(),
        occurrence: occurrence.to_owned(),
    }
}
