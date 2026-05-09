//! Public upsert apply through record WAL.

use crate::error::StoreError;

pub(crate) fn apply_upsert() -> Result<(), StoreError> {
    Err(StoreError::Invariant {
        what: "record WAL upsert apply is not implemented yet".to_owned(),
    })
}
