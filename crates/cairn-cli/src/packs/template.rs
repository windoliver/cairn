//! Fixed-token scaffold rendering for `cairn skill new`.

use std::path::{Path, PathBuf};

use crate::packs::manifest::{Harness, PackError};
use crate::packs::source::FsPackSource;
use include_dir::{Dir, DirEntry, include_dir};

/// Embedded `cairn skill new` reference templates.
pub static PACK_TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../packs/templates");

/// Values available to fixed-token scaffold templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateVars {
    /// Safe path token used as the generated pack id.
    pub pack_id: String,
    /// Human-readable pack name derived from `pack_id`.
    pub display_name: String,
    /// Harness id written to `pack.json`.
    pub harness: String,
    /// Initial pack semver.
    pub version: String,
    /// Harness-specific manual fragment filename.
    pub manual_fragment: String,
    /// Slash command id.
    pub command_id: String,
    /// Subagent id.
    pub subagent_id: String,
}

/// Options for rendering a skill-pack scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldOpts {
    /// Requested pack name.
    pub name: String,
    /// Target harness.
    pub harness: Harness,
    /// Directory where the scaffold should be written.
    pub output_dir: PathBuf,
}

/// Receipt emitted after a skill-pack scaffold is written.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScaffoldReceipt {
    /// Generated pack id.
    pub pack_id: String,
    /// Target harness id.
    pub harness: String,
    /// Directory where files were written.
    pub output_dir: PathBuf,
    /// Pack-relative files created by the renderer.
    pub files_created: Vec<PathBuf>,
    /// Suggested verification command.
    pub verify_command: String,
}

/// Errors from skill-pack scaffold rendering.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScaffoldError {
    /// Pack name is not a safe scaffold token.
    #[error("invalid pack name `{name}`: use nonempty ASCII alphanumeric characters, '-' or '_'")]
    InvalidPackName {
        /// Invalid requested pack name.
        name: String,
    },
    /// Output directory already has content.
    #[error("output directory is not empty: {}", path.display())]
    OutputDirNotEmpty {
        /// Non-empty output directory.
        path: PathBuf,
    },
    /// No template tree exists for the requested harness.
    #[error("template missing for harness `{harness}` at `{path}`")]
    TemplateMissing {
        /// Harness id.
        harness: String,
        /// Missing template path.
        path: String,
    },
    /// Template rendering left a fixed token unresolved.
    #[error("unresolved template token in `{token}`")]
    UnresolvedToken {
        /// Remaining unresolved token text.
        token: String,
    },
    /// Pack validation or install error.
    #[error(transparent)]
    Pack(#[from] PackError),
    /// Filesystem error while writing the scaffold.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Replace fixed scaffold tokens with values from `vars`.
///
/// # Errors
///
/// Returns [`ScaffoldError::UnresolvedToken`] if template delimiters remain
/// after known token replacement.
pub fn render_tokens(input: &str, vars: &TemplateVars) -> Result<String, ScaffoldError> {
    let mut rendered = input.to_string();
    for (token, value) in [
        ("{{pack_id}}", vars.pack_id.as_str()),
        ("{{display_name}}", vars.display_name.as_str()),
        ("{{harness}}", vars.harness.as_str()),
        ("{{version}}", vars.version.as_str()),
        ("{{manual_fragment}}", vars.manual_fragment.as_str()),
        ("{{command_id}}", vars.command_id.as_str()),
        ("{{subagent_id}}", vars.subagent_id.as_str()),
    ] {
        rendered = rendered.replace(token, value);
    }

    if rendered.contains("{{") || rendered.contains("}}") {
        return Err(ScaffoldError::UnresolvedToken {
            token: first_unresolved_token(&rendered),
        });
    }

    Ok(rendered)
}

/// Render a complete skill-pack scaffold into `opts.output_dir`.
///
/// # Errors
///
/// Returns [`ScaffoldError`] if the pack name is unsafe, the output directory
/// is non-empty, a template is missing or invalid, writing fails, or the
/// rendered pack fails conformance.
pub fn render_scaffold(opts: &ScaffoldOpts) -> Result<ScaffoldReceipt, ScaffoldError> {
    if !is_safe_pack_name(&opts.name) {
        return Err(ScaffoldError::InvalidPackName {
            name: opts.name.clone(),
        });
    }

    if opts.output_dir.exists() && !is_dir_empty(&opts.output_dir)? {
        return Err(ScaffoldError::OutputDirNotEmpty {
            path: opts.output_dir.clone(),
        });
    }

    let harness = harness_id(opts.harness);
    let template_dir =
        PACK_TEMPLATES
            .get_dir(harness)
            .ok_or_else(|| ScaffoldError::TemplateMissing {
                harness: harness.to_string(),
                path: harness.to_string(),
            })?;
    let vars = TemplateVars {
        pack_id: opts.name.clone(),
        display_name: display_name(&opts.name),
        harness: harness.to_string(),
        version: "0.1.0".to_string(),
        manual_fragment: manual_fragment(opts.harness).to_string(),
        command_id: "cairn-context".to_string(),
        subagent_id: "context-loader".to_string(),
    };

    let output_parent = output_parent(&opts.output_dir);
    std::fs::create_dir_all(&output_parent)?;
    let staged = tempfile::Builder::new()
        .prefix(".cairn-scaffold-")
        .tempdir_in(&output_parent)?;
    let staged_path = staged.path().to_path_buf();

    let mut files_created = Vec::new();
    render_template_dir(
        template_dir,
        Path::new(""),
        &staged_path,
        &vars,
        &mut files_created,
    )?;
    files_created.sort();

    let source = FsPackSource::new(staged_path.clone());
    let outcomes = crate::packs::verify::run_pack_source_conformance(&source);
    let failures = outcomes
        .iter()
        .filter_map(|outcome| {
            outcome
                .status
                .as_ref()
                .err()
                .map(|reason| format!("{}: {reason}", outcome.id))
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        return Err(ScaffoldError::Pack(PackError::ManifestInvalid {
            reason: format!(
                "rendered scaffold failed conformance: {}",
                failures.join("; ")
            ),
        }));
    }

    if opts.output_dir.exists() {
        std::fs::remove_dir(&opts.output_dir)?;
    }
    std::fs::rename(&staged_path, &opts.output_dir)?;

    Ok(ScaffoldReceipt {
        pack_id: opts.name.clone(),
        harness: harness.to_string(),
        output_dir: opts.output_dir.clone(),
        files_created,
        verify_command: format!(
            "cairn plugins verify --pack-path {} --strict",
            shell_single_quote_path(&opts.output_dir)
        ),
    })
}

/// Convert a safe pack id into a title-cased display name.
#[must_use]
pub fn display_name(pack_id: &str) -> String {
    pack_id
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(title_case_ascii)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return the template harness id.
#[must_use]
pub fn harness_id(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Gemini => "gemini",
    }
}

/// Return the harness-specific manual fragment path.
#[must_use]
pub fn manual_fragment(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "manual.md",
        Harness::Codex => "AGENTS.md",
        Harness::Gemini => "GEMINI.md",
    }
}

/// Return true when `name` can safely be used as a pack directory token.
#[must_use]
pub fn is_safe_pack_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Return true when `path` has no directory entries.
///
/// # Errors
///
/// Returns an I/O error if the directory cannot be read.
pub fn is_dir_empty(path: &Path) -> Result<bool, std::io::Error> {
    Ok(std::fs::read_dir(path)?.next().is_none())
}

fn output_parent(output_dir: &Path) -> PathBuf {
    output_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn shell_single_quote_path(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_template_dir(
    template_dir: &Dir<'_>,
    relative_dir: &Path,
    output_dir: &Path,
    vars: &TemplateVars,
    files_created: &mut Vec<PathBuf>,
) -> Result<(), ScaffoldError> {
    for entry in template_dir.entries() {
        match entry {
            DirEntry::Dir(dir) => {
                let next_relative_dir =
                    relative_dir.join(dir.path().file_name().ok_or_else(|| {
                        ScaffoldError::TemplateMissing {
                            harness: vars.harness.clone(),
                            path: dir.path().display().to_string(),
                        }
                    })?);
                render_template_dir(dir, &next_relative_dir, output_dir, vars, files_created)?;
            }
            DirEntry::File(file) => {
                let template_name =
                    file.path()
                        .file_name()
                        .ok_or_else(|| ScaffoldError::TemplateMissing {
                            harness: vars.harness.clone(),
                            path: file.path().display().to_string(),
                        })?;
                let Some(output_name) = template_name
                    .to_string_lossy()
                    .strip_suffix(".template")
                    .map(str::to_owned)
                else {
                    continue;
                };
                let relative_output = relative_dir.join(output_name);
                let rendered = render_template_file(
                    file.contents_utf8()
                        .ok_or_else(|| ScaffoldError::TemplateMissing {
                            harness: vars.harness.clone(),
                            path: file.path().display().to_string(),
                        })?,
                    file.path(),
                    vars,
                )?;
                let target = output_dir.join(&relative_output);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, rendered)?;
                files_created.push(relative_output);
            }
        }
    }
    Ok(())
}

fn render_template_file(
    input: &str,
    template_path: &Path,
    vars: &TemplateVars,
) -> Result<String, ScaffoldError> {
    render_tokens(input, vars).map_err(|err| match err {
        ScaffoldError::UnresolvedToken { token } => ScaffoldError::UnresolvedToken {
            token: format!("{} in {}", token, template_path.display()),
        },
        other => other,
    })
}

fn title_case_ascii(part: &str) -> String {
    let lower = part.to_ascii_lowercase();
    let mut chars = lower.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut title = String::new();
    title.extend(first.to_uppercase());
    title.push_str(chars.as_str());
    title
}

fn first_unresolved_token(rendered: &str) -> String {
    if let Some(start) = rendered.find("{{")
        && let Some(end) = rendered[start + 2..].find("}}")
    {
        return rendered[start..start + 2 + end + 2].to_string();
    }
    if rendered.contains("{{") {
        return "{{".to_string();
    }
    "}}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packs::install::{PackInstallOpts, install_pack_from_source};
    use crate::packs::source::FsPackSource;
    use crate::packs::verify::run_pack_source_conformance;

    fn sample_vars() -> TemplateVars {
        TemplateVars {
            pack_id: "sample-pack".to_string(),
            display_name: "Sample Pack".to_string(),
            harness: "codex".to_string(),
            version: "0.1.0".to_string(),
            manual_fragment: "AGENTS.md".to_string(),
            command_id: "cairn-context".to_string(),
            subagent_id: "context-loader".to_string(),
        }
    }

    #[test]
    fn render_tokens_replaces_known_values_and_rejects_unresolved_tokens() {
        let vars = sample_vars();

        assert_eq!(
            render_tokens("{{pack_id}} {{display_name}}", &vars).unwrap(),
            "sample-pack Sample Pack"
        );

        let err = render_tokens("{{missing_token}}", &vars).unwrap_err();
        assert!(
            err.to_string().contains("unresolved template token"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn display_name_title_cases_safe_pack_id() {
        assert_eq!(display_name("my-pack"), "My Pack");
        assert_eq!(display_name("ops_pack"), "Ops Pack");
    }

    #[test]
    fn render_scaffold_writes_verifying_pack_for_all_harnesses() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for harness in [Harness::ClaudeCode, Harness::Codex, Harness::Gemini] {
            let output_dir = tmp.path().join(harness_id(harness));
            let opts = ScaffoldOpts {
                name: format!("sample-{}", harness_id(harness)),
                harness,
                output_dir: output_dir.clone(),
            };

            let receipt = render_scaffold(&opts).expect("render scaffold");

            assert_eq!(receipt.pack_id, opts.name);
            assert!(output_dir.join("pack.json").is_file());
            assert!(output_dir.join(manual_fragment(harness)).is_file());
            assert!(output_dir.join(".github/workflows/verify.yml").is_file());
            assert!(output_dir.join("tests/smoke.sh").is_file());

            let source = FsPackSource::new(output_dir.clone());
            let outcomes = run_pack_source_conformance(&source);
            assert!(
                outcomes.iter().all(|outcome| outcome.status.is_ok()),
                "expected rendered pack to verify for {harness:?}: {outcomes:#?}"
            );

            let source_smoke = std::process::Command::new("bash")
                .arg("tests/smoke.sh")
                .current_dir(&output_dir)
                .output()
                .expect("run source smoke");
            assert!(
                source_smoke.status.success(),
                "source smoke failed for {harness:?}: status={:?} stdout={} stderr={}",
                source_smoke.status,
                String::from_utf8_lossy(&source_smoke.stdout),
                String::from_utf8_lossy(&source_smoke.stderr)
            );

            let installed_dir = tmp
                .path()
                .join(format!("{}-installed", harness_id(harness)));
            let opts = PackInstallOpts {
                harness,
                project_dir: installed_dir.clone(),
                force: false,
            };
            install_pack_from_source(&source, &opts).expect("install scaffold");
            let smoke_path = installed_dir.join("smoke.sh");
            std::fs::copy(output_dir.join("tests/smoke.sh"), &smoke_path)
                .expect("copy smoke script");
            let smoke = std::process::Command::new("bash")
                .arg("smoke.sh")
                .current_dir(&installed_dir)
                .output()
                .expect("run smoke");
            assert!(
                smoke.status.success(),
                "smoke failed for {harness:?}: status={:?} stdout={} stderr={}",
                smoke.status,
                String::from_utf8_lossy(&smoke.stdout),
                String::from_utf8_lossy(&smoke.stderr)
            );
        }
    }

    #[test]
    fn render_scaffold_rejects_invalid_name_without_creating_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let output_dir = tmp.path().join("bad output");
        let opts = ScaffoldOpts {
            name: "bad name".to_string(),
            harness: Harness::Codex,
            output_dir: output_dir.clone(),
        };

        let err = render_scaffold(&opts).expect_err("invalid name rejected");

        assert!(matches!(err, ScaffoldError::InvalidPackName { .. }));
        assert!(!output_dir.exists());
    }

    #[test]
    fn render_scaffold_quotes_verify_command_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let output_dir = tmp.path().join("path with spaces").join("sample-pack");
        let opts = ScaffoldOpts {
            name: "sample-pack".to_string(),
            harness: Harness::Codex,
            output_dir: output_dir.clone(),
        };

        let receipt = render_scaffold(&opts).expect("render scaffold");

        assert_eq!(
            receipt.verify_command,
            format!(
                "cairn plugins verify --pack-path '{}' --strict",
                output_dir.display()
            )
        );
    }

    #[test]
    fn render_scaffold_rejects_non_empty_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("existing.txt"), "content").expect("write");
        let opts = ScaffoldOpts {
            name: "sample-pack".to_string(),
            harness: Harness::Codex,
            output_dir: tmp.path().to_path_buf(),
        };

        let err = render_scaffold(&opts).expect_err("non-empty output rejected");

        assert!(matches!(err, ScaffoldError::OutputDirNotEmpty { .. }));
    }
}
