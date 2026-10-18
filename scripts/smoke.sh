#!/usr/bin/env bash
# Fast health check at the start of every session: build, unit tests, one quick E2E if the testkit exists.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
[ -f Cargo.toml ] || { echo "SMOKE_SKIP: no workspace yet (F001 creates it)"; exit 0; }
set -e
cargo build --workspace --all-targets -q
cargo test --workspace -q
"$ROOT/scripts/check-no-bevy.sh"
if [ -d "$ROOT/crates/rmc_testkit/src" ] && grep -rq "fn boot_renders" tests/e2e/tests 2>/dev/null; then
  "$ROOT/scripts/e2e.sh" boot_renders
fi
echo SMOKE_OK
