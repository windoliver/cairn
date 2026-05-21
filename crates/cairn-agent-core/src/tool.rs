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
            let Some(query) = first_string(args, &["q", "query"]) else {
                return Err(AgentProviderError::InvalidRequest {
                    message: "search tool requires `q` or `query`".to_string(),
                });
            };
            let argv = vec![
                "search".to_string(),
                "--mode".to_string(),
                "keyword".to_string(),
                query.to_string(),
                "--json".to_string(),
            ];
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
            let mut argv = vec!["lint".to_string()];
            if let Some(plan) = first_string(args, &["plan"]) {
                argv.push("--plan".to_string());
                argv.push(plan.to_string());
            }
            argv.push("--json".to_string());
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

#[cfg(test)]
mod tests {
    use cairn_core::contract::{AgentProviderError, AgentToolCall, CairnVerb};

    use super::build_argv;

    #[test]
    fn search_argv_uses_keyword_mode_query_then_json() {
        let argv = build_argv(
            &AgentToolCall::new(CairnVerb::Search),
            &serde_json::json!({ "query": "prior decision" }),
        )
        .expect("search argv builds");

        assert_eq!(
            argv,
            ["search", "--mode", "keyword", "prior decision", "--json"]
        );
    }

    #[test]
    fn search_argv_accepts_short_query_key() {
        let argv = build_argv(
            &AgentToolCall::new(CairnVerb::Search),
            &serde_json::json!({ "q": "short" }),
        )
        .expect("search argv builds");

        assert_eq!(argv, ["search", "--mode", "keyword", "short", "--json"]);
    }

    #[test]
    fn lint_argv_is_read_only_json_without_dry_run() {
        let argv = build_argv(
            &AgentToolCall::lint_dry(),
            &serde_json::json!({ "target": "ignored" }),
        )
        .expect("lint argv builds");

        assert_eq!(argv, ["lint", "--json"]);
    }

    #[test]
    fn lint_argv_denies_write_report() {
        let err = build_argv(
            &AgentToolCall {
                verb: CairnVerb::Lint,
                write_report: true,
                persist: false,
            },
            &serde_json::json!({}),
        )
        .expect_err("write-report lint is denied");

        assert!(matches!(
            err,
            AgentProviderError::ToolNotAllowed {
                verb: CairnVerb::Lint
            }
        ));
    }
}
