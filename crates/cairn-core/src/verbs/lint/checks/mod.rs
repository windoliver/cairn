//! Per-check implementations. Each module's `run(&LintInputs) -> Vec<Finding>`.

pub mod actor_chain;
pub mod consent;
pub mod hot_memory;
pub mod hot_memory_walker;
pub mod index_drift;
pub mod malformed;
pub mod projection;
pub mod provenance;
pub mod salience;
pub mod schema;
pub mod taxonomy_conventions;
pub mod trace_reasoning;
pub mod workflow_health;
