//! `SkillPack` archive builder and unpacker.

use std::fs;
use std::path::{Path, PathBuf};

use cairn_core::pipeline::skillify::{
    SkillArtifactBundle, SkillArtifactKind, SkillPackEntry, SkillPackError, SkillPackManifest,
    SkillifyGate, SkillifyGateReport, SkillifyGateStatus,
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
    /// Rollback could not fully restore the pre-install vault state.
    /// The vault is in an inconsistent state — at least one candidate
    /// directory was neither cleaned up nor restored from its backup.
    /// Backups (`.bak-*`) may still be present alongside the candidate
    /// directories and require manual recovery.
    #[error("install rollback incomplete ({} errors): {}", errors.len(), errors.join("; "))]
    RollbackIncomplete {
        /// Per-action error messages.
        errors: Vec<String>,
    },
    /// Commit succeeded (new candidate bytes are in place) but one or
    /// more backup directories could not be deleted. Vault state is
    /// correct; leftover `.bak-*` directories can be removed manually.
    #[error("install committed but backup cleanup failed ({} errors): {}", errors.len(), errors.join("; "))]
    CommitCleanupFailed {
        /// Per-action error messages.
        errors: Vec<String>,
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
    #[allow(
        clippy::too_many_lines,
        reason = "linear pack-build flow: name validation, per-candidate read+hash+spec lookup, manifest assembly, tar emission. Splitting obscures the ordering."
    )]
    pub fn build(self, vault_root: &Path) -> Result<SkillPackArchive, SkillPackBuildError> {
        // Validate name BEFORE deriving any filesystem path from it. Without
        // this check a caller passing `--name ../target` (or similar) would
        // truncate a file outside the vault root before any manifest
        // validation would reject the name.
        let probe_manifest = SkillPackManifest {
            pack_id: String::from("skp_probe"),
            name: self.name.clone(),
            version: self.version.clone(),
            cairn_compat: self.cairn_compat.clone(),
            description: self.description.clone(),
            skills: Vec::new(),
            requires: Vec::new(),
            provides: Vec::new(),
            content_sha256: String::new(),
        };
        // Use the same name validator (reject empty, separators, etc.) that
        // the install-time validator uses. cairn_compat / deps are checked
        // later against the actual entries; for now we only need the name
        // check before touching the filesystem.
        if let Err(e) = probe_manifest.validate(env!("CARGO_PKG_VERSION")) {
            // Forward only the InvalidName error; ignore other unrelated
            // validation failures (compat etc.) that the full validate call
            // happens to surface against the empty-entries probe.
            if matches!(e, SkillPackError::InvalidName { .. }) {
                return Err(SkillPackBuildError::Pack(e));
            }
        }

        let mut entries = Vec::new();
        let mut all_provides: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut all_requires: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();

        for cid in &self.candidate_ids {
            // Round-13 hardening: validate every caller-supplied candidate
            // id with the same safe-token rules used by install. Without
            // this a CLI caller passing `--candidates ../escape` could
            // make build read or scan outside the skillify directory
            // before any later check trips.
            super::materialize::validate_path_token("candidate id", cid).map_err(|e| {
                SkillPackBuildError::Pack(SkillPackError::InvalidName {
                    reason: format!("candidate id `{cid}` is not a safe path token: {e}"),
                })
            })?;

            let cand_dir = vault_root.join(".cairn/evolution/skillify").join(cid);

            let manifest_path = cand_dir.join("manifest.json");
            if !manifest_path.exists() {
                return Err(SkillPackBuildError::CandidateNotFound {
                    candidate_id: cid.clone(),
                });
            }

            let bundle: SkillArtifactBundle = serde_json::from_slice(&fs::read(&manifest_path)?)?;

            // Round-14 hardening: validate every artifact path in the
            // candidate manifest stays UNDER bundle/. Without this an
            // absolute path like `/etc/passwd` or a `..` escape could
            // make the archive loop below read a host file via
            // `cand_dir.join(allowed_rel)` and embed it under
            // `skills/<id>/...`. The symlink scan only walks cand_dir;
            // join with an absolute path would silently escape it.
            bundle.validate().map_err(|e| {
                SkillPackBuildError::Pack(SkillPackError::IntegrityFailure {
                    expected: format!("valid bundle paths for {cid}"),
                    actual: format!("validation failed: {e}"),
                })
            })?;
            // Defence in depth: also assert bundle.candidate_id matches the
            // requested cid (catches stale/swapped manifest.json).
            if bundle.candidate_id != *cid {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::IntegrityFailure {
                        expected: format!("candidate_id={cid}"),
                        actual: format!("candidate_id={}", bundle.candidate_id),
                    },
                ));
            }

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

            // Round 6 hardening: populate provides/requires from the
            // candidate's persisted spec draft (when present) so the
            // manifest's dependency closure check actually means something.
            // Without this, every pack shipped with `requires: []` and the
            // SkillPackManifest::validate dependency-closure check was a no-op.
            all_provides.insert(lane.clone());
            // Round 7 hardening: spec.draft.json is REQUIRED for pack
            // build. Without a parseable spec we cannot populate
            // requires/provides, so the dependency-closure check becomes a
            // no-op and a pack can install with missing capabilities.
            let spec_path = cand_dir.join("skill-spec.draft.json");
            let spec_bytes = fs::read(&spec_path).map_err(|e| {
                SkillPackBuildError::Pack(SkillPackError::IntegrityFailure {
                    expected: format!("readable skill-spec.draft.json for {cid}"),
                    actual: format!("read failed: {e}"),
                })
            })?;
            let spec: cairn_core::pipeline::skillify::SkillSpecDraft =
                serde_json::from_slice(&spec_bytes).map_err(|e| {
                    SkillPackBuildError::Pack(SkillPackError::IntegrityFailure {
                        expected: format!("valid SkillSpecDraft JSON for {cid}"),
                        actual: format!("parse failed: {e}"),
                    })
                })?;
            // Sanity check: the spec's lane/slug should agree with the
            // bundle's frontmatter-derived lane and slug. A mismatch
            // suggests a stale or swapped spec file.
            if spec.lane != lane {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::IntegrityFailure {
                        expected: format!("spec.lane={lane}"),
                        actual: format!("spec.lane={}", spec.lane),
                    },
                ));
            }
            if spec.slug != slug {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::IntegrityFailure {
                        expected: format!("spec.slug={slug}"),
                        actual: format!("spec.slug={}", spec.slug),
                    },
                ));
            }
            for cap in spec.provides {
                all_provides.insert(cap);
            }
            for dep in spec.requires {
                all_requires.insert(dep);
            }

            entries.push(SkillPackEntry {
                candidate_id: cid.clone(),
                lane: lane.clone(),
                slug,
                bundle_version: bundle.version,
                artifact_sha256: artifact_hash,
            });
        }

        let candidate_ids_refs: Vec<&str> = self.candidate_ids.iter().map(String::as_str).collect();
        let pack_id =
            SkillPackManifest::derive_pack_id(&self.name, &self.version, &candidate_ids_refs);

        let provides_vec: Vec<String> = all_provides.into_iter().collect();
        let requires_vec: Vec<String> = all_requires.into_iter().collect();

        let mut manifest = SkillPackManifest {
            pack_id,
            name: self.name.clone(),
            version: self.version.clone(),
            cairn_compat: self.cairn_compat.clone(),
            description: self.description.clone(),
            skills: entries,
            requires: requires_vec,
            provides: provides_vec,
            content_sha256: String::new(),
        };

        // With requires/provides populated, validate dependency closure
        // and lane uniqueness at pack-build time so a candidate whose
        // `requires` isn't satisfied by any skill in this pack fails fast.
        // We deliberately skip the `cairn_compat` check here — a pack can
        // be built for a future Cairn version that the build host does not
        // run; that check belongs at install time.
        let provided_set: std::collections::BTreeSet<&str> =
            manifest.provides.iter().map(String::as_str).collect();
        for dep in &manifest.requires {
            if !provided_set.contains(dep.as_str()) {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::DependencyMissing { dep: dep.clone() },
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &manifest.skills {
            if !seen.insert(entry.lane.clone()) {
                return Err(SkillPackBuildError::Pack(SkillPackError::DuplicateLane {
                    lane: entry.lane.clone(),
                }));
            }
        }

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

        // Round 9 hardening: archive ONLY files on a per-candidate
        // allowlist (manifest.json, gate-report.json, every declared
        // artifact). Previously `append_dir_recursive` packed the whole
        // candidate directory, so a script that wrote scratch files into
        // its bundle dir could leak them into shipped archives. Symlinks
        // are rejected explicitly via symlink_metadata.
        for (entry_idx, cid) in self.candidate_ids.iter().enumerate() {
            let cand_dir = vault_root.join(".cairn/evolution/skillify").join(cid);
            let entry = &manifest.skills[entry_idx];

            // Per-candidate allowlist mirrors the install-time strip set
            // so what the packer ships is exactly what install will keep.
            let mut allowed: std::collections::BTreeSet<std::path::PathBuf> =
                std::collections::BTreeSet::new();
            allowed.insert(std::path::PathBuf::from("manifest.json"));
            allowed.insert(std::path::PathBuf::from("gate-report.json"));
            // Each declared artifact path from the candidate's bundle.
            let cand_manifest: SkillArtifactBundle =
                serde_json::from_slice(&fs::read(cand_dir.join("manifest.json"))?)?;
            for artifact in &cand_manifest.artifacts {
                allowed.insert(std::path::PathBuf::from(&artifact.path));
            }

            // Reject symlinks anywhere in the candidate tree before we
            // touch them. tar's append_file would dereference, and a
            // symlink under bundle/scripts could exfiltrate sensitive
            // content into the archive.
            let mut walk: Vec<std::path::PathBuf> = vec![cand_dir.clone()];
            while let Some(dir) = walk.pop() {
                for entry_res in fs::read_dir(&dir)? {
                    let dir_entry = entry_res?;
                    let path = dir_entry.path();
                    let meta = fs::symlink_metadata(&path)?;
                    if meta.file_type().is_symlink() {
                        return Err(SkillPackBuildError::Pack(
                            SkillPackError::IntegrityFailure {
                                expected: "no symlinks in candidate tree".to_owned(),
                                actual: format!("symlink at {}", path.display()),
                            },
                        ));
                    }
                    if path.is_dir() {
                        walk.push(path);
                    }
                }
            }

            // Append only allowlisted files. Use rel-path lookups so the
            // archive tree mirrors the candidate dir under skills/<cid>/.
            // Round-14 hardening: reject any allowlist entry whose path
            // is absolute or contains parent components. `bundle.validate()`
            // above already enforces this on declared artifacts, but the
            // hardcoded `manifest.json`/`gate-report.json` entries plus
            // future additions to the allowlist deserve the same check
            // applied at the call site.
            for allowed_rel in &allowed {
                if allowed_rel.is_absolute()
                    || allowed_rel
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(SkillPackBuildError::Pack(
                        SkillPackError::IntegrityFailure {
                            expected: "relative path with no parent components".to_owned(),
                            actual: format!("unsafe allowlist entry: {}", allowed_rel.display()),
                        },
                    ));
                }
                let src_path = cand_dir.join(allowed_rel);

                // Round 10 hardening: re-check symlink_metadata right
                // before the read. The earlier candidate-tree scan can be
                // raced by a concurrent writer (or a backgrounded script
                // descendant the timeout-kill missed) that swaps an
                // allowlisted file to a symlink between scan and read.
                // `fs::read` follows symlinks; `symlink_metadata` does
                // not. If the metadata says symlink → fail loudly.
                let meta = match fs::symlink_metadata(&src_path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        // gate-report.json is always present; other
                        // allowlist entries may be absent for partially-
                        // populated candidates. Skip silently.
                        continue;
                    }
                    Err(e) => return Err(SkillPackBuildError::Io(e)),
                };
                if meta.file_type().is_symlink() {
                    return Err(SkillPackBuildError::Pack(
                        SkillPackError::IntegrityFailure {
                            expected: format!("regular file at {}", src_path.display()),
                            actual: "symlink (TOCTOU after scan)".to_owned(),
                        },
                    ));
                }
                if !meta.is_file() {
                    continue;
                }

                let data = fs::read(&src_path)?;
                let mode = file_mode(&src_path);
                let archive_path =
                    format!("skills/{}/{}", entry.candidate_id, allowed_rel.display());
                append_bytes(&mut tar, &archive_path, &data, mode)?;
            }
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
#[allow(
    clippy::too_many_lines,
    reason = "single linear flow: lock, extract, validate, per-artifact verify, atomic swap, rollback. Splitting would obscure the ordering invariants."
)]
pub fn unpack_archive_staged(
    archive_path: &Path,
    vault_root: &Path,
    cairn_version: &str,
) -> Result<(SkillPackManifest, InstallTransaction), SkillPackBuildError> {
    // Serialize concurrent installs against the same vault. Without this,
    // two installs operating on the same candidate id can interleave their
    // rename+rollback steps and silently delete each other's results.
    // An advisory file-lock (no OS-level cooperative requirement) keeps the
    // critical section to one process at a time.
    let parent_dir = vault_root.join(".cairn/evolution/skillify");
    fs::create_dir_all(&parent_dir)?;
    let lock_path = parent_dir.join(".install.lock");
    // Round-18 hardening: the install lock MUST be held until the caller
    // commits or rolls back the transaction (not just until this function
    // returns). Otherwise: install A acquires the lock, swaps, releases
    // when this fn returns, re-gates; install B acquires, swaps,
    // commits; install A then fails re-gate and rolls back, deleting
    // B's data. Move the lock into the returned InstallTransaction so
    // its Drop fires only when the transaction is consumed.
    let install_lock = acquire_install_lock(&lock_path)?;

    let file = fs::File::open(archive_path)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);

    // Per-call temp directory with PID + random suffix so two concurrent
    // installs (in the rare case the lock fails or is disabled) cannot
    // collide on the temp dir name.
    let nonce = generate_nonce();
    let extract_dir = parent_dir.join(format!(".unpack-tmp-{}-{nonce}", std::process::id()));
    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)?;
    }
    fs::create_dir_all(&extract_dir)?;

    // Use a guard so the temp dir is always cleaned up, even on error paths.
    let _cleanup = TempDirGuard(extract_dir.clone());

    extract_archive_safely(&mut archive, &extract_dir)?;

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

    // PRE-FLIGHT: validate every manifest entry's content matches what the
    // archive claims. This is the rollback guarantee Codex review #128
    // flagged. Per-artifact byte verification (added in review round 2)
    // closes the loophole where a tampered script wouldn't change the
    // manifest digest because the digest only covers the candidate's
    // manifest.json hash.
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        if !src.is_dir() {
            return Err(SkillPackBuildError::Pack(SkillPackError::MissingSkill {
                candidate_id: entry.candidate_id.clone(),
            }));
        }

        let candidate_manifest_path = src.join("manifest.json");
        if !candidate_manifest_path.is_file() || !src.join("bundle").is_dir() {
            return Err(SkillPackBuildError::Pack(SkillPackError::MissingSkill {
                candidate_id: entry.candidate_id.clone(),
            }));
        }

        // Verify the candidate's manifest hash matches the pack manifest's
        // claimed artifact_sha256 — catches manifest tampering.
        let cand_manifest_hash = sha256_file(&candidate_manifest_path)?;
        if cand_manifest_hash != entry.artifact_sha256 {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: entry.artifact_sha256.clone(),
                    actual: cand_manifest_hash,
                },
            ));
        }

        // Parse the candidate manifest and verify every declared artifact
        // file exists and its bytes hash to the declared content_sha256.
        // This catches scripts/tests/markdown being swapped out without
        // updating the candidate manifest.
        let bundle: SkillArtifactBundle =
            serde_json::from_slice(&fs::read(&candidate_manifest_path)?)?;

        // The candidate manifest's candidate_id must match the pack
        // manifest's claimed id — catches an attacker swapping a bundle
        // that lies about its identity.
        if bundle.candidate_id != entry.candidate_id {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: format!("candidate_id={}", entry.candidate_id),
                    actual: format!("candidate_id={}", bundle.candidate_id),
                },
            ));
        }

        for artifact in &bundle.artifacts {
            // Reject artifact paths that escape the candidate dir.
            let rel_path = std::path::Path::new(&artifact.path);
            if rel_path.is_absolute()
                || rel_path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::IntegrityFailure {
                        expected: format!("safe path for {}", artifact.kind.as_str()),
                        actual: artifact.path.clone(),
                    },
                ));
            }
            let artifact_path = src.join(&artifact.path);
            if !artifact_path.is_file() {
                return Err(SkillPackBuildError::Pack(SkillPackError::MissingSkill {
                    candidate_id: entry.candidate_id.clone(),
                }));
            }
            let actual_hash = sha256_file(&artifact_path)?;
            if actual_hash != artifact.content_sha256 {
                return Err(SkillPackBuildError::Pack(
                    SkillPackError::IntegrityFailure {
                        expected: artifact.content_sha256.clone(),
                        actual: actual_hash,
                    },
                ));
            }
        }

        // Force-regenerate gate-report.json with all gates Blocked, even if
        // the archive contained a forged "passed" report. This ensures the
        // handler's replay guard (candidate_ready) does not short-circuit
        // re-gating after install. Round 3 hardening.
        let blocked_report = make_blocked_report(&entry.candidate_id);
        fs::write(
            src.join("gate-report.json"),
            serde_json::to_vec_pretty(&blocked_report)?,
        )?;

        // Round 7 hardening: reject any file in the extracted candidate
        // tree that is NOT in the per-candidate allowlist. The allowlist
        // is: manifest.json, gate-report.json, and every declared
        // artifact path. Without this check, the install rename moves
        // the entire extracted directory into the vault — letting a
        // crafted archive smuggle unreviewed scripts, data files, or
        // other content into .cairn/evolution/skillify/<id>/.
        // Round 8 hardening: the strict allowlist accepts ONLY files whose
        // bytes are covered by an integrity check. manifest.json and every
        // declared artifact are byte-hashed above; gate-report.json was
        // just regenerated by us from a trusted template. Non-artifact
        // metadata files that an archive could plausibly carry
        // (skill-spec.draft.json, promotion-plan.json, versions/v1/...)
        // are STRIPPED here rather than allowlisted, so a tampered copy
        // cannot ride into the vault. The pipeline regenerates them
        // post-install when it re-runs gates against this candidate.
        let mut allowed: std::collections::BTreeSet<std::path::PathBuf> =
            std::collections::BTreeSet::new();
        allowed.insert(std::path::PathBuf::from("manifest.json"));
        allowed.insert(std::path::PathBuf::from("gate-report.json"));
        for artifact in &bundle.artifacts {
            allowed.insert(std::path::PathBuf::from(&artifact.path));
        }

        // First pass: strip any file NOT in the allowlist (e.g.
        // skill-spec.draft.json, promotion-plan.json, versions/v1/...
        // that the archive may have shipped). Their content is unverified;
        // we regenerate them locally on the next pipeline run.
        let mut strip_pass: Vec<std::path::PathBuf> = vec![src.clone()];
        let mut to_remove: Vec<std::path::PathBuf> = Vec::new();
        while let Some(dir) = strip_pass.pop() {
            for entry_res in fs::read_dir(&dir)? {
                let entry = entry_res?;
                let path = entry.path();
                if path.is_dir() {
                    strip_pass.push(path);
                    continue;
                }
                let rel = path.strip_prefix(&src).unwrap_or(&path).to_path_buf();
                if !allowed.contains(&rel) {
                    to_remove.push(path);
                }
            }
        }
        for path in to_remove {
            let _ = fs::remove_file(&path);
        }

        // Second pass: assert nothing undeclared remains. This is
        // defense-in-depth against directory-walk races or anything the
        // strip step missed (it shouldn't have).
        let mut stack: Vec<std::path::PathBuf> = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry_res in fs::read_dir(&dir)? {
                let entry = entry_res?;
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = path.strip_prefix(&src).unwrap_or(&path).to_path_buf();
                if !allowed.contains(&rel) {
                    return Err(SkillPackBuildError::Pack(
                        SkillPackError::IntegrityFailure {
                            expected: "only declared artifacts in candidate tree".to_owned(),
                            actual: format!("undeclared file: {}", rel.display()),
                        },
                    ));
                }
            }
        }
    }

    // ATOMIC SWAP: now that every source is validated, move each skill into
    // place. For pre-existing destinations, swap-then-delete keeps the old
    // copy until the new one is in place; on any io error the old copy is
    // restored where possible. Errors mid-install leave the vault in a
    // partially-installed state — that is logged via the returned error so
    // the operator can re-run install.
    // `parent_dir` (computed above for the install lock) is reused so we do
    // not race against another caller creating it concurrently.
    // Track every swap so rollback can restore the previous state of the
    // vault on any later failure. `Replaced` means the destination already
    // existed and is preserved as a backup. `Created` means we installed
    // into a previously-empty slot — rollback must remove the new copy.
    let mut swap_log: Vec<SwapAction> = Vec::new();
    for entry in &manifest.skills {
        let src = extract_dir.join(format!("skills/{}", entry.candidate_id));
        let dst = parent_dir.join(&entry.candidate_id);
        let backup = parent_dir.join(format!(
            ".bak-{}-{}-{nonce}",
            entry.candidate_id,
            std::process::id()
        ));
        let preexisting = dst.exists();
        if preexisting {
            // Rename existing → backup first; rename new → dst; delete backup last.
            if let Err(e) = fs::rename(&dst, &backup) {
                log_rollback_errors(rollback_install(&swap_log), "mid-install dst→backup");
                return Err(SkillPackBuildError::Io(e));
            }
        }
        if let Err(e) = fs::rename(&src, &dst) {
            log_rollback_errors(rollback_install(&swap_log), "mid-install src→dst");
            // The dst-to-backup rename above (if it happened) needs to be
            // undone here — restore the backup since the new dst rename
            // failed.
            if preexisting && let Err(restore_err) = fs::rename(&backup, &dst) {
                tracing::error!(
                    target: "cairn_workflows::skillify::packer",
                    dst = %dst.display(),
                    backup = %backup.display(),
                    error = %restore_err,
                    "failed to restore backup after src→dst rename failure — vault is in inconsistent state",
                );
            }
            return Err(SkillPackBuildError::Io(e));
        }
        if preexisting {
            swap_log.push(SwapAction::Replaced {
                dst: dst.clone(),
                backup: backup.clone(),
            });
        } else {
            swap_log.push(SwapAction::Created { dst: dst.clone() });
        }
    }

    // Round-17 hardening: return the swap log so the caller can roll
    // back if post-install re-gate fails. Previously backups were deleted
    // unconditionally here, which made install non-transactional through
    // the CLI's re-gate step: a re-gate failure (e.g. trigger collision
    // with existing vault skills) would leave the previous candidate
    // permanently gone.
    Ok((
        manifest,
        InstallTransaction {
            swap_log,
            _install_lock: install_lock,
        },
    ))
}

/// In-flight install state returned by [`unpack_archive`]. The caller MUST
/// call [`InstallTransaction::commit`] on success (deletes backups) or
/// [`InstallTransaction::rollback`] on failure (restores previous candidate
/// state). If neither is called (transaction dropped), backups remain on
/// disk until manual cleanup — explicit commit/rollback avoids ambiguous
/// vault state.
#[must_use = "InstallTransaction must be committed or rolled back"]
pub struct InstallTransaction {
    swap_log: Vec<SwapAction>,
    // Round-18: holding the install lock for the full transaction
    // lifetime (until commit/rollback) prevents a concurrent install
    // from sneaking in between staged swap and rollback, which could
    // otherwise let an earlier rollback delete a later install's data.
    _install_lock: InstallLockGuard,
}

/// Backward-compatible wrapper: runs `unpack_archive_staged` then commits
/// immediately. New callers should prefer the staged form so they can
/// re-gate before committing.
///
/// # Errors
/// Propagates errors from [`unpack_archive_staged`] and from
/// [`InstallTransaction::commit`] (backup cleanup failures).
pub fn unpack_archive(
    archive_path: &Path,
    vault_root: &Path,
    cairn_version: &str,
) -> Result<SkillPackManifest, SkillPackBuildError> {
    let (manifest, tx) = unpack_archive_staged(archive_path, vault_root, cairn_version)?;
    tx.commit()?;
    Ok(manifest)
}

impl InstallTransaction {
    /// Commit the install: delete all backups, leaving the new candidate
    /// bytes in place.
    ///
    /// # Errors
    /// Returns [`SkillPackBuildError::CommitCleanupFailed`] when one or
    /// more backup directories could not be deleted. The vault state is
    /// still correct (the new candidate bytes are in place); the error
    /// surfaces leftover `.bak-*` directories so the operator can clean
    /// them up.
    pub fn commit(self) -> Result<(), SkillPackBuildError> {
        let mut errors = Vec::new();
        for action in &self.swap_log {
            if let SwapAction::Replaced { backup, .. } = action
                && let Err(e) = fs::remove_dir_all(backup)
                // ENOENT means the backup was never created or was
                // cleaned up by another process — not an error.
                && e.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(format!("remove backup {}: {e}", backup.display()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SkillPackBuildError::CommitCleanupFailed { errors })
        }
    }

    /// Roll back the install: remove every newly-installed candidate dir
    /// and restore replaced candidates from their backups. After rollback
    /// the vault is in the state it was before [`unpack_archive`] ran.
    ///
    /// # Errors
    /// Returns [`SkillPackBuildError::RollbackIncomplete`] when one or
    /// more swap actions could not be undone. The vault is in an
    /// inconsistent state — at least one candidate directory was neither
    /// fully cleaned up nor restored from its backup. Manual recovery
    /// may be required.
    pub fn rollback(self) -> Result<(), SkillPackBuildError> {
        let errors = rollback_install(&self.swap_log);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SkillPackBuildError::RollbackIncomplete { errors })
        }
    }
}

/// One install rename, recorded for rollback.
enum SwapAction {
    /// Destination did not exist; we wrote the new copy.
    Created { dst: std::path::PathBuf },
    /// Destination existed; we backed it up then wrote the new copy.
    Replaced {
        dst: std::path::PathBuf,
        backup: std::path::PathBuf,
    },
}

/// Log mid-install rollback errors at `error` level. Mid-install rollback
/// errors do not preempt the original triggering error (the caller's user
/// already gets that one), but they MUST be visible in operator logs since
/// they leave the vault in an inconsistent state requiring manual cleanup.
fn log_rollback_errors(errors: Vec<String>, context: &str) {
    for err in errors {
        tracing::error!(
            target: "cairn_workflows::skillify::packer",
            context = context,
            error = %err,
            "rollback action failed during install — vault may be in inconsistent state",
        );
    }
}

/// Undo every recorded swap, leaving the vault as it was before install.
/// Called when a later rename fails mid-install so a partial pack does not
/// pollute snapshots/gates.
///
/// Returns the set of per-action error messages encountered. An empty
/// vector means the rollback fully restored the prior state. ENOENT on
/// the candidate directory is normal (it may have been removed mid-install
/// before its swap completed) and is not reported.
fn rollback_install(log: &[SwapAction]) -> Vec<String> {
    let mut errors = Vec::new();
    for action in log.iter().rev() {
        match action {
            SwapAction::Created { dst } => {
                if let Err(e) = fs::remove_dir_all(dst)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    errors.push(format!("remove created {}: {e}", dst.display()));
                }
            }
            SwapAction::Replaced { dst, backup } => {
                // Remove the new install bytes; ENOENT is benign.
                if let Err(e) = fs::remove_dir_all(dst)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    errors.push(format!("remove replaced {}: {e}", dst.display()));
                    // Renaming the backup over a still-present dst
                    // would fail (or worse, succeed and corrupt
                    // state). Skip the restore for this entry; the
                    // backup remains on disk under its `.bak-*`
                    // name so a human can recover.
                    continue;
                }
                if let Err(e) = fs::rename(backup, dst) {
                    errors.push(format!(
                        "restore {} from backup {}: {e}",
                        dst.display(),
                        backup.display()
                    ));
                }
            }
        }
    }
    errors
}

/// Resource limits for archive extraction. Without these, a malicious
/// `.cairnpack` could fill the vault filesystem before any manifest
/// validation runs.
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_FILE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB per file
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 512 * 1024 * 1024; // 512 MiB cumulative

/// Stream-extract a tar archive into `extract_dir` rejecting unsafe entries
/// and enforcing resource limits.
///
/// Each entry is checked before any bytes are written:
/// - reject absolute paths and parent-component (`..`) segments;
/// - reject symlinks, hardlinks, FIFOs, devices, sockets;
/// - enforce per-file and cumulative size caps;
/// - enforce an entry count cap;
/// - reject empty paths.
///
/// On any rejection or limit breach, the partially-extracted directory is
/// removed and the call returns an error before the install proceeds.
fn extract_archive_safely<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    extract_dir: &Path,
) -> Result<(), SkillPackBuildError> {
    let entries = archive.entries()?;
    let mut count: usize = 0;
    let mut total_bytes: u64 = 0;

    for entry_result in entries {
        let mut entry = entry_result?;
        count += 1;
        if count > MAX_ARCHIVE_ENTRIES {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: format!("≤{MAX_ARCHIVE_ENTRIES} entries"),
                    actual: format!("{count}+ entries"),
                },
            ));
        }

        let header = entry.header().clone();
        let entry_type = header.entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: "regular file or directory".to_owned(),
                    actual: format!("entry type {entry_type:?}"),
                },
            ));
        }

        let entry_path = entry.path()?.into_owned();
        let path_str = entry_path.to_string_lossy();
        if path_str.is_empty() {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: "non-empty path".to_owned(),
                    actual: "empty entry path".to_owned(),
                },
            ));
        }
        if entry_path.is_absolute() {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: "relative path".to_owned(),
                    actual: path_str.to_string(),
                },
            ));
        }
        if entry_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: "no parent components".to_owned(),
                    actual: path_str.to_string(),
                },
            ));
        }

        let entry_size = header.size().unwrap_or(0);
        if entry_size > MAX_ARCHIVE_FILE_BYTES {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: format!("≤{MAX_ARCHIVE_FILE_BYTES} bytes per file"),
                    actual: format!("{entry_size} bytes in `{path_str}`"),
                },
            ));
        }
        total_bytes = total_bytes.saturating_add(entry_size);
        if total_bytes > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: format!("≤{MAX_ARCHIVE_TOTAL_BYTES} cumulative bytes"),
                    actual: format!("≥{total_bytes} bytes after `{path_str}`"),
                },
            ));
        }

        // Compute the canonical destination and assert it stays under
        // `extract_dir`. `unpack_in` performs the rooted unpack but we
        // already validated the path above; the extra check guards against
        // tar implementations that allow surprising path resolution.
        let target = extract_dir.join(&entry_path);
        if !target.starts_with(extract_dir) {
            return Err(SkillPackBuildError::Pack(
                SkillPackError::IntegrityFailure {
                    expected: format!("path under {}", extract_dir.display()),
                    actual: target.display().to_string(),
                },
            ));
        }
        // `unpack_in` writes the entry into the rooted directory and
        // enforces a second path-escape check internally.
        entry.unpack_in(extract_dir)?;
    }

    Ok(())
}

/// Build a fresh "all gates Blocked" gate-report for a candidate. Used at
/// install time to overwrite whatever gate-report the archive contained, so
/// the installed candidate is forced to re-gate locally.
fn make_blocked_report(candidate_id: &str) -> SkillifyGateReport {
    SkillifyGateReport {
        candidate_id: candidate_id.to_owned(),
        gates: SkillArtifactKind::required()
            .iter()
            .map(|kind| SkillifyGate {
                name: kind.as_str().to_owned(),
                status: SkillifyGateStatus::Blocked,
                message: Some("installed via skillpack — re-gate required".to_owned()),
            })
            .collect(),
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

/// RAII guard that holds an OS-level advisory file lock for the duration of
/// an install. Released automatically on drop (the lock is tied to the file
/// descriptor; dropping the `File` releases it).
struct InstallLockGuard(fs::File);

/// Acquire an exclusive advisory lock on `path` (blocks if another process
/// holds it). Uses the `fs4` crate so we stay within the workspace's
/// `forbid(unsafe_code)` policy.
fn acquire_install_lock(path: &Path) -> Result<InstallLockGuard, SkillPackBuildError> {
    use fs4::fs_std::FileExt as _;
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.lock_exclusive()?;
    Ok(InstallLockGuard(file))
}

impl Drop for InstallLockGuard {
    fn drop(&mut self) {
        // Closing the File releases the advisory lock; no explicit unlock
        // needed (fs4 docs note the lock is fd-scoped).
        let _ = &self.0;
    }
}

/// Cryptographically-random short string used to disambiguate concurrent
/// install temp/backup directory names.
fn generate_nonce() -> String {
    let mut h = Sha256::new();
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0u128, |d| d.as_nanos())
            .to_le_bytes(),
    );
    h.update(std::process::id().to_le_bytes());
    // Mix in the thread id (Debug formatting is stable, even though
    // `ThreadId::as_u64` is unstable).
    h.update(format!("{:?}", std::thread::current().id()).as_bytes());
    format!("{:x}", h.finalize()).chars().take(12).collect()
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

// `append_dir_recursive` was replaced by the per-candidate allowlist loop
// in `SkillPackBuilder::build`. The new loop only archives files we've
// integrity-hashed (manifest.json, gate-report.json, each declared
// artifact path) so scripts cannot leak side-channel files into shipped
// packs. Removed at round 9.
