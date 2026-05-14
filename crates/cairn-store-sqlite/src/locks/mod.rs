//! Lock-table acquisition for §5.6. See module-level doc once `acquire` lands.

mod acquire;
mod error;
mod fence;
mod handle;
mod incarnation;
mod kinds;

pub use acquire::acquire;
pub use error::{
    LockError, RetryHint, default_drain_retry, default_fenced_retry, default_held_retry,
};
pub use fence::{
    clear as clear_reader_fence, register_pending as register_reader_fence,
    wait_for_drain as wait_for_reader_drain,
};
pub use handle::{LockHandle, release_by_holder};
pub use incarnation::{current_incarnation, current_incarnation_owner, init_incarnation};
pub use kinds::{LockMode, LockScope, ResourceKey};
