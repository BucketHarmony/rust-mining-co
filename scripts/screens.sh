#!/usr/bin/env bash
# Run E2E scenarios and collect their screenshots into docs/screens/ for review.
#   screens.sh [scenario]
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
mkdir -p docs/screens
"$ROOT/scripts/e2e.sh" "${1:-}" || true
cp -f target/e2e/*.png docs/screens/ 2>/dev/null || true
ls -1 docs/screens/ | sed 's/^/screen: /'
