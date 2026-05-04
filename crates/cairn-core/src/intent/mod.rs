//! Trust-boundary verification helpers — the canonical signed payload
//! builder (added in Task 4) and the typed error enum.

pub mod verify_error;

pub use verify_error::{ExpiryReason, VerifyError};
