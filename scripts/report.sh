#!/usr/bin/env bash
# Write docs/REPORT.md (SAD §16.6): features passed/failed with test names,
# PROGRESS.md decisions and blockers, perf numbers, screenshots inline, and the
# exact commands to play.
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
out=docs/REPORT.md
{
  echo "# Rust Mining Co — overnight build report"
  echo
  echo "Generated $(date -u +%FT%TZ) at commit $(git rev-parse --short HEAD 2>/dev/null)"
  echo
  total=$(jq '.features | length' features.json)
  passed=$(jq '[.features[] | select(.passes)] | length' features.json)
  skipped=$(jq '[.features[] | select((.passes | not) and .skipped)] | length' features.json)
  echo "**$passed / $total features passing, $skipped skipped, $((total - passed - skipped)) not passing.**"
  # Phase 1 ids are F0xx (docs/SAD.md), Phase 2 ids are F1xx (docs/SAD-P2.md).
  for phase in 1 2; do
    jq -r --arg p "F$((phase - 1))" '[.features[] | select(.id | startswith($p))]
      | "- Phase '"$phase"': \([.[] | select(.passes)] | length) / \(length) passing, \([.[] | select((.passes | not) and .skipped)] | length) skipped"' features.json
  done
  echo
  echo "## How to play (Windows)"
  echo
  echo '```'
  # Drop any user@ from the remote URL; the reader clones with their own GitHub credentials (the repo is private).
  repo=$(git remote get-url origin 2>/dev/null | sed -E 's#^(https?://)[^@/]*@#\1#')
  echo "git clone ${repo:-<repo>} C:\\src\\rust-mining-co   (or download the CI \"windows-release\" artifact)"
  echo "cd C:\\src\\rust-mining-co"
  echo "cargo run --release -p rmc_app -- --seed 42"
  echo '```'
  echo
  echo "The \`windows-release\` artifact from the latest CI run on main holds \`rust-mining-co.exe\`, \`assets/\` and \`CONTROLS.md\`; run the exe from that folder. Controls: see [docs/CONTROLS.md](CONTROLS.md). Playtester reports: [docs/PLAYTEST.md](PLAYTEST.md)."
  echo
  echo "## Milestones"
  echo
  echo "| milestone | passing | skipped | not passing |"
  echo "|---|---|---|---|"
  jq -r '.features | group_by(.milestone) | sort_by(.[0].id) | .[]
    | "| \(.[0].milestone) | \([.[] | select(.passes)] | length) / \(length) | \([.[] | select((.passes | not) and .skipped)] | length) | \([.[] | select((.passes | not) and (.skipped | not)) | .id] | join(", ")) |"' features.json
  echo
  echo "## Feature status"
  echo
  echo "| id | milestone | title | status | tests |"
  echo "|---|---|---|---|---|"
  jq -r '.features[]
    | "| \(.id) | \(.milestone) | \(.title | gsub("\\|"; "\\|")) | \(if .passes then "passed" elif .skipped then "skipped: \(.skipReason // .skippedReason // "see PROGRESS.md")" else "**no**" end) | \([.tests[] | "`\(.)`"] | join("<br>")) |"' features.json
  echo
  echo "## Screenshots"
  echo
  for f in docs/screens/*.png; do [ -f "$f" ] && echo "![${f##*/}](screens/${f##*/})"; done
  echo
  # perf.md's own title becomes this section's heading; its subsections nest under it.
  if [ -f docs/perf.md ]; then sed -e 's/^#/##/' docs/perf.md; else printf '## Performance\n\n(docs/perf.md not written)\n'; fi
  echo
  echo "## Decisions and blockers (from PROGRESS.md)"
  echo
  # Each session block's full Decisions section, and its Blockers unless they
  # start with "none", under that block's heading. A section whose first line is
  # empty (bullets follow) gets its label when its first bullet arrives.
  awk '
    # out() tracks the last line so a label never lands on a list item and blank lines never double.
    function out(l) { if (l ~ /^[[:space:]]*$/ && blank) return; print l; blank = (l ~ /^[[:space:]]*$/) }
    function label(l) { if (!shown) { if (any) out(""); out("### " head); shown = 1; any = 1 }; out(""); out(l); out("") }
    /^## / { head = substr($0, 4); shown = 0; mode = ""; next }
    /^(Done|Tests added|Next):/ { mode = ""; next }
    /^(Decisions|Blockers):/ {
      mode = substr($0, 1, index($0, ":") - 1); rest = substr($0, length(mode) + 2)
      if (mode == "Blockers" && rest ~ /^[[:space:]]*(none|-[[:space:]]*$)/) { mode = ""; next }
      if (rest ~ /^[[:space:]]*$/) { pending = 1; next }
      label("**" mode ":**" rest); pending = 0; next
    }
    mode == "" { next }
    pending && /^[[:space:]]*$/ { next }
    pending { label("**" mode ":**"); pending = 0 }
    { out($0) }
  ' PROGRESS.md || echo "(none recorded)"
  echo
  echo "## Commits"
  echo
  echo '```'
  git log --oneline -60
  echo '```'
} > "$out"
echo "wrote $out"
