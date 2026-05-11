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

/// Canonical pre-state hash for a session metadata document, used to
/// populate `FlushPlan.target_hashes` when authoring a session patch.
/// Exposed for integration tests; planners should call this when
/// building real plans.
#[must_use]
pub fn session_drift_hash(session: &Session) -> String {
    // serde_json::to_string never fails for these scalar/Vec fields.
    let doc = SessionPatchDocument::from(session);
    let json = serde_json::to_string(&doc).expect("SessionPatchDocument serialization");
    BodyHash::compute(&json).as_str().to_owned()
}

/// Round 10 review fix: enforce that each plan dimension that is set
/// matches the record's stored scope. Unset plan dimensions are
/// unrestricted (the planner did not narrow on them). Returns the
/// offending dimension name on the first mismatch so the error message
/// can point operators at the right field.
fn scope_satisfies(plan: &cairn_core::domain::ScopeTuple, record: &cairn_core::domain::ScopeTuple)
    -> Result<(), &'static str>
{
    fn check(
        plan: Option<&str>,
        record: Option<&str>,
        name: &'static str,
    ) -> Result<(), &'static str> {
        match (plan, record) {
            (Some(p), Some(r)) if p == r => Ok(()),
            (Some(_), _) => Err(name),
            (None, _) => Ok(()),
        }
    }
    check(plan.tenant.as_deref(), record.tenant.as_deref(), "tenant")?;
    check(plan.workspace.as_deref(), record.workspace.as_deref(), "workspace")?;
    check(plan.project.as_deref(), record.project.as_deref(), "project")?;
    check(plan.session_id.as_deref(), record.session_id.as_deref(), "session_id")?;
    check(plan.entity.as_deref(), record.entity.as_deref(), "entity")?;
    check(plan.user.as_deref(), record.user.as_deref(), "user")?;
    check(plan.agent.as_deref(), record.agent.as_deref(), "agent")?;
    Ok(())
}

fn check_scope(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    plan: &FlushPlan,
    mutation: &PlannedMutation,
) -> Result<(), StoreError> {
    match mutation {
        PlannedMutation::Patch {
            target: PatchTarget::Record(target),
            ..
        }
        | PlannedMutation::Rename {
            record_id: target, ..
        } => {
            let Some(stored) = tx.get_active_by_target(target)? else {
                return Err(StoreError::PatchTargetMissing {
                    target_id: target.as_str().to_owned(),
                });
            };
            enforce_record_scope(plan, &stored.record, target)
        }
        PlannedMutation::Patch {
            target: PatchTarget::Session(session_id),
            ..
        } => {
            let session = tx.get_live_session(session_id)?;
            enforce_session_scope(plan, &session)
        }
        _ => Ok(()),
    }
}

fn enforce_record_scope(
    plan: &FlushPlan,
    record: &cairn_core::domain::MemoryRecord,
    target: &cairn_core::domain::TargetId,
) -> Result<(), StoreError> {
    scope_satisfies(&plan.scope, &record.scope).map_err(|dim| StoreError::Invariant {
        what: format!(
            "flush apply: plan scope dimension `{dim}` does not match record `{}` — \
             refusing mutation outside authorized scope",
            target.as_str(),
        ),
    })
}

fn enforce_session_scope(
    plan: &FlushPlan,
    session: &Session,
) -> Result<(), StoreError> {
    let plan_scope = &plan.scope;
    // session_id dimension
    if let Some(scoped) = &plan_scope.session_id
        && scoped != session.id.as_str()
    {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply: plan scope `session_id={scoped}` does not match session `{}`",
                session.id.as_str(),
            ),
        });
    }
    // user / agent dimensions
    if let Some(user) = &plan_scope.user
        && user != session.identity.user.as_str()
    {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply: plan scope `user={user}` does not match session `{}` user `{}`",
                session.id.as_str(),
                session.identity.user.as_str(),
            ),
        });
    }
    if let Some(agent) = &plan_scope.agent
        && agent != session.identity.agent.as_str()
    {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply: plan scope `agent={agent}` does not match session `{}` agent `{}`",
                session.id.as_str(),
                session.identity.agent.as_str(),
            ),
        });
    }
    // Re-loop round 1 finding 2: sessions are keyed by
    // `(user, agent, project_root)`, so a project-scoped plan must not
    // be able to mutate a same-user/same-agent session belonging to a
    // different project. Compare `plan.scope.project` against
    // `session.identity.project_root`; if the plan narrows on project,
    // the session must declare the same project_root.
    if let Some(project) = &plan_scope.project {
        let session_project = session.identity.project_root.as_deref();
        if session_project != Some(project.as_str()) {
            return Err(StoreError::Invariant {
                what: format!(
                    "flush apply: plan scope `project={project}` does not match session \
                     `{}` project_root `{}`",
                    session.id.as_str(),
                    session_project.unwrap_or("<none>"),
                ),
            });
        }
    }
    Ok(())
}

pub(crate) async fn apply_real_plan(
    _store: &Arc<dyn MemoryStore>,
    sqlite: &Arc<SqliteMemoryStore>,
    plan: &FlushPlan,
) -> Result<()> {
    let plan = plan.clone();
    sqlite
        .with_tx(move |tx| {
            // Round 3 review fix: drift-check the ORIGINAL pre-state once
            // per mutation BEFORE any apply runs. Doing the check inline
                // would compare later mutations against the post-first-write
                // state for plans that touch the same target twice
                // (patch-then-rename, multi-patch chains), so legitimate
                // multi-step plans would self-trip. Phase 1 = drift,
                // Phase 2 = apply.
            //
            // Round 10 review fix: enforce plan scope against each
            // target's stored scope BEFORE mutating, so a pending plan
            // staged outside the reviewer's authorized scope cannot
            // patch/rename rows it does not own.
            for mutation in &plan.mutations {
                check_drift(tx, &plan, mutation)?;
                check_scope(tx, &plan, mutation)?;
            }
            for mutation in &plan.mutations {
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
    match mutation {
        PlannedMutation::Patch {
            target: PatchTarget::Record(target),
            ..
        } => check_record_drift(tx, plan, target),
        PlannedMutation::Rename { record_id, .. } => check_record_drift(tx, plan, record_id),
        PlannedMutation::Patch {
            target: PatchTarget::Session(session_id),
            ..
        } => check_session_drift(tx, plan, session_id),
        _ => Ok(()),
    }
}

fn check_record_drift(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    plan: &FlushPlan,
    target: &cairn_core::domain::TargetId,
) -> Result<(), StoreError> {
    // Round 6 review fix: non-placeholder Patch/Rename must declare a
    // pre-state hash. Skipping drift when the map is empty made the
    // protection optional for exactly the mutations that need it.
    let Some(expected) = plan.target_hash(target) else {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply: non-placeholder plan is missing `target_hashes` entry \
                 for record `{}`; drift protection requires a pre-state hash for every \
                 patch/rename target",
                target.as_str(),
            ),
        });
    };
    let Some(stored) = tx.get_active_by_target(target)? else {
        return Err(StoreError::PatchTargetMissing {
            target_id: target.as_str().to_owned(),
        });
    };
    let actual = record_drift_hash(&stored.record);
    if actual != expected {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply drift: target `{}` record hash `{}` does not match plan \
                 pre-state hash `{}`; live record changed between plan creation and apply",
                target.as_str(),
                actual,
                expected,
            ),
        });
    }
    Ok(())
}

/// Canonical pre-state hash for a record. Covers the full reviewed
/// state (body + scope/visibility/frontmatter/signature/etc) by hashing
/// its serde JSON serialization — not just the body — so metadata-only
/// rewrites between plan creation and apply still trip the drift gate.
/// Exposed for integration tests and planners building real patch/rename
/// plans.
#[must_use]
pub fn record_drift_hash(record: &cairn_core::domain::MemoryRecord) -> String {
    let json = serde_json::to_string(record).expect("MemoryRecord serialization");
    BodyHash::compute(&json).as_str().to_owned()
}

/// Round 2 review fix: session patches must also reject stale-state apply.
/// Computes the same canonical [`SessionPatchDocument`] JSON the apply path
/// will mutate, hashes it with [`BodyHash`], and compares against the
/// plan's `target_hashes` entry keyed by `session_id`. Skipped when the
/// plan author did not record a session hash (stub-planner output).
fn check_session_drift(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    plan: &FlushPlan,
    session_id: &cairn_core::domain::SessionId,
) -> Result<(), StoreError> {
    let Some(expected) = plan.session_hash(session_id) else {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply: non-placeholder plan is missing `target_hashes` entry \
                 for session `{}`; drift protection requires a pre-state hash for every \
                 session metadata patch",
                session_id.as_str(),
            ),
        });
    };
    let session = tx.get_live_session(session_id)?;
    let doc_json = serde_json::to_string(&SessionPatchDocument::from(&session))?;
    let actual = BodyHash::compute(&doc_json);
    if actual.as_str() != expected {
        return Err(StoreError::Invariant {
            what: format!(
                "flush apply drift: session `{}` metadata hash `{}` does not match plan \
                 pre-state hash `{}`; live session changed between plan creation and apply",
                session_id.as_str(),
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
    let prior_signature = stored.record.signature.as_str().to_owned();
    let target_label = format!("record:{}", target.as_str());
    stored.record.body = apply_str_replacements(&stored.record.body, replacements, &target_label)?;
    stored.record.updated_at = current_timestamp()?;
    append_patch_audit(
        &mut stored.record,
        operation_id,
        prior_hash.as_str(),
        &prior_signature,
    )?;
    let _ = tx.upsert(&stored.record)?;
    Ok(())
}

fn apply_session_patch(
    tx: &mut cairn_store_sqlite::StoreTx<'_>,
    session_id: &cairn_core::domain::SessionId,
    replacements: &[StrReplace],
) -> Result<(), StoreError> {
    let mut session = tx.get_live_session(session_id)?;
    // Round 4/5 review fix: patch session fields by typed-field
    // routing, NOT by string-replacing a serialized JSON blob.
    // The previous approach allowed `StrReplace.new` to contain
    // JSON syntax (e.g. `","priority":"high","x":"`), letting one
    // replacement reach across to unrelated fields while still
    // producing a parseable document. Pick the single field whose
    // value contains `old` and apply the replacement to that field
    // only; reject ambiguous (multi-field) matches.
    for replacement in replacements {
        apply_one_session_replacement(&mut session, replacement, session_id)?;
    }
    tx.update_session_metadata(&session)?;
    Ok(())
}

/// Apply a single [`StrReplace`] to exactly one resolved session field.
/// Rejects matches in zero or >1 field — the wire format does not name
/// the target field, so the substring must uniquely identify it.
fn apply_one_session_replacement(
    session: &mut Session,
    replacement: &StrReplace,
    session_id: &cairn_core::domain::SessionId,
) -> Result<(), StoreError> {
    if replacement.old.is_empty() {
        return Err(StoreError::Invariant {
            what: format!(
                "session patch for `{}`: empty `old` is not allowed",
                session_id.as_str()
            ),
        });
    }
    let target_label = format!("session:{}", session_id.as_str());
    let mut hits: Vec<SessionField> = Vec::new();
    if session.title.contains(&replacement.old) {
        hits.push(SessionField::Title);
    }
    if let Some(ch) = &session.channel
        && ch.contains(&replacement.old)
    {
        hits.push(SessionField::Channel);
    }
    if let Some(pr) = &session.priority
        && pr.contains(&replacement.old)
    {
        hits.push(SessionField::Priority);
    }
    for (idx, tag) in session.tags.iter().enumerate() {
        if tag.contains(&replacement.old) {
            hits.push(SessionField::Tag(idx));
        }
    }
    match hits.len() {
        0 => Err(patch_substring_missing(
            &target_label,
            &replacement.old,
            match replacement.occurrence {
                ReplaceOccurrence::First => "first",
                ReplaceOccurrence::All => "all",
                ReplaceOccurrence::Nth(_) => "nth",
            },
        )),
        1 => {
            let field = hits.remove(0);
            apply_replacement_to_field(session, field, replacement, &target_label)
        }
        _ => Err(StoreError::Invariant {
            what: format!(
                "session patch for `{}`: `old` substring `{}` is ambiguous — \
                 matches multiple fields ({hits:?}). Author a narrower replacement \
                 or split into per-field plans.",
                session_id.as_str(),
                replacement.old,
            ),
        }),
    }
}

#[derive(Debug, Clone, Copy)]
enum SessionField {
    Title,
    Channel,
    Priority,
    Tag(usize),
}

fn apply_replacement_to_field(
    session: &mut Session,
    field: SessionField,
    replacement: &StrReplace,
    target_label: &str,
) -> Result<(), StoreError> {
    match field {
        SessionField::Title => {
            session.title =
                apply_one_replacement(&session.title.clone(), replacement, target_label)?;
        }
        SessionField::Channel => {
            let current = session.channel.clone().unwrap_or_default();
            session.channel = Some(apply_one_replacement(&current, replacement, target_label)?);
        }
        SessionField::Priority => {
            let current = session.priority.clone().unwrap_or_default();
            session.priority = Some(apply_one_replacement(&current, replacement, target_label)?);
        }
        SessionField::Tag(idx) => {
            let current = session.tags[idx].clone();
            session.tags[idx] = apply_one_replacement(&current, replacement, target_label)?;
        }
    }
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
    // Round 9 review fix: rejecting only the live destination left a
    // hole — a retired (tombstoned) target_id could be reused, merging
    // unrelated histories under one lineage. The destination must be
    // entirely fresh.
    if tx.target_id_ever_used(new_target)? {
        return Err(StoreError::RenameTargetConflict {
            target_id: new_target.as_str().to_owned(),
        });
    }

    // Round 7 review fix: mint a fresh `record_id` for the renamed
    // record. Previously this reused `new_target` as the row's primary
    // key, which collided with any historical (now-tombstoned) lineage
    // that had once owned that ULID. The `target_id` is what stays
    // stable across renames; `record_id` is per-version.
    let mut renamed = source.record.clone();
    let fresh_record_id = ulid::Ulid::new().to_string();
    renamed.id =
        RecordId::parse(fresh_record_id.clone()).map_err(|e| StoreError::Invariant {
            what: format!(
                "rename: fresh record_id `{fresh_record_id}` failed validation: {e}"
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
    old_signature: &str,
) -> Result<(), StoreError> {
    let applied_at = current_timestamp()?.to_string();
    // Round 4 review fix: record the pre-mutation signature so downstream
    // verifiers can detect that the record was rewritten by a flush apply
    // (the carried-over signature attests the pre-mutation body, not the
    // post-mutation body — re-signing belongs to a separate trust-model
    // change tracked outside this issue).
    let entry = json!({
        "operation_id": operation_id,
        "old_body_hash": old_body_hash,
        "old_signature": old_signature,
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
