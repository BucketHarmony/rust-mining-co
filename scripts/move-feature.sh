#!/usr/bin/env bash
# Move one unpassed feature earlier or later in features.json (pick order is file order). Changes nothing else.
#   move-feature.sh <id> --before <id>
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
id="${1:-}"; before="${3:-}"
[ -n "$id" ] && [ "${2:-}" = "--before" ] && [ -n "$before" ] && [ "$id" != "$before" ] || { echo "usage: $0 <id> --before <id>"; exit 2; }
cd "$ROOT"
for x in "$id" "$before"; do
  [ "$(jq -r --arg x "$x" '[.features[].id] | index($x) // "none"' features.json)" = "none" ] && { echo "REFUSED: no feature $x"; exit 1; }
done
[ "$(jq -r --arg x "$id" '.features[] | select(.id==$x) | .passes' features.json)" = "true" ] && { echo "REFUSED: $id already passes"; exit 1; }
tmp="$(mktemp)"
jq --arg id "$id" --arg b "$before" '
  (.features[] | select(.id==$id)) as $f
  | .features |= map(select(.id != $id))
  | ([.features[].id] | index($b)) as $i
  | .features = .features[:$i] + [$f] + .features[$i:]' features.json > "$tmp"
[ "$(jq '.features | length' "$tmp")" = "$(jq '.features | length' features.json)" ] || { echo "REFUSED: count changed"; rm -f "$tmp"; exit 1; }
mv "$tmp" features.json
echo "moved $id before $before"
