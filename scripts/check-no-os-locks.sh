#!/usr/bin/env bash
# Brief §5.6 forbids OS advisory locks (flock(2), fcntl(F_SETLK), etc.).
# Locks live as rows in .cairn/cairn.db, governed by SQLite write
# serialization + the trigger-enforced semantics of migration 0004.
#
# This gate prevents accidental introduction of OS advisory primitives.
# Issue: #254 (brief §5.6, line 1815).
#
# Regex matches:
#   - flock\( : flock() function call (avoids doc comments with just "flock")
#   - fcntl::lock : fcntl::lock Rust path (avoids doc text "fcntl(F_SETLK)")
# Grep with --include='*.rs' to target only source code, not comments in code.
# Doc comments (///) are still matched, but we exclude them via -v for common patterns.
set -euo pipefail

# Search for actual lock calls in Rust source.
# Filter out lines that are pure documentation comments explaining why locks are forbidden.
if grep -RnE 'flock\(|fcntl::lock' crates/ --include='*.rs' | grep -v '//.*brief\|//.*rejects' ; then
  echo
  echo "ERROR: OS advisory lock primitive found above." >&2
  echo "Use cairn_store_sqlite::locks::acquire_exclusive (§5.6 lock table)." >&2
  exit 1
fi

echo "ok: no OS advisory lock primitives in crates/"
