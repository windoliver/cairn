//! Temporary scheduler handler for skill emission jobs.

use cairn_core::contract::job_store::{JobKind, JobPayload};

use crate::scheduler::{HandlerOutcome, JobHandler};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const SKILLIFY_KIND: &str = "skillify.emit";

/// Temporary payload-validating handler until materialization lands.
#[derive(Default)]
pub struct SkillifyHandler;

#[async_trait::async_trait]
impl JobHandler for SkillifyHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(SKILLIFY_KIND)
    }

    async fn handle(&self, payload: &JobPayload) -> HandlerOutcome {
        match super::SkillifyPayload::from_bytes(payload) {
            Ok(_) => HandlerOutcome::Done,
            Err(e) => {
                HandlerOutcome::validation_permanent(format!("invalid skillify payload: {e}"))
            }
        }
    }
}
