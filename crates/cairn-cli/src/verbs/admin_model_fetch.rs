//! `cairn admin model fetch [--model <kind>] [--force]`
//!
//! Downloads the configured embedding model from `HuggingFace` Hub into the
//! vault's `.cairn/models/` directory. Idempotent — skips download if the
//! model is already present (unless `--force`).

use std::path::Path;
use std::process::ExitCode;

use cairn_core::config::CairnConfig;
use clap::ArgMatches;
use serde::Serialize;

use super::envelope::{human_error, new_operation_id};

#[derive(Debug, Serialize)]
struct FetchOutput {
    kind: String,
    bytes_downloaded: u64,
    integrity: String,
    already_cached: bool,
}

/// Run `cairn admin model fetch`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path, config: &CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let force = sub.get_flag("force");

    // --model overrides config; default is config.search.embedding_model.
    let kind = config.search.embedding_model;

    let models_root = vault_root.join(".cairn").join("models");

    if force {
        // Remove the cached directory so fetch proceeds unconditionally.
        let cache = cairn_embeddings_local::ModelCache::new(&models_root);
        let dir = cache.model_dir(kind);
        if dir.exists() {
            let rm_result = std::fs::remove_dir_all(&dir);
            if let Err(e) = rm_result {
                let op_id = new_operation_id();
                if json {
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "operation_id": op_id.0,
                            "error": { "code": "IoError", "message": format!("{e}") }
                        })
                    );
                } else {
                    human_error(
                        "admin model fetch",
                        "IoError",
                        &format!("removing cached model dir: {e}"),
                        &op_id,
                    );
                }
                return ExitCode::from(74); // EX_IOERR
            }
        }
    }

    eprintln!(
        "cairn admin model fetch: fetching model '{}' (~25 MB)…",
        kind.as_str()
    );

    let cache = cairn_embeddings_local::ModelCache::new(&models_root);
    let report = match cache.fetch(kind) {
        Ok(r) => r,
        Err(e) => {
            let op_id = new_operation_id();
            let msg = format!("{e}");
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "operation_id": op_id.0,
                        "error": { "code": "FetchError", "message": msg }
                    })
                );
            } else {
                human_error("admin model fetch", "FetchError", &msg, &op_id);
            }
            return ExitCode::from(74); // EX_IOERR
        }
    };

    let out = FetchOutput {
        kind: report.kind.as_str().to_owned(),
        bytes_downloaded: report.bytes_downloaded,
        integrity: report.integrity,
        already_cached: report.already_cached,
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&out)
                .expect("invariant: FetchOutput is always serializable")
        );
    } else if out.already_cached {
        println!(
            "cairn admin model fetch: '{}' already cached (integrity: {})",
            out.kind,
            &out.integrity[..out.integrity.len().min(12)]
        );
    } else {
        println!(
            "cairn admin model fetch: '{}' fetched ({} bytes, integrity: {})",
            out.kind,
            out.bytes_downloaded,
            &out.integrity[..out.integrity.len().min(12)]
        );
    }

    ExitCode::SUCCESS
}
