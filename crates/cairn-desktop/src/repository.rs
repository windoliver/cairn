//! In-memory fixture repository for the desktop GUI alpha.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use cairn_core::contract::frontend_adapter::FrontendFieldPolicy;

use crate::{
    fixture::DesktopFixture,
    model::{
        DesktopFolder, DesktopGraph, DesktopGraphEdge, DesktopGraphNode, DesktopLintFinding,
        DesktopReconcileApplyRequest, DesktopReconcileApplyResult, DesktopReconcilePreview,
        DesktopReconcilePreviewRequest, DesktopRecordDetail, DesktopRecordSummary,
        DesktopRejectedField, DesktopSearchResult, DesktopVaultSummary,
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
            {
                if record.body != body {
                    record.body = body.to_string();
                    changed = true;
                }
            }
            if let Some(tags) = preview.mutable_diff.get("tags").and_then(string_array) {
                if record.tags != tags {
                    record.tags = tags;
                    changed = true;
                }
            }
            if let Some(links) = preview.mutable_diff.get("wikilinks").and_then(string_array) {
                if record.links != links {
                    record.links = links;
                    changed = true;
                }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutableFieldValidation {
    Accepted,
    InvalidShape,
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
        "tags" if string_array(value).is_some() => MutableFieldValidation::Accepted,
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
