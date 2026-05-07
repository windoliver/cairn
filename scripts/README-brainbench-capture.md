# Capturing the BrainBench upstream baseline

`cairn-bench` consumes two derived JSON files captured once from upstream
`gbrain-evals` and committed alongside the verbatim page corpus:

- `fixtures/v0/brainbench-world-v1/queries.json` — graded queries
- `fixtures/v0/brainbench-world-v1/upstream-baseline.json` — per-adapter
  per-query metrics for the four upstream reference adapters

These files are static fixtures, captured once and committed. **Re-capture
only when bumping the upstream pin.**

## Pinned upstream

- Repo: <https://github.com/garrytan/gbrain-evals>
- Commit: `b8cf8ad057635cbb03c0f3996acb693afbcae605`
- License: MIT

The pin lives in `fixtures/v0/brainbench-world-v1/LICENSE.NOTICE`. Bumping
it is a deliberate two-step:

1. Re-copy `pages/*.json` from the new upstream commit.
2. Re-run this capture procedure to refresh `queries.json` +
   `upstream-baseline.json`.

## Steps

```sh
# 1. Install Bun (https://bun.sh/install) if not already present.

# 2. Clone gbrain-evals at the pin.
git clone https://github.com/garrytan/gbrain-evals.git /tmp/gbrain-evals
cd /tmp/gbrain-evals
git checkout b8cf8ad

# 3. (If gbrain-evals depends on a local gbrain checkout, also clone gbrain
#    at the version listed in gbrain-evals/package.json and `bun link` it.
#    Skip if upstream's package.json points at a published gbrain release.)
bun install

# 4. Export your OpenAI key (the upstream runner uses it for embeddings +
#    LLM-based query derivation).
export OPENAI_API_KEY=sk-...

# 5. Run the capture script. The first arg is the output dir — point it
#    at the cairn fixture directory.
bun run /path/to/cairn/scripts/capture-brainbench-baseline.ts \
  /path/to/cairn/fixtures/v0/brainbench-world-v1/

# 6. Inspect the output.
jq '. | length' /path/to/cairn/fixtures/v0/brainbench-world-v1/queries.json
jq '.adapters | keys' /path/to/cairn/fixtures/v0/brainbench-world-v1/upstream-baseline.json

# 7. Commit the two JSON files.
cd /path/to/cairn
git add fixtures/v0/brainbench-world-v1/queries.json \
        fixtures/v0/brainbench-world-v1/upstream-baseline.json
git commit -m "fixture(bench): capture upstream queries + baseline (gbrain-evals b8cf8ad)"
```

## Cost / time

The capture costs roughly **$0.50 in OpenAI fees** (query derivation +
the upstream `vector` and `vector-grep-rrf-fusion` adapters call OpenAI
embeddings) and takes **about 3 minutes** on an M-series laptop.

## Adapting to API drift

Upstream's `multiAdapterRun` signature and adapter names shift between
releases. The capture script (`scripts/capture-brainbench-baseline.ts`) is
a template, not a guaranteed-runnable artifact. If upstream renames an
adapter, drops a metric, or moves the runner module, edit the script's
import + the field-mapping inside the `for (const adapter of …)` loop to
match. The *output shapes* the cairn side expects are documented at the
top of the script and stay fixed.
