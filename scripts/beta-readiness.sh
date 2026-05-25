#!/usr/bin/env bash
# scripts/beta-readiness.sh — issue #138 beta readiness gate runner.
#
# Wraps every automatable gate from CLAUDE.md §8 plus the
# docs/site/src/maintainers/beta-readiness.md runbook. Exits 0 only when
# every required gate passes; manual gates (9-14) are listed at the end of
# the run and never claimed as passed.
#
# Modes:
#   --quick (default): fmt, clippy, check, nextest, doc tests,
#     scripts/check-*.sh, codegen --check, docgen --check, deny, machete.
#   --full: --quick + cargo bench all + coherence beta gate + mdbook +
#     rustdoc + audit + install-smoke + cargo package + leaf publish dry-run.
#
# Env passthrough: CAIRN_BIN, CARGO_TARGET_DIR, RUST_LOG.
#
# POSIX-y bash; targets macOS default /bin/bash 3.2 — no associative arrays,
# no `mapfile`. Style follows scripts/install-smoke.sh.

set -euo pipefail

MODE="quick"
case "${1:-}" in
  --quick) MODE="quick" ;;
  --full) MODE="full" ;;
  "") ;;
  -h|--help)
    cat <<'EOF'
Usage: beta-readiness.sh [--quick|--full]

  --quick   (default) ~3 min: fmt, clippy, check, nextest, doctests,
            core-boundary scripts, codegen/docgen --check, deny, machete.
  --full    ~15 min: --quick + bench + coherence gate + mdbook + rustdoc +
            audit + install-smoke + cargo package + leaf publish dry-run.

Honors CAIRN_BIN, CARGO_TARGET_DIR, RUST_LOG.

Manual gates (9-14 in docs/site/src/maintainers/beta-readiness.md) are
listed after the automated run and never claimed as passed.
EOF
    exit 0
    ;;
  *)
    echo "fail: unknown arg '$1'. Try --help." >&2
    exit 2
    ;;
esac

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OK=0
FAIL=0
SKIP=0
FIRST_FAIL=""

# `gate <name> <command>` runs the command, logs ok/fail, increments counters.
# A failure short-circuits later gates via set -e; we capture the first
# failure name to print at the end before exiting.
gate() {
  name="$1"
  shift
  printf 'run: %s\n' "$name"
  if "$@" >/tmp/cairn-beta-readiness.$$.log 2>&1; then
    OK=$((OK + 1))
    printf 'ok: %s\n' "$name"
  else
    FAIL=$((FAIL + 1))
    if [ -z "$FIRST_FAIL" ]; then
      FIRST_FAIL="$name"
    fi
    printf 'fail: %s\n' "$name" >&2
    sed 's/^/    /' /tmp/cairn-beta-readiness.$$.log >&2
    rm -f /tmp/cairn-beta-readiness.$$.log
    print_summary
    exit 1
  fi
  rm -f /tmp/cairn-beta-readiness.$$.log
}

# `gate_optional <name> <required-binary> <command>` skips when the binary is
# missing. Prints a remediation hint.
gate_optional() {
  name="$1"
  bin="$2"
  shift 2
  if ! command -v "$bin" >/dev/null 2>&1; then
    SKIP=$((SKIP + 1))
    printf 'skip: %s — install %s\n' "$name" "$bin"
    return 0
  fi
  gate "$name" "$@"
}

print_summary() {
  printf -- '---\n'
  printf 'beta-readiness: %d ok, %d fail, %d skip\n' "$OK" "$FAIL" "$SKIP"
  if [ -n "$FIRST_FAIL" ]; then
    printf 'first failure: %s\n' "$FIRST_FAIL"
  fi
  print_manual_gates
}

print_manual_gates() {
  cat <<'EOF'
manual gates remaining (see docs/site/src/maintainers/beta-readiness.md):
  - 9: capability sync (cairn status --json vs reference/capability-matrix.md)
  - 10: migration guide review (usage/migration/v0.X-to-v0.Y.md)
  - 11: known limitations (status.md vs capability matrix)
  - 12: cassette replay (cargo run -p cairn-bench -- coherence run --gate beta)
  - 13: privacy posture (forget round-trip + presidio scrub)
  - 14: release notes draft
EOF
}

# ---- Gate 1: code quality ----
gate "fmt" cargo fmt --all --check
gate "clippy" cargo clippy --workspace --all-targets --locked -- -D warnings
gate "check" cargo check --workspace --all-targets --locked
gate_optional "nextest" cargo-nextest cargo nextest run --workspace --locked --no-fail-fast
gate "doc-tests" cargo test --doc --workspace --locked

# ---- Gate 2: core boundary ----
gate "check-core-boundary" scripts/check-core-boundary.sh
gate "check-no-os-locks" scripts/check-no-os-locks.sh
gate "check-lint-readonly-sources" scripts/check-lint-readonly-sources.sh

# ---- Gate 3: generated code drift ----
gate "codegen --check" cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
gate "docgen --check" cargo run -p cairn-cli --bin cairn-docgen --locked -- --check

# ---- Gate 6 (partial — fast): supply chain that's quick ----
gate_optional "cargo deny" cargo-deny cargo deny check
gate_optional "cargo machete" cargo-machete cargo machete

if [ "$MODE" = "quick" ]; then
  print_summary
  exit 0
fi

# ---- Gate 4: eval gates (full only) ----
gate "bench all" cargo run -p cairn-bench --release --locked -- all
gate "coherence beta gate" cargo run -p cairn-bench --release --locked -- coherence run --gate beta

# ---- Gate 5: docs build (full only) ----
gate_optional "mdbook" mdbook mdbook build docs/site
gate "rustdoc" env RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

# ---- Gate 6 (rest, slower) ----
gate_optional "cargo audit" cargo-audit cargo audit --deny warnings

# ---- Gate 7: install smoke (full only) ----
# Builds the release binary if not already present, then runs the smoke
# script against it.
gate "release build" cargo build --release --locked -p cairn-cli --bin cairn
gate "install smoke" env CAIRN_BIN="$REPO_ROOT/target/release/cairn" scripts/install-smoke.sh

# ---- Gate 8: package dry-run (full only) ----
gate "cargo package" cargo package --workspace --no-verify --locked --allow-dirty
gate "publish dry-run (cairn-idl)" cargo publish --dry-run --locked --allow-dirty -p cairn-idl
gate "publish dry-run (cairn-core)" cargo publish --dry-run --locked --allow-dirty -p cairn-core

print_summary
exit 0
