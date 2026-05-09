//! Public expire apply through record WAL.

use crate::error::StoreError;

pub(crate) fn apply_expire() -> Result<(), StoreError> {
    Err(StoreError::Invariant {
        what: "record WAL expire apply is not implemented yet".to_owned(),
    })
}
