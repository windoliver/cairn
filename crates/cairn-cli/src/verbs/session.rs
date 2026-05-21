//! `cairn session ...` management commands.

use std::path::Path;
use std::process::ExitCode;

use cairn_core::domain::{MergeStrategy, SessionId, SessionTree};
use clap::ArgMatches;
use serde::Serialize;

/// Build the session management command tree.
#[must_use]
pub fn command() -> clap::Command {
    clap::Command::new("session")
        .about("Inspect session-tree metadata (brief §5.7)")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            clap::Command::new("tree")
                .about("Print the session tree containing SESSION")
                .arg(
                    clap::Arg::new("session")
                        .value_name("SESSION")
                        .required(true)
                        .help("Session id whose tree should be inspected"),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .action(clap::ArgAction::SetTrue)
                        .help("Emit scriptable JSON"),
                ),
        )
}

/// Run `cairn session ...`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    match sub.subcommand() {
        Some(("tree", tree)) => run_tree(tree, vault_root),
        _ => unreachable!("session subcommand_required(true) ensures a subcommand"),
    }
}

fn run_tree(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = sub.get_flag("json");
    let Some(raw_session) = sub.get_one::<String>("session") else {
        eprintln!("cairn session tree: missing SESSION");
        return ExitCode::from(64);
    };
    let session_id = match SessionId::parse(raw_session) {
        Ok(session_id) => session_id,
        Err(e) => {
            emit_tree_error(json, &format!("invalid session id: {e}"));
            return ExitCode::from(64);
        }
    };
    let db_path = vault_root.join(".cairn/cairn.db");
    if !db_path.is_file() {
        emit_tree_error(
            json,
            &format!(
                "no Cairn vault at {}: .cairn/cairn.db is missing",
                vault_root.display()
            ),
        );
        return ExitCode::from(78);
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            emit_tree_error(json, &format!("tokio init: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let tree = match rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path).await?;
        store.get_session_tree(&session_id).await
    }) {
        Ok(Some(tree)) => tree,
        Ok(None) => {
            emit_tree_error(json, &format!("session {raw_session} was not found"));
            return ExitCode::from(66);
        }
        Err(e) => {
            emit_tree_error(json, &format!("session tree: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let data = match SessionTreeInspect::from_tree(&tree) {
        Ok(data) => data,
        Err(e) => {
            emit_tree_error(json, &format!("session tree: {e}"));
            return ExitCode::FAILURE;
        }
    };
    if json {
        match serde_json::to_string_pretty(&data) {
            Ok(body) => println!("{body}"),
            Err(e) => {
                emit_tree_error(false, &format!("serialize session tree: {e}"));
                return ExitCode::FAILURE;
            }
        }
    } else {
        emit_tree_human(&data);
    }
    ExitCode::SUCCESS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTreeInspect {
    root: String,
    nodes: Vec<SessionTreeInspectNode>,
    merges: Vec<SessionTreeInspectMerge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTreeInspectNode {
    id: String,
    parent_id: Option<String>,
    branch_kind: Option<String>,
    at_turn_id: Option<String>,
    tool_call_id: Option<String>,
    children: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTreeInspectMerge {
    source: String,
    destination: String,
    strategy: String,
    summary_record_id: Option<String>,
    first_turn_id: Option<String>,
    last_turn_id: Option<String>,
    applied_at_turn_id: String,
}

impl SessionTreeInspect {
    fn from_tree(tree: &SessionTree) -> Result<Self, cairn_core::domain::SessionTreeError> {
        let ids = tree.subtree_preorder(tree.root())?;
        let mut nodes = Vec::with_capacity(ids.len());
        for id in ids {
            let parent = tree.parent(&id)?;
            nodes.push(SessionTreeInspectNode {
                id: id.as_str().to_owned(),
                parent_id: parent.map(|p| p.session_id.as_str().to_owned()),
                branch_kind: parent.map(|p| branch_kind_key(p.kind).to_owned()),
                at_turn_id: parent.map(|p| p.at_turn_id.clone()),
                tool_call_id: parent.and_then(|p| p.tool_call_id.clone()),
                children: tree
                    .children(&id)?
                    .into_iter()
                    .map(|child| child.as_str().to_owned())
                    .collect(),
            });
        }
        let merges = tree
            .merges()
            .iter()
            .map(|merge| {
                let (strategy, summary_record_id, first_turn_id, last_turn_id) =
                    merge_strategy_fields(&merge.strategy);
                SessionTreeInspectMerge {
                    source: merge.source.as_str().to_owned(),
                    destination: merge.destination.as_str().to_owned(),
                    strategy,
                    summary_record_id,
                    first_turn_id,
                    last_turn_id,
                    applied_at_turn_id: merge.applied_at_turn_id.clone(),
                }
            })
            .collect();
        Ok(Self {
            root: tree.root().as_str().to_owned(),
            nodes,
            merges,
        })
    }
}

fn branch_kind_key(kind: cairn_core::domain::BranchKind) -> &'static str {
    match kind {
        cairn_core::domain::BranchKind::Fork => "fork",
        cairn_core::domain::BranchKind::Clone => "clone",
        cairn_core::domain::BranchKind::ToolSpawned => "tool_spawned",
        _ => "unknown",
    }
}

fn merge_strategy_fields(
    strategy: &MergeStrategy,
) -> (String, Option<String>, Option<String>, Option<String>) {
    match strategy {
        MergeStrategy::ReasoningSummary { summary_record_id } => (
            "reasoning_summary".to_owned(),
            Some(summary_record_id.as_str().to_owned()),
            None,
            None,
        ),
        MergeStrategy::ControlledSplice {
            first_turn_id,
            last_turn_id,
        } => (
            "controlled_splice".to_owned(),
            None,
            Some(first_turn_id.clone()),
            Some(last_turn_id.clone()),
        ),
        _ => ("unknown".to_owned(), None, None, None),
    }
}

fn emit_tree_error(json: bool, message: &str) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "error": {
                    "code": "session_tree_error",
                    "message": message
                }
            })
        );
    } else {
        eprintln!("cairn session tree: {message}");
    }
}

fn emit_tree_human(data: &SessionTreeInspect) {
    println!("root: {}", data.root);
    for node in &data.nodes {
        let parent = node.parent_id.as_deref().unwrap_or("-");
        let branch = node.branch_kind.as_deref().unwrap_or("root");
        println!("node: {} parent={parent} branch={branch}", node.id);
    }
    for merge in &data.merges {
        println!(
            "merge: {} -> {} strategy={} at={}",
            merge.source, merge.destination, merge.strategy, merge.applied_at_turn_id
        );
    }
}
