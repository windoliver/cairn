#!/usr/bin/env bash
# scripts/install-smoke.sh — issue #100 install smoke for the cairn binary.
#
# Exercises the P0 verb set against a freshly bootstrapped temp vault. Honoured
# by both `cargo install` and `brew install` paths: runs against the binary at
# $CAIRN_BIN (default: `cairn` from PATH).
#
# Local embeddings are turned off via a pre-seeded `.cairn/config.yaml` so the
# smoke does not depend on the one-time embedding-model fetch (~25 MB). That
# path is covered separately by `crates/cairn-cli/tests/bootstrap.rs`.
#
# Verb order: bootstrap → status → ingest → search → retrieve → lint → forget.
# Each step that succeeds prints `ok: <verb>`. On the first failure the script
# exits 1 with `fail: <verb> — <reason>` on stderr.
#
# POSIX-y bash; targets macOS default /bin/bash 3.2 — no associative arrays,
# no `mapfile`, no `&>>`. Grep + sed only — `jq` is NOT a dependency.

set -euo pipefail

CAIRN_BIN="${CAIRN_BIN:-cairn}"

# Resolve the binary either via PATH or as a direct executable path.
if ! command -v "$CAIRN_BIN" >/dev/null 2>&1 && [ ! -x "$CAIRN_BIN" ]; then
  echo "fail: cairn binary not found at '$CAIRN_BIN'" >&2
  exit 1
fi

VAULT="$(mktemp -d -t cairn-smoke-XXXXXX)"
# shellcheck disable=SC2064  # expand $VAULT now so the trap survives unset
trap "rm -rf '$VAULT'" EXIT INT TERM
cd "$VAULT"

# Pre-seed the YAML config so `cairn bootstrap` skips the embedding-model
# fetch. `bootstrap` calls `write_once` for `.cairn/config.yaml` and respects
# any pre-existing file (see crates/cairn-cli/src/vault/bootstrap.rs).
mkdir -p .cairn
cat >.cairn/config.yaml <<'YAML'
search:
  local_embeddings: false
YAML

# ── helpers ────────────────────────────────────────────────────────────────
# fail <verb> <reason> — print error and exit 1.
fail() {
  echo "fail: $1 — $2" >&2
  exit 1
}

# Assert that a JSON blob contains "status":"committed" (whitespace-tolerant).
# Returns 0 if found, 1 otherwise. Uses grep -E — no jq.
assert_committed() {
  printf '%s' "$1" | grep -Eq '"status"[[:space:]]*:[[:space:]]*"committed"'
}

# Extract a string field's value from a JSON blob with a primitive grep+sed.
# Only safe for short, single-line-encoded string values like record_id —
# adequate for the smoke. $1=field name, $2=json blob.
extract_string() {
  printf '%s' "$2" \
    | tr -d '\n' \
    | grep -Eo "\"$1\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" \
    | head -n1 \
    | sed -E "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"([^\"]*)\".*/\1/"
}

# ── 1. bootstrap ───────────────────────────────────────────────────────────
# Returns a BootstrapReceipt (NOT an envelope) — see
# crates/cairn-cli/src/main.rs::run_bootstrap. Assert by `vault_id`.
boot_out="$("$CAIRN_BIN" bootstrap --vault-path . --json 2>"$VAULT/bootstrap.err")" \
  || fail "bootstrap" "exit $? — $(cat "$VAULT/bootstrap.err")"
printf '%s' "$boot_out" | grep -Eq '"vault_id"' \
  || fail "bootstrap" "missing vault_id in receipt: $boot_out"
echo "ok: bootstrap"

# ── 2. status ──────────────────────────────────────────────────────────────
# `status` returns a capabilities snapshot, NOT a committed envelope —
# see crates/cairn-cli/tests/envelope_tests.rs::status_in_bound_vault_*.
status_out="$("$CAIRN_BIN" status --json 2>"$VAULT/status.err")" \
  || fail "status" "exit $? — $(cat "$VAULT/status.err")"
printf '%s' "$status_out" | grep -Eq '"capabilities"' \
  || fail "status" "missing capabilities array: $status_out"
echo "ok: status"

# ── 3. ingest ──────────────────────────────────────────────────────────────
ingest_out="$("$CAIRN_BIN" ingest --kind user --body "install smoke probe" --json \
  2>"$VAULT/ingest.err")" \
  || fail "ingest" "exit $? — $(cat "$VAULT/ingest.err")"
assert_committed "$ingest_out" \
  || fail "ingest" "envelope not committed: $ingest_out"
RECORD_ID="$(extract_string record_id "$ingest_out")"
[ -n "$RECORD_ID" ] || fail "ingest" "no record_id in envelope: $ingest_out"
echo "ok: ingest"

# ── 4. search ──────────────────────────────────────────────────────────────
# `search` takes the query positionally — see
# crates/cairn-cli/tests/envelope_tests.rs and crates/cairn-cli/tests/search_explain.rs.
search_out="$("$CAIRN_BIN" search "install smoke" --mode keyword --json \
  2>"$VAULT/search.err")" \
  || fail "search" "exit $? — $(cat "$VAULT/search.err")"
assert_committed "$search_out" \
  || fail "search" "envelope not committed: $search_out"
echo "ok: search"

# ── 5. retrieve ────────────────────────────────────────────────────────────
# `retrieve` takes the record id positionally — see
# crates/cairn-cli/tests/envelope_tests.rs::retrieve_returns_committed_envelope.
retrieve_out="$("$CAIRN_BIN" retrieve "$RECORD_ID" --json \
  2>"$VAULT/retrieve.err")" \
  || fail "retrieve" "exit $? — $(cat "$VAULT/retrieve.err")"
assert_committed "$retrieve_out" \
  || fail "retrieve" "envelope not committed: $retrieve_out"
echo "ok: retrieve"

# ── 6. lint ────────────────────────────────────────────────────────────────
# `lint` exits 1 when findings exist but still emits a committed envelope —
# see crates/cairn-cli/tests/lint_cli.rs. Tolerate exit codes 0 and 1, and
# assert by envelope shape.
set +e
lint_out="$("$CAIRN_BIN" lint --json 2>"$VAULT/lint.err")"
lint_rc=$?
set -e
case "$lint_rc" in
  0|1) ;;
  *) fail "lint" "exit $lint_rc — $(cat "$VAULT/lint.err")" ;;
esac
assert_committed "$lint_out" \
  || fail "lint" "envelope not committed: $lint_out"
echo "ok: lint"

# ── 7. forget ──────────────────────────────────────────────────────────────
# `forget --record <id>` (the flag is `--record`, NOT `--record-id`) —
# see crates/cairn-cli/tests/envelope_tests.rs::forget_record_returns_committed_envelope.
forget_out="$("$CAIRN_BIN" forget --record "$RECORD_ID" --json \
  2>"$VAULT/forget.err")" \
  || fail "forget" "exit $? — $(cat "$VAULT/forget.err")"
assert_committed "$forget_out" \
  || fail "forget" "envelope not committed: $forget_out"
echo "ok: forget"

echo "smoke: all verbs passed"
