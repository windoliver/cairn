# Beta Readiness

This checklist is the maintainer gate every Cairn build must pass before it
is eligible for the public beta channel. If any item fails, the build is not
beta-eligible.

## Quick start

```bash
# Runs gates 1-8 (automatable). ~3 min.
scripts/beta-readiness.sh --quick

# Runs gates 1-8 plus the long-running eval + package + audit gates. ~15 min.
scripts/beta-readiness.sh --full
```

The script honors `CAIRN_BIN`, `CARGO_TARGET_DIR`, and `RUST_LOG`. Gates 9-15
are manual and listed at the end of the script output.

## Gate categories

### 1. Code quality

| Item | Command |
|------|---------|
| Format | `cargo fmt --all --check` |
| Lint | `cargo clippy --workspace --all-targets --locked -- -D warnings` |
| Compile | `cargo check --workspace --all-targets --locked` |
| Unit + integration tests | `cargo nextest run --workspace --locked --no-fail-fast` |
| Doc tests | `cargo test --doc --workspace --locked` |

**Pass:** every command exits 0.
**Common failure:** a recent PR added a clippy violation. First place to look: latest diff to the failing crate.

### 2. Core boundary

| Item | Command |
|------|---------|
| Core dep freeness | `scripts/check-core-boundary.sh` |
| No OS locks in core | `scripts/check-no-os-locks.sh` |
| Lint reads readonly sources | `scripts/check-lint-readonly-sources.sh` |

**Pass:** all three exit 0.
**Common failure:** `cairn-core` grew a dependency on an adapter crate. Move the dependency to the adapter and call back via a trait.

### 3. Generated code drift

| Item | Command |
|------|---------|
| Codegen | `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` |
| Docgen | `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check` |

**Pass:** both exit 0.
**Common failure:** an IDL or CLI change shipped without re-running codegen / docgen. Re-run with `--write` and commit.

### 4. Eval gates

| Item | Command |
|------|---------|
| Full bench | `cargo run -p cairn-bench --release --locked -- all` |
| Coherence gate | `cargo run -p cairn-bench --release --locked -- coherence run --gate beta` |

**Pass:** all bench scores within floor, no metric regression > 2 % from baseline (forget_completeness: 0 % tolerance per #137).
**Common failure:** a metric dropped > 2 %. Investigate the failing metric; if intentional, update the baseline trend file in a separate commit.

### 5. Docs build

| Item | Command |
|------|---------|
| mdBook | `mdbook build docs/site` |
| Rustdoc + broken-link check | `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked` |

**Pass:** both exit 0.
**Common failure:** a renamed item left a stale intra-doc link. Fix the link target.

### 6. Supply chain

| Item | Command |
|------|---------|
| License + advisory | `cargo deny check` |
| Audit | `cargo audit --deny warnings` |
| Unused deps | `cargo machete` |

**Pass:** all three exit 0.
**Common failure:** a transitive dep ships under a license not in `deny.toml`. Either pin the dep to a compatible version or update the allowlist (maintainer sign-off required).

### 7. Install smoke

| Item | Command |
|------|---------|
| End-to-end CLI smoke | `CAIRN_BIN=target/release/cairn scripts/install-smoke.sh` |

**Pass:** all eight P0 verbs round-trip on a fresh temp vault.
**Common failure:** a verb regressed against the default-off `search.local_embeddings` profile.

### 8. Package dry-run (only when touching publish-affecting metadata)

| Item | Command |
|------|---------|
| Workspace package | `cargo package --workspace --no-verify --locked --allow-dirty` |
| Publish dry-run (idl) | `cargo publish --dry-run --locked --allow-dirty -p cairn-idl` |
| Publish dry-run (core) | `cargo publish --dry-run --locked --allow-dirty -p cairn-core` |

**Pass:** all three exit 0.
**Common failure:** a workspace package added a path-only dep without a version constraint. Add the version per CLAUDE.md §9 publish-order rules.

### 9. Capability sync (manual)

Run `target/release/cairn status --json` and compare the `capabilities[]`
array against the [capability matrix](../reference/capability-matrix.md) row
for the target phase.

**Pass:** the advertised set equals the matrix row exactly. No extras, no omissions.
**Failure:** the runtime advertises a capability the matrix says shouldn't ship yet (or vice versa). Reconcile in `cairn-core::status::advertise` plus the matching `wiring::*_WIRED` constant per CLAUDE.md §4 invariant 6.

### 10. Contract freeze verified (manual)

Verify the `contract-drift` CI job is green on the **release SHA**
specifically — not on whatever the branch's latest run happened to
cover. From the PR or the release branch, pin to the exact commit:

```bash
SHA=$(git rev-parse HEAD)
RUN_ID=$(gh run list --commit "$SHA" --workflow ci.yml \
  --limit 1 --json databaseId --jq '.[0].databaseId // ""')
if [ -z "$RUN_ID" ]; then
  echo "fail: no ci.yml run found for $SHA — push the commit and wait for CI"
  exit 1
fi
gh run view "$RUN_ID" --json jobs \
  --jq '.jobs[] | select(.name | startswith("contract-drift")) | .conclusion'
```

Expected: `"success"`. The matrix-shaped job name (`contract-drift / wire-compat …`) is matched via `startswith` because `gh` appends the matrix suffix to the configured job name.

If no run exists for the release SHA, the gate **fails closed** —
never validate against an older run, because the commit you're
releasing has not been CI-verified yet.

**Pass:** `contract-drift` succeeded on the release SHA, **and** no
schema file under `crates/cairn-idl/schema/` was changed without an
accompanying ADR amendment, **and** no `x-cairn-deprecated` markers were
added or removed since the previous release without a CHANGELOG entry.

**Failure:** `contract-drift` is red. Inspect the failing step
(`cairn-codegen --check`, `cairn-docgen --check`, `wire_compat_v1`,
`capability_matrix_v1`, the per-surface status / parity snapshots,
or the SDK transport filter test); if the change is intended and
additive, follow the snapshot-accept recipe in
[MCP Semver Policy](mcp-semver-policy.md) ("Adding a capability" /
"Adding an optional field"). The MCP envelope conformance suite
(`mcp_conformance`) ships in the regular `test` job — verify that's
green too. If the change is breaking, **stop**: file a v2 design
issue and follow the procedure in
[MCP Semver Policy](mcp-semver-policy.md).

See [ADR 0004](https://github.com/windoliver/cairn/blob/main/docs/design/decisions/0004-mcp-v1-semver-freeze.md)
for the authoritative freeze rules.

### 11. Migration guide review (manual)

Open the per-pair migration guide for the target phase
([usage/migration/](../usage/migration/index.md)). Verify all seven sections
(phase summary, capability deltas, config deltas, CLI/MCP/SDK/skill deltas,
WAL/store deltas, sensor deltas, upgrade steps) are populated for surfaces
that actually ship in the target phase.

**Pass:** no `_To be filled when v0.Y ships._` markers remain for capabilities
the runtime now advertises.
**Failure:** a capability advertised by `cairn status` has no migration
content. Fill the section.

### 12. Known limitations (manual)

Review [status.md](../status.md) "Stubbed or pending" against the current
capability matrix. Anything still stubbed must be either:

- removed from the stubbed list (because it now ships), or
- explicitly called out in the release notes as a known limitation.

### 13. Cassette replay (manual)

```bash
cargo run -p cairn-bench --release --locked -- coherence run --gate beta
```

**Pass:** all replay cassettes from #136 pass under the beta gate; all five
coherence metrics (per #137) meet their floors.

### 14. Privacy posture (manual)

Exercise the consent + forget round-trip on a real session:

```bash
cairn ingest --kind user --body "test memory"
RECORD=$(cairn search "test memory" --json | jq -r '.hits[0].id')
cairn forget --record "$RECORD"
cairn search "test memory" --json | jq '.hits | length'   # 0
```

Spot-check `.cairn/consent.log` for the `delete` entry. Verify the presidio
scrub pass redacts at least one PII pattern in a known-PII fixture.

### 15. Release notes draft (manual)

Populate the per-phase release notes template. Cross-link every capability
delta to the matching row in the [migration guide](../usage/migration/index.md).

## Failure remediation

| Failed gate | First place to look |
|-------------|----------------------|
| Format | `cargo fmt --all` then commit. |
| Clippy | Latest PR diff to the failing crate. |
| Nextest | `cargo nextest run --workspace --no-capture` for the failing test. |
| Doc tests | The failing doctest's source file; the example may have drifted from the API. |
| Codegen / docgen drift | Re-run with `--write` and commit the result. |
| Bench / coherence | `crates/cairn-bench/baseline/trend.json` for the prior baseline; the failing metric's source data. |
| mdBook | Search for the broken link's target file. |
| Rustdoc | The `[broken_link]` target — usually a renamed item. |
| Deny / audit / machete | `Cargo.lock` for the offending dep; pin or remove. |
| Install smoke | The verb that printed `fail: <verb>` and the temp vault path. |
| Package dry-run | The crate that failed; check its `Cargo.toml` for missing version metadata. |
| Capability sync (gate 9) | `cairn-core::status::advertise` and the `wiring::*_WIRED` constants. |
| Contract freeze (gate 10) | `cairn-core::status::advertise`, `crates/cairn-idl/schema/`, ADR 0004. |

## Sign-off block

Copy this block into the release issue:

````markdown
## Beta readiness sign-off — v0.X.Y-beta.N

- [ ] Gate 1: code quality
- [ ] Gate 2: core boundary
- [ ] Gate 3: generated code drift
- [ ] Gate 4: eval gates
- [ ] Gate 5: docs build
- [ ] Gate 6: supply chain
- [ ] Gate 7: install smoke
- [ ] Gate 8: package dry-run
- [ ] Gate 9: capability sync (manual)
- [ ] Gate 10: contract freeze verified (manual)
- [ ] Gate 11: migration guide review (manual)
- [ ] Gate 12: known limitations (manual)
- [ ] Gate 13: cassette replay (manual)
- [ ] Gate 14: privacy posture (manual)
- [ ] Gate 15: release notes draft (manual)

Reviewed by: <maintainer>
Date: <YYYY-MM-DD>
Commit: <sha>
````
