//! Pure lint check functions (brief §1223, issue #96 dispatch).
//!
//! Each check is a pure function that takes a record (and any pre-fetched
//! state from outer-layer I/O) and returns an `Option<Finding>`. The
//! dispatch shell tracked in #96 fans these out across active records and
//! aggregates findings into the `lint` verb response. Keeping the checks
//! pure preserves `cairn-core`'s no-I/O invariant — async lookups against
//! `IdentityRegistry`, `MemoryStore`, etc. live one layer up.

pub mod signature;
