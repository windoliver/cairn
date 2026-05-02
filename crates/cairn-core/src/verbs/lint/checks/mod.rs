//! Per-check implementations. Each module's `run(&LintInputs) -> Vec<Finding>`.

pub mod actor_chain;
pub mod consent_deferred;
pub mod hot_memory;
pub mod index_drift;
pub mod malformed;
pub mod provenance;
pub mod schema;
