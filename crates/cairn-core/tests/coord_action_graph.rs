//! Pure coordination action graph behavior.

use cairn_core::coord::{ActionGraph, ActionNode, ActionStatus, CoordError, LeaseTable};
use cairn_core::domain::{Identity, TargetId};

fn id(suffix: &str) -> TargetId {
    TargetId::parse(format!("01JTS6R4J70000000000000{suffix}")).unwrap()
}

fn actor(name: &str) -> Identity {
    Identity::parse(format!("agt:{name}:v1")).unwrap()
}

fn action(
    suffix: &str,
    depends_on: Vec<TargetId>,
    priority: i32,
    created_at_ms: u64,
) -> ActionNode {
    ActionNode {
        id: id(suffix),
        title: format!("action {suffix}"),
        depends_on,
        priority,
        created_at_ms,
        status: ActionStatus::Pending,
    }
}

#[test]
fn rejects_circular_dependencies_at_create_time() {
    let root = id("000");
    let child = id("001");
    let grandchild = id("002");
    let graph = ActionGraph::try_new(vec![
        ActionNode {
            id: root.clone(),
            title: "root".into(),
            depends_on: vec![grandchild.clone()],
            priority: 0,
            created_at_ms: 0,
            status: ActionStatus::Pending,
        },
        ActionNode {
            id: child.clone(),
            title: "child".into(),
            depends_on: vec![root],
            priority: 0,
            created_at_ms: 1,
            status: ActionStatus::Pending,
        },
        ActionNode {
            id: grandchild,
            title: "grandchild".into(),
            depends_on: vec![child],
            priority: 0,
            created_at_ms: 2,
            status: ActionStatus::Pending,
        },
    ]);

    assert!(
        matches!(graph, Err(CoordError::CycleDetected { .. })),
        "cycle must be rejected; got {graph:?}"
    );
}

#[test]
fn frontier_returns_unblocked_top_n_by_priority_then_age() {
    let completed = ActionNode {
        status: ActionStatus::Completed,
        ..action("010", vec![], 0, 0)
    };
    let completed_id = completed.id.clone();
    let incomplete_dep = action("012", vec![], 0, 1);
    let blocked = action("011", vec![incomplete_dep.id.clone()], 100, 2);
    let high_new = action("013", vec![completed_id.clone()], 10, 50);
    let high_old = action("014", vec![completed_id], 10, 10);
    let low_old = action("015", vec![], 1, 0);

    let graph = ActionGraph::try_new(vec![
        completed,
        incomplete_dep,
        blocked,
        high_new,
        high_old,
        low_old,
    ])
    .expect("acyclic graph");
    let leases = LeaseTable::default();
    let frontier = graph.frontier(3, &leases, None, 0);
    let ids: Vec<_> = frontier.iter().map(|node| node.id.as_str()).collect();

    assert_eq!(
        ids,
        [
            "01JTS6R4J70000000000000014",
            "01JTS6R4J70000000000000013",
            "01JTS6R4J70000000000000015",
        ]
    );
}

#[test]
fn frontier_handles_100_action_graph_under_10ms() {
    let mut actions = Vec::new();
    for i in 0..100 {
        let suffix = format!("{i:03}");
        let depends_on = if i == 0 { vec![] } else { vec![id("000")] };
        let status = if i == 0 {
            ActionStatus::Completed
        } else {
            ActionStatus::Pending
        };
        actions.push(ActionNode {
            status,
            ..action(&suffix, depends_on, i, u64::try_from(i).unwrap())
        });
    }

    let graph = ActionGraph::try_new(actions).expect("acyclic graph");
    let start = std::time::Instant::now();
    let leases = LeaseTable::default();
    let frontier = graph.frontier(5, &leases, None, 0);
    let elapsed = start.elapsed();

    assert_eq!(frontier.len(), 5);
    assert!(
        elapsed < std::time::Duration::from_millis(10),
        "frontier over 100 actions should be cheap; elapsed={elapsed:?}"
    );
    assert_eq!(frontier[0].id.as_str(), "01JTS6R4J70000000000000099");
}

#[test]
fn rejects_dangling_dependencies_at_create_time() {
    let action = action("030", vec![id("031")], 0, 0);
    let expected_action_id = action.id.clone();
    let expected_missing_id = action.depends_on[0].clone();

    let graph = ActionGraph::try_new(vec![action]);

    assert!(
        matches!(
            graph,
            Err(CoordError::MissingDependency {
                ref action_id,
                ref dependency_id,
            }) if action_id == &expected_action_id && dependency_id == &expected_missing_id
        ),
        "dangling dependency must be rejected; got {graph:?}"
    );
}

#[test]
fn frontier_excludes_actions_leased_by_another_actor() {
    let leased_action = action("040", vec![], 100, 0);
    let other = action("041", vec![], 10, 1);
    let action_id = leased_action.id.clone();
    let graph = ActionGraph::try_new(vec![leased_action, other]).expect("acyclic graph");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(action_id, actor("holder"), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let frontier = graph.frontier(5, &leases, Some(&actor("requester")), 1_001);

    assert_eq!(frontier.len(), 1);
    assert_eq!(frontier[0].id, id("041"));
}

#[test]
fn frontier_includes_pending_actions_leased_by_requesting_actor_for_resume() {
    let action = action("042", vec![], 100, 0);
    let action_id = action.id.clone();
    let holder = actor("holder");
    let graph = ActionGraph::try_new(vec![action]).expect("acyclic graph");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(action_id.clone(), holder.clone(), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let frontier = graph.frontier(5, &leases, Some(&holder), 1_001);

    assert_eq!(frontier.len(), 1);
    assert_eq!(frontier[0].id, action_id);
}

#[test]
fn frontier_includes_actions_after_lease_expires() {
    let action = action("043", vec![], 100, 0);
    let action_id = action.id.clone();
    let holder = actor("holder");
    let graph = ActionGraph::try_new(vec![action]).expect("acyclic graph");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(action_id.clone(), holder, 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let frontier = graph.frontier(5, &leases, Some(&actor("requester")), 11_000);

    assert_eq!(frontier.len(), 1);
    assert_eq!(frontier[0].id, action_id);
}

#[test]
fn frontier_returns_requesters_in_progress_leased_work_before_new_work() {
    let in_progress = ActionNode {
        status: ActionStatus::InProgress,
        ..action("044", vec![], 10, 0)
    };
    let pending = action("045", vec![], 100, 1);
    let in_progress_id = in_progress.id.clone();
    let holder = actor("holder");
    let graph = ActionGraph::try_new(vec![in_progress, pending]).expect("acyclic graph");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(in_progress_id.clone(), holder.clone(), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let frontier = graph.frontier(5, &leases, Some(&holder), 1_001);

    assert_eq!(frontier[0].id, in_progress_id);
}

#[test]
fn action_status_transition_rejects_completed_to_pending() {
    let action_id = id("020");
    let mut graph = ActionGraph::try_new(vec![ActionNode {
        id: action_id.clone(),
        status: ActionStatus::Completed,
        title: "done".into(),
        depends_on: vec![],
        priority: 0,
        created_at_ms: 0,
    }])
    .expect("acyclic graph");
    let worker = actor("worker");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(action_id.clone(), worker.clone(), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let err = graph
        .transition_action(
            &action_id,
            ActionStatus::Pending,
            "reopen".into(),
            &leases,
            &worker,
            1_001,
        )
        .expect_err("completed actions must not reopen to pending");

    assert!(matches!(
        err,
        CoordError::InvalidStatusTransition {
            from: ActionStatus::Completed,
            to: ActionStatus::Pending,
            ..
        }
    ));
    assert_eq!(
        graph.node(&action_id).expect("action exists").status,
        ActionStatus::Completed
    );
}

#[test]
fn action_status_transition_requires_completed_dependencies_to_start() {
    let incomplete = action("021", vec![], 0, 0);
    let blocked = action("022", vec![incomplete.id.clone()], 0, 1);
    let blocked_id = blocked.id.clone();
    let mut graph = ActionGraph::try_new(vec![incomplete, blocked]).expect("acyclic graph");
    let mut leases = LeaseTable::default();
    let worker = actor("worker");
    assert_eq!(
        leases.acquire(blocked_id.clone(), worker.clone(), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    let err = graph
        .transition_action(
            &blocked_id,
            ActionStatus::InProgress,
            "claim".into(),
            &leases,
            &worker,
            1_001,
        )
        .expect_err("action with incomplete dependency cannot start");

    assert!(matches!(err, CoordError::DependencyNotCompleted { .. }));
}

#[test]
fn action_status_transition_requires_live_lease_to_complete() {
    let mut graph = ActionGraph::try_new(vec![ActionNode {
        status: ActionStatus::InProgress,
        ..action("023", vec![], 0, 0)
    }])
    .expect("acyclic graph");
    let action_id = id("023");
    let leases = LeaseTable::default();

    let err = graph
        .transition_action(
            &action_id,
            ActionStatus::Completed,
            "done".into(),
            &leases,
            &actor("worker"),
            1_000,
        )
        .expect_err("completion requires a live lease");

    assert!(matches!(err, CoordError::LeaseRequired { .. }));
}

#[test]
fn action_status_transition_requires_live_lease_to_block_or_cancel() {
    let mut graph = ActionGraph::try_new(vec![action("025", vec![], 0, 0)]).expect("acyclic graph");
    let action_id = id("025");
    let leases = LeaseTable::default();
    let worker = actor("worker");

    let block_err = graph
        .transition_action(
            &action_id,
            ActionStatus::Blocked,
            "blocked".into(),
            &leases,
            &worker,
            1_000,
        )
        .expect_err("blocking requires a live lease");
    let cancel_err = graph
        .transition_action(
            &action_id,
            ActionStatus::Cancelled,
            "cancelled".into(),
            &leases,
            &worker,
            1_000,
        )
        .expect_err("cancelling requires a live lease");

    assert!(matches!(block_err, CoordError::LeaseRequired { .. }));
    assert!(matches!(cancel_err, CoordError::LeaseRequired { .. }));
}

#[test]
fn action_status_transition_allows_pending_to_in_progress_to_completed_with_lease() {
    let mut graph = ActionGraph::try_new(vec![action("024", vec![], 0, 0)]).expect("acyclic graph");
    let action_id = id("024");
    let worker = actor("worker");
    let mut leases = LeaseTable::default();
    assert_eq!(
        leases.acquire(action_id.clone(), worker.clone(), 1_000, 10_000, None),
        cairn_core::coord::LeaseAcquireOutcome::Acquired
    );

    graph
        .transition_action(
            &action_id,
            ActionStatus::InProgress,
            "claim".into(),
            &leases,
            &worker,
            1_001,
        )
        .expect("pending action can start");
    graph
        .transition_action(
            &action_id,
            ActionStatus::Completed,
            "done".into(),
            &leases,
            &worker,
            1_002,
        )
        .expect("in-progress action can complete");

    assert_eq!(
        graph.node(&action_id).expect("action exists").status,
        ActionStatus::Completed
    );
}
