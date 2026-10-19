#!/usr/bin/env bash
# Run E2E scenarios and collect their screenshots into docs/screens/ for review.
#   screens.sh [scenario]
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
mkdir -p docs/screens
# Drop old shots first, so only this run's frames are copied. For one scenario,
# also drop its committed shots, whose frame numbers may have changed.
if [ -n "${1:-}" ]; then
  rm -f target/e2e/"$1"-*.png docs/screens/"$1"-*.png
else
  rm -f target/e2e/*.png
fi
"$ROOT/scripts/e2e.sh" "${1:-}" || true
cp -f target/e2e/"${1:+$1-}"*.png docs/screens/ 2>/dev/null || true
ls -1 docs/screens/ | sed 's/^/screen: /'
