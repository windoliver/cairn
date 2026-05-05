//! Per-check implementations. Each module's `run(&LintInputs) -> Vec<Finding>`.

pub mod actor_chain;
pub mod consent;
pub mod hot_memory;
pub mod index_drift;
pub mod malformed;
pub mod projection;
pub mod provenance;
pub mod schema;
