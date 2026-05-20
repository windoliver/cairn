//! Consent receipt timeline — Issue #253, brief §14.
//!
//! Append-only events keyed by `(consent_ref, seq)`. Each event is a
//! transition for a covering grant: `issued` (came into force), `expired`
//! (TTL elapsed via writer-emitted event), `revoked` (user/operator
//! pulled it). The pure resolver [`CoveringGrant::resolve`] walks an
//! event slice and decides whether a candidate `(sensor, scope, t)` tuple
//! is covered. Pure data + pure resolver. No I/O.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::domain::{Rfc3339Timestamp, SensorLabel};

/// One transition in a consent grant's timeline. Events are append-only;
/// the `(consent_ref, seq)` pair is unique. `decided_at` is the wall-clock
/// instant the transition applies; for `Issued`, `expires_at` carries the
/// optional TTL after which the grant lapses (a writer-emitted `Expired`
/// event is also accepted to make expiry visible without reading the
/// `Issued` row).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentTimelineEvent {
    /// Stable identifier for the underlying grant. Multiple events share
    /// the same `consent_ref`.
    pub consent_ref: String,
    /// Append-only monotonic sequence within `consent_ref`. Used to
    /// disambiguate events at the same `decided_at`.
    pub seq: u64,
    /// Transition kind.
    pub kind: ConsentTimelineEventKind,
    /// Sensor whose captures this grant authorises.
    pub sensor_id: SensorLabel,
    /// Scope token authorised by this grant.
    ///
    /// **Encoding contract (Issue #253, brief §14):** writers MUST
    /// populate this with the canonical wire form of the corresponding
    /// `ScopeTuple` — see [`crate::domain::scope::ScopeTuple::canonical_wire`].
    /// The lint check joins records to grants by exact-match on this
    /// string; coarser representations like `"private"` will never
    /// match a record whose `canonical_wire` is e.g.
    /// `"user=hmn:tafeng"`. Phase-B (#255) ingest writers honor this
    /// contract; older test seeds that pre-date the join change are
    /// kept only for resolver-level unit tests where the join key is
    /// irrelevant.
    pub scope: String,
    /// When the transition takes effect.
    pub decided_at: Rfc3339Timestamp,
    /// For `Issued`, the optional expiry instant. `None` means open-ended
    /// (revocation only). Ignored for `Expired` / `Revoked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Rfc3339Timestamp>,
}

/// Discriminator for [`ConsentTimelineEvent::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsentTimelineEventKind {
    /// A grant came into force.
    Issued,
    /// A previously-issued grant's TTL has elapsed (writer-emitted).
    Expired,
    /// A previously-issued grant was pulled by the user or operator.
    Revoked,
}

/// The covering grant for a candidate `(sensor, scope, t)` tuple.
/// Returned by [`CoveringGrant::resolve`] when one exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveringGrant {
    /// Identifier of the grant whose `Issued` event covers `t`.
    pub consent_ref: String,
    /// Sensor the grant authorises.
    pub sensor_id: SensorLabel,
    /// Scope the grant authorises.
    pub scope: String,
    /// `decided_at` of the covering `Issued` event.
    pub issued_at: Rfc3339Timestamp,
    /// `expires_at` carried by the covering `Issued` event, if any.
    pub expires_at: Option<Rfc3339Timestamp>,
}

impl CoveringGrant {
    /// Resolve the covering grant for `(sensor, scope)` at instant `at`.
    ///
    /// Pure function. Filters events to the matching `(sensor, scope)` at
    /// or before `at`, sorts ascending by `(decided_at, seq)`, and replays
    /// transitions: `Issued` sets the current grant, `Expired` / `Revoked`
    /// clear it. Returns the most recent still-current `Issued` event, or
    /// `None` if no grant covers `at`. If the surviving grant carries an
    /// `expires_at` and `at` is at or after it, returns `None` (the
    /// caller may not have observed a writer-emitted `Expired` row yet).
    #[must_use]
    pub fn resolve(
        events: &[ConsentTimelineEvent],
        sensor: &SensorLabel,
        scope: &str,
        at: &Rfc3339Timestamp,
    ) -> Option<Self> {
        let mut relevant: Vec<&ConsentTimelineEvent> = events
            .iter()
            .filter(|e| {
                e.sensor_id == *sensor
                    && e.scope == scope
                    && !matches!(e.decided_at.cmp_chronological(at), Ordering::Greater)
            })
            .collect();
        // Round 7: sort by `seq` alone, not `(decided_at, seq)`.
        // `decided_at` is wall-clock metadata that the schema does
        // not constrain to be monotonic per `consent_ref` — a
        // backfill, repair, or skewed-clock writer could insert a
        // terminal event whose `decided_at` is earlier than its
        // issuing event, and a `(decided_at, seq)` sort would replay
        // the revoke BEFORE the issue and silently reactivate the
        // grant. `seq` is the append-order PK and is structurally
        // monotonic per `consent_ref`, so it's the only sound
        // replay axis for a security-bearing decision.
        // Loop 3 round 8: replay each `consent_ref`'s events
        // independently, then choose among still-active grants.
        // `seq` is monotonic ONLY within a single `consent_ref`, so
        // sorting a mixed slice by `seq` alone is order-sensitive
        // (two grants both at seq=1 collide). Group by `consent_ref`
        // first; within each group sort by `seq` and replay to find
        // the still-active issued event (if any). Pick the active
        // grant with the latest `decided_at` as the covering grant —
        // ties broken by `consent_ref` for determinism.
        relevant.sort_by(|a, b| {
            a.consent_ref
                .cmp(&b.consent_ref)
                .then_with(|| a.seq.cmp(&b.seq))
        });

        let mut active_per_ref: Vec<&ConsentTimelineEvent> = Vec::new();
        let mut group_start = 0usize;
        while group_start < relevant.len() {
            let group_ref = &relevant[group_start].consent_ref;
            let mut group_end = group_start + 1;
            while group_end < relevant.len() && &relevant[group_end].consent_ref == group_ref {
                group_end += 1;
            }
            let mut current: Option<&ConsentTimelineEvent> = None;
            for ev in &relevant[group_start..group_end] {
                match ev.kind {
                    ConsentTimelineEventKind::Issued => current = Some(ev),
                    ConsentTimelineEventKind::Revoked | ConsentTimelineEventKind::Expired => {
                        current = None;
                    }
                }
            }
            if let Some(ev) = current {
                active_per_ref.push(ev);
            }
            group_start = group_end;
        }

        // Filter to grants whose expires_at (if set) is strictly
        // greater than `at`. A still-active issued event with an
        // expired window does not cover `at`, so it must not be the
        // candidate that masks an older open-ended grant which is
        // still valid.
        let issued = active_per_ref
            .into_iter()
            .filter(|ev| {
                ev.expires_at
                    .as_ref()
                    .is_none_or(|exp| matches!(at.cmp_chronological(exp), Ordering::Less))
            })
            .max_by(|a, b| {
                a.decided_at
                    .cmp_chronological(&b.decided_at)
                    .then_with(|| a.consent_ref.cmp(&b.consent_ref))
            })?;
        Some(Self {
            consent_ref: issued.consent_ref.clone(),
            sensor_id: issued.sensor_id.clone(),
            scope: issued.scope.clone(),
            issued_at: issued.decided_at.clone(),
            expires_at: issued.expires_at.clone(),
        })
    }
}

/// Per-record consent-storage model. PR-1 always sees `LegacyEvent`;
/// `ReceiptTimeline` is wired in #253.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ConsentModel {
    /// Pre-#253 storage model: generic consent events.
    LegacyEvent,
    /// Post-#253 storage model: per-grant timeline.
    ReceiptTimeline,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(raw: &str) -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse(raw).expect("invariant: test timestamp literal is valid")
    }

    fn sensor(label: &str) -> SensorLabel {
        SensorLabel::parse(label).expect("invariant: test sensor label is valid")
    }

    fn issued(
        consent_ref: &str,
        seq: u64,
        sensor_label: &str,
        scope: &str,
        decided_at: &str,
        expires_at: Option<&str>,
    ) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: sensor(sensor_label),
            scope: scope.to_owned(),
            decided_at: ts(decided_at),
            expires_at: expires_at.map(ts),
        }
    }

    fn revoked(
        consent_ref: &str,
        seq: u64,
        sensor_label: &str,
        scope: &str,
        decided_at: &str,
    ) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq,
            kind: ConsentTimelineEventKind::Revoked,
            sensor_id: sensor(sensor_label),
            scope: scope.to_owned(),
            decided_at: ts(decided_at),
            expires_at: None,
        }
    }

    fn expired(
        consent_ref: &str,
        seq: u64,
        sensor_label: &str,
        scope: &str,
        decided_at: &str,
    ) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq,
            kind: ConsentTimelineEventKind::Expired,
            sensor_id: sensor(sensor_label),
            scope: scope.to_owned(),
            decided_at: ts(decided_at),
            expires_at: None,
        }
    }

    #[test]
    fn covering_grant_resolves_when_only_issued_event_in_window() {
        let events = vec![issued(
            "g1",
            1,
            "hooks.claude_code",
            "code",
            "2026-04-22T10:00:00Z",
            Some("2026-04-22T18:00:00Z"),
        )];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T12:00:00Z"),
        )
        .expect("expected covering grant");
        assert_eq!(g.consent_ref, "g1");
        assert_eq!(g.issued_at, ts("2026-04-22T10:00:00Z"));
    }

    #[test]
    fn covering_grant_none_after_expiry() {
        let events = vec![issued(
            "g1",
            1,
            "hooks.claude_code",
            "code",
            "2026-04-22T10:00:00Z",
            Some("2026-04-22T11:00:00Z"),
        )];
        // At-or-after expiry → None.
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.claude_code"),
                "code",
                &ts("2026-04-22T11:00:00Z"),
            )
            .is_none()
        );
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.claude_code"),
                "code",
                &ts("2026-04-22T12:00:00Z"),
            )
            .is_none()
        );
    }

    #[test]
    fn covering_grant_none_after_revoke() {
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T10:00:00Z",
                Some("2026-04-22T18:00:00Z"),
            ),
            revoked("g1", 2, "hooks.claude_code", "code", "2026-04-22T11:00:00Z"),
        ];
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.claude_code"),
                "code",
                &ts("2026-04-22T12:00:00Z"),
            )
            .is_none()
        );
        // Before the revoke, still covered.
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T10:30:00Z"),
        )
        .expect("expected covering grant before revoke");
        assert_eq!(g.consent_ref, "g1");
    }

    #[test]
    fn covering_grant_distinguishes_sensor_mismatch() {
        let events = vec![issued(
            "g1",
            1,
            "hooks.claude_code",
            "code",
            "2026-04-22T10:00:00Z",
            Some("2026-04-22T18:00:00Z"),
        )];
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.codex"),
                "code",
                &ts("2026-04-22T12:00:00Z"),
            )
            .is_none()
        );
    }

    #[test]
    fn covering_grant_distinguishes_scope_mismatch() {
        let events = vec![issued(
            "g1",
            1,
            "hooks.claude_code",
            "code",
            "2026-04-22T10:00:00Z",
            Some("2026-04-22T18:00:00Z"),
        )];
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.claude_code"),
                "secrets",
                &ts("2026-04-22T12:00:00Z"),
            )
            .is_none()
        );
    }

    #[test]
    fn covering_grant_picks_latest_issued_before_t() {
        // Two grants for the same (sensor, scope); the later one supersedes
        // the earlier one. The returned grant must be the latest issued
        // event whose timestamp is <= t.
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T10:00:00Z",
                Some("2026-04-22T18:00:00Z"),
            ),
            // A different grant comes into force later.
            issued(
                "g2",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T12:00:00Z",
                Some("2026-04-22T20:00:00Z"),
            ),
        ];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T13:00:00Z"),
        )
        .expect("expected covering grant");
        assert_eq!(g.consent_ref, "g2");
        assert_eq!(g.issued_at, ts("2026-04-22T12:00:00Z"));

        // And before the second grant, we still see the first.
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T11:00:00Z"),
        )
        .expect("expected first covering grant");
        assert_eq!(g.consent_ref, "g1");
    }

    #[test]
    fn writer_emitted_expired_event_clears_grant() {
        // Ensures the `Expired` arm of the resolver is exercised: a writer
        // can persist an explicit Expired transition without relying on
        // `expires_at` introspection.
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T10:00:00Z",
                None,
            ),
            expired("g1", 2, "hooks.claude_code", "code", "2026-04-22T11:00:00Z"),
        ];
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.claude_code"),
                "code",
                &ts("2026-04-22T12:00:00Z"),
            )
            .is_none()
        );
    }

    /// Round 1 (overlapping grants): a `Revoked` event for grant `g1`
    /// must not clear an `Issued` event for grant `g2` on the same
    /// `(sensor, scope)`. The replay state is keyed by `consent_ref`,
    /// so terminal events only affect the matching grant.
    #[test]
    fn covering_grant_revoke_of_older_grant_does_not_clear_newer() {
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T10:00:00Z",
                None,
            ),
            issued(
                "g2",
                2,
                "hooks.claude_code",
                "code",
                "2026-04-22T11:00:00Z",
                None,
            ),
            revoked("g1", 3, "hooks.claude_code", "code", "2026-04-22T12:00:00Z"),
        ];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T13:00:00Z"),
        )
        .expect("g2 must survive g1's revocation");
        assert_eq!(g.consent_ref, "g2");
    }

    #[test]
    fn covering_grant_expire_of_older_grant_does_not_clear_newer() {
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T10:00:00Z",
                None,
            ),
            issued(
                "g2",
                2,
                "hooks.claude_code",
                "code",
                "2026-04-22T11:00:00Z",
                None,
            ),
            expired("g1", 3, "hooks.claude_code", "code", "2026-04-22T12:00:00Z"),
        ];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T13:00:00Z"),
        )
        .expect("g2 must survive g1's expiry row");
        assert_eq!(g.consent_ref, "g2");
    }

    /// Round 7: terminal events with backdated `decided_at` (clock
    /// skew, backfill, retry) must NOT replay before their issue and
    /// silently reactivate the grant. The schema only enforces
    /// `(consent_ref, seq)` uniqueness, so writers can append a
    /// `seq=2` revoke whose `decided_at` is earlier than the
    /// `seq=1` issue. Replay must be by `seq` alone — using
    /// `decided_at` as the primary sort key would let an attacker
    /// or a buggy writer reactivate a revoked grant.
    #[test]
    fn covering_grant_revoke_with_backdated_decided_at_still_clears() {
        let events = vec![
            // Issue at seq=1, decided LATER.
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T12:00:00Z",
                None,
            ),
            // Revoke at seq=2 with EARLIER decided_at — adversarial.
            revoked("g1", 2, "hooks.claude_code", "code", "2026-04-22T10:00:00Z"),
        ];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T13:00:00Z"),
        );
        assert!(
            g.is_none(),
            "backdated revoke at seq=2 must clear seq=1 issue regardless of decided_at order; got {g:?}",
        );
    }

    /// Same case for `Expired`: backdated terminal event should not
    /// replay-reorder around its issue.
    #[test]
    fn covering_grant_expire_with_backdated_decided_at_still_clears() {
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.claude_code",
                "code",
                "2026-04-22T12:00:00Z",
                None,
            ),
            expired("g1", 2, "hooks.claude_code", "code", "2026-04-22T10:00:00Z"),
        ];
        let g = CoveringGrant::resolve(
            &events,
            &sensor("hooks.claude_code"),
            "code",
            &ts("2026-04-22T13:00:00Z"),
        );
        assert!(
            g.is_none(),
            "backdated expire at seq=2 must clear seq=1 issue regardless of decided_at order; got {g:?}",
        );
    }

    // ─── Overlapping-grant matrix (loop 3 round 8 + 10) ──────────────
    //
    // These tests pin behavior under multiple grants on the same
    // (sensor, scope). The persistent risk is order-sensitivity: if
    // two grants both have seq=1, naive sort-by-seq preserves caller
    // order and one grant's terminal event can wrongly clear another
    // grant. Per-consent_ref grouped replay must produce the same
    // answer regardless of the input vec's order.

    /// Older open-ended grant + newer expired grant, both still
    /// "issued" structurally. The newer grant's expiry must NOT mask
    /// the older still-valid grant. Regression test for the round-10
    /// resolver fix.
    #[test]
    fn covering_grant_falls_back_to_older_when_newer_has_expired() {
        let events = vec![
            issued(
                "g_old",
                1,
                "hooks.a",
                "private",
                "2026-01-01T00:00:00Z",
                None,
            ),
            issued(
                "g_new",
                1,
                "hooks.a",
                "private",
                "2026-06-01T00:00:00Z",
                Some("2026-06-02T00:00:00Z"),
            ),
        ];
        // `at` is well past g_new's expiry, but inside g_old's
        // open-ended window. Coverage must come from g_old.
        let at = ts("2026-09-01T00:00:00Z");
        let g = CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at)
            .expect("older open-ended grant must still cover");
        assert_eq!(g.consent_ref, "g_old");
    }

    /// Same as above but with input vec reversed: per-consent_ref
    /// grouping must make the answer order-independent.
    #[test]
    fn covering_grant_falls_back_to_older_independent_of_order() {
        let events = vec![
            issued(
                "g_new",
                1,
                "hooks.a",
                "private",
                "2026-06-01T00:00:00Z",
                Some("2026-06-02T00:00:00Z"),
            ),
            issued(
                "g_old",
                1,
                "hooks.a",
                "private",
                "2026-01-01T00:00:00Z",
                None,
            ),
        ];
        let at = ts("2026-09-01T00:00:00Z");
        let g = CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at)
            .expect("order must not change resolution");
        assert_eq!(g.consent_ref, "g_old");
    }

    /// Newer revoked + older still-issued: revoke for `g_new` must
    /// not bleed into `g_old`. The per-`consent_ref` replay isolates
    /// terminal events to their own grant.
    #[test]
    fn covering_grant_newer_revoke_does_not_clear_older() {
        let events = vec![
            issued(
                "g_old",
                1,
                "hooks.a",
                "private",
                "2026-01-01T00:00:00Z",
                None,
            ),
            issued(
                "g_new",
                1,
                "hooks.a",
                "private",
                "2026-06-01T00:00:00Z",
                None,
            ),
            revoked("g_new", 2, "hooks.a", "private", "2026-07-01T00:00:00Z"),
        ];
        let at = ts("2026-08-01T00:00:00Z");
        let g = CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at)
            .expect("g_old must still cover after g_new revoked");
        assert_eq!(g.consent_ref, "g_old");
    }

    /// Two grants with the SAME `decided_at` must resolve
    /// deterministically (`consent_ref` tiebreak: lexicographically
    /// greater wins under `max_by`).
    #[test]
    fn covering_grant_same_decided_at_breaks_by_consent_ref() {
        let events = vec![
            issued("g_a", 1, "hooks.a", "private", "2026-04-01T00:00:00Z", None),
            issued("g_b", 1, "hooks.a", "private", "2026-04-01T00:00:00Z", None),
        ];
        let at = ts("2026-04-02T00:00:00Z");
        let g = CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at)
            .expect("must pick one deterministically");
        assert_eq!(
            g.consent_ref, "g_b",
            "lexicographically-greater consent_ref breaks the tie",
        );
    }

    /// `at == expires_at` is at-or-after expiry → None. Boundary
    /// pinning so the strict `at < expires_at` rule does not drift.
    #[test]
    fn covering_grant_at_equal_to_expires_at_is_none() {
        let events = vec![issued(
            "g1",
            1,
            "hooks.a",
            "private",
            "2026-04-22T10:00:00Z",
            Some("2026-04-22T11:00:00Z"),
        )];
        assert!(
            CoveringGrant::resolve(
                &events,
                &sensor("hooks.a"),
                "private",
                &ts("2026-04-22T11:00:00Z"),
            )
            .is_none(),
            "at == expires_at must NOT cover (exclusive upper bound)",
        );
    }

    /// All grants on the same `(sensor, scope)` are expired at `at`:
    /// no covering grant. Pins that the round-10 expiry filter
    /// rejects every candidate, not just the chosen one.
    #[test]
    fn covering_grant_none_when_every_overlapping_grant_expired() {
        let events = vec![
            issued(
                "g1",
                1,
                "hooks.a",
                "private",
                "2026-01-01T00:00:00Z",
                Some("2026-02-01T00:00:00Z"),
            ),
            issued(
                "g2",
                1,
                "hooks.a",
                "private",
                "2026-03-01T00:00:00Z",
                Some("2026-04-01T00:00:00Z"),
            ),
        ];
        let at = ts("2026-09-01T00:00:00Z");
        assert!(
            CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at).is_none(),
            "every grant has expired before `at` -- coverage must be None",
        );
    }

    /// Mixed `(sensor, scope)` rows in the slice must not cross-
    /// contaminate. Verifies the early `e.sensor_id == *sensor &&
    /// e.scope == scope` filter holds before the per-consent_ref
    /// replay.
    #[test]
    fn covering_grant_ignores_other_sensor_scope_grants() {
        let events = vec![
            issued(
                "g_other",
                1,
                "hooks.b",
                "private",
                "2026-01-01T00:00:00Z",
                None,
            ),
            issued(
                "g_me",
                1,
                "hooks.a",
                "private",
                "2026-04-01T00:00:00Z",
                None,
            ),
        ];
        let at = ts("2026-04-02T00:00:00Z");
        let g = CoveringGrant::resolve(&events, &sensor("hooks.a"), "private", &at)
            .expect("must find g_me");
        assert_eq!(g.consent_ref, "g_me");
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::domain::{Rfc3339Timestamp, SensorLabel};
    use proptest::prelude::*;

    fn arb_kind() -> impl Strategy<Value = ConsentTimelineEventKind> {
        prop_oneof![
            Just(ConsentTimelineEventKind::Issued),
            Just(ConsentTimelineEventKind::Revoked),
            Just(ConsentTimelineEventKind::Expired),
        ]
    }

    fn arb_event() -> impl Strategy<Value = ConsentTimelineEvent> {
        (
            1u64..=100,
            arb_kind(),
            1_700_000_000i64..1_800_000_000,
            prop::option::of(1_800_000_001i64..1_900_000_000),
        )
            .prop_map(|(seq, kind, decided, expires)| ConsentTimelineEvent {
                consent_ref: "c:1".to_owned(),
                seq,
                kind,
                sensor_id: SensorLabel::parse("local:s:h:v1").expect("invariant: valid sensor"),
                scope: "private".to_owned(),
                decided_at: Rfc3339Timestamp::from_unix_secs(decided)
                    .expect("invariant: valid timestamp"),
                expires_at: expires.map(|s| {
                    Rfc3339Timestamp::from_unix_secs(s).expect("invariant: valid timestamp")
                }),
            })
    }

    proptest! {
        #[test]
        fn resolve_is_order_independent(
            events in prop::collection::vec(arb_event(), 0..20)
        ) {
            // Round 7: replay sorts by `seq` alone, so ties on seq
            // would make `sort_by` (a stable sort) order-dependent
            // in the input. The DB schema rejects duplicate
            // (consent_ref, seq) pairs via the PK, so a real
            // timeline cannot contain ties — dedupe the generated
            // vec to mirror that invariant.
            let mut events = events;
            let mut seen_seqs = std::collections::HashSet::new();
            events.retain(|e| seen_seqs.insert(e.seq));

            let sensor = SensorLabel::parse("local:s:h:v1")
                .expect("invariant: valid sensor");
            let at = Rfc3339Timestamp::from_unix_secs(1_750_000_000)
                .expect("invariant: valid timestamp");

            let forward = CoveringGrant::resolve(&events, &sensor, "private", &at);
            let mut reversed = events.clone();
            reversed.reverse();
            let backward = CoveringGrant::resolve(&reversed, &sensor, "private", &at);

            prop_assert_eq!(forward, backward);
        }
    }
}
