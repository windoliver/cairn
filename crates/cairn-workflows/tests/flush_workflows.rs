// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::sync::Arc;

use cairn_core::domain::FlushPlan;
use cairn_test_fixtures::flush_plan::sample_plan;
use cairn_workflows::{
    ConsolidateWorkflow, ExpireWorkflow, PromoteWorkflow, Workflow, WorkflowContext, WorkflowError,
    WorkflowPlanSource,
};

#[derive(Default)]
struct StaticSource;

#[async_trait::async_trait]
impl WorkflowPlanSource for StaticSource {
    async fn plan(
        &self,
        workflow: &'static str,
        _ctx: &WorkflowContext,
    ) -> Result<Vec<FlushPlan>, WorkflowError> {
        Ok(vec![sample_plan(
            match workflow {
                "consolidate" => "01HQZK000000000000000000C1",
                "promote" => "01HQZK000000000000000000P1",
                "expire" => "01HQZK000000000000000000E1",
                _ => "01HQZK000000000000000000X1",
            },
            cairn_core::domain::FlushMode::Autonomous,
        )])
    }
}

#[tokio::test]
async fn concrete_workflows_have_stable_names_and_delegate_planning() {
    let source = Arc::new(StaticSource);
    let ctx = WorkflowContext::default();

    let consolidate = ConsolidateWorkflow::new(source.clone());
    let promote = PromoteWorkflow::new(source.clone());
    let expire = ExpireWorkflow::new(source);

    assert_eq!(consolidate.name(), "consolidate");
    assert_eq!(
        consolidate.plan(&ctx).await.expect("consolidate plans")[0]
            .operation_id
            .0,
        "01HQZK000000000000000000C1"
    );
    assert_eq!(promote.name(), "promote");
    assert_eq!(
        promote.plan(&ctx).await.expect("promote plans")[0]
            .operation_id
            .0,
        "01HQZK000000000000000000P1"
    );
    assert_eq!(expire.name(), "expire");
    assert_eq!(
        expire.plan(&ctx).await.expect("expire plans")[0]
            .operation_id
            .0,
        "01HQZK000000000000000000E1"
    );
}
