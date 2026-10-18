#!/usr/bin/env bash
# Append a phase's features (e.g. features-p2.json) into features.json. Refuses duplicates and unknown blockedBy ids.
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="${1:-}"
[ -f "$src" ] || { echo "usage: $0 features-pN.json"; exit 2; }
cd "$ROOT"
dups="$(jq -r --slurpfile n "$src" '[.features[].id] as $have | $n[0].features[].id | select(. as $i | $have | index($i))' features.json)"
[ -n "$dups" ] && { echo "REFUSED: ids already present: $dups"; exit 1; }
tmp="$(mktemp)"
jq --slurpfile n "$src" '.features += $n[0].features' features.json > "$tmp"
missing="$(jq -r '[.features[].id] as $ids | .features[] | .blockedBy[] | select(. as $b | $ids | index($b) | not)' "$tmp" | sort -u)"
[ -n "$missing" ] && { echo "REFUSED: unknown blockedBy ids: $missing"; rm -f "$tmp"; exit 1; }
mv "$tmp" features.json
echo "merged $(jq '.features | length' "$src") features from $src"
