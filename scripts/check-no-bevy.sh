#!/usr/bin/env bash
# Fail if any Bevy crate appears in the normal dependency tree of the core crates (SAD §4).
# A missing crate or a failing `cargo tree` is a failure, never a silent pass.
# Exception: rmc_eco (Phase 2) is skipped until F100 creates it.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
rc=0
for crate in rmc_world rmc_worldgen rmc_protocol rmc_host rmc_eco; do
  if [ ! -d "crates/$crate" ]; then
    if [ "$crate" = rmc_eco ]; then echo "SKIP: rmc_eco not created yet"; continue; fi
    echo "FAIL: crates/$crate does not exist"; rc=1; continue
  fi
  if ! tree="$(cargo tree -p "$crate" -e normal --prefix none 2>&1)"; then
    echo "FAIL: cargo tree -p $crate failed:"; printf '%s\n' "$tree" | tail -5; rc=1; continue
  fi
  offenders="$(printf '%s\n' "$tree" | tr -d '\r' | grep -E '^bevy' | sed 's/ (\*)$//' | sort -u)"
  if [ -n "$offenders" ]; then
    echo "FAIL: $crate depends on a bevy crate:"; printf '%s\n' "$offenders" | head -10 | sed 's/^/  /'; rc=1
  else
    echo "OK: $crate is Bevy-free"
  fi
done
exit $rc
