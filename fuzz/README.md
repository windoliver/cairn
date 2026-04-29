# cairn-fuzz

Fuzz harnesses for cairn-core. Out-of-workspace package (libFuzzer
needs nightly + sanitizer flags incompatible with the pinned MSRV).

## Targets

- `squash` — drives the squash pipeline through `fuzz_entrypoint`,
  asserting the load-bearing invariants on every input (fits budget,
  valid UTF-8, no escape leaks, bounded drop counters).

## Run

```sh
# One-time install (per-system).
cargo install cargo-fuzz

# Quick smoke (60 s).
cd fuzz
cargo +nightly fuzz run squash -- -max_total_time=60

# Overnight (10 min budget per run, restart on findings).
cargo +nightly fuzz run squash -- -max_total_time=600

# Reproduce a finding from the corpus.
cargo +nightly fuzz run squash fuzz/artifacts/squash/<crash-input>
```

## Corpus

Seed `fuzz/corpus/squash/` with real terminal payloads to bias the
fuzzer toward realistic adversarial bytes. Good seeds:

```sh
mkdir -p fuzz/corpus/squash
cp ../fixtures/v0/squash/*.txt fuzz/corpus/squash/
cp ../fixtures/v0/squash/*.bin fuzz/corpus/squash/
```

## CI

Not wired into CI — fuzzing is a soak workload, not a per-commit
gate. Run periodically (e.g. weekly) and triage findings as bugs.
