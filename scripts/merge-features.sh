#!/usr/bin/env bash
# Merge a phase's features (e.g. features-p2.json) into features.json. Refuses duplicates and unknown blockedBy ids.
#   merge-features.sh features-pN.json                  append at the end
#   merge-features.sh features-pN.json --before <id>    insert just before feature <id> (pick order is file order)
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="${1:-}"
[ -f "$src" ] || { echo "usage: $0 features-pN.json [--before <id>]"; exit 2; }
before=""
if [ "${2:-}" = "--before" ]; then
  before="${3:-}"
  [ -n "$before" ] || { echo "usage: $0 features-pN.json [--before <id>]"; exit 2; }
fi
cd "$ROOT"
dups="$(jq -r --slurpfile n "$src" '[.features[].id] as $have | $n[0].features[].id | select(. as $i | $have | index($i))' features.json)"
[ -n "$dups" ] && { echo "REFUSED: ids already present: $dups"; exit 1; }
if [ -n "$before" ]; then
  [ "$(jq -r --arg b "$before" '[.features[].id] | index($b) // "none"' features.json)" = "none" ] && { echo "REFUSED: no feature $before"; exit 1; }
fi
tmp="$(mktemp)"
jq --slurpfile n "$src" --arg b "$before" '
  if $b == "" then .features += $n[0].features
  else ([.features[].id] | index($b)) as $i | .features = .features[:$i] + $n[0].features + .features[$i:]
  end' features.json > "$tmp"
missing="$(jq -r '[.features[].id] as $ids | .features[] | .blockedBy[] | select(. as $b | $ids | index($b) | not)' "$tmp" | sort -u)"
[ -n "$missing" ] && { echo "REFUSED: unknown blockedBy ids: $missing"; rm -f "$tmp"; exit 1; }
mv "$tmp" features.json
echo "merged $(jq '.features | length' "$src") features from $src${before:+ before $before}"
