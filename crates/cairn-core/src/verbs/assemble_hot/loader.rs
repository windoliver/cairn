//! Hot-memory source loading and budget trimming for issue #61.

use std::io::Read as _;
use std::path::{Component, Path};

use crate::generated::verbs::assemble_hot::HotRecipeStep;

/// Trim recipe bodies to a total byte budget without splitting UTF-8 code points.
#[must_use]
pub fn trim_bodies_to_budget(
    recipe: &[HotRecipeStep],
    mut bodies: Vec<String>,
    budget: u64,
) -> Vec<String> {
    if recipe.is_empty() {
        return Vec::new();
    }

    bodies.truncate(recipe.len());
    while bodies.len() < recipe.len() {
        bodies.push(String::new());
    }

    let mut used = 0_u64;
    for body in &mut bodies {
        let remaining = budget.saturating_sub(used);
        if remaining == 0 {
            body.clear();
            continue;
        }
        if body.len() as u64 > remaining {
            let mut end = usize::try_from(remaining).unwrap_or(0).min(body.len());
            while !body.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            body.truncate(end);
        }
        used = used.saturating_add(body.len() as u64);
    }
    bodies
}

/// Read a UTF-8 markdown file from the vault root without following source symlinks.
///
/// The caller decides whether a missing source is optional or fail-closed.
pub fn read_vault_markdown_file(
    vault_root: &Path,
    relative_path: &Path,
    max_bytes: u64,
) -> Result<String, std::io::Error> {
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hot-memory source path must be a simple vault-relative path",
        ));
    }

    let root = vault_root.canonicalize()?;
    let path = vault_root.join(relative_path);
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hot-memory source path must not be a symlink",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hot-memory source path must be a regular file",
        ));
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "hot-memory source path resolved outside the vault",
        ));
    }

    let max_len = usize::try_from(max_bytes).unwrap_or(usize::MAX.saturating_sub(4));
    let read_cap = max_len.saturating_add(4);
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0).min(read_cap));
    std::fs::File::open(canonical)?
        .take(u64::try_from(read_cap).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let end = bytes.len().min(max_len);
    if let Err(err) = std::str::from_utf8(&bytes[..end]) {
        if err.error_len().is_some() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
        }
        bytes.truncate(err.valid_up_to());
    } else {
        bytes.truncate(end);
    }
    String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}
