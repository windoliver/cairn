//! Pure coordination helpers for `cairn.coord.v1`.
//!
//! This module contains no storage or I/O. It validates action DAGs and
//! computes the frontier used by both CLI/MCP/SDK surfaces once the runtime
//! dispatch path is wired.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::domain::{Identity, TargetId, flush_plan::CoordSignalKind};

const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// One action node in the coordination DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionNode {
    /// Stable action id.
    pub id: TargetId,
    /// Human-readable title.
    pub title: String,
    /// Action ids that must complete before this action is unblocked.
    pub depends_on: Vec<TargetId>,
    /// Higher priority sorts earlier in frontier results.
    pub priority: i32,
    /// Monotonic creation timestamp in milliseconds.
    pub created_at_ms: u64,
    /// Current action lifecycle state.
    pub status: ActionStatus,
}

/// Coordination action lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActionStatus {
    /// Ready once dependencies complete.
    Pending,
    /// Claimed or actively running.
    InProgress,
    /// Finished successfully.
    Completed,
    /// Cannot proceed until an external condition changes.
    Blocked,
    /// Intentionally abandoned.
    Cancelled,
}

/// Validated action graph.
#[derive(Debug, Clone)]
pub struct ActionGraph {
    nodes: BTreeMap<TargetId, ActionNode>,
}

/// Active lease for one coordination action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Action protected by the lease.
    pub action_id: TargetId,
    /// Actor that currently holds the lease.
    pub actor: Identity,
    /// Monotonic acquisition timestamp in milliseconds.
    pub acquired_at_ms: u64,
    /// Monotonic expiration timestamp in milliseconds.
    pub expires_at_ms: u64,
}

/// Result of attempting to acquire a coordination lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquireOutcome {
    /// Lease was absent or expired and is now held by the caller.
    Acquired,
    /// Caller already held the lease; the TTL was refreshed.
    AlreadyHeld,
    /// Another actor holds an unexpired, non-stealable lease.
    Busy {
        /// Current lease holder.
        holder: Identity,
        /// Current lease expiration timestamp.
        expires_at_ms: u64,
    },
    /// Another actor held the lease long enough for the caller to steal it.
    Stolen {
        /// Actor replaced by the caller.
        previous_actor: Identity,
    },
}

/// In-memory lease table used by coordination adapters.
#[derive(Debug, Clone, Default)]
pub struct LeaseTable {
    leases: BTreeMap<TargetId, Lease>,
}

/// Inter-agent coordination signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordSignal {
    /// Stable signal id.
    pub id: String,
    /// Sending actor.
    pub from_actor: Identity,
    /// Receiving actor.
    pub to_actor: Identity,
    /// Signal kind.
    pub signal_kind: CoordSignalKind,
    /// Optional related action/payload id.
    pub payload_id: Option<TargetId>,
    /// Monotonic send timestamp in milliseconds.
    pub sent_at_ms: u64,
}

/// Append-only signal log with retention helpers.
#[derive(Debug, Clone, Default)]
pub struct SignalLog {
    signals: Vec<LoggedSignal>,
    next_sequence: u64,
}

#[derive(Debug, Clone)]
struct LoggedSignal {
    sequence: u64,
    signal: CoordSignal,
}

/// Monotonic signal receive cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalCursor {
    /// Last delivered signal sequence.
    pub sequence: u64,
}

/// Cursor-based signal receive result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalBatch {
    /// Signals newer than the requested cursor.
    pub signals: Vec<CoordSignal>,
    /// Cursor to pass to the next receive call.
    pub next_cursor: SignalCursor,
}

/// Signal retention policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRetention {
    /// Retain signals newer than this many milliseconds.
    pub keep_ms: u64,
}

impl Default for SignalRetention {
    fn default() -> Self {
        Self::from_days(30)
    }
}

/// Declarative routine template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineTemplate {
    /// Template name.
    pub name: String,
    /// Actions created for each routine instance.
    pub actions: Vec<RoutineActionTemplate>,
}

/// One action declared by a routine template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineActionTemplate {
    /// Action id for the instantiated action.
    pub id: TargetId,
    /// Title template. Supports `{{var}}` placeholders.
    pub title: String,
    /// Dependencies inside the routine/action graph.
    pub depends_on: Vec<TargetId>,
    /// Frontier priority for the instantiated action.
    pub priority: i32,
}

/// Instantiated routine output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineInstance {
    /// Template name.
    pub routine_name: String,
    /// Caller-provided instance id.
    pub instance_id: String,
    /// Actions produced from the template.
    pub actions: Vec<ActionNode>,
}

/// Coordination graph validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CoordError {
    /// The same action id appeared more than once.
    #[error("duplicate action id `{id}`")]
    DuplicateAction {
        /// Duplicated action id.
        id: TargetId,
    },
    /// The action graph contains a dependency cycle.
    #[error("coord action graph contains a cycle involving `{action_id}`")]
    CycleDetected {
        /// Action id found while detecting the cycle.
        action_id: TargetId,
    },
    /// An action depends on an id that is not present in the graph.
    #[error("coord action `{action_id}` depends on missing action `{dependency_id}`")]
    MissingDependency {
        /// Action that references the missing dependency.
        action_id: TargetId,
        /// Missing dependency id.
        dependency_id: TargetId,
    },
    /// A deterministic routine-derived action id failed domain validation.
    #[error("derived routine action id was invalid: {message}")]
    InvalidDerivedActionId {
        /// Validation error message.
        message: String,
    },
    /// Routine instantiation could not find the derived id for a template action.
    #[error("routine template action `{template_action_id}` was missing from derived id map")]
    RoutineIdMapMissing {
        /// Template action id missing from the derived-id map.
        template_action_id: TargetId,
    },
    /// Routine template referenced a variable that was not supplied.
    #[error("routine template variable `{variable}` was not supplied")]
    MissingRoutineVariable {
        /// Missing variable name.
        variable: String,
    },
    /// Action status transition is not valid.
    #[error("invalid coord action status transition `{from:?}` -> `{to:?}`: {reason}")]
    InvalidStatusTransition {
        /// Current status.
        from: ActionStatus,
        /// Requested status.
        to: ActionStatus,
        /// Human-readable reason.
        reason: String,
    },
    /// Action id was not present in the graph.
    #[error("coord action `{action_id}` was not found")]
    ActionNotFound {
        /// Missing action id.
        action_id: TargetId,
    },
    /// Action cannot start because a dependency is not completed.
    #[error("coord action `{action_id}` dependency `{dependency_id}` is not completed")]
    DependencyNotCompleted {
        /// Action being started.
        action_id: TargetId,
        /// Dependency that is not completed.
        dependency_id: TargetId,
    },
    /// Status transition requires a live lease held by the actor.
    #[error("coord action `{action_id}` requires a live lease held by `{actor}`")]
    LeaseRequired {
        /// Action being transitioned.
        action_id: TargetId,
        /// Actor attempting the transition.
        actor: Identity,
    },
}

impl ActionNode {
    /// Apply a constrained lifecycle status transition.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::InvalidStatusTransition`] for lifecycle rewinds,
    /// completed/cancelled terminal-state exits, and direct pending → completed
    /// transitions.
    fn transition(&mut self, to: ActionStatus, reason: String) -> Result<(), CoordError> {
        let from = self.status;
        if is_valid_transition(from, to) {
            self.status = to;
            Ok(())
        } else {
            Err(CoordError::InvalidStatusTransition { from, to, reason })
        }
    }
}

fn is_valid_transition(from: ActionStatus, to: ActionStatus) -> bool {
    from == to
        || matches!(
            (from, to),
            (
                ActionStatus::Pending,
                ActionStatus::InProgress | ActionStatus::Blocked | ActionStatus::Cancelled,
            ) | (
                ActionStatus::InProgress,
                ActionStatus::Completed | ActionStatus::Blocked | ActionStatus::Cancelled,
            ) | (
                ActionStatus::Blocked,
                ActionStatus::Pending | ActionStatus::Cancelled,
            )
        )
}

impl ActionGraph {
    /// Build and validate an action graph.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::DuplicateAction`] for duplicate ids and
    /// [`CoordError::MissingDependency`] for dangling dependency edges, and
    /// [`CoordError::CycleDetected`] for dependency cycles.
    pub fn try_new(actions: Vec<ActionNode>) -> Result<Self, CoordError> {
        let mut nodes = BTreeMap::new();
        for action in actions {
            let id = action.id.clone();
            if nodes.insert(id.clone(), action).is_some() {
                return Err(CoordError::DuplicateAction { id });
            }
        }
        let graph = Self { nodes };
        graph.validate_dependencies()?;
        graph.validate_acyclic()?;
        Ok(graph)
    }

    /// Return unblocked pending actions sorted by priority descending, then age
    /// ascending, then id for deterministic ties.
    #[must_use]
    pub fn frontier(
        &self,
        limit: usize,
        leases: &LeaseTable,
        actor: Option<&Identity>,
        now_ms: u64,
    ) -> Vec<&ActionNode> {
        let mut candidates: Vec<_> = self
            .nodes
            .values()
            .filter(|node| {
                (node.status == ActionStatus::Pending
                    && leases.is_selectable_for(&node.id, actor, now_ms)
                    && node.depends_on.iter().all(|dep| {
                        self.nodes
                            .get(dep)
                            .is_some_and(|node| node.status == ActionStatus::Completed)
                    }))
                    || (node.status == ActionStatus::InProgress
                        && actor.is_some_and(|actor| leases.is_held_by(&node.id, actor, now_ms)))
            })
            .collect();
        candidates.sort_by(|a, b| {
            resume_rank(b).cmp(&resume_rank(a)).then_with(|| {
                b.priority
                    .cmp(&a.priority)
                    .then_with(|| a.created_at_ms.cmp(&b.created_at_ms))
                    .then_with(|| a.id.as_str().cmp(b.id.as_str()))
            })
        });
        candidates.truncate(limit);
        candidates
    }

    /// Lookup one action node.
    #[must_use]
    pub fn node(&self, action_id: &TargetId) -> Option<&ActionNode> {
        self.nodes.get(action_id)
    }

    /// Apply an action status transition with graph and lease invariants.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is missing, dependencies are not
    /// completed, the actor lacks a live lease, or the lifecycle transition is
    /// invalid.
    pub fn transition_action(
        &mut self,
        action_id: &TargetId,
        to: ActionStatus,
        reason: String,
        leases: &LeaseTable,
        actor: &Identity,
        now_ms: u64,
    ) -> Result<(), CoordError> {
        let node = self
            .nodes
            .get(action_id)
            .ok_or_else(|| CoordError::ActionNotFound {
                action_id: action_id.clone(),
            })?;
        if matches!(to, ActionStatus::InProgress) {
            for dep in &node.depends_on {
                if !self
                    .nodes
                    .get(dep)
                    .is_some_and(|dep| dep.status == ActionStatus::Completed)
                {
                    return Err(CoordError::DependencyNotCompleted {
                        action_id: action_id.clone(),
                        dependency_id: dep.clone(),
                    });
                }
            }
        }
        if node.status != to && !leases.is_held_by(action_id, actor, now_ms) {
            return Err(CoordError::LeaseRequired {
                action_id: action_id.clone(),
                actor: actor.clone(),
            });
        }
        self.nodes
            .get_mut(action_id)
            .ok_or_else(|| CoordError::ActionNotFound {
                action_id: action_id.clone(),
            })?
            .transition(to, reason)
    }

    fn validate_dependencies(&self) -> Result<(), CoordError> {
        for node in self.nodes.values() {
            for dep in &node.depends_on {
                if !self.nodes.contains_key(dep) {
                    return Err(CoordError::MissingDependency {
                        action_id: node.id.clone(),
                        dependency_id: dep.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_acyclic(&self) -> Result<(), CoordError> {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.nodes.keys() {
            self.visit(id, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn visit(
        &self,
        id: &TargetId,
        visiting: &mut BTreeSet<TargetId>,
        visited: &mut BTreeSet<TargetId>,
    ) -> Result<(), CoordError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(CoordError::CycleDetected {
                action_id: id.clone(),
            });
        }
        if let Some(node) = self.nodes.get(id) {
            for dep in &node.depends_on {
                self.visit(dep, visiting, visited)?;
            }
        }
        visiting.remove(id);
        visited.insert(id.clone());
        Ok(())
    }
}

fn resume_rank(node: &ActionNode) -> u8 {
    u8::from(node.status == ActionStatus::InProgress)
}

impl LeaseTable {
    /// Acquire or refresh a lease for an action.
    ///
    /// `now_ms`, `ttl_ms`, and `steal_after_ms` use the same monotonic clock.
    /// A lease is expired once `now_ms >= expires_at_ms`; an unexpired lease
    /// may still be stolen once it is at least `steal_after_ms` old.
    #[must_use]
    pub fn acquire(
        &mut self,
        action_id: TargetId,
        actor: Identity,
        now_ms: u64,
        ttl_ms: u64,
        steal_after_ms: Option<u64>,
    ) -> LeaseAcquireOutcome {
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        match self.leases.get(&action_id) {
            None => {
                self.insert(action_id, actor, now_ms, expires_at_ms);
                LeaseAcquireOutcome::Acquired
            }
            Some(existing) if now_ms >= existing.expires_at_ms => {
                self.insert(action_id, actor, now_ms, expires_at_ms);
                LeaseAcquireOutcome::Acquired
            }
            Some(existing) if existing.actor == actor => {
                self.insert(action_id, actor, now_ms, expires_at_ms);
                LeaseAcquireOutcome::AlreadyHeld
            }
            Some(existing)
                if steal_after_ms.is_some_and(|threshold| {
                    now_ms.saturating_sub(existing.acquired_at_ms) >= threshold
                }) =>
            {
                let previous_actor = existing.actor.clone();
                self.insert(action_id, actor, now_ms, expires_at_ms);
                LeaseAcquireOutcome::Stolen { previous_actor }
            }
            Some(existing) => LeaseAcquireOutcome::Busy {
                holder: existing.actor.clone(),
                expires_at_ms: existing.expires_at_ms,
            },
        }
    }

    /// Acquire or refresh a lease and emit a signal to the previous holder if
    /// the lease is stolen.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn acquire_with_signal(
        &mut self,
        action_id: TargetId,
        actor: Identity,
        now_ms: u64,
        ttl_ms: u64,
        steal_after_ms: Option<u64>,
        signals: &mut SignalLog,
        signal_id: String,
    ) -> LeaseAcquireOutcome {
        let outcome = self.acquire(
            action_id.clone(),
            actor.clone(),
            now_ms,
            ttl_ms,
            steal_after_ms,
        );
        if let LeaseAcquireOutcome::Stolen { previous_actor } = &outcome {
            signals.append(CoordSignal {
                id: signal_id,
                from_actor: actor,
                to_actor: previous_actor.clone(),
                signal_kind: CoordSignalKind::LeaseReleased,
                payload_id: Some(action_id),
                sent_at_ms: now_ms,
            });
        }
        outcome
    }

    /// Release a held lease. Returns false if the action is unleased or held
    /// by another actor.
    pub fn release(&mut self, action_id: &TargetId, actor: &Identity) -> bool {
        if self
            .leases
            .get(action_id)
            .is_some_and(|lease| &lease.actor == actor)
        {
            self.leases.remove(action_id);
            return true;
        }
        false
    }

    /// Lookup one active lease.
    #[must_use]
    pub fn get(&self, action_id: &TargetId) -> Option<&Lease> {
        self.leases.get(action_id)
    }

    fn is_selectable_for(
        &self,
        action_id: &TargetId,
        actor: Option<&Identity>,
        now_ms: u64,
    ) -> bool {
        self.leases.get(action_id).is_none_or(|lease| {
            now_ms >= lease.expires_at_ms || actor.is_some_and(|actor| actor == &lease.actor)
        })
    }

    fn is_held_by(&self, action_id: &TargetId, actor: &Identity, now_ms: u64) -> bool {
        self.leases
            .get(action_id)
            .is_some_and(|lease| now_ms < lease.expires_at_ms && &lease.actor == actor)
    }

    fn insert(
        &mut self,
        action_id: TargetId,
        actor: Identity,
        acquired_at_ms: u64,
        expires_at_ms: u64,
    ) {
        self.leases.insert(
            action_id.clone(),
            Lease {
                action_id,
                actor,
                acquired_at_ms,
                expires_at_ms,
            },
        );
    }
}

impl SignalLog {
    /// Append a signal to the log.
    pub fn append(&mut self, signal: CoordSignal) {
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.signals.push(LoggedSignal {
            sequence: self.next_sequence,
            signal,
        });
    }

    /// Retain signals newer than `now_ms - retention_ms`.
    pub fn retain_since(&mut self, now_ms: u64, retention_ms: u64) {
        let cutoff = now_ms.saturating_sub(retention_ms);
        self.signals
            .retain(|entry| entry.signal.sent_at_ms >= cutoff);
    }

    /// Apply a signal retention policy.
    pub fn retain_with(&mut self, retention: SignalRetention, now_ms: u64) {
        self.retain_since(now_ms, retention.keep_ms);
    }

    /// Receive retained signals addressed to an actor after `cursor`.
    #[must_use]
    pub fn recv_since(&self, actor: &Identity, cursor: SignalCursor) -> SignalBatch {
        let signals: Vec<_> = self
            .signals
            .iter()
            .filter(|entry| &entry.signal.to_actor == actor && entry.sequence > cursor.sequence)
            .map(|entry| entry.signal.clone())
            .collect();
        let next_sequence = self
            .signals
            .iter()
            .filter(|entry| &entry.signal.to_actor == actor && entry.sequence > cursor.sequence)
            .map(|entry| entry.sequence)
            .max()
            .unwrap_or(cursor.sequence);
        SignalBatch {
            signals,
            next_cursor: SignalCursor {
                sequence: next_sequence,
            },
        }
    }

    /// Number of retained signals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// True when no signals are retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }
}

impl SignalRetention {
    /// Build a retention policy from a day count.
    #[must_use]
    pub const fn from_days(days: u64) -> Self {
        Self {
            keep_ms: days.saturating_mul(24 * 60 * 60 * 1_000),
        }
    }
}

impl RoutineTemplate {
    /// Instantiate this template with caller-provided variables.
    ///
    /// # Errors
    ///
    /// Returns [`CoordError::MissingRoutineVariable`] when a `{{var}}`
    /// placeholder has no entry in `vars`.
    pub fn instantiate(
        &self,
        instance_id: String,
        vars: &BTreeMap<String, String>,
        created_at_ms: u64,
    ) -> Result<RoutineInstance, CoordError> {
        let mut actions = Vec::with_capacity(self.actions.len());
        let mut id_map = BTreeMap::new();
        for action in &self.actions {
            id_map.insert(
                action.id.clone(),
                derive_instance_action_id(&self.name, &instance_id, &action.id)?,
            );
        }
        for action in &self.actions {
            let id =
                id_map
                    .get(&action.id)
                    .cloned()
                    .ok_or_else(|| CoordError::RoutineIdMapMissing {
                        template_action_id: action.id.clone(),
                    })?;
            actions.push(ActionNode {
                id,
                title: render_template(&action.title, vars)?,
                depends_on: action
                    .depends_on
                    .iter()
                    .map(|dep| id_map.get(dep).cloned().unwrap_or_else(|| dep.clone()))
                    .collect(),
                priority: action.priority,
                created_at_ms,
                status: ActionStatus::Pending,
            });
        }
        ActionGraph::try_new(actions.clone())?;
        Ok(RoutineInstance {
            routine_name: self.name.clone(),
            instance_id,
            actions,
        })
    }
}

fn render_template(template: &str, vars: &BTreeMap<String, String>) -> Result<String, CoordError> {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            rendered.push_str(&rest[start..]);
            return Ok(rendered);
        };
        let variable = after_start[..end].trim();
        let value = vars
            .get(variable)
            .ok_or_else(|| CoordError::MissingRoutineVariable {
                variable: variable.to_owned(),
            })?;
        rendered.push_str(value);
        rest = &after_start[end + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

fn derive_instance_action_id(
    routine_name: &str,
    instance_id: &str,
    template_action_id: &TargetId,
) -> Result<TargetId, CoordError> {
    let input = format!("{routine_name}\0{instance_id}\0{template_action_id}");
    let digest = blake3::hash(input.as_bytes());
    let bytes = digest.as_bytes();
    let mut raw = String::with_capacity(26);
    raw.push(char::from(
        CROCKFORD_BASE32[usize::from(bytes[0] & 0b0000_0111)],
    ));
    for i in 1..26 {
        raw.push(char::from(
            CROCKFORD_BASE32[usize::from(bytes[i] & 0b0001_1111)],
        ));
    }
    TargetId::parse(raw).map_err(|source| CoordError::InvalidDerivedActionId {
        message: source.to_string(),
    })
}
