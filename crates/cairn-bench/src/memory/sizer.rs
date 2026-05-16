//! Memory-gate sizer. Resolves asset paths, sums sizes, checks per-asset
//! tolerance band and total budget for enforced assets only.

use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::gates::report::GateOutcome;
use crate::memory::manifest::ResolvedProfile;

/// Default per-asset tolerance percentage applied when checking size bands.
pub const DEFAULT_ASSET_TOLERANCE_PCT: f64 = 25.0;

/// Measurement result for a single asset.
#[derive(Debug, Serialize, Deserialize)]
pub struct AssetResult {
    /// Asset source name.
    pub name: String,
    /// Measured size in MiB (0.0 for deferred or missing assets).
    pub size_mb: f64,
    /// Expected size in MiB from the manifest.
    pub expected_mb: f64,
    /// Tolerance percentage applied to the expected size.
    pub tolerance_pct: f64,
    /// Whether this asset was enforced in the manifest.
    pub enforced: bool,
    /// Whether this asset's check passed.
    pub ok: bool,
    /// Human-readable note (e.g. "deferred — no path helper at v0.1").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Full memory gate report, serialised to `target/cairn-bench/memory.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryReport {
    /// Schema version for forward-compat parsing.
    pub schema_version: u32,
    /// Profile name that was measured.
    pub profile: String,
    /// Budget from the manifest (MiB).
    pub budget_mb: f64,
    /// Total enforced asset size (MiB).
    pub total_mb: f64,
    /// Per-asset results.
    pub assets: Vec<AssetResult>,
    /// `true` iff all enforced checks passed and total is within budget.
    pub ok: bool,
    /// Human-readable failure descriptions.
    pub failures: Vec<String>,
}

/// Resolve a manifest `source` string into a filesystem path.
///
/// `embedding_model` resolves to the path under a vault's `.cairn/models/`.
/// All other sources are deferred (return `None`) — they're tracked in the
/// manifest as `enforced = false` and not measured in P0.
///
/// # Errors
/// Returns an error for unknown source names.
pub fn resolve_asset(source: &str, vault_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    match source {
        "embedding_model" => Ok(Some(vault_dir.join(".cairn").join("models"))),
        "sherpa_onnx_voice" | "screen_default" | "screenpipe_runtime" => Ok(None),
        other => anyhow::bail!("unknown memory-manifest asset source: {other}"),
    }
}

fn directory_size_bytes(path: &Path) -> anyhow::Result<u64> {
    let mut total = 0u64;
    if !path.exists() {
        return Ok(0);
    }
    if path.is_file() {
        return Ok(std::fs::metadata(path)?.len());
    }
    for entry in walkdir::WalkDir::new(path) {
        let entry = entry?;
        if entry.file_type().is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn mb(bytes: u64) -> f64 {
    // Precision loss is acceptable here: file sizes at MiB granularity never
    // need more than ~52 bits of integer precision.
    #[allow(clippy::cast_precision_loss)]
    let result = bytes as f64 / (1024.0 * 1024.0);
    result
}

/// Measure the resolved profile against a vault directory.
///
/// `vault_dir` is used to resolve runtime-fetched assets like the embedding
/// model. Pass a bootstrapped tempdir if `bootstrap` was run, or any directory
/// if `--no-bootstrap` was specified (the model will show 0 MB, which is
/// acceptable when the directory doesn't exist).
///
/// # Errors
/// Returns an error if binary metadata cannot be read or asset walks hit I/O errors.
pub fn measure(profile: &ResolvedProfile, vault_dir: &Path) -> anyhow::Result<MemoryReport> {
    let mut failures = Vec::new();
    let mut assets = Vec::new();
    let mut total = 0.0;

    // Binary
    let bin_path = Path::new(&profile.binary);
    let bin_size = if bin_path.exists() {
        mb(std::fs::metadata(bin_path)?.len())
    } else {
        failures.push(format!(
            "binary not found at {} — run `cargo build --release` first",
            profile.binary
        ));
        0.0
    };
    let bin_ok = bin_size > 0.0;
    total += bin_size;
    // Binary expected size: ~36 MB for the cairn debug/release binary.
    // This is a soft reference; the real gate is the total budget.
    assets.push(AssetResult {
        name: "binary".into(),
        size_mb: bin_size,
        expected_mb: 36.0,
        tolerance_pct: DEFAULT_ASSET_TOLERANCE_PCT,
        enforced: true,
        ok: bin_ok,
        note: None,
    });

    // Manifest-declared assets
    for spec in &profile.assets {
        if !spec.enforced {
            assets.push(AssetResult {
                name: spec.source.clone(),
                size_mb: 0.0,
                expected_mb: spec.expected_mb,
                tolerance_pct: DEFAULT_ASSET_TOLERANCE_PCT,
                enforced: false,
                ok: true,
                note: Some("deferred — no path helper at v0.1".into()),
            });
            continue;
        }
        let maybe_path = resolve_asset(&spec.source, vault_dir)?;
        let Some(path) = maybe_path else {
            assets.push(AssetResult {
                name: spec.source.clone(),
                size_mb: 0.0,
                expected_mb: spec.expected_mb,
                tolerance_pct: DEFAULT_ASSET_TOLERANCE_PCT,
                enforced: spec.enforced,
                ok: true,
                note: Some("deferred — no path helper".into()),
            });
            continue;
        };
        let size = if path.exists() {
            mb(directory_size_bytes(&path)?)
        } else {
            // Missing models directory means the model wasn't downloaded —
            // treat as 0 MB and report so the operator knows.
            failures.push(format!(
                "asset path missing: {} ({})",
                spec.source,
                path.display()
            ));
            0.0
        };
        let tol = DEFAULT_ASSET_TOLERANCE_PCT;
        let lo = spec.expected_mb * (1.0 - tol / 100.0);
        let hi = spec.expected_mb * (1.0 + tol / 100.0);
        let ok = size >= lo && size <= hi;
        if !ok {
            failures.push(format!(
                "asset {}: {:.1} MB outside band [{:.1}, {:.1}] (expected {:.1})",
                spec.source, size, lo, hi, spec.expected_mb
            ));
        }
        total += size;
        assets.push(AssetResult {
            name: spec.source.clone(),
            size_mb: size,
            expected_mb: spec.expected_mb,
            tolerance_pct: tol,
            enforced: spec.enforced,
            ok,
            note: None,
        });
    }

    if total > profile.budget_mb {
        failures.push(format!(
            "total {:.1} MB exceeds budget {:.1} MB",
            total, profile.budget_mb
        ));
    }

    let ok = failures.is_empty();
    Ok(MemoryReport {
        schema_version: 1,
        profile: profile.name.clone(),
        budget_mb: profile.budget_mb,
        total_mb: total,
        assets,
        ok,
        failures,
    })
}

/// Convert a [`MemoryReport`] to a [`GateOutcome`].
#[must_use]
pub fn outcome(r: &MemoryReport) -> GateOutcome {
    if r.ok {
        GateOutcome::Pass
    } else {
        GateOutcome::Fail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_pass_when_ok() {
        let r = MemoryReport {
            schema_version: 1,
            profile: "x".into(),
            budget_mb: 100.0,
            total_mb: 50.0,
            assets: vec![],
            ok: true,
            failures: vec![],
        };
        assert_eq!(outcome(&r), GateOutcome::Pass);
    }
}
