//! `cairn reindex` handler for Nexus projection rebuilds.

use std::process::ExitCode;
use std::time::Duration;

use cairn_core::config::StoreKind;
use cairn_core::domain::projection::ProjectionTarget;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};
use crate::nexus::projection::{ProjectionApplyRequest, ProjectionClient};

use super::envelope::{emit_json, new_operation_id};

/// Run `cairn reindex`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    if !sub.get_flag("from-db") {
        eprintln!("cairn reindex: --from-db is required");
        return ExitCode::from(64);
    }

    let vault_path = match std::env::current_dir() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("cairn reindex: failed to resolve current directory: {err}");
            return ExitCode::FAILURE;
        }
    };

    let active = match config::load(&vault_path, &CliOverrides::default()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("cairn reindex: {err:#}");
            return ExitCode::from(78);
        }
    };

    if active.store.kind != StoreKind::NexusSandbox {
        eprintln!("cairn reindex: requires store.kind: nexus-sandbox");
        return ExitCode::from(78);
    }

    let target = ProjectionTarget::Bm25sLexical.as_key();
    let request = ProjectionApplyRequest {
        operation_id: new_operation_id().0,
        target: target.clone(),
        items: vec![],
    };
    let client = ProjectionClient::new(
        active.store.nexus.endpoint.clone(),
        "/projection/apply".to_owned(),
        Duration::from_millis(active.store.nexus.health_timeout_ms),
    );

    let response = match client.apply(&request) {
        Ok(response) => response,
        Err(err) => {
            eprintln!("cairn reindex: {err}");
            return ExitCode::from(69);
        }
    };
    let item_count = response.items.len();

    if sub.get_flag("json") {
        emit_json(&serde_json::json!({
            "target": target,
            "items": item_count,
        }));
    } else {
        println!("target: {target}");
        println!("items: {item_count}");
    }

    ExitCode::SUCCESS
}
