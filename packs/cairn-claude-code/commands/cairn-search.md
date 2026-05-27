---
description: Direct Cairn `search` verb.
argument-hint: "<query> --mode <keyword|semantic|hybrid>"
---

<!-- BEGIN CAIRN PACK -->
Run `cairn search $ARGUMENTS`.

Default to `--mode keyword` if the user passed only a query — keyword
is the only search mode guaranteed available on every vault. Semantic
and hybrid require an embedding provider; if the user wants those,
they pass `--mode semantic` or `--mode hybrid` explicitly. Run
`cairn status` first to confirm the requested mode is advertised
before retrying with `semantic` or `hybrid`.

Render the top results with `id`, `score`, and a one-line snippet each.
<!-- END CAIRN PACK -->
