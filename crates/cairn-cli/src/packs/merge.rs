//! Block-marker merge helpers for `.claude/settings.json` and `.mcp.json`.
//!
//! Pack-owned entries are tagged with a `_pack` sibling field so re-install
//! can identify and update them without trampling user customisations.

use serde_json::Value;

use crate::packs::manifest::PackError;

const PACK_MARKER_KEY: &str = "_pack";

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

        // Drop any prior pack-owned entries for this pack id (round-trip).
        existing_array.retain(|entry| {
            entry.get(PACK_MARKER_KEY).and_then(Value::as_str) != Some(pack_id_at_version)
        });

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
        let once = merge_settings_json(Value::Null, &pack_payload(), "cairn-claude-code@0.1.0")
            .unwrap();
        let twice = merge_settings_json(
            once.clone(),
            &pack_payload(),
            "cairn-claude-code@0.1.0",
        )
        .unwrap();
        assert_eq!(once, twice);
    }
}
