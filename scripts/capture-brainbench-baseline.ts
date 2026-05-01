// Capture upstream gbrain-evals queries + per-adapter per-query metrics
// against the pinned world-v1 corpus. Output is two JSON files cairn-bench
// consumes as `queries.json` + `upstream-baseline.json`.
//
// Run with:  bun run scripts/capture-brainbench-baseline.ts <output-dir>
//
// Pre-requisites
// --------------
//   1. Clone gbrain-evals at the pinned commit (see fixtures/v0/brainbench-world-v1/LICENSE.NOTICE).
//   2. Clone gbrain (sibling repo) at the version that gbrain-evals's package.json depends on.
//   3. `bun install` in gbrain-evals; `bun link gbrain` to point at the local clone.
//   4. `export OPENAI_API_KEY=sk-...`
//
// The actual API surface inside gbrain-evals shifts between releases. Treat
// this script as a template — adjust the imports and runner call to match
// upstream's current exports. The shapes the *cairn* side expects are
// fixed:
//
//   queries.json:
//     [ { id, query, relevant: string[], grades: { [slug]: number } } ]
//
//   upstream-baseline.json:
//     {
//       adapters: {
//         <name>: {
//           aggregate: { p_at_5, r_at_5, mrr, ndcg_at_5 },
//           per_query: [{ query_id, p_at_5, r_at_5, mrr, ndcg_at_5 }],
//         }
//       }
//     }

import * as fs from "node:fs";
import * as path from "node:path";

// Adjust this import to match upstream's current eval runner export.
// As of pin b8cf8ad057635cbb03c0f3996acb693afbcae605, the runner lives at
// `gbrain-evals/eval/runner/multi-adapter.ts` and exports `multiAdapterRun`.
import { multiAdapterRun } from "/path/to/gbrain-evals/eval/runner/multi-adapter.ts";

const outDir = process.argv[2] ?? "./out";
fs.mkdirSync(outDir, { recursive: true });

const corpusPath = process.env.GBRAIN_CORPUS_PATH ?? "/path/to/gbrain-evals/eval/data/world-v1";

const adapters = ["gbrain", "vector-grep-rrf-fusion", "grep-only", "vector"];

console.log(`running upstream eval (${adapters.length} adapters) over ${corpusPath} …`);

const result = await multiAdapterRun({
  adapters,
  corpusPath,
  n: 1,
});

// queries.json (gold + grades).
fs.writeFileSync(
  path.join(outDir, "queries.json"),
  JSON.stringify(
    result.queries.map((q: any) => ({
      id: q.id,
      query: q.text,
      relevant: q.gold,
      grades: q.grades ?? {},
    })),
    null,
    2,
  ),
);

// upstream-baseline.json (per-adapter aggregate + per-query metrics).
const baseline: any = { adapters: {} };
for (const adapter of result.adapters) {
  baseline.adapters[adapter.name] = {
    aggregate: {
      p_at_5: adapter.aggregate.precision5,
      r_at_5: adapter.aggregate.recall5,
      mrr: adapter.aggregate.mrr,
      ndcg_at_5: adapter.aggregate.ndcg5,
    },
    per_query: adapter.runs.map((r: any) => ({
      query_id: r.queryId,
      p_at_5: r.precision5,
      r_at_5: r.recall5,
      mrr: r.mrr,
      ndcg_at_5: r.ndcg5,
    })),
  };
}
fs.writeFileSync(
  path.join(outDir, "upstream-baseline.json"),
  JSON.stringify(baseline, null, 2),
);

console.log(
  `wrote ${result.queries.length} queries + ${
    Object.keys(baseline.adapters).length
  } adapter baselines to ${outDir}`,
);
