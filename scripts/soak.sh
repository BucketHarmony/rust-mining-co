#!/usr/bin/env bash
# Long chaos run. RMC_SOAK_FRAMES controls length (default 3600 ≈ one minute of game time).
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export RMC_SOAK_FRAMES="${RMC_SOAK_FRAMES:-3600}"
export RMC_SOAK_SEED="${RMC_SOAK_SEED:-$RANDOM}"
echo "soak seed=$RMC_SOAK_SEED frames=$RMC_SOAK_FRAMES"
"$ROOT/scripts/e2e.sh" soak_chaos
