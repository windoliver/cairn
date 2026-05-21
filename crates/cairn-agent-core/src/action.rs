use cairn_core::contract::{AgentProviderError, AgentToolCall};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
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
