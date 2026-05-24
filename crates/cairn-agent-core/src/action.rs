use cairn_core::contract::{AgentProviderError, AgentToolCall};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentAction {
    Tool {
        tool: AgentToolCall,
        #[serde(default)]
        args: serde_json::Value,
    },
    Final {
        output: serde_json::Value,
    },
}

pub fn parse_action(value: serde_json::Value) -> Result<AgentAction, AgentProviderError> {
    serde_json::from_value(value).map_err(|source| AgentProviderError::InvalidOutput {
        message: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use cairn_core::contract::AgentProviderError;

    use super::parse_action;

    #[test]
    fn final_action_rejects_tool_fields() {
        let err = parse_action(serde_json::json!({
            "action": "final",
            "output": { "answer": "done" },
            "tool": { "verb": "search", "write_report": false, "persist": false },
            "args": { "query": "extra" }
        }))
        .expect_err("extra final fields must be rejected");

        assert!(matches!(err, AgentProviderError::InvalidOutput { .. }));
    }

    #[test]
    fn tool_action_rejects_unexpected_fields() {
        let err = parse_action(serde_json::json!({
            "action": "tool",
            "tool": { "verb": "search", "write_report": false, "persist": false },
            "args": { "query": "extra" },
            "output": { "unexpected": true }
        }))
        .expect_err("extra tool fields must be rejected");

        assert!(matches!(err, AgentProviderError::InvalidOutput { .. }));
    }
}
