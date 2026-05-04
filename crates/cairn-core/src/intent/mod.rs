//! Trust-boundary verification helpers — the canonical signed payload
//! builder and the typed error enum.

pub mod canonical_envelope;
pub mod verify_error;

pub use verify_error::{ExpiryReason, VerifyError};
