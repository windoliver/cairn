# CI

Docs CI has three lanes:

- `docs / cargo doc`: rustdoc for Rust API references.
- `docs / generated reference`: `cairn-docgen --check` for committed generated
  Markdown drift.
- `docs / mdbook build`: structural docs-site build.

The link checker remains advisory because external hosts can rate-limit or
block bot user agents. It still reports useful link rot, but it should not
block unrelated code changes.

GitHub Pages deployment is separate from PR checks. It builds and deploys the
mdBook site only on pushes to `main` or manual workflow dispatch.
