//! In-memory fixture repository for the desktop GUI alpha.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use cairn_core::{
    contract::frontend_adapter::FrontendFieldPolicy,
    domain::{
        SreDetail, SreGateResult, SreGateSummary, SreMeasurement, SrePrivacySummary,
        SreProjectionSummary, SreProjectionTargetSummary, SreRehydrationSummary, SreReport,
        SreSearchModeSummary, SreSearchSummary, SreStatus, SreVaultSummary, SreWorkflowKindSummary,
        SreWorkflowSummary,
    },
};

use crate::{
    fixture::DesktopFixture,
    model::{
        DesktopFolder, DesktopGraph, DesktopGraphEdge, DesktopGraphNode, DesktopLintFinding,
        DesktopReconcileApplyRequest, DesktopReconcileApplyResult, DesktopReconcilePreview,
        DesktopReconcilePreviewRequest, DesktopRecordDetail, DesktopRecordSummary,
        DesktopRejectedField, DesktopSearchResult, DesktopSessionTree, DesktopSreReport,
        DesktopVaultSummary,
    },
};

/// Fixture-backed repository used by the desktop alpha.
#[derive(Debug, Clone)]
pub struct DesktopRepository {
    fixture: Arc<RwLock<DesktopFixture>>,
}

impl DesktopRepository {
    /// Build a repository from a fixture.
    #[must_use]
    pub fn from_fixture(fixture: DesktopFixture) -> Self {
        Self {
            fixture: Arc::new(RwLock::new(fixture)),
        }
    }

    /// Return the loaded vault summary.
    #[must_use]
    pub fn vault(&self) -> DesktopVaultSummary {
        self.fixture().vault.clone()
    }

    /// Return all folders.
    #[must_use]
    pub fn folders(&self) -> Vec<DesktopFolder> {
        self.fixture().folders.clone()
    }

    /// Return record summaries.
    #[must_use]
    pub fn records(&self) -> Vec<DesktopRecordSummary> {
        self.fixture()
            .records
            .iter()
            .map(|record| DesktopRecordSummary {
                id: record.id.clone(),
                title: record.title.clone(),
                folder_id: record.folder_id.clone(),
                kind: record.kind.clone(),
                tags: record.tags.clone(),
                version: record.version,
                confidence: record.confidence,
            })
            .collect()
    }

    /// Return one record detail by id.
    #[must_use]
    pub fn record(&self, id: &str) -> Option<DesktopRecordDetail> {
        self.fixture()
            .records
            .iter()
            .find(|record| record.id == id)
            .cloned()
    }

    /// Return derived graph data.
    #[must_use]
    pub fn graph(&self) -> DesktopGraph {
        let nodes = self
            .fixture()
            .records
            .iter()
            .map(|record| DesktopGraphNode {
                id: record.id.clone(),
                label: record.title.clone(),
                kind: record.kind.clone(),
                group: record.folder_id.clone(),
            })
            .collect();

        let edges = self
            .fixture()
            .records
            .iter()
            .flat_map(|record| {
                record.links.iter().map(|target| DesktopGraphEdge {
                    id: format!("{}--{}", record.id, target),
                    source: record.id.clone(),
                    target: target.clone(),
                    label: "wikilink".to_string(),
                })
            })
            .collect();

        DesktopGraph { nodes, edges }
    }

    /// Return session-tree data for visualization.
    #[must_use]
    pub fn session_tree(&self) -> DesktopSessionTree {
        self.fixture().session_tree.clone()
    }

    /// Return deterministic fixture search results.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<DesktopSearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let mut results: Vec<_> = self
            .fixture()
            .records
            .iter()
            .filter_map(|record| {
                let haystack =
                    format!("{} {} {}", record.title, record.tags.join(" "), record.body)
                        .to_lowercase();
                if !haystack.contains(&query) {
                    return None;
                }
                let title_hit = record.title.to_lowercase().contains(&query);
                Some(DesktopSearchResult {
                    record_id: record.id.clone(),
                    title: record.title.clone(),
                    snippet: record.body.chars().take(96).collect(),
                    score: if title_hit { 1.0 } else { 0.7 },
                })
            })
            .collect();
        results.sort_by(|left, right| right.score.total_cmp(&left.score));
        results
    }

    /// Return fixture lint findings.
    #[must_use]
    pub fn lint_findings(&self) -> Vec<DesktopLintFinding> {
        self.fixture().lint_findings.clone()
    }

    /// Return deterministic fixture SRE data for the desktop dashboard alpha.
    #[must_use]
    pub fn sre_report(&self) -> DesktopSreReport {
        let vault = self.vault();
        SreReport {
            schema_version: 1,
            captured_at_ms: 1_700_000_000_000,
            vault: SreVaultSummary {
                id_hash: "sha256:desktop-alpha".to_string(),
                name: vault.name,
            },
            workflow: fixture_sre_workflow(),
            rehydration: fixture_sre_rehydration(),
            projection: fixture_sre_projection(),
            search: fixture_sre_search(),
            gates: fixture_sre_gates(),
            privacy: SrePrivacySummary {
                scrubbed: true,
                forbidden_field_count: 0,
            },
        }
    }

    /// Preview a reconcile request without mutating backend state.
    #[must_use]
    pub fn preview_reconcile(
        &self,
        request: DesktopReconcilePreviewRequest,
    ) -> DesktopReconcilePreview {
        let Some(record) = self.record(&request.target_id) else {
            return rejected_preview(
                request,
                "target",
                "record_not_found",
                "Record was not found",
            );
        };
        if record.version != request.expected_version {
            return rejected_preview(
                request,
                "version",
                "version_conflict",
                "Record version does not match the projected version",
            );
        }
        if record.backend_hash != request.backend_hash {
            return rejected_preview(
                request,
                "backendHash",
                "target_hash_mismatch",
                "Backend hash does not match the projected record hash",
            );
        }

        let record_ids: BTreeSet<_> = self
            .fixture()
            .records
            .iter()
            .map(|record| record.id.clone())
            .collect();
        let mut mutable_diff = BTreeMap::new();
        let mut rejected_fields = Vec::new();
        for (field, value) in request.field_diff {
            match validate_mutable_field(&field, &value, &record_ids) {
                MutableFieldValidation::Accepted => {
                    mutable_diff.insert(field, value);
                }
                MutableFieldValidation::InvalidShape => {
                    rejected_fields.push(DesktopRejectedField {
                        field,
                        code: "invalid_field_shape".to_string(),
                        message:
                            "Mutable field has an unsupported value shape for the desktop alpha"
                                .to_string(),
                    });
                }
                MutableFieldValidation::DuplicateTag => {
                    rejected_fields.push(DesktopRejectedField {
                        field,
                        code: "duplicate_tag".to_string(),
                        message: "Tags must be unique in the desktop fixture".to_string(),
                    });
                }
                MutableFieldValidation::UnknownWikilinkTarget => {
                    rejected_fields.push(DesktopRejectedField {
                        field,
                        code: "unknown_wikilink_target".to_string(),
                        message: "Wikilink target was not found in the desktop fixture".to_string(),
                    });
                }
                MutableFieldValidation::DuplicateWikilinkTarget => {
                    rejected_fields.push(DesktopRejectedField {
                        field,
                        code: "duplicate_wikilink_target".to_string(),
                        message: "Wikilink targets must be unique in the desktop fixture"
                            .to_string(),
                    });
                }
                MutableFieldValidation::Immutable => {
                    rejected_fields.push(DesktopRejectedField {
                        field,
                        code: "immutable_field_changed".to_string(),
                        message: "Field is owned by the backend and cannot be changed by the GUI"
                            .to_string(),
                    });
                }
            }
        }

        if !rejected_fields.is_empty() {
            mutable_diff.clear();
        }

        DesktopReconcilePreview {
            accepted: rejected_fields.is_empty(),
            target_id: request.target_id,
            expected_version: request.expected_version,
            mutable_diff,
            rejected_fields,
        }
    }

    /// Apply a reconcile request against the in-memory fixture model.
    #[must_use]
    pub fn apply_reconcile(
        &self,
        request: DesktopReconcileApplyRequest,
    ) -> DesktopReconcileApplyResult {
        let preview = self.preview_reconcile(request.preview);
        if !preview.accepted {
            return DesktopReconcileApplyResult {
                accepted: false,
                record: None,
                rejected_fields: preview.rejected_fields,
            };
        }

        let mut fixture = self.fixture_mut();
        let record = fixture
            .records
            .iter_mut()
            .find(|record| record.id == preview.target_id);
        let mut updated_record = None;
        if let Some(record) = record {
            let mut changed = false;
            if let Some(body) = preview
                .mutable_diff
                .get("body")
                .and_then(serde_json::Value::as_str)
                && record.body != body
            {
                record.body = body.to_string();
                changed = true;
            }
            if let Some(tags) = preview.mutable_diff.get("tags").and_then(string_array)
                && record.tags != tags
            {
                record.tags = tags;
                changed = true;
            }
            if let Some(links) = preview.mutable_diff.get("wikilinks").and_then(string_array)
                && record.links != links
            {
                record.links = links;
                changed = true;
            }
            if changed {
                record.version += 1;
                record.backend_hash = format!("sha256:{}-v{}", record.id, record.version);
            }
            updated_record = Some(record.clone());
        }

        DesktopReconcileApplyResult {
            accepted: updated_record.is_some(),
            record: updated_record,
            rejected_fields: Vec::new(),
        }
    }

    fn fixture(&self) -> RwLockReadGuard<'_, DesktopFixture> {
        self.fixture.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn fixture_mut(&self) -> RwLockWriteGuard<'_, DesktopFixture> {
        self.fixture.write().unwrap_or_else(PoisonError::into_inner)
    }
}

fn measurement(value: f64) -> SreMeasurement {
    SreMeasurement::new(value).expect("finite desktop SRE fixture measurement")
}

fn fixture_sre_workflow() -> SreWorkflowSummary {
    SreWorkflowSummary {
        status: SreStatus::Warning,
        oldest_queued_age_ms: Some(742_000),
        longest_held_lease_ms: Some(125_000),
        dead_letter_count: 1,
        kinds: vec![
            SreWorkflowKindSummary {
                kind: "expire.tier".to_string(),
                queued: 2,
                leased: 1,
                done_recent: 3,
                failed_recent: 0,
                oldest_queued_age_ms: Some(742_000),
                last_success_age_ms: Some(50_000),
                backlog_threshold_ms: 600_000,
                status: SreStatus::Warning,
            },
            SreWorkflowKindSummary {
                kind: "projection.rebuild".to_string(),
                queued: 0,
                leased: 0,
                done_recent: 5,
                failed_recent: 0,
                oldest_queued_age_ms: None,
                last_success_age_ms: Some(12_000),
                backlog_threshold_ms: 600_000,
                status: SreStatus::Ok,
            },
        ],
    }
}

fn fixture_sre_rehydration() -> SreRehydrationSummary {
    SreRehydrationSummary {
        status: SreStatus::Ok,
        latest_latency_ms: Some(2_100),
        p95_latency_ms: Some(measurement(2_210.0)),
        slo_ms: measurement(3_000.0),
        sample_count: 12,
        last_gate: Some(SreGateResult {
            name: "cold_rehydrate_p95".to_string(),
            status: SreStatus::Ok,
            measured: Some(measurement(2_210.0)),
            threshold: Some(measurement(3_000.0)),
            unit: "ms".to_string(),
            detail: SreDetail::stable("desktop_fixture"),
        }),
    }
}

fn fixture_sre_projection() -> SreProjectionSummary {
    SreProjectionSummary {
        status: SreStatus::Warning,
        nexus_state: "degraded".to_string(),
        nexus_reason: Some("sidecar_unavailable".to_string()),
        targets: vec![SreProjectionTargetSummary {
            target: "nexus".to_string(),
            current: 128,
            stale: 2,
            failed: 0,
            missing: 0,
            max_lag_ms: Some(90_000),
            last_rebuild_latency_ms: Some(410),
            status: SreStatus::Warning,
        }],
    }
}

fn fixture_sre_search() -> SreSearchSummary {
    SreSearchSummary {
        status: SreStatus::Warning,
        modes: vec![
            SreSearchModeSummary {
                mode: "keyword".to_string(),
                advertised: true,
                invocations: 64,
                degraded: 0,
                failed: 0,
                p95_latency_ms: Some(measurement(18.0)),
                status: SreStatus::Ok,
            },
            SreSearchModeSummary {
                mode: "semantic".to_string(),
                advertised: true,
                invocations: 42,
                degraded: 3,
                failed: 0,
                p95_latency_ms: Some(measurement(54.0)),
                status: SreStatus::Warning,
            },
        ],
    }
}

fn fixture_sre_gates() -> SreGateSummary {
    SreGateSummary {
        status: SreStatus::Warning,
        gates: vec![
            SreGateResult {
                name: "migration_backlog".to_string(),
                status: SreStatus::Warning,
                measured: Some(measurement(742_000.0)),
                threshold: Some(measurement(600_000.0)),
                unit: "ms".to_string(),
                detail: SreDetail::stable("desktop_fixture"),
            },
            SreGateResult {
                name: "sre_privacy_scrub".to_string(),
                status: SreStatus::Ok,
                measured: Some(measurement(0.0)),
                threshold: Some(measurement(0.0)),
                unit: "forbidden_fields".to_string(),
                detail: SreDetail::stable("redacted"),
            },
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutableFieldValidation {
    Accepted,
    InvalidShape,
    DuplicateTag,
    UnknownWikilinkTarget,
    DuplicateWikilinkTarget,
    Immutable,
}

fn validate_mutable_field(
    field: &str,
    value: &serde_json::Value,
    record_ids: &BTreeSet<String>,
) -> MutableFieldValidation {
    match field {
        "body" if value.is_string() => MutableFieldValidation::Accepted,
        "tags" => {
            let Some(tags) = string_array(value) else {
                return MutableFieldValidation::InvalidShape;
            };
            let unique_tags: BTreeSet<_> = tags.iter().collect();
            if unique_tags.len() == tags.len() {
                MutableFieldValidation::Accepted
            } else {
                MutableFieldValidation::DuplicateTag
            }
        }
        "wikilinks" => {
            let Some(links) = string_array(value) else {
                return MutableFieldValidation::InvalidShape;
            };
            let unique_links: BTreeSet<_> = links.iter().collect();
            if unique_links.len() != links.len() {
                MutableFieldValidation::DuplicateWikilinkTarget
            } else if links.iter().all(|link| record_ids.contains(link)) {
                MutableFieldValidation::Accepted
            } else {
                MutableFieldValidation::UnknownWikilinkTarget
            }
        }
        _ if FrontendFieldPolicy::is_mutable_from_frontend(field) => {
            MutableFieldValidation::InvalidShape
        }
        _ => MutableFieldValidation::Immutable,
    }
}

fn string_array(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect()
}

fn rejected_preview(
    request: DesktopReconcilePreviewRequest,
    field: &str,
    code: &str,
    message: &str,
) -> DesktopReconcilePreview {
    DesktopReconcilePreview {
        accepted: false,
        target_id: request.target_id,
        expected_version: request.expected_version,
        mutable_diff: BTreeMap::new(),
        rejected_fields: vec![DesktopRejectedField {
            field: field.to_string(),
            code: code.to_string(),
            message: message.to_string(),
        }],
    }
}
