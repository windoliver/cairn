# Skill-pack authoring guide and scaffold - design

- **Issue:** [#183](https://github.com/windoliver/cairn/issues/183)
- **Parent:** [#28](https://github.com/windoliver/cairn/issues/28) Implement EvolutionWorkflow, Skillify, skill graph, and SkillPacks
- **Brief refs:** section 8 (CLI ground truth and four isomorphic surfaces), section 11.b (Skillify and SkillPacks), `CLAUDE.md` section 4 invariant 1 (harness-agnostic core)
- **Status:** approved, pre-implementation

## 1. Purpose

Issue #182 landed the first bundled harness pack:
`packs/cairn-claude-code/`, the `cairn-pack/v1` manifest runtime, pack
installation, and bundled-pack conformance inside `cairn plugins verify`.
Issue #183 turns that internal pattern into an authoring contract that an
external pack author can follow without reading Rust source.

The outcome is a guide plus a runnable scaffold. A third-party author should be
able to run `cairn skill new my-pack`, inspect the emitted files, run the
documented verification command, and have a small but valid pack with one
subagent, one slash command, one hook binding, one operating-manual fragment,
and one smoke test.

This design keeps `cairn-core` harness-agnostic. Harness-specific content lives
in templates and generated pack files, while `cairn-cli` owns only generic
template rendering and generic pack validation.

## 2. Scope and non-goals

In scope:

- `docs/skill-pack-authoring.md`, with stable anchors for every author-facing
  contract surface in issue #183.
- `cairn skill new <name>` for scaffold generation.
- Reference scaffold templates for `claude-code`, `codex`, and `gemini`.
- Path-based pack verification so generated packs can be checked outside the
  bundled-pack registry.
- A copyable GitHub Actions snippet for pack repositories.
- Focused tests proving all three first-class harness scaffolds render and
  verify.

Out of scope:

- Shipping full Codex or Gemini reference packs. Each harness pack still gets
  its own implementation issue.
- Pack registry hosting, search, ratings, trust distribution, or remote
  publishing.
- Replacing the Skillify `.cairnpack` archive format from issue #128.
- Adding harness-specific logic to `cairn-core`.
- Executing harnesses in end-to-end mode. The scaffold smoke test exercises the
  generated pack shape and the Cairn verification path, not a live Codex,
  Gemini, or Claude Code process.

## 3. Existing foundations

The implementation should build on these existing repo surfaces:

- `packs/cairn-claude-code/pack.json` is the current concrete
  `cairn-pack/v1` manifest.
- `crates/cairn-cli/src/packs/manifest.rs` already parses and validates
  bundled pack manifests, checks safe relative paths, hook names, capabilities,
  MCP tool references, and Claude Code subagent frontmatter.
- `crates/cairn-cli/src/packs/install.rs` installs the bundled Claude Code pack
  into a project directory.
- `crates/cairn-cli/src/packs/verify.rs` currently verifies bundled packs only.
  It must gain reusable path-based verification for external scaffolds.
- `crates/cairn-cli/src/skill.rs`, `crates/cairn-cli/src/command.rs`, and
  `crates/cairn-cli/src/main.rs` own the `cairn skill` command surface.
- `crates/cairn-cli/src/setup/codex.rs` and
  `crates/cairn-sensors-local/src/hook.rs` already recognize Codex and Gemini
  hook concepts. The scaffold should document and template against those names
  rather than invent new event vocabulary.
- `crates/cairn-core/src/pipeline/skillify/pack.rs` remains the Skillify
  archive manifest. The new guide must explain that this is distinct from
  `cairn-pack/v1` harness packs.

## 4. Terminology

The guide will use two precise terms:

- **Harness pack:** a `cairn-pack/v1` directory with a `pack.json`, subagent or
  instruction files, slash commands, hook bindings, and a manual fragment. This
  is what #182 introduced for Claude Code and what `cairn skill new` emits.
- **Skillify pack:** a `.cairnpack` archive created by the Skillify pipeline and
  managed by `cairn skillpack`. It carries evolved skills, scripts, tests,
  resolver entries, gate reports, and compatibility metadata.

The authoring guide is about harness packs. It may point to Skillify packs as
future pack content, but it must not make authors reuse
`SkillPackManifest` from `cairn-core::pipeline::skillify`.

## 5. Authoring guide

Create `docs/skill-pack-authoring.md`. The document should be concise enough
for a new author to complete a pack in one sitting, but explicit enough that
they do not need to infer contracts from source.

Required sections and stable anchors:

| Anchor | Content |
|---|---|
| `#pack-layout` | Required files and directories, using the scaffold tree as the canonical example. |
| `#manifest-schema` | `cairn-pack/v1` fields, required vs optional fields, valid `harness` values, safe path rules, and uniqueness rules. |
| `#capability-declarations` | How to list `requires_capabilities`, how those strings map to `cairn.mcp.v1`, and what happens when a capability is unavailable. |
| `#hook-binding-contract` | Canonical event names: `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`; per-harness file/config shape; payload is passed to `cairn hook <event> --payload-file - --json`. |
| `#subagent-prompt-contract` | Subagents must use typed MCP tool calls only; no direct DB writes, no WAL bypass, no shell-out for core verb behavior. |
| `#slash-command-contract` | Slash commands wrap CLI ground truth; command output should be JSON or a small deterministic envelope suitable for snapshot tests. |
| `#operating-manual-fragments` | Fragments must be block-guarded, composable, and scoped to one pack so `CLAUDE.md`, `AGENTS.md`, and `GEMINI.md` do not collide. |
| `#versioning-and-compatibility` | Pack semver, `cairn_mcp_compat`, compatibility with `cairn-core`, and safe change categories. |
| `#publishing-and-ci` | Checklist for local verification, `cairn plugins verify --pack-path`, scaffold smoke test, and GitHub Actions. |
| `#not-in-scope-for-packs` | No direct DB writes, no hidden state, no bypassing WAL, no harness-specific code in `cairn-core`, no cloud calls unless explicitly gated by pack config. |
| `#verification` | Exact commands authors and CI should run. |

The guide should include a full minimal `pack.json` from the scaffold and link
back to the current Claude Code reference pack as the larger example.

## 6. Scaffold templates

Add source templates under:

```
packs/templates/
|-- claude-code/
|   |-- pack.json.template
|   |-- manual.md.template
|   |-- agents/context-loader.md.template
|   |-- commands/cairn-context.md.template
|   |-- hooks/settings.json.template
|   |-- tests/smoke.sh.template
|   `-- .github/workflows/verify.yml.template
|-- codex/
|   |-- pack.json.template
|   |-- AGENTS.md.template
|   |-- agents/context-loader.md.template
|   |-- commands/cairn-context.md.template
|   |-- hooks/hooks.json.template
|   |-- tests/smoke.sh.template
|   `-- .github/workflows/verify.yml.template
`-- gemini/
    |-- pack.json.template
    |-- GEMINI.md.template
    |-- agents/context-loader.md.template
    |-- commands/cairn-context.md.template
    |-- hooks/hooks.json.template
    |-- tests/smoke.sh.template
    `-- .github/workflows/verify.yml.template
```

Template variables:

| Variable | Source |
|---|---|
| `{{pack_id}}` | Sanitized name argument. |
| `{{display_name}}` | Name converted to title case for human text. |
| `{{harness}}` | Selected harness id. |
| `{{version}}` | Initial `0.1.0`. |
| `{{manual_fragment}}` | Harness-specific manual file path. |
| `{{command_id}}` | Initial command id, default `cairn-context`. |
| `{{subagent_id}}` | Initial subagent id, default `context-loader`. |

The first generated pack for every harness should contain:

- One subagent or equivalent instruction file that can call
  `mcp__cairn__assemble_hot`, `mcp__cairn__retrieve`, and
  `mcp__cairn__search`.
- One slash command named `cairn-context` that wraps `cairn assemble_hot` or
  the equivalent CLI-grounded context workflow.
- One hook binding for `SessionStart`.
- One operating-manual fragment for the harness' canonical instruction file.
- One smoke script that performs nonrecursive local checks against the
  generated pack directory. The CI snippet runs the verifier and this smoke
  script as separate commands.

The template files are plain text. Rendering is a small token-replacement pass,
not a full template language.

## 7. CLI design

Extend `cairn skill` with:

```bash
cairn skill new <name> --harness <claude-code|codex|gemini> [--output <dir>] [--json]
```

Behavior:

1. Validate `<name>` using the same safe token predicate as pack ids: ASCII
   alphanumeric plus `-` and `_`, no dot components, no path separators.
2. Resolve output directory. If `--output` is absent, write to `./<name>`.
3. Fail if the output directory exists and is non-empty.
4. Render the selected template tree into the output directory.
5. Parse the generated `pack.json`.
6. Run path-based pack validation against the generated directory.
7. Emit a human receipt listing generated files and the next verification
   command. With `--json`, emit the same data as JSON.

No `--force` is part of the first implementation. Authors can remove the
directory or choose a new output path, which avoids accidental overwrite of
hand-authored pack content.

Errors:

- `InvalidPackName` for unsafe names.
- `UnknownTemplateHarness` for unsupported harnesses.
- `OutputDirectoryNotEmpty` for existing non-empty targets.
- `TemplateMissing` if the embedded template tree is incomplete.
- Existing `PackError` variants for manifest, path, tool, hook, and capability
  validation failures.

## 8. Path-based pack verification

Current `cairn plugins verify` verifies bundled plugins and bundled packs. Add
an external-pack path mode:

```bash
cairn plugins verify --pack-path <dir> [--strict] [--json]
```

Behavior:

1. If `--pack-path` is absent, preserve current behavior exactly.
2. If `--pack-path` is present, load `<dir>/pack.json`, validate pass A and
   pass B, assert referenced paths exist, run harness-specific static checks,
   install the pack into a temp project, and run the scaffold smoke test when
   present. The smoke test must not call `cairn plugins verify`, because the
   verifier is already the caller.
3. Human output should name the pack path and pack id. JSON output should keep
   the same top-level `plugins` and `summary` shape by emitting a synthetic
   `contract: "pack"` report.
4. `--strict` remains unchanged: pending cases become non-zero.

Harness-specific static checks:

- Claude Code: `tools:` frontmatter in subagents must match
  `uses_mcp_tools`.
- Codex: `AGENTS.md` fragment must contain the guarded pack block and the hook
  file must be valid JSON.
- Gemini: `GEMINI.md` fragment must contain the guarded pack block and the hook
  file must be valid JSON.

This path verifier is also reused by `cairn skill new` after rendering.

The install round-trip should use the same generic install code path as bundled
packs, refactored to accept either an embedded source directory or a filesystem
source directory. The verifier does not persist anything into the author's
current working tree; it writes into a temporary project, confirms the expected
manual fragment, command file, hook file, and subagent or instruction file were
materialized, then deletes the temp directory.

## 9. Manifest shape

Keep the `cairn-pack/v1` manifest compatible with the #182 runtime. The minimal
generated manifest should look like:

```json
{
  "schema": "cairn-pack/v1",
  "pack_id": "my-pack",
  "name": "my-pack",
  "version": "0.1.0",
  "harness": "codex",
  "cairn_mcp_compat": ">=1.0.0",
  "description": "Starter Cairn skill-pack for Codex.",
  "requires_capabilities": [
    "cairn.mcp.v1.search.keyword",
    "cairn.mcp.v1.retrieve.record"
  ],
  "subagents": [
    {
      "id": "context-loader",
      "path": "agents/context-loader.md",
      "uses_mcp_tools": ["assemble_hot", "retrieve", "search"]
    }
  ],
  "commands": [
    {
      "id": "cairn-context",
      "path": "commands/cairn-context.md",
      "kind": "verb-direct",
      "verb": "assemble_hot"
    }
  ],
  "hooks": {
    "SessionStart": { "command": "cairn hook SessionStart" }
  },
  "manual_fragment": "AGENTS.md"
}
```

To support Codex and Gemini, `Harness` in
`crates/cairn-cli/src/packs/manifest.rs` should grow `Codex` and `Gemini`
variants. `bundled_pack_for()` remains Claude Code only until full first-party
packs exist. Template rendering and path verification must not require a
bundled registry entry.

## 10. File structure

Create:

- `docs/skill-pack-authoring.md`
- `packs/templates/claude-code/**`
- `packs/templates/codex/**`
- `packs/templates/gemini/**`
- `crates/cairn-cli/src/packs/template.rs`

Modify:

- `crates/cairn-cli/src/command.rs` to add `skill new`.
- `crates/cairn-cli/src/main.rs` to dispatch `skill new`.
- `crates/cairn-cli/src/skill.rs` to expose scaffold options, receipts, and
  rendering.
- `crates/cairn-cli/src/packs/manifest.rs` to support `Codex` and `Gemini`
  harness ids.
- `crates/cairn-cli/src/packs/verify.rs` to verify external pack paths.
- `crates/cairn-cli/src/plugins/verify.rs` to route `--pack-path`.
- `crates/cairn-cli/tests/*` for CLI, scaffold, and verifier coverage.
- `docs/site/src/SUMMARY.md` and generated docs only if the existing docgen
  gate expects the new guide to be linked in the public docs.

No `cairn-core` source files should change for this issue.

## 11. Testing strategy

Use test-first implementation for behavior changes.

Focused test cases:

- `skill_new_rejects_unsafe_name`: `../bad` and `bad.name` fail before any
  filesystem writes.
- `skill_new_fails_on_non_empty_output`: existing user content is preserved.
- `skill_new_claude_code_scaffold_verifies`: generated Claude Code pack passes
  path-based verification, including install round-trip.
- `skill_new_codex_scaffold_verifies`: generated Codex pack passes path-based
  verification, including install round-trip.
- `skill_new_gemini_scaffold_verifies`: generated Gemini pack passes
  path-based verification, including install round-trip.
- `plugins_verify_pack_path_json_shape`: `cairn plugins verify --pack-path
  <dir> --json` keeps the existing summary shape and emits
  `contract: "pack"`.
- `guide_has_required_anchors`: the committed guide contains each required
  anchor.
- `templates_have_ci_snippet`: each harness template includes
  `.github/workflows/verify.yml`.

Verification commands for the implementation plan:

```bash
cargo test --locked -p cairn-cli skill_new
cargo test --locked -p cairn-cli pack_path
cargo test --locked -p cairn-cli --test cli skill_new
cargo run -p cairn-cli -- skill new my-pack --harness codex --output /tmp/cairn-pack-smoke
cargo run -p cairn-cli -- plugins verify --pack-path /tmp/cairn-pack-smoke --strict
cargo fmt --all --check
./scripts/check-core-boundary.sh
```

The final implementation PR should also run the existing bundled-pack snapshot
coverage if template or verifier changes touch shared pack code:

```bash
cargo test -p cairn-cli --test claude_code_pack_install --locked
cargo test -p cairn-cli --test claude_code_pack_verify --locked
```

## 12. Acceptance mapping

| Issue acceptance criterion | Design coverage |
|---|---|
| External author can produce a passing pack from the guide | `docs/skill-pack-authoring.md`, `cairn skill new`, path-based verifier, CI snippet. |
| `cairn skill new my-pack` produces a passing scaffold | CLI design plus per-harness scaffold verification tests. |
| Claude Code reference pack and later first-party packs match scaffold and guide | Guide anchors plus template layout intentionally mirror `packs/cairn-claude-code/`; later packs should start from templates. |
| Doc lists every contract surface with stable anchor links | Section 5 required anchor table plus `guide_has_required_anchors` test. |
| Throwaway second pack installs and runs end-to-end | Path-based verifier installs a generated Codex pack into a temp project and runs the nonrecursive scaffold smoke script. |
| CI smoke test for `cairn skill new` | GitHub Actions snippet plus CLI tests. |
| Lint doc for broken intra-doc links and missing schema fields | `guide_has_required_anchors` and manifest/schema test coverage. |

## 13. Risks and mitigations

- **Risk: `cairn-pack/v1` and Skillify `.cairnpack` remain confusing.**
  Mitigation: define terms early, include side-by-side examples, and avoid
  reusing `SkillPackManifest` names in the guide for harness packs.
- **Risk: External pack verification drifts from bundled-pack verification.**
  Mitigation: refactor `packs::verify` around a shared verifier that accepts
  either an embedded dir or filesystem dir.
- **Risk: Template rendering becomes a hidden mini-language.**
  Mitigation: use fixed token replacement only and fail if any unresolved
  template token remains after rendering.
- **Risk: Harness-specific checks leak into core.** Mitigation: keep all
  checks in `cairn-cli::packs` and template files; leave `cairn-core`
  untouched.
- **Risk: Scaffold overwrites author work.** Mitigation: first slice has no
  force mode and refuses non-empty outputs.

## 14. Implementation handoff

After this design is reviewed, write an implementation plan in
`docs/superpowers/plans/2026-05-27-issue-183-skill-pack-authoring.md`. The plan
should sequence the work as:

1. Add failing tests for `skill new` argument validation and output behavior.
2. Add template embedding and rendering.
3. Add external pack path verification.
4. Render all three harness scaffolds and verify them.
5. Add the authoring guide and anchor tests.
6. Add CI snippet coverage and final verification.
