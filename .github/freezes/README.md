# Active path freezes

Freeze files in this directory are consumed by
`scripts/check-freeze.sh` (run from the required
`governance / freeze` status check in
`.github/workflows/governance.yml`). When one is present, any PR whose
diff touches a path listed under `paths:` fails the check and cannot
merge.

Freezes are created under ADR 0001 "Immediate containment when a hard
trigger fires" (`docs/design/decisions/0001-monorepo-governance.md`).
They are **not** a routine mechanism — they exist to halt merges on a
specific crate or path while a licensing, security, or external-SLA
hard trigger is being resolved.

## File format

One YAML file per active freeze; filename convention
`YYYY-MM-DD-<trigger>-<slug>.yaml`. Only the `paths:` list is machine-
parsed; every other field is human audit context.

```yaml
trigger: H2                # hard-trigger ID from ADR 0001
issue: https://github.com/windoliver/cairn/issues/NNN
owner: "@windoliver"
opened: 2026-04-24
deadline: 2026-05-08       # 14 days max per ADR 0001 containment rule
reason: |
  One-paragraph rationale. Summary of the hard trigger and interim
  mitigation in place.
paths:
  - crates/cairn-store-sqlite/
  - crates/cairn-store-sqlite/Cargo.toml
```

## Removing a freeze

Open a PR that deletes the file. Label it `governance:freeze-removal`
(exempt from the freeze gate so the freeze itself can be lifted). The
PR description must link to the resolution: either the extraction ADR
that supersedes the freeze, or a written statement from the owner that
the trigger no longer applies.
