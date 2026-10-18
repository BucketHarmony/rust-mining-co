#!/usr/bin/env bash
# Write docs/REPORT.md: feature status, decisions, perf, screenshots, how to play.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
out=docs/REPORT.md
{
  echo "# Rust Mining Co — overnight build report"
  echo
  echo "Generated $(date -u +%FT%TZ) at commit $(git rev-parse --short HEAD 2>/dev/null)"
  echo
  echo "## Feature status"
  echo
  echo "| id | milestone | title | passes |"
  echo "|---|---|---|---|"
  jq -r '.features[] | "| \(.id) | \(.milestone) | \(.title) | \(if .passes then "yes" else "**no**" end) |"' features.json
  echo
  total=$(jq '.features | length' features.json); passed=$(jq '[.features[] | select(.passes)] | length' features.json)
  echo "**$passed / $total features passing.**"
  echo
  echo "## How to play (Windows)"
  echo
  echo '```'
  echo "git clone $(git remote get-url origin 2>/dev/null || echo '<repo>') C:\\src\\rust-mining-co"
  echo "cd C:\\src\\rust-mining-co"
  echo "cargo run --release -p rmc_app -- --seed 42"
  echo '```'
  echo
  echo "Or download the \`windows-release\` artifact from the latest CI run on main. Controls: see \`docs/CONTROLS.md\`."
  echo
  echo "## Screenshots"
  echo
  for f in docs/screens/*.png; do [ -f "$f" ] && echo "![${f##*/}](screens/${f##*/})"; done
  echo
  echo "## Performance"
  echo
  [ -f docs/perf.md ] && cat docs/perf.md || echo "(docs/perf.md not written)"
  echo
  echo "## Decisions and blockers (from PROGRESS.md)"
  echo
  grep -E -A3 '^(Decisions|Blockers):' PROGRESS.md 2>/dev/null | grep -vE '^(Decisions|Blockers): *(none|-)?$' || echo "(none recorded)"
  echo
  echo "## Commits"
  echo
  echo '```'
  git log --oneline -60
  echo '```'
} > "$out"
echo "wrote $out"
