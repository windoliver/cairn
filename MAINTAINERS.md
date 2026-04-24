# Cairn Maintainers

This file is the authoritative list of humans with maintainer authority, per
[`GOVERNANCE.md`](GOVERNANCE.md). CODEOWNERS ([`.github/CODEOWNERS`](.github/CODEOWNERS))
mirrors the ownership map derived from this list.

Entry format:

```
- GitHub handle — Name — contract areas (brief §4) — contact
```

Contract areas match the seven-area partition in `GOVERNANCE.md` §2. Until the
project grows a second maintainer, every area routes to the sole maintainer.

---

## Active maintainers

- **@windoliver** — Tao Feng — all areas (core / traits / IDL, store, sensors,
  API/MCP, workflows, packaging, docs) — GitHub: [@windoliver](https://github.com/windoliver)

## Emeritus

*(none)*

---

## Change log

- **2026-04-24** — Initial file created alongside `GOVERNANCE.md` and
  `.github/CODEOWNERS`, closing the governance open question recorded in ADR
  0002 and design brief §20.1. Single-maintainer period begins.
- **2026-04-24** — `GOVERNANCE.md` §5 amended: removed the
  external-reviewer + `Reviewed-by:` trailer gate (created a hard deadlock
  for the sole maintainer); adopted solo-author workflow with admin-merge
  on green CI until a second maintainer joins. `scripts/check-reviewed-by.sh`
  and the `reviewed-by-load-bearing` CI job deleted. See amendment note
  at the top of ADR 0002.
