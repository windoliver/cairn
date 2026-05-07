//! Lock-table acquisition for §5.6. See module-level doc once `acquire` lands.

mod acquire;
mod error;
mod fence;
mod handle;
mod incarnation;
mod kinds;

pub use acquire::acquire;
pub use error::{
    LockError as LockErrorV2, RetryHint, default_drain_retry, default_fenced_retry,
    default_held_retry,
};
pub use fence::{
    clear as clear_reader_fence, register_pending as register_reader_fence,
    wait_for_drain as wait_for_reader_drain,
};
pub use handle::{LockHandleV2, release_by_holder_v2};
pub use incarnation::{current_incarnation, init_incarnation};
pub use kinds::{LockMode, LockScope, ResourceKey};

// Transitional re-exports — the legacy surface stays callable while we
// migrate the new typed API in. Tasks 3–6 add first-class implementations;
// Task 9 migrates the only in-tree caller and Step 8 there removes this
// module entirely.
pub use crate::locks_legacy::{LockError, LockHandle, acquire_exclusive, release_by_holder};
