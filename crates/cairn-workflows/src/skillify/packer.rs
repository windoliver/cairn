//! `SkillPack` archive builder and unpacker.

use std::fs;
use std::path::{Path, PathBuf};

use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillPackEntry, SkillPackError, SkillPackManifest, SkillifyGateReport,
};
use sha2::{Digest, Sha256};

/// Error from `SkillPackBuilder::build` or `unpack_archive`.
#[derive(Debug, thiserror::Error)]
pub enum SkillPackBuildError {
    /// `SkillPack` validation failed.
    #[error(transparent)]
    Pack(#[from] SkillPackError),
    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON error.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Candidate gate report not passing.
    #[error("candidate `{candidate_id}` gate report not passing")]
    GateNotPassing {
        /// Candidate id.
        candidate_id: String,
    },
    /// Candidate not found.
    #[error("candidate `{candidate_id}` not found")]
    CandidateNotFound {
        /// Candidate id.
        candidate_id: String,
    },
}

/// Built archive result.
#[derive(Debug)]
pub struct SkillPackArchive {
    /// Validated manifest.
    pub manifest: SkillPackManifest,
    /// Path to the `.cairnpack` archive file.
    pub archive_path: PathBuf,
}

/// Builder for [`SkillPackArchive`] tar.gz archives.
pub struct SkillPackBuilder {
    name: String,
    version: String,
    cairn_compat: String,
    description: String,
    candidate_ids: Vec<String>,
}

impl SkillPackBuilder {
    /// Create a new builder.
    #[must_use]
    pub fn new(name: &str, version: &str, cairn_compat: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
            cairn_compat: cairn_compat.to_owned(),
            description: description.to_owned(),
            candidate_ids: Vec::new(),
        }
    }

    /// Add a candidate to the pack.
    #[must_use]
    pub fn add_candidate(mut self, candidate_id: &str) -> Self {
        self.candidate_ids.push(candidate_id.to_owned());
        self
    }

    /// Build the `.cairnpack` archive.
    ///
    /// # Errors
    /// Returns when a candidate is missing, has failing gates, or archive
    /// creation fails.
    pub fn build(self, vault_root: &Path) -> Result<SkillPackArchive, SkillPackBuildError> {
        let mut entries = Vec::new();
        let mut all_provides = Vec::new();

        for cid in &self.candidate_ids {
            let cand_dir = vault_root
                .join(".cairn/evolution/skillify")
                .join(cid);

            let manifest_path = cand_dir.join("manifest.json");
            if !manifest_path.exists() {
                return Err(SkillPackBuildError::CandidateNotFound {
                    candidate_id: cid.clone(),
                });
            }

            let bundle: SkillArtifactBundle =
                serde_json::from_slice(&fs::read(&manifest_path)?)?;

            let report_path = cand_dir.join("gate-report.json");
            let report: SkillifyGateReport =
                serde_json::from_slice(&fs::read(&report_path)?)?;

            if !report.ready_for_promotion() {
                return Err(SkillPackBuildError::GateNotPassing {
                    candidate_id: cid.clone(),
                });
            }

            let artifact_hash = sha256_file(&manifest_path)?;

            // Derive slug from the skill contract artifact path.
            let slug = bundle
                .artifacts
                .iter()
                .find(|a| {
                    a.path.contains("skills/skill_")
                        && std::path::Path::new(&a.path)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                })
                .and_then(|a| {
                    a.path
                        .rsplit('/')
                        .next()
                        .and_then(|f| f.strip_prefix("skill_"))
                        .and_then(|s| s.strip_suffix(".md"))
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| cid.clone());

            let lane = find_lane_from_bundle(&cand_dir, &slug)?;

            entries.push(SkillPackEntry {
                candidate_id: cid.clone(),
                lane: lane.clone(),
                slug,
                bundle_version: bundle.version,
                artifact_sha256: artifact_hash,
            });

            all_provides.push(lane);
        }

        let candidate_ids_refs: Vec<&str> =
            self.candidate_ids.iter().map(String::as_str).collect();
        let pack_id =
            SkillPackManifest::derive_pack_id(&self.name, &self.version, &candidate_ids_refs);

        let manifest = SkillPackManifest {
            pack_id,
            name: self.name.clone(),
            version: self.version.clone(),
            cairn_compat: self.cairn_compat.clone(),
            description: self.description.clone(),
            skills: entries,
            requires: Vec::new(),
            provides: all_provides,
            content_sha256: String::new(),
        };

        // Build the tar.gz archive in a temporary file then rename.
        let archive_path = vault_root.join(format!("{}.cairnpack", self.name));
        let file = fs::File::create(&archive_path)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        append_bytes(&mut tar, "manifest.json", &manifest_json)?;

        for cid in &self.candidate_ids {
            let cand_dir = vault_root
                .join(".cairn/evolution/skillify")
                .join(cid);
            append_dir_recursive(&mut tar, &cand_dir, &format!("skills/{cid}"))?;
        }

        // Flush and finish the archive.
        let enc = tar.into_inner()?;
        enc.finish()?;

        // Recompute content hash after archive is complete.
        let content_hash = sha256_file(&archive_path)?;
        let mut final_manifest = manifest;
        final_manifest.content_sha256 = content_hash;

        Ok(SkillPackArchive {
            manifest: final_manifest,
            archive_path,
        })
    }
}

/// Unpack a `.cairnpack` archive into a vault.
///
/// Validates the manifest's `cairn_compat` requirement against the running
/// version before extracting any files.
///
/// # Errors
/// Returns on incompatible version, corrupt archive, or I/O failure.
pub fn unpack_archive(
    archive_path: &Path,
    vault_root: &Path,
    cairn_version: &str,
) -> Result<SkillPackManifest, SkillPackBuildError> {
    let file = fs::File::open(archive_path)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    let extract_dir = vault_root.join(".cairn/evolution/skillify/.unpack-tmp");
    fs::create_dir_all(&extract_dir)?;
    archive.unpack(&extract_dir)?;

    let manifest: SkillPackManifest = serde_json::from_slice(&fs::read(
        extract_dir.join("manifest.json"),
    )?)?;

    // Validate compat before moving any files.
    manifest
        .validate(cairn_version)
        .map_err(SkillPackBuildError::Pack)?;

    // Move skill directories into their final locations.
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        let dst = vault_root
            .join(".cairn/evolution/skillify")
            .join(&entry.candidate_id);
        if src.exists() {
            if dst.exists() {
                fs::remove_dir_all(&dst)?;
            }
            fs::rename(&src, &dst)?;
        }
    }

    // Clean up temporary extraction directory.
    let _ = fs::remove_dir_all(&extract_dir);

    Ok(manifest)
}

// -- Internal helpers -------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("sha256:{:x}", h.finalize()))
}

/// Read the `lane:` value from the skill contract markdown in the candidate bundle.
fn find_lane_from_bundle(
    cand_dir: &Path,
    slug: &str,
) -> Result<String, SkillPackBuildError> {
    let skill_path = cand_dir.join(format!("bundle/skills/skill_{slug}.md"));
    if skill_path.exists() {
        let content = fs::read_to_string(&skill_path)?;
        for line in content.lines() {
            if let Some(lane) = line.strip_prefix("lane:") {
                let trimmed = lane.trim();
                if !trimmed.is_empty() {
                    return Ok(trimmed.to_owned());
                }
            }
        }
    }
    // Fallback: scan all markdown files in the bundle/skills dir.
    let skills_dir = cand_dir.join("bundle/skills");
    if skills_dir.exists() {
        for entry in fs::read_dir(&skills_dir)? {
            let entry = entry?;
            let content = fs::read_to_string(entry.path())?;
            for line in content.lines() {
                if let Some(lane) = line.strip_prefix("lane:") {
                    let trimmed = lane.trim();
                    if !trimmed.is_empty() {
                        return Ok(trimmed.to_owned());
                    }
                }
            }
        }
    }
    Ok("unknown".to_owned())
}

fn append_bytes<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, data)?;
    Ok(())
}

fn append_dir_recursive<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    dir: &Path,
    prefix: &str,
) -> Result<(), std::io::Error> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let archive_path = format!("{prefix}/{name}");
        if path.is_dir() {
            append_dir_recursive(tar, &path, &archive_path)?;
        } else {
            let data = fs::read(&path)?;
            append_bytes(tar, &archive_path, &data)?;
        }
    }
    Ok(())
}
