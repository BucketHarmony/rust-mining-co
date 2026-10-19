#!/usr/bin/env bash
# Run end-to-end scenarios headlessly on a software rasterizer.
#   e2e.sh            run all scenarios
#   e2e.sh <name>     run one scenario (the #[test] fn named <name> in tests/e2e)
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export RMC_E2E=1
export RUST_BACKTRACE=1
export RMC_E2E_OUT="$ROOT/target/e2e"
mkdir -p "$RMC_E2E_OUT"

# Pick a software adapter unless the caller already chose one.
if [ -z "${WGPU_ADAPTER_NAME:-}" ] && [ -z "${WGPU_FORCE_FALLBACK_ADAPTER:-}" ]; then
  case "$(uname -s)" in
    Linux*)  export WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe ;;
    MINGW*|MSYS*|CYGWIN*|Windows*) export WGPU_BACKEND=dx12 WGPU_FORCE_FALLBACK_ADAPTER=1 ;;
    Darwin*) : ;;
  esac
fi

filter="${1:-}"
# One scenario at a time: each creates its own render device.
log="$(mktemp)"
cargo test -q -p rmc_e2e --test '*' -- --test-threads=1 --include-ignored ${filter:+--exact "$filter"} 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
# A filter that matches no scenario runs 0 tests and exits 0; that must not count as a pass.
ran="$(grep -oE '[0-9]+ passed; [0-9]+ failed' "$log" | awk '{s += $1 + $3} END {print s + 0}')"
rm -f "$log"
[ "$rc" -ne 0 ] && exit "$rc"
[ "$ran" -eq 0 ] && { echo "E2E FAILED: no scenario ran${filter:+ (no test named $filter)}"; exit 1; }
exit 0
