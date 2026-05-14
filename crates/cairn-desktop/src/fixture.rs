//! Fixture loading for the desktop GUI alpha.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use cairn_core::contract::frontend_adapter::FrontendFieldPolicy;

use crate::{
    error::{DesktopError, DesktopResult},
    model::{DesktopFolder, DesktopLintFinding, DesktopRecordDetail, DesktopVaultSummary},
};

/// Built-in fixture path used by tests and local development.
pub const DEFAULT_FIXTURE_PATH: &str = "fixtures/desktop-gui-alpha/vault.json";

/// Complete desktop fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopFixture {
    /// Vault summary.
    pub vault: DesktopVaultSummary,
    /// Fixture folders.
    pub folders: Vec<DesktopFolder>,
    /// Fixture records.
    pub records: Vec<DesktopRecordDetail>,
    /// Fixture lint findings.
    pub lint_findings: Vec<DesktopLintFinding>,
    /// Fixture reconcile examples.
    pub reconcile_examples: DesktopReconcileExamples,
}

/// Fixture examples for reconcile tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileExamples {
    /// Record id that accepts mutable edits.
    pub mutable_record_id: String,
    /// Field expected to be rejected as immutable.
    pub immutable_field: String,
}

impl DesktopFixture {
    /// Load the default repo fixture.
    pub fn load_default() -> DesktopResult<Self> {
        Self::load_from_path(default_fixture_path())
    }

    /// Load a fixture from a JSON path.
    pub fn load_from_path(path: impl AsRef<Path>) -> DesktopResult<Self> {
        let path = path.as_ref();
        let body = fs::read_to_string(path).map_err(|source| DesktopError::Fixture {
            message: format!("failed to read {}: {source}", path.display()),
        })?;
        let fixture: Self =
            serde_json::from_str(&body).map_err(|source| DesktopError::Fixture {
                message: format!("failed to parse {}: {source}", path.display()),
            })?;
        fixture.validate()?;
        Ok(fixture)
    }

    fn validate(&self) -> DesktopResult<()> {
        if self.vault.record_count != self.records.len() {
            return Err(DesktopError::Fixture {
                message: format!(
                    "vault recordCount {} does not match {} loaded records",
                    self.vault.record_count,
                    self.records.len()
                ),
            });
        }
        if self.vault.folder_count != self.folders.len() {
            return Err(DesktopError::Fixture {
                message: format!(
                    "vault folderCount {} does not match {} loaded folders",
                    self.vault.folder_count,
                    self.folders.len()
                ),
            });
        }
        let mut folder_ids = BTreeSet::new();
        for folder in &self.folders {
            if !folder_ids.insert(&folder.id) {
                return Err(DesktopError::Fixture {
                    message: format!("duplicate folder id {}", folder.id),
                });
            }
        }
        let folder_parents: BTreeMap<_, _> = self
            .folders
            .iter()
            .map(|folder| (&folder.id, folder.parent_id.as_ref()))
            .collect();
        for folder in &self.folders {
            if let Some(parent_id) = &folder.parent_id {
                if parent_id == &folder.id {
                    return Err(DesktopError::Fixture {
                        message: format!("folder {} references parentId itself", folder.id),
                    });
                }
                if !folder_ids.contains(parent_id) {
                    return Err(DesktopError::Fixture {
                        message: format!(
                            "folder {} references unknown parentId {}",
                            folder.id, parent_id
                        ),
                    });
                }
            }
            let mut ancestors = BTreeSet::new();
            let mut next_parent = folder.parent_id.as_ref();
            while let Some(parent_id) = next_parent {
                if !ancestors.insert(parent_id) {
                    return Err(DesktopError::Fixture {
                        message: format!("folder {} has a parent cycle", folder.id),
                    });
                }
                next_parent = folder_parents.get(parent_id).and_then(|parent| *parent);
            }
        }
        let mut record_ids = BTreeSet::new();
        for record in &self.records {
            if !record_ids.insert(&record.id) {
                return Err(DesktopError::Fixture {
                    message: format!("duplicate record id {}", record.id),
                });
            }
        }
        for record in &self.records {
            if !folder_ids.contains(&record.folder_id) {
                return Err(DesktopError::Fixture {
                    message: format!(
                        "record {} references unknown folderId {}",
                        record.id, record.folder_id
                    ),
                });
            }
            let mut tag_ids = BTreeSet::new();
            for tag in &record.tags {
                if !tag_ids.insert(tag) {
                    return Err(DesktopError::Fixture {
                        message: format!("record {} has duplicate tag {}", record.id, tag),
                    });
                }
            }
            let mut link_ids = BTreeSet::new();
            for link in &record.links {
                if !link_ids.insert(link) {
                    return Err(DesktopError::Fixture {
                        message: format!("record {} has duplicate link {}", record.id, link),
                    });
                }
                if !record_ids.contains(link) {
                    return Err(DesktopError::Fixture {
                        message: format!("record {} links to unknown record {}", record.id, link),
                    });
                }
            }
        }
        let mut lint_ids = BTreeSet::new();
        for finding in &self.lint_findings {
            if !lint_ids.insert(&finding.id) {
                return Err(DesktopError::Fixture {
                    message: format!("duplicate lint finding id {}", finding.id),
                });
            }
            if let Some(record_id) = &finding.record_id {
                if !record_ids.contains(record_id) {
                    return Err(DesktopError::Fixture {
                        message: format!(
                            "lint finding {} references unknown recordId {}",
                            finding.id, record_id
                        ),
                    });
                }
            }
        }
        if !record_ids.contains(&self.reconcile_examples.mutable_record_id) {
            return Err(DesktopError::Fixture {
                message: format!(
                    "reconcile example references unknown record {}",
                    self.reconcile_examples.mutable_record_id
                ),
            });
        }
        if FrontendFieldPolicy::is_mutable_from_frontend(&self.reconcile_examples.immutable_field) {
            return Err(DesktopError::Fixture {
                message: format!(
                    "reconcile example immutableField {} is mutable from the frontend",
                    self.reconcile_examples.immutable_field
                ),
            });
        }
        Ok(())
    }
}

fn default_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DEFAULT_FIXTURE_PATH)
}
