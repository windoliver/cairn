//! Lock-table acquisition for §5.6. See module-level doc once `acquire` lands.

mod error;
mod kinds;

pub use error::{
    LockError as LockErrorV2, RetryHint, default_drain_retry, default_fenced_retry,
    default_held_retry,
};
pub use kinds::{LockMode, LockScope, ResourceKey};

// Transitional re-exports — the legacy surface stays callable while we
// migrate the new typed API in. Tasks 3–6 add first-class implementations;
// Task 9 migrates the only in-tree caller and Step 8 there removes this
// module entirely.
pub use crate::locks_legacy::{LockError, LockHandle, acquire_exclusive, release_by_holder};
