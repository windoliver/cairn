//! Block-marker merge helpers for `.claude/settings.json` and `.mcp.json`.
//!
//! Pack-owned entries are tagged with a `_pack` sibling field so re-install
//! can identify and update them without trampling user customisations.

use serde_json::Value;

use crate::packs::manifest::PackError;

const PACK_MARKER_KEY: &str = "_pack";

/// Detect a `SessionStart` entry written by the pre-pack inline
/// installer (`cairn-cli/src/skill.rs` on main prior to #182). The
/// legacy command was hard-coded:
///
/// ```text
/// cairn ingest --folder . --mode keyword >/tmp/cairn-session-start.log 2>&1 &
/// ```
///
/// We scan the entry's `hooks[].command` for that signature and treat
/// any match as legacy. False positives would require a user to write
/// the same exact command, which is unlikely enough to accept the
/// trade for migration completeness.
fn is_legacy_pre_pack_hook(entry: &Value) -> bool {
    const LEGACY_SIGNATURE: &str = "cairn ingest --folder . --mode keyword";
    let Some(hooks) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    hooks.iter().any(|h| {
        h.get("command")
            .and_then(Value::as_str)
            .is_some_and(|cmd| cmd.contains(LEGACY_SIGNATURE))
    })
}

/// Return `true` iff the `_pack` marker on `entry` belongs to the same
/// pack as `pack_id` (matching by id, ignoring any `@version` tail).
///
/// Pack version bumps must still match prior entries so upgrades replace
/// rather than duplicate them.
pub(crate) fn marker_matches_pack(entry: &Value, pack_id: &str) -> bool {
    let Some(marker) = entry.get(PACK_MARKER_KEY).and_then(Value::as_str) else {
        return false;
    };
    let marker_pack = marker.split_once('@').map_or(marker, |(id, _)| id);
    marker_pack == pack_id
}

/// Merge the pack's `settings.json` payload into the existing user JSON.
///
/// User-added entries for the same hook event are preserved. Pack-owned
/// entries are identified by a sibling `_pack` field.
///
/// # Errors
/// Returns [`PackError::MergeConflict`] if the existing JSON has a
/// non-object value at `hooks` or a non-array value at any event key.
pub fn merge_settings_json(
    existing: Value,
    pack_payload: &Value,
    pack_id_at_version: &str,
) -> Result<Value, PackError> {
    let mut out = match existing {
        Value::Null => serde_json::json!({}),
        Value::Object(_) => existing,
        other => {
            return Err(PackError::MergeConflict {
                file: ".claude/settings.json".to_owned(),
                reason: format!("expected object, got {other:?}"),
            });
        }
    };

    // Match prior entries by pack id only, so a version bump (e.g.
    // 0.1.0 → 0.2.0) replaces the old hooks instead of stacking.
    let pack_id = pack_id_at_version
        .split_once('@')
        .map_or(pack_id_at_version, |(id, _)| id);

    let pack_hooks = pack_payload
        .get("hooks")
        .and_then(Value::as_object)
        .ok_or_else(|| PackError::MergeConflict {
            file: ".claude/settings.json".to_owned(),
            reason: "pack payload missing `hooks` object".to_owned(),
        })?;

    let out_hooks = out
        .as_object_mut()
        .expect("invariant: out is always an object after the match above")
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::default()));
    let out_hooks = out_hooks
        .as_object_mut()
        .ok_or_else(|| PackError::MergeConflict {
            file: ".claude/settings.json".to_owned(),
            reason: "existing `hooks` is not an object".to_owned(),
        })?;

    for (event, pack_entries) in pack_hooks {
        let pack_array = pack_entries
            .as_array()
            .ok_or_else(|| PackError::MergeConflict {
                file: ".claude/settings.json".to_owned(),
                reason: format!("pack `hooks.{event}` is not an array"),
            })?;

        let existing_array = match out_hooks.entry(event.clone()) {
            serde_json::map::Entry::Vacant(v) => {
                v.insert(Value::Array(vec![]));
                out_hooks
                    .get_mut(event)
                    .expect("just inserted")
                    .as_array_mut()
                    .expect("just inserted as array")
            }
            serde_json::map::Entry::Occupied(o) => {
                let v = o.into_mut();
                v.as_array_mut().ok_or_else(|| PackError::MergeConflict {
                    file: ".claude/settings.json".to_owned(),
                    reason: format!("existing `hooks.{event}` is not an array"),
                })?
            }
        };

        // Drop any prior pack-owned entries for this pack id (round-trip
        // across version bumps).
        existing_array.retain(|entry| !marker_matches_pack(entry, pack_id));

        // Drop legacy pre-pack entries. The pre-pack inline installer
        // wrote unmarked SessionStart hooks running
        // `cairn ingest --folder . --mode keyword ... &` into
        // `.claude/settings.json`. Without this sweep an upgrade
        // preserves the legacy background-ingest hook as if it were
        // user-owned, running BOTH paths on every session.
        existing_array.retain(|entry| !is_legacy_pre_pack_hook(entry));

        // Append the pack entries, each tagged with the marker.
        for entry in pack_array {
            let mut tagged = entry.clone();
            if let Some(obj) = tagged.as_object_mut() {
                obj.insert(
                    PACK_MARKER_KEY.to_owned(),
                    Value::String(pack_id_at_version.to_owned()),
                );
            }
            existing_array.push(tagged);
        }
    }

    Ok(out)
}

const CLAUDE_MD_BEGIN: &str = "<!-- BEGIN CAIRN PACK MANUAL -->";
const CLAUDE_MD_END: &str = "<!-- END CAIRN PACK MANUAL -->";

/// Inject (or replace) the cairn pack manual block in a `CLAUDE.md` body.
///
/// `block_body` MUST start with `<!-- BEGIN CAIRN PACK MANUAL -->` and
/// end with `<!-- END CAIRN PACK MANUAL -->`; it is written between
/// those markers verbatim.
///
/// # Errors
/// Returns [`PackError::MergeConflict`] if `block_body` is malformed
/// (missing markers) or the existing file has only one marker.
pub fn inject_block(existing: Option<String>, block_body: &str) -> Result<String, PackError> {
    if !block_body.starts_with(CLAUDE_MD_BEGIN) || !block_body.trim_end().ends_with(CLAUDE_MD_END) {
        return Err(PackError::MergeConflict {
            file: "CLAUDE.md".to_owned(),
            reason: "block_body must be wrapped with CAIRN PACK MANUAL markers".to_owned(),
        });
    }
    let normalised_body = block_body.trim_end();
    let Some(existing) = existing else {
        return Ok(format!("{normalised_body}\n"));
    };

    let begin = existing.find(CLAUDE_MD_BEGIN);
    let end = existing.find(CLAUDE_MD_END);
    match (begin, end) {
        (Some(b), Some(e)) if b < e => {
            let end_after = e + CLAUDE_MD_END.len();
            let mut out = String::with_capacity(existing.len());
            out.push_str(&existing[..b]);
            out.push_str(normalised_body);
            out.push_str(&existing[end_after..]);
            Ok(out)
        }
        (None, None) => {
            let separator = if existing.ends_with('\n') { "" } else { "\n" };
            Ok(format!("{existing}{separator}{normalised_body}\n"))
        }
        _ => Err(PackError::MergeConflict {
            file: "CLAUDE.md".to_owned(),
            reason: "existing file has unbalanced CAIRN PACK MANUAL markers".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack_payload() -> Value {
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "cairn hook SessionStart" }] }
                ]
            }
        })
    }

    #[test]
    fn merge_into_empty_adds_tagged_entries() {
        let out = merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0")
            .expect("merge ok");
        let event = &out["hooks"]["SessionStart"][0];
        assert_eq!(
            event[PACK_MARKER_KEY].as_str(),
            Some("cairn-claude-code@0.1.0")
        );
    }

    #[test]
    fn merge_preserves_user_entries() {
        let existing = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "*", "hooks": [{ "type": "command", "command": "user-custom" }] }
                ]
            }
        });
        let out =
            merge_settings_json(existing, &pack_payload(), "cairn-claude-code@0.1.0").unwrap();
        let arr = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0][PACK_MARKER_KEY].is_null()); // user entry untouched
        assert!(arr[1][PACK_MARKER_KEY].is_string()); // pack entry tagged
    }

    #[test]
    fn merge_is_idempotent() {
        let once =
            merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0").unwrap();
        let twice =
            merge_settings_json(once.clone(), &pack_payload(), "cairn-claude-code@0.1.0").unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn merge_removes_legacy_pre_pack_session_start_hook() {
        // Pre-pack inline installer wrote this exact SessionStart hook
        // into .claude/settings.json (no _pack marker). Upgrade MUST
        // remove it — otherwise both the background ingest AND the new
        // `cairn hook SessionStart` run on every session.
        let legacy = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "*",
                        "hooks": [{
                            "type": "command",
                            "command": "cairn ingest --folder . --mode keyword >/tmp/cairn-session-start.log 2>&1 &"
                        }]
                    }
                ]
            }
        });
        let out = merge_settings_json(legacy, &pack_payload(), "cairn-claude-code@0.1.0")
            .expect("merge ok");
        let arr = out["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            1,
            "legacy hook must be removed, only pack entry remains"
        );
        assert_eq!(
            arr[0][PACK_MARKER_KEY].as_str(),
            Some("cairn-claude-code@0.1.0")
        );
    }

    #[test]
    fn merge_replaces_prior_version_pack_entries_on_upgrade() {
        // Install v0.1.0.
        let v1 = merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0")
            .expect("install v1");
        // Upgrade to v0.2.0 — must replace, not stack.
        let v2 = merge_settings_json(v1, &pack_payload(), "cairn-claude-code@0.2.0")
            .expect("upgrade to v2");
        let arr = v2["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            1,
            "upgrade must replace prior pack entries, not duplicate them"
        );
        assert_eq!(
            arr[0][PACK_MARKER_KEY].as_str(),
            Some("cairn-claude-code@0.2.0")
        );
    }
}

#[cfg(test)]
mod claude_md_tests {
    use super::*;

    const BEGIN: &str = "<!-- BEGIN CAIRN PACK MANUAL -->";
    const END: &str = "<!-- END CAIRN PACK MANUAL -->";

    #[test]
    fn injects_into_empty_file() {
        let body = format!("{BEGIN}\nfragment\n{END}");
        let out = inject_block(None, &body).unwrap();
        assert!(out.contains("fragment"));
        assert!(out.contains(BEGIN));
        assert!(out.contains(END));
    }

    #[test]
    fn replaces_existing_block_preserving_surrounding() {
        let existing = format!("# Project\n\nbefore\n\n{BEGIN}\nold fragment\n{END}\n\nafter\n");
        let body = format!("{BEGIN}\nnew fragment\n{END}");
        let out = inject_block(Some(existing), &body).unwrap();
        assert!(out.contains("new fragment"));
        assert!(!out.contains("old fragment"));
        assert!(out.contains("# Project"));
        assert!(out.contains("after"));
    }

    #[test]
    fn appends_block_when_absent() {
        let existing = "# Project\n\nuser stuff\n".to_owned();
        let body = format!("{BEGIN}\nfragment\n{END}");
        let out = inject_block(Some(existing), &body).unwrap();
        assert!(out.contains("user stuff"));
        assert!(out.ends_with("fragment\n<!-- END CAIRN PACK MANUAL -->\n"));
    }
}
