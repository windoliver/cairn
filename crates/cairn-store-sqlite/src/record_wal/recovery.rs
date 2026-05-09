//! Record WAL recovery registry.

use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RecordWalRegistry {
    incarnation: Arc<str>,
}

impl RecordWalRegistry {
    #[must_use]
    pub fn new(incarnation: Arc<str>) -> Self {
        Self { incarnation }
    }

    #[must_use]
    pub fn incarnation(&self) -> &Arc<str> {
        &self.incarnation
    }
}
