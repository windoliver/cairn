//! `JobHandler` trait + `HandlerRegistry`. The scheduler dispatches
//! leased jobs by [`cairn_core::contract::job_store::JobKind`] to one
//! registered handler.

use std::collections::HashMap;
use std::sync::Arc;

use cairn_core::contract::job_store::{JobKind, JobPayload};

/// Outcome a handler returns after running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerOutcome {
    /// Handler succeeded; scheduler calls `complete`.
    Done,
    /// Retryable failure; scheduler calls `fail(Retry)`.
    Retry {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
    },
    /// Permanent failure; scheduler calls `fail(Permanent)`.
    Permanent {
        /// Error message persisted into `workflow_jobs.last_error`.
        reason: String,
    },
}

/// Errors from registry plumbing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HandlerDispatchError {
    /// No handler registered for the leased job's kind.
    #[error("no handler registered for kind {0}")]
    Unknown(JobKind),
}

/// One workflow handler.
#[async_trait::async_trait]
pub trait JobHandler: Send + Sync + 'static {
    /// Stable kind discriminator. Matches `JobKind` on enqueue.
    fn kind(&self) -> JobKind;
    /// Run the handler with the opaque payload bytes.
    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome;
}

/// Map of `JobKind → Arc<dyn JobHandler>`. Cheap to clone.
#[derive(Clone, Default)]
pub struct HandlerRegistry {
    handlers: Arc<HashMap<JobKind, Arc<dyn JobHandler>>>,
}

/// Builder for [`HandlerRegistry`].
#[derive(Default)]
pub struct HandlerRegistryBuilder {
    handlers: HashMap<JobKind, Arc<dyn JobHandler>>,
}

impl HandlerRegistryBuilder {
    /// Register a handler. Panics in debug if `kind` collides — kinds
    /// must be unique by construction; this is a programmer-error guard,
    /// not a runtime concern.
    #[must_use]
    pub fn with(mut self, handler: Arc<dyn JobHandler>) -> Self {
        let k = handler.kind();
        debug_assert!(!self.handlers.contains_key(&k),
            "duplicate handler for kind {k:?}");
        self.handlers.insert(k, handler);
        self
    }
    /// Freeze the builder into a shareable registry.
    #[must_use]
    pub fn build(self) -> HandlerRegistry {
        HandlerRegistry { handlers: Arc::new(self.handlers) }
    }
}

impl HandlerRegistry {
    /// Look up a handler.
    ///
    /// # Errors
    /// [`HandlerDispatchError::Unknown`] when no handler matches.
    pub fn lookup(&self, kind: &JobKind) -> Result<Arc<dyn JobHandler>, HandlerDispatchError> {
        self.handlers
            .get(kind)
            .cloned()
            .ok_or_else(|| HandlerDispatchError::Unknown(kind.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    #[async_trait::async_trait]
    impl JobHandler for Noop {
        fn kind(&self) -> JobKind { JobKind::new("noop") }
        async fn handle(&self, _: &JobPayload) -> HandlerOutcome { HandlerOutcome::Done }
    }

    #[tokio::test]
    async fn registry_dispatches_by_kind() {
        let reg = HandlerRegistryBuilder::default()
            .with(Arc::new(Noop))
            .build();
        let h = reg.lookup(&JobKind::new("noop")).unwrap();
        assert_eq!(h.handle(&Vec::new()).await, HandlerOutcome::Done);
    }

    #[test]
    fn unknown_kind_errors() {
        let reg = HandlerRegistry::default();
        let result = reg.lookup(&JobKind::new("missing"));
        assert!(result.is_err());
        assert!(matches!(
            result.err().expect("is_err() asserted above"),
            HandlerDispatchError::Unknown(_)
        ));
    }
}
