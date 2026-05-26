//! Build-time embedding of bundled packs via `include_dir!`.

use include_dir::{Dir, include_dir};

/// Bundled `cairn-claude-code` pack content.
///
/// Embedded from `packs/cairn-claude-code/` at build time. The path is
/// relative to the crate's `CARGO_MANIFEST_DIR`
/// (`crates/cairn-cli/`), so the pack source lives at
/// `<workspace>/packs/cairn-claude-code/`.
pub static CAIRN_CLAUDE_CODE_PACK: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/../../packs/cairn-claude-code");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_contains_pack_json() {
        assert!(
            CAIRN_CLAUDE_CODE_PACK.get_file("pack.json").is_some(),
            "embedded pack must contain pack.json"
        );
    }
}
