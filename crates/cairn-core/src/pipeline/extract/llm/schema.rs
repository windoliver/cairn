//! JSON-schema constant for the `LLMExtractor` structured output (spec §5.1).
//!
//! The schema is built once at first access and cached in a `OnceLock`.
//! Building it requires `jsonschema::validator_for`, which is fallible
//! in principle but always succeeds for the constant literal here — the
//! `valid_draft_response_passes_schema` test guarantees that. The
//! `expect("invariant: …")` in `validator()` is therefore permitted by
//! CLAUDE.md §6.2.

use std::sync::OnceLock;

use jsonschema::Validator;
use serde_json::Value;

/// The 19 wire-form `MemoryKind` strings the schema's `kind` enum
/// accepts. Hand-listed to avoid pulling a new workspace dependency
/// (e.g. `strum`); the `schema_kind_enum_round_trips_via_parse` test
/// asserts byte-for-byte agreement with `MemoryKind::parse`.
pub const SCHEMA_KIND_ENUM: &[&str] = &[
    "user",
    "feedback",
    "project",
    "reference",
    "fact",
    "belief",
    "opinion",
    "event",
    "entity",
    "workflow",
    "rule",
    "strategy_success",
    "strategy_failure",
    "trace",
    "reasoning",
    "playbook",
    "sensor_observation",
    "user_signal",
    "knowledge_gap",
];

/// Lazily-built JSON schema value mirroring spec §5.1 exactly.
pub fn schema_value() -> &'static Value {
    static V: OnceLock<Value> = OnceLock::new();
    V.get_or_init(|| {
        serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["items"],
            "additionalProperties": false,
            "properties": {
                "items": {
                    "type": "array",
                    "maxItems": 16,
                    "items": {
                        "oneOf": [
                            {
                                "type": "object",
                                "required": ["type", "kind", "body", "confidence", "source"],
                                "additionalProperties": false,
                                "properties": {
                                    "type": { "const": "draft" },
                                    "kind": { "enum": SCHEMA_KIND_ENUM },
                                    "body": { "type": "string", "minLength": 1, "maxLength": 4096 },
                                    "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                                    "entities": {
                                        "type": "array",
                                        "maxItems": 32,
                                        "items": { "type": "string", "maxLength": 128 }
                                    },
                                    "evidence": { "type": "string", "maxLength": 512 },
                                    "source": {
                                        "type": "object",
                                        "required": ["region_id", "text_excerpt"],
                                        "additionalProperties": false,
                                        "properties": {
                                            "region_id": { "type": "integer", "minimum": 0 },
                                            "text_excerpt": { "type": "string", "minLength": 16, "maxLength": 1024 }
                                        }
                                    }
                                }
                            },
                            {
                                "type": "object",
                                "required": ["type", "reason", "source"],
                                "additionalProperties": false,
                                "properties": {
                                    "type": { "const": "discard" },
                                    "reason": {
                                        "enum": ["volatile", "tool_lookup", "competing_source", "low_salience", "other"]
                                    },
                                    "evidence": { "type": "string", "maxLength": 512 },
                                    "source": {
                                        "$ref": "#/properties/items/items/oneOf/0/properties/source"
                                    }
                                }
                            }
                        ]
                    }
                }
            }
        })
    })
}

/// Compiled JSON-schema validator. The schema is a constant controlled
/// by us; the build-time test below ensures validity.
#[allow(clippy::expect_used)] // invariant: schema literal is well-formed; tested.
pub fn validator() -> &'static Validator {
    static V: OnceLock<Validator> = OnceLock::new();
    V.get_or_init(|| {
        jsonschema::validator_for(schema_value())
            .expect("invariant: SCHEMA_VALUE is a well-formed JSON schema")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::taxonomy::MemoryKind;
    use serde_json::json;

    #[test]
    fn valid_draft_response_passes_schema() {
        let v = json!({
            "items": [
                {
                    "type": "draft",
                    "kind": "user",
                    "body": "user prefers tabs over spaces",
                    "confidence": 0.92,
                    "entities": [],
                    "evidence": "explicit preference statement",
                    "source": { "region_id": 0, "text_excerpt": "I prefer tabs over spaces" }
                }
            ]
        });
        assert!(
            validator().validate(&v).is_ok(),
            "schema rejected valid draft: {:?}",
            validator().validate(&v).err()
        );
    }

    #[test]
    fn valid_discard_response_passes_schema() {
        let v = json!({
            "items": [
                {
                    "type": "discard",
                    "reason": "volatile",
                    "evidence": "transient lookup",
                    "source": { "region_id": 0, "text_excerpt": "what's the weather today" }
                }
            ]
        });
        assert!(validator().validate(&v).is_ok());
    }

    #[test]
    fn valid_empty_items_passes_schema() {
        let v = json!({ "items": [] });
        assert!(validator().validate(&v).is_ok());
    }

    #[test]
    fn missing_required_kind_fails() {
        let v = json!({ "items": [{ "type": "draft", "body": "x", "confidence": 0.5,
                                    "source": {"region_id": 0, "text_excerpt": "sixteen-or-more!"} }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn unknown_kind_fails() {
        let v = json!({ "items": [{
            "type": "draft", "kind": "totally-made-up", "body": "x", "confidence": 0.5,
            "source": {"region_id": 0, "text_excerpt": "sixteen-or-more!"}
        }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn excerpt_under_16_chars_fails() {
        let v = json!({ "items": [{
            "type": "draft", "kind": "user", "body": "x", "confidence": 0.5,
            "source": {"region_id": 0, "text_excerpt": "short"}
        }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn confidence_above_one_fails() {
        let v = json!({ "items": [{
            "type": "draft", "kind": "user", "body": "x", "confidence": 1.5,
            "source": {"region_id": 0, "text_excerpt": "sixteen-or-more!"}
        }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn unknown_discard_reason_fails() {
        let v = json!({ "items": [{
            "type": "discard", "reason": "made_up_reason",
            "source": {"region_id": 0, "text_excerpt": "sixteen-or-more!"}
        }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn additional_property_on_draft_fails() {
        let v = json!({ "items": [{
            "type": "draft", "kind": "user", "body": "x", "confidence": 0.5,
            "source": {"region_id": 0, "text_excerpt": "sixteen-or-more!"},
            "uninvited_field": "no"
        }] });
        assert!(validator().validate(&v).is_err());
    }

    #[test]
    fn schema_kind_enum_round_trips_via_parse() {
        // Every entry in SCHEMA_KIND_ENUM must be parseable as a MemoryKind,
        // and every MemoryKind's as_str() must appear in SCHEMA_KIND_ENUM.
        // This catches drift if MemoryKind grows a variant the schema doesn't know about.
        for s in SCHEMA_KIND_ENUM {
            assert!(
                MemoryKind::parse(s).is_ok(),
                "SCHEMA_KIND_ENUM entry not parseable: {s}"
            );
        }
        // Reverse: ensure every MemoryKind appears. Without an ALL constant, list
        // them explicitly here and check coverage is exactly SCHEMA_KIND_ENUM.len().
        let all: &[MemoryKind] = &[
            MemoryKind::User,
            MemoryKind::Feedback,
            MemoryKind::Project,
            MemoryKind::Reference,
            MemoryKind::Fact,
            MemoryKind::Belief,
            MemoryKind::Opinion,
            MemoryKind::Event,
            MemoryKind::Entity,
            MemoryKind::Workflow,
            MemoryKind::Rule,
            MemoryKind::StrategySuccess,
            MemoryKind::StrategyFailure,
            MemoryKind::Trace,
            MemoryKind::Reasoning,
            MemoryKind::Playbook,
            MemoryKind::SensorObservation,
            MemoryKind::UserSignal,
            MemoryKind::KnowledgeGap,
        ];
        assert_eq!(
            SCHEMA_KIND_ENUM.len(),
            all.len(),
            "SCHEMA_KIND_ENUM length ({}) != MemoryKind variant count ({})",
            SCHEMA_KIND_ENUM.len(),
            all.len()
        );
        for k in all {
            assert!(
                SCHEMA_KIND_ENUM.contains(&k.as_str()),
                "MemoryKind::{k:?} ({}) missing from SCHEMA_KIND_ENUM",
                k.as_str()
            );
        }
    }
}
