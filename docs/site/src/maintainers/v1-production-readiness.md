# v1.0 Production Readiness

This is the release-owner runbook for issue
[#142](https://github.com/windoliver/cairn/issues/142): validate v1.0
production readiness across three harnesses, package/install smoke, release
gates, and known limitations.

Use this page for GA sign-off. The beta checklist remains the prerequisite
automation and manual gate surface; this page adds the three-harness acceptance
matrix required by design brief §18 and §19.

## Prerequisites

Before starting the matrix:

- Dependency issues #25, #139, #140, and #141 are closed.
- The release SHA is fixed and pushed.
- The release artifacts or package dry-run outputs are available.
- [Beta readiness](beta-readiness.md) has been attempted on the release SHA.
- [MCP semver policy](mcp-semver-policy.md) gate 10 is green for the release
  SHA, not just for a nearby branch tip.
- [status.md](../status.md) "Stubbed or pending" has been reviewed against the
  [capability matrix](../reference/capability-matrix.md).

Do not close issue #142 while a prerequisite dependency is still open or while
any harness row below is missing evidence.

## Release Setup

Run the matrix from a clean checkout at the release SHA:

```bash
set -euo pipefail

SHA=$(git rev-parse HEAD)
ACCEPT_ROOT=$(mktemp -d -t cairn-v1-acceptance-XXXXXX)
export SHA ACCEPT_ROOT

cargo build --release --locked -p cairn-cli --bin cairn
export CAIRN_BIN="$PWD/target/release/cairn"

"$CAIRN_BIN" --version | tee "$ACCEPT_ROOT/cairn-version.txt"
"$CAIRN_BIN" bootstrap --vault-path "$ACCEPT_ROOT/vault" --json \
  | tee "$ACCEPT_ROOT/bootstrap.json"
```

Keep the whole `ACCEPT_ROOT` directory until the release issue has the sign-off
block. It is the local evidence bundle for the release owner.

Keep running the remaining snippets in the same shell so `pipefail` stays
enabled; the evidence commands intentionally use `tee`, and failed gates must
not be masked by successful log writes.

## Automated Gates

Run the package/install and release gates before harness-specific checks:

```bash
CAIRN_BIN="$CAIRN_BIN" scripts/install-smoke.sh \
  | tee "$ACCEPT_ROOT/install-smoke.log"

scripts/beta-readiness.sh --full \
  | tee "$ACCEPT_ROOT/beta-readiness-full.log"

cargo run -p cairn-bench --release --locked -- coherence run --gate rc \
  | tee "$ACCEPT_ROOT/coherence-rc.log"
```

The `--full` beta readiness runner lists manual gates 9-15 at the end. For
v1.0, those manual gates still need human review; this page does not convert
them into automation.

## Harness Matrix

Each harness row must prove install/bootstrap, MCP or skill availability,
the eight core verbs, hooks or explicit trace capture, search modes, hot memory,
workflow visibility, privacy, forget, and package/update behavior. Use a fresh
temporary vault per harness if practical; otherwise record why the shared
acceptance vault was used.

| Harness | Primary path | Required evidence | Known limitation policy |
|---------|--------------|-------------------|-------------------------|
| Claude Code | `cairn setup claude-code` plus `cairn doctor claude-code` | setup receipt, doctor JSON, five-hook loop smoke, P0 story checklist from [Claude Code Reference Consumer](../usage/claude-code-reference.md) | No limitation may be hidden; failed doctor stages block sign-off. |
| Codex | `cairn setup codex` plus project hook file and skill fallback | setup receipt, generated `.codex/config.toml`, generated `.codex/hooks.json`, Codex-shaped trace replay, skill install receipt | Codex hook loading is best-effort per [Codex](../usage/codex.md); unsupported hook loading must be called out in known limitations. |
| Third supported harness | `cairn skill install --harness gemini` or another supported skill harness | skill install receipt, explicit CLI verb smoke, trace capture/import evidence, documented integration notes | If the harness has no first-party `setup` or `doctor` command, sign-off must say that the skill path is the supported GA surface. |

The expected third harness for GA is Gemini unless the release owner explicitly
chooses another supported skill harness (`opencode`, `cursor`, or `custom`) and
records that substitution in the release issue.

### Claude Code

```bash
CC_PROJECT="$ACCEPT_ROOT/claude-code-project"
CC_VAULT="$ACCEPT_ROOT/claude-code-vault"
mkdir -p "$CC_PROJECT"
"$CAIRN_BIN" bootstrap --vault-path "$CC_VAULT" --json \
  | tee "$ACCEPT_ROOT/claude-code-bootstrap.json"

"$CAIRN_BIN" setup claude-code \
  --scope project \
  --project-dir "$CC_PROJECT" \
  --vault "$CC_VAULT" \
  --binary "$CAIRN_BIN" \
  --json | tee "$ACCEPT_ROOT/claude-code-setup.json"

"$CAIRN_BIN" doctor claude-code \
  --project-dir "$CC_PROJECT" \
  --json | tee "$ACCEPT_ROOT/claude-code-doctor.json"
```

After doctor passes, run the hook-loop smoke from
[Claude Code Reference Consumer](../usage/claude-code-reference.md) against
`$CC_VAULT`, then capture the artifact list:

```bash
find "$CC_VAULT/.cairn/hooks" -type f | sort \
  | tee "$ACCEPT_ROOT/claude-code-hook-artifacts.txt"
```

Source-level regression checks:

```bash
cargo test -p cairn-cli --test claude_code_setup --locked \
  | tee "$ACCEPT_ROOT/claude-code-setup-test.log"
```

### Codex

```bash
CODEX_PROJECT="$ACCEPT_ROOT/codex-project"
CODEX_HOME="$ACCEPT_ROOT/codex-home"
CODEX_VAULT="$ACCEPT_ROOT/codex-vault"
mkdir -p "$CODEX_PROJECT" "$CODEX_HOME"
"$CAIRN_BIN" bootstrap --vault-path "$CODEX_VAULT" --json \
  | tee "$ACCEPT_ROOT/codex-bootstrap.json"

"$CAIRN_BIN" setup codex \
  --project-dir "$CODEX_PROJECT" \
  --home-dir "$CODEX_HOME" \
  --vault "$CODEX_VAULT" \
  --binary "$CAIRN_BIN" \
  --json | tee "$ACCEPT_ROOT/codex-setup.json"

test -f "$CODEX_HOME/.codex/config.toml"
test -f "$CODEX_PROJECT/.codex/hooks.json"
```

Codex does not currently have a dedicated `cairn doctor codex` command. Treat
that as an explicit known limitation unless it is added before the release.
Validate the generated setup shape and trace behavior with source-level checks:

```bash
cargo test -p cairn-cli --test codex_setup --locked \
  | tee "$ACCEPT_ROOT/codex-setup-test.log"

cargo test -p cairn-cli --test capture_trace_verb --locked \
  | tee "$ACCEPT_ROOT/codex-capture-trace-test.log"
```

Install the skill fallback, because Codex deployments that do not load
`.codex/hooks.json` still need an explicit supported path:

```bash
"$CAIRN_BIN" skill install \
  --harness codex \
  --target-dir "$ACCEPT_ROOT/codex-skill" \
  --force \
  --json | tee "$ACCEPT_ROOT/codex-skill.json"
```

### Third Supported Harness

Use Gemini by default:

```bash
GEMINI_VAULT="$ACCEPT_ROOT/gemini-vault"
"$CAIRN_BIN" bootstrap --vault-path "$GEMINI_VAULT" --json \
  | tee "$ACCEPT_ROOT/gemini-bootstrap.json"

"$CAIRN_BIN" skill install \
  --harness gemini \
  --target-dir "$ACCEPT_ROOT/gemini-skill" \
  --force \
  --json | tee "$ACCEPT_ROOT/gemini-skill.json"
```

Then exercise the lowest-common-denominator skill surface through the CLI:

```bash
"$CAIRN_BIN" ingest \
  --vault "$GEMINI_VAULT" \
  --kind user \
  --harness gemini \
  --body "Gemini v1 acceptance memory" \
  --json | tee "$ACCEPT_ROOT/gemini-ingest.json"

"$CAIRN_BIN" search \
  --vault "$GEMINI_VAULT" \
  --mode keyword \
  "Gemini v1 acceptance" \
  --json | tee "$ACCEPT_ROOT/gemini-search.json"

"$CAIRN_BIN" assemble_hot \
  --vault "$GEMINI_VAULT" \
  --json | tee "$ACCEPT_ROOT/gemini-assemble-hot.json"
```

Source-level regression check:

```bash
cargo test -p cairn-cli --test skill_agent_pack --locked \
  | tee "$ACCEPT_ROOT/skill-agent-pack-test.log"
```

If the third harness is not Gemini, replace `--harness gemini` with the chosen
supported value and record the reason in the sign-off block.

## Capability Checklist

For each harness, attach evidence for every row:

| Capability area | Evidence |
|-----------------|----------|
| Install and bootstrap | `install-smoke.log`, harness bootstrap JSON, binary version. |
| MCP, CLI, or skill use | setup JSON, doctor JSON where available, or skill install JSON. |
| Hooks | five-hook artifact list, or explicit note that the harness uses skill/trace capture instead of first-party hooks. |
| Search modes | `status --json` capabilities plus keyword run; semantic/hybrid run only when advertised. |
| Hot memory | `assemble_hot --json` output or hook-produced hot-memory artifact path. |
| Workflows | `cairn admin sre report --json` and beta readiness gate 4 / 13 evidence. |
| Privacy | beta readiness gate 14 and `cargo run -p cairn-bench --release --locked -- privacy` or `bench all` evidence. |
| Forget | `install-smoke.sh` forget step plus a harness-specific record-forget round trip when possible. |
| Package update behavior | release dry-run, issue #141 update metadata tests, and package dry-run logs. |
| Known limitations | Release notes section copied from `status.md` and capability matrix review. |

## Known Limitations

Known limitations are release facts, not marketing copy. The release sign-off
must include every limitation that affects a supported harness. At minimum,
review:

- [status.md](../status.md) "Stubbed or pending".
- [Capability Matrix](../reference/capability-matrix.md) deferred-wiring rows.
- [Codex](../usage/codex.md) hook loading caveat.
- Any manual beta readiness gate that failed or was skipped.

If a limitation affects acceptance, the release owner must either document it
in the release notes or stop the release.

## Sign-off Block

Copy this block into the release issue:

````markdown
## v1.0 production readiness sign-off

Release SHA: <sha>
Release artifacts: <links or local artifact ids>
Evidence bundle: <path or attached archive>

- [ ] Dependency issues closed: #25, #139, #140, #141
- [ ] Beta readiness full run passed with no release-blocking skips
- [ ] Package/install smoke passed
- [ ] Release gate command passed
- [ ] Contract freeze verified on the release SHA
- [ ] Claude Code acceptance row complete
- [ ] Codex acceptance row complete
- [ ] Third supported harness acceptance row complete
- [ ] Known limitations copied into release notes
- [ ] Package update behavior verified

Known limitations:
- <limitation or "None beyond status.md Stubbed or pending">

Reviewed by: <maintainer>
Date: <YYYY-MM-DD>
````
