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
            let cand_dir = vault_root.join(".cairn/evolution/skillify").join(cid);

            let manifest_path = cand_dir.join("manifest.json");
            if !manifest_path.exists() {
                return Err(SkillPackBuildError::CandidateNotFound {
                    candidate_id: cid.clone(),
                });
            }

            let bundle: SkillArtifactBundle = serde_json::from_slice(&fs::read(&manifest_path)?)?;

            let report_path = cand_dir.join("gate-report.json");
            let report: SkillifyGateReport = serde_json::from_slice(&fs::read(&report_path)?)?;

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

        let candidate_ids_refs: Vec<&str> = self.candidate_ids.iter().map(String::as_str).collect();
        let pack_id =
            SkillPackManifest::derive_pack_id(&self.name, &self.version, &candidate_ids_refs);

        let mut manifest = SkillPackManifest {
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

        // Compute a deterministic content digest BEFORE serializing the
        // manifest into the tarball. The digest covers the manifest metadata
        // (with `content_sha256` empty) plus the sorted list of per-skill
        // artifact hashes. This is verifiable at install time without the
        // chicken-and-egg problem of hashing the archive that contains the
        // hash field.
        let content_hash = compute_manifest_digest(&manifest);
        manifest.content_sha256 = content_hash;

        // Build the tar.gz archive.
        let archive_path = vault_root.join(format!("{}.cairnpack", self.name));
        let file = fs::File::create(&archive_path)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let manifest_json = serde_json::to_vec_pretty(&manifest)?;
        append_bytes(&mut tar, "manifest.json", &manifest_json, 0o644)?;

        for cid in &self.candidate_ids {
            let cand_dir = vault_root.join(".cairn/evolution/skillify").join(cid);
            append_dir_recursive(&mut tar, &cand_dir, &format!("skills/{cid}"))?;
        }

        let enc = tar.into_inner()?;
        enc.finish()?;

        Ok(SkillPackArchive {
            manifest,
            archive_path,
        })
    }
}

/// Compute a deterministic digest over manifest metadata + sorted artifact
/// hashes. The `content_sha256` field is excluded so the digest can be
/// embedded in the manifest itself (verified by recomputing with the field
/// cleared at install time).
fn compute_manifest_digest(manifest: &SkillPackManifest) -> String {
    let mut hasher = Sha256::new();
    let h = &mut hasher;
    update_field(h, "pack_id", &manifest.pack_id);
    update_field(h, "name", &manifest.name);
    update_field(h, "version", &manifest.version);
    update_field(h, "cairn_compat", &manifest.cairn_compat);
    update_field(h, "description", &manifest.description);

    // Sort skills by candidate_id for determinism.
    let mut skills: Vec<&cairn_core::pipeline::skillify::SkillPackEntry> =
        manifest.skills.iter().collect();
    skills.sort_by(|a, b| a.candidate_id.cmp(&b.candidate_id));
    hasher.update(b"skills:");
    hasher.update(skills.len().to_le_bytes());
    for entry in skills {
        update_field(&mut hasher, "  candidate_id", &entry.candidate_id);
        update_field(&mut hasher, "  lane", &entry.lane);
        update_field(&mut hasher, "  slug", &entry.slug);
        hasher.update(b"  bundle_version:");
        hasher.update(entry.bundle_version.to_le_bytes());
        update_field(&mut hasher, "  artifact_sha256", &entry.artifact_sha256);
    }

    let mut requires = manifest.requires.clone();
    requires.sort();
    hasher.update(b"requires:");
    hasher.update(requires.len().to_le_bytes());
    for v in requires {
        update_field(&mut hasher, "  requires_item", &v);
    }

    let mut provides = manifest.provides.clone();
    provides.sort();
    hasher.update(b"provides:");
    hasher.update(provides.len().to_le_bytes());
    for v in provides {
        update_field(&mut hasher, "  provides_item", &v);
    }

    format!("sha256:{:x}", hasher.finalize())
}

fn update_field(hasher: &mut Sha256, label: &str, value: &str) {
    hasher.update(label.as_bytes());
    hasher.update(b":");
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
    hasher.update(b"\0");
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

    // Use a per-call temp directory to avoid colliding with concurrent installs.
    let extract_dir = vault_root.join(format!(
        ".cairn/evolution/skillify/.unpack-tmp-{}",
        std::process::id()
    ));
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)?;
    }
    fs::create_dir_all(&extract_dir)?;

    // Use a guard so the temp dir is always cleaned up, even on error paths.
    let _cleanup = TempDirGuard(extract_dir.clone());

    archive.unpack(&extract_dir)?;

    let manifest: SkillPackManifest =
        serde_json::from_slice(&fs::read(extract_dir.join("manifest.json"))?)?;

    // Validate manifest fields and Cairn-version compat before touching the vault.
    manifest
        .validate(cairn_version)
        .map_err(SkillPackBuildError::Pack)?;

    // Verify integrity digest. Recompute the digest with the field cleared
    // and compare against the claimed value. A mismatch means the manifest
    // has been tampered with (or the pack was built by a version with a
    // different digest algorithm — operator must rebuild).
    let claimed = manifest.content_sha256.clone();
    if !claimed.is_empty() {
        let mut for_digest = manifest.clone();
        for_digest.content_sha256 = String::new();
        let actual = compute_manifest_digest(&for_digest);
        if actual != claimed {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: claimed,
                    actual,
                },
            ));
        }
    }

    // PRE-FLIGHT: every manifest entry must have a complete bundle in the
    // extracted temp dir. Reject the whole install if any skill is missing —
    // we will not delete a valid installed candidate just to replace it with
    // an incomplete one. This is the rollback guarantee Codex review #128
    // flagged.
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        if !src.is_dir() {
            return Err(SkillPackBuildError::Pack(SkillPackError::MissingSkill {
                candidate_id: entry.candidate_id.clone(),
            }));
        }
        // Require a manifest.json and bundle/ inside the candidate; this is
        // the minimum proof that the source is a complete bundle, not a stub.
        if !src.join("manifest.json").is_file() || !src.join("bundle").is_dir() {
            return Err(SkillPackBuildError::Pack(SkillPackError::MissingSkill {
                candidate_id: entry.candidate_id.clone(),
            }));
        }
    }

    // ATOMIC SWAP: now that every source is validated, move each skill into
    // place. For pre-existing destinations, swap-then-delete keeps the old
    // copy until the new one is in place; on any io error the old copy is
    // restored where possible. Errors mid-install leave the vault in a
    // partially-installed state — that is logged via the returned error so
    // the operator can re-run install.
    let parent = vault_root.join(".cairn/evolution/skillify");
    fs::create_dir_all(&parent)?;
    let mut swapped_backups: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        let dst = parent.join(&entry.candidate_id);
        let backup = parent.join(format!(
            ".bak-{}-{}",
            entry.candidate_id,
            std::process::id()
        ));
        if dst.exists() {
            // Rename existing → backup first; rename new → dst; delete backup last.
            if let Err(e) = fs::rename(&dst, &backup) {
                // Restore any prior swaps we already performed.
                rollback_swaps(&swapped_backups);
                return Err(SkillPackBuildError::Io(e));
            }
            swapped_backups.push((dst.clone(), backup.clone()));
        }
        if let Err(e) = fs::rename(&src, &dst) {
            rollback_swaps(&swapped_backups);
            return Err(SkillPackBuildError::Io(e));
        }
    }

    // All swaps succeeded — remove backups.
    for (_dst, backup) in &swapped_backups {
        let _ = fs::remove_dir_all(backup);
    }

    Ok(manifest)
}

/// Restore destination directories from their backup names after a failed swap.
fn rollback_swaps(backups: &[(std::path::PathBuf, std::path::PathBuf)]) {
    for (dst, backup) in backups.iter().rev() {
        // If dst now contains the (broken) new content, remove it first.
        if dst.exists() {
            let _ = fs::remove_dir_all(dst);
        }
        let _ = fs::rename(backup, dst);
    }
}

/// RAII guard that removes its tracked directory when dropped. Ensures the
/// unpack temp directory is cleaned up on every code path, including panics.
struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// -- Internal helpers -------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let bytes = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(format!("sha256:{:x}", h.finalize()))
}

/// Read the `lane:` value from the skill contract markdown in the candidate bundle.
fn find_lane_from_bundle(cand_dir: &Path, slug: &str) -> Result<String, SkillPackBuildError> {
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
    mode: u32,
) -> Result<(), std::io::Error> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    tar.append_data(&mut header, path, data)?;
    Ok(())
}

/// Return 0o755 if the file is executable by owner (Unix), else 0o644.
/// On non-Unix, returns 0o644.
fn file_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let perm_mode = meta.permissions().mode();
            if perm_mode & 0o100 != 0 {
                return 0o755;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    0o644
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
            let mode = file_mode(&path);
            append_bytes(tar, &archive_path, &data, mode)?;
        }
    }
    Ok(())
}
