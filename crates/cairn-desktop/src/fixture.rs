//! Fixture loading for the desktop GUI alpha.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

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
        Ok(())
    }
}

fn default_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DEFAULT_FIXTURE_PATH)
}
