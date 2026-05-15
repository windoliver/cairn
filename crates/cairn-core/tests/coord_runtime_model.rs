//! Pure coordination runtime model behavior.

use std::collections::BTreeMap;

use cairn_core::coord::{
    ActionStatus, CoordError, CoordSignal, LeaseAcquireOutcome, LeaseTable, RoutineActionTemplate,
    RoutineTemplate, SignalCursor, SignalLog, SignalRetention,
};
use cairn_core::domain::flush_plan::CoordSignalKind;
use cairn_core::domain::{Identity, TargetId};

fn id(suffix: &str) -> TargetId {
    TargetId::parse(format!("01JTS6R4J70000000000000{suffix}")).unwrap()
}

fn actor(name: &str) -> Identity {
    Identity::parse(format!("agt:{name}:v1")).unwrap()
}

#[test]
fn lease_acquire_respects_ttl_expiry() {
    let action_id = id("100");
    let first_actor = actor("first");
    let second_actor = actor("second");
    let mut leases = LeaseTable::default();

    assert_eq!(
        leases.acquire(
            action_id.clone(),
            first_actor.clone(),
            1_000,
            500,
            Some(10_000)
        ),
        LeaseAcquireOutcome::Acquired
    );
    assert_eq!(
        leases.acquire(
            action_id.clone(),
            second_actor.clone(),
            1_499,
            500,
            Some(10_000)
        ),
        LeaseAcquireOutcome::Busy {
            holder: first_actor,
            expires_at_ms: 1_500,
        }
    );
    assert_eq!(
        leases.acquire(
            action_id.clone(),
            second_actor.clone(),
            1_500,
            250,
            Some(10_000)
        ),
        LeaseAcquireOutcome::Acquired
    );

    let lease = leases.get(&action_id).expect("lease must be held");
    assert_eq!(lease.actor, second_actor);
    assert_eq!(lease.expires_at_ms, 1_750);
}

#[test]
fn lease_acquire_allows_steal_after_threshold() {
    let action_id = id("101");
    let first_actor = actor("first");
    let second_actor = actor("second");
    let mut leases = LeaseTable::default();

    assert_eq!(
        leases.acquire(
            action_id.clone(),
            first_actor.clone(),
            10_000,
            10_000,
            Some(1_000)
        ),
        LeaseAcquireOutcome::Acquired
    );
    assert_eq!(
        leases.acquire(
            action_id.clone(),
            second_actor.clone(),
            10_999,
            10_000,
            Some(1_000)
        ),
        LeaseAcquireOutcome::Busy {
            holder: first_actor.clone(),
            expires_at_ms: 20_000,
        }
    );
    assert_eq!(
        leases.acquire(
            action_id.clone(),
            second_actor.clone(),
            11_000,
            10_000,
            Some(1_000)
        ),
        LeaseAcquireOutcome::Stolen {
            previous_actor: first_actor,
        }
    );

    assert_eq!(
        leases.get(&action_id).expect("lease must be held").actor,
        second_actor
    );
}

#[test]
fn lease_steal_signals_previous_holder() {
    let action_id = id("102");
    let first_actor = actor("first");
    let second_actor = actor("second");
    let mut leases = LeaseTable::default();
    let mut signals = SignalLog::default();

    assert_eq!(
        leases.acquire(
            action_id.clone(),
            first_actor.clone(),
            10_000,
            10_000,
            Some(1_000)
        ),
        LeaseAcquireOutcome::Acquired
    );
    assert_eq!(
        leases.acquire_with_signal(
            action_id.clone(),
            second_actor.clone(),
            11_000,
            10_000,
            Some(1_000),
            &mut signals,
            "lease-stolen-1".into(),
        ),
        LeaseAcquireOutcome::Stolen {
            previous_actor: first_actor.clone(),
        }
    );

    let received = signals
        .recv_since(&first_actor, SignalCursor::default())
        .signals;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].id, "lease-stolen-1");
    assert_eq!(received[0].from_actor, second_actor);
    assert_eq!(received[0].to_actor, first_actor);
    assert_eq!(received[0].signal_kind, CoordSignalKind::LeaseReleased);
    assert_eq!(received[0].payload_id, Some(action_id));
    assert_eq!(received[0].sent_at_ms, 11_000);
}

#[test]
fn lease_acquire_without_steal_after_does_not_steal_live_lease() {
    let action_id = id("103");
    let first_actor = actor("first");
    let second_actor = actor("second");
    let mut leases = LeaseTable::default();

    assert_eq!(
        leases.acquire(action_id.clone(), first_actor.clone(), 10_000, 10_000, None),
        LeaseAcquireOutcome::Acquired
    );
    assert_eq!(
        leases.acquire(action_id, second_actor, 19_999, 10_000, None),
        LeaseAcquireOutcome::Busy {
            holder: first_actor,
            expires_at_ms: 20_000,
        }
    );
}

#[test]
fn signal_log_retains_recent_signals_and_receives_by_actor() {
    let receiver = actor("receiver");
    let mut log = SignalLog::default();
    log.append(CoordSignal {
        id: "old".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: Some(id("110")),
        sent_at_ms: 100,
    });
    log.append(CoordSignal {
        id: "fresh".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::RequestReview,
        payload_id: Some(id("111")),
        sent_at_ms: 1_000,
    });
    log.append(CoordSignal {
        id: "other".into(),
        from_actor: actor("sender"),
        to_actor: actor("other"),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 1_100,
    });

    log.retain_since(1_100, 250);

    let signal_ids: Vec<_> = log
        .recv_since(&receiver, SignalCursor::default())
        .signals
        .into_iter()
        .map(|signal| signal.id)
        .collect();
    assert_eq!(signal_ids, ["fresh"]);
    assert_eq!(log.len(), 2);
}

#[test]
fn signal_log_default_retention_keeps_last_30_days_per_actor() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    let receiver = actor("receiver");
    let mut log = SignalLog::default();
    log.append(CoordSignal {
        id: "expired".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: DAY_MS,
    });
    log.append(CoordSignal {
        id: "retained".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 2 * DAY_MS,
    });
    log.append(CoordSignal {
        id: "other-retained".into(),
        from_actor: actor("sender"),
        to_actor: actor("other"),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 2 * DAY_MS,
    });

    log.retain_with(SignalRetention::default(), 32 * DAY_MS);

    let signal_ids: Vec<_> = log
        .recv_since(&receiver, SignalCursor::default())
        .signals
        .into_iter()
        .map(|signal| signal.id)
        .collect();
    assert_eq!(signal_ids, ["retained"]);
    assert_eq!(log.len(), 2);
}

#[test]
fn signal_log_retention_is_configurable() {
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    let receiver = actor("receiver");
    let mut log = SignalLog::default();
    log.append(CoordSignal {
        id: "older".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 20 * DAY_MS,
    });
    log.append(CoordSignal {
        id: "newer".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 29 * DAY_MS,
    });

    log.retain_with(SignalRetention::from_days(5), 30 * DAY_MS);

    let signal_ids: Vec<_> = log
        .recv_since(&receiver, SignalCursor::default())
        .signals
        .into_iter()
        .map(|signal| signal.id)
        .collect();
    assert_eq!(signal_ids, ["newer"]);
}

#[test]
fn signal_log_receive_uses_cursor_to_avoid_redelivery() {
    let receiver = actor("receiver");
    let mut log = SignalLog::default();
    log.append(CoordSignal {
        id: "first".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 100,
    });
    log.append(CoordSignal {
        id: "second".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 200,
    });

    let first_batch = log.recv_since(&receiver, SignalCursor::default());
    assert_eq!(
        first_batch
            .signals
            .iter()
            .map(|signal| signal.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );

    let second_batch = log.recv_since(&receiver, first_batch.next_cursor);

    assert!(second_batch.signals.is_empty());
    assert_eq!(second_batch.next_cursor, first_batch.next_cursor);
}

#[test]
fn signal_log_cursor_does_not_drop_same_timestamp_messages() {
    let receiver = actor("receiver");
    let mut log = SignalLog::default();
    log.append(CoordSignal {
        id: "first".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 100,
    });

    let first_batch = log.recv_since(&receiver, SignalCursor::default());
    log.append(CoordSignal {
        id: "same-ms-late".into(),
        from_actor: actor("sender"),
        to_actor: receiver.clone(),
        signal_kind: CoordSignalKind::Info,
        payload_id: None,
        sent_at_ms: 100,
    });

    let second_batch = log.recv_since(&receiver, first_batch.next_cursor);

    assert_eq!(second_batch.signals.len(), 1);
    assert_eq!(second_batch.signals[0].id, "same-ms-late");
}

#[test]
fn routine_instantiation_renders_vars_and_dependencies() {
    let review = id("120");
    let fix = id("121");
    let template = RoutineTemplate {
        name: "review-loop".into(),
        actions: vec![
            RoutineActionTemplate {
                id: review.clone(),
                title: "Review {{topic}}".into(),
                depends_on: vec![],
                priority: 10,
            },
            RoutineActionTemplate {
                id: fix.clone(),
                title: "Patch {{topic}}".into(),
                depends_on: vec![review.clone()],
                priority: 5,
            },
        ],
    };
    let vars = BTreeMap::from([("topic".into(), "coord lease flow".into())]);

    let instance = template
        .instantiate("run-001".into(), &vars, 50)
        .expect("template should render");

    assert_eq!(instance.routine_name, "review-loop");
    assert_eq!(instance.instance_id, "run-001");
    assert_eq!(instance.actions[0].title, "Review coord lease flow");
    assert_eq!(instance.actions[1].title, "Patch coord lease flow");
    assert_ne!(instance.actions[0].id, review);
    assert_ne!(instance.actions[1].id, fix);
    assert_eq!(
        instance.actions[1].depends_on,
        vec![instance.actions[0].id.clone()]
    );
    assert_eq!(instance.actions[1].status, ActionStatus::Pending);
}

#[test]
fn routine_instantiation_expands_five_actions_and_four_edges_deterministically() {
    let ids = [id("130"), id("131"), id("132"), id("133"), id("134")];
    let template = RoutineTemplate {
        name: "review-loop".into(),
        actions: vec![
            RoutineActionTemplate {
                id: ids[0].clone(),
                title: "Fetch {{target}}".into(),
                depends_on: vec![],
                priority: 50,
            },
            RoutineActionTemplate {
                id: ids[1].clone(),
                title: "Test {{target}}".into(),
                depends_on: vec![ids[0].clone()],
                priority: 40,
            },
            RoutineActionTemplate {
                id: ids[2].clone(),
                title: "Review {{target}}".into(),
                depends_on: vec![ids[1].clone()],
                priority: 30,
            },
            RoutineActionTemplate {
                id: ids[3].clone(),
                title: "Patch {{target}}".into(),
                depends_on: vec![ids[2].clone()],
                priority: 20,
            },
            RoutineActionTemplate {
                id: ids[4].clone(),
                title: "Summarize {{target}}".into(),
                depends_on: vec![ids[3].clone()],
                priority: 10,
            },
        ],
    };

    let instance = template
        .instantiate(
            "run-review-001".into(),
            &BTreeMap::from([("target".into(), "PR 314".into())]),
            123,
        )
        .expect("five-action template should instantiate");

    assert_eq!(instance.actions.len(), 5);
    assert_eq!(
        instance
            .actions
            .iter()
            .map(|action| action.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Fetch PR 314",
            "Test PR 314",
            "Review PR 314",
            "Patch PR 314",
            "Summarize PR 314",
        ]
    );
    assert_eq!(instance.actions[0].depends_on, Vec::<TargetId>::new());
    assert_eq!(
        instance.actions[1].depends_on,
        vec![instance.actions[0].id.clone()]
    );
    assert_eq!(
        instance.actions[2].depends_on,
        vec![instance.actions[1].id.clone()]
    );
    assert_eq!(
        instance.actions[3].depends_on,
        vec![instance.actions[2].id.clone()]
    );
    assert_eq!(
        instance.actions[4].depends_on,
        vec![instance.actions[3].id.clone()]
    );
}

#[test]
fn routine_instantiation_derives_distinct_ids_per_instance() {
    let first = id("140");
    let second = id("141");
    let template = RoutineTemplate {
        name: "review-loop".into(),
        actions: vec![
            RoutineActionTemplate {
                id: first.clone(),
                title: "Review {{topic}}".into(),
                depends_on: vec![],
                priority: 10,
            },
            RoutineActionTemplate {
                id: second,
                title: "Patch {{topic}}".into(),
                depends_on: vec![first],
                priority: 5,
            },
        ],
    };
    let vars = BTreeMap::from([("topic".into(), "coord lease flow".into())]);

    let run_one = template
        .instantiate("run-001".into(), &vars, 50)
        .expect("first template run should instantiate");
    let run_two = template
        .instantiate("run-002".into(), &vars, 50)
        .expect("second template run should instantiate");

    assert_ne!(run_one.actions[0].id, run_two.actions[0].id);
    assert_ne!(run_one.actions[1].id, run_two.actions[1].id);
    assert_eq!(
        run_one.actions[1].depends_on,
        vec![run_one.actions[0].id.clone()]
    );
    assert_eq!(
        run_two.actions[1].depends_on,
        vec![run_two.actions[0].id.clone()]
    );
}

#[test]
fn routine_instantiation_rejects_missing_var() {
    let template = RoutineTemplate {
        name: "review-loop".into(),
        actions: vec![RoutineActionTemplate {
            id: id("122"),
            title: "Review {{topic}}".into(),
            depends_on: vec![],
            priority: 10,
        }],
    };

    let err = template
        .instantiate("run-001".into(), &BTreeMap::new(), 50)
        .unwrap_err();

    assert!(matches!(
        err,
        CoordError::MissingRoutineVariable { variable } if variable == "topic"
    ));
}
