//! JSON schema for the read-only agent extractor output.

/// JSON schema that agent extractor responses must match.
pub const AGENT_EXTRACTOR_OUTPUT_SCHEMA: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["drafts", "discards", "evidence"],
  "additionalProperties": false,
  "properties": {
    "drafts": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "body", "confidence", "span"],
        "additionalProperties": false,
        "properties": {
          "kind": {
            "enum": [
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
              "knowledge_gap"
            ]
          },
          "body": { "type": "string", "minLength": 1 },
          "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
          "span": { "$ref": "#/$defs/span" },
          "evidence": {
            "type": "array",
            "items": { "$ref": "#/$defs/evidence" }
          }
        }
      }
    },
    "discards": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["reason", "span"],
        "additionalProperties": false,
        "properties": {
          "reason": { "type": "string", "minLength": 1 },
          "span": { "$ref": "#/$defs/span" }
        }
      }
    },
    "evidence": {
      "type": "array",
      "items": { "$ref": "#/$defs/evidence" }
    }
  },
  "$defs": {
    "span": {
      "type": "object",
      "required": ["start", "end"],
      "additionalProperties": false,
      "properties": {
        "start": { "type": "integer", "minimum": 0 },
        "end": { "type": "integer", "minimum": 0 }
      }
    },
    "evidence": {
      "type": "object",
      "required": ["tool", "claim"],
      "additionalProperties": false,
      "properties": {
        "tool": { "type": "string", "minLength": 1 },
        "claim": { "type": "string", "minLength": 1 },
        "record_id": {
          "anyOf": [
            { "type": "string", "minLength": 1 },
            { "type": "null" }
          ]
        }
      }
    }
  }
}"##;
