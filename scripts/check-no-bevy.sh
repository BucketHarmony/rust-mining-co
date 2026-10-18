#!/usr/bin/env bash
# Fail if any Bevy crate appears in the normal dependency tree of the core crates.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
rc=0
for crate in rmc_world rmc_worldgen rmc_protocol rmc_host; do
  [ -d "crates/$crate" ] || continue
  if cargo tree -p "$crate" -e normal --prefix none 2>/dev/null | grep -qE '^bevy'; then
    echo "FAIL: $crate depends on a bevy crate"; rc=1
  else
    echo "OK: $crate is Bevy-free"
  fi
done
exit $rc
