use std::process::Command;

use cairn_core::contract::{AgentProviderError, AgentToolCall, CairnVerb};

/// Result from executing an admitted tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecution {
    /// JSON tool output returned to the model as compact history.
    pub output: serde_json::Value,
    /// Provider-defined budget charge for this execution.
    pub cost_units: u64,
}

/// Executes admitted agent tool calls.
#[async_trait::async_trait]
pub trait AgentToolExecutor: Send + Sync {
    /// Execute a policy-admitted tool call with model-provided JSON arguments.
    async fn execute(
        &self,
        call: &AgentToolCall,
        args: serde_json::Value,
    ) -> Result<ToolExecution, AgentProviderError>;
}

/// Read-only Cairn CLI subprocess executor.
#[derive(Debug, Clone)]
pub struct CairnCliToolExecutor {
    command: String,
}

impl CairnCliToolExecutor {
    /// Build an executor for a `cairn`-compatible command.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
        }
    }
}

impl Default for CairnCliToolExecutor {
    fn default() -> Self {
        Self::new("cairn")
    }
}

#[async_trait::async_trait]
impl AgentToolExecutor for CairnCliToolExecutor {
    async fn execute(
        &self,
        call: &AgentToolCall,
        args: serde_json::Value,
    ) -> Result<ToolExecution, AgentProviderError> {
        let argv = build_argv(call, &args)?;
        let command = self.command.clone();
        let output = tokio::task::spawn_blocking(move || Command::new(command).args(argv).output())
            .await
            .map_err(|source| AgentProviderError::ProviderUnavailable {
                message: format!("tool task join failed: {source}"),
            })?
            .map_err(|source| AgentProviderError::ProviderUnavailable {
                message: format!("failed to execute cairn cli: {source}"),
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Err(AgentProviderError::ProviderUnavailable {
                message: format!(
                    "cairn cli exited with status {}: {}",
                    output
                        .status
                        .code()
                        .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                    stderr.trim()
                ),
            });
        }

        let parsed = serde_json::from_str(&stdout).unwrap_or_else(|_| {
            serde_json::json!({
                "stdout": stdout,
            })
        });

        Ok(ToolExecution {
            output: parsed,
            cost_units: 1,
        })
    }
}

fn build_argv(
    call: &AgentToolCall,
    args: &serde_json::Value,
) -> Result<Vec<String>, AgentProviderError> {
    match call.verb {
        CairnVerb::Search => {
            let mut argv = vec!["search".to_string(), "--json".to_string()];
            if let Some(query) = first_string(args, &["q", "query"]) {
                argv.push(query.to_string());
            }
            Ok(argv)
        }
        CairnVerb::Retrieve => {
            let mut argv = vec!["retrieve".to_string(), "--json".to_string()];
            if let Some(id) = first_string(args, &["id", "record_id", "key"]) {
                argv.push(id.to_string());
            }
            Ok(argv)
        }
        CairnVerb::Lint if !call.write_report && !call.persist => {
            let mut argv = vec![
                "lint".to_string(),
                "--dry-run".to_string(),
                "--json".to_string(),
            ];
            if let Some(target) = first_string(args, &["target", "path"]) {
                argv.push(target.to_string());
            }
            Ok(argv)
        }
        CairnVerb::Lint => Err(AgentProviderError::ToolNotAllowed {
            verb: CairnVerb::Lint,
        }),
        verb => Err(AgentProviderError::ToolNotAllowed { verb }),
    }
}

fn first_string<'a>(value: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .filter(|s| !s.trim().is_empty())
}
