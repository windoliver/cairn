# BrainBench world-v1 fixture

Vendored retrieval corpus consumed by `cairn-bench`. See `LICENSE.NOTICE`
for upstream attribution and version pin.

## Layout

```
brainbench-world-v1/
├── LICENSE.NOTICE                ← provenance + license
├── README.md                     ← you are here
├── pages/                        ← 241 verbatim pages from gbrain-evals
│   ├── companies__nimbus-5.json
│   ├── people__alice-chen-1.json
│   └── … (239 more)
├── queries.json                  ← derived; produced by capture script
└── upstream-baseline.json        ← derived; per-adapter per-query metrics
```

## Pinned upstream version

- Repo: <https://github.com/garrytan/gbrain-evals>
- Commit: `b8cf8ad057635cbb03c0f3996acb693afbcae605`

To bump the pin: re-run the capture procedure (see
`scripts/README-brainbench-capture.md`) and commit the regenerated
`queries.json` + `upstream-baseline.json` together with the new pages.

## Page schema

```jsonc
{
  "slug": "companies/nimbus-5",
  "type": "company",
  "title": "Nimbus",
  "compiled_truth": "Nimbus is a climate-tech startup …",
  "_facts": { … },                 // free-form, opaque to cairn-bench
  "timeline": [ … ]                // free-form, opaque to cairn-bench
}
```

`cairn-bench`'s `Page` struct loads `compiled_truth` as `body` via a
serde alias so the fixture is consumed verbatim without a rewrite
step.

## Running

```sh
cargo run --release -p cairn-bench --features openai --locked -- \
  --fixture fixtures/v0/brainbench-world-v1 \
  --out-dir target/brainbench
```

Add `--skip-openai` if `OPENAI_API_KEY` is unset.
