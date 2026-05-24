//! Evolution workflow support (brief §11 and §11.3).

pub mod handler;
pub mod materialize;
pub mod payload;

pub use handler::{EVOLUTION_KIND, EvolutionHandler};
pub use materialize::{EvolutionMaterializeError, MaterializedEvolutionDecision, materialize_run};
pub use payload::EvolutionPayload;
