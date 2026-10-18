#!/usr/bin/env bash
# Mark a feature as passing, but only after its full test list passes.
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FEATURES="$ROOT/features.json"
id="${1:-}"
[ -z "$id" ] && { echo "usage: $0 <feature-id>"; exit 2; }

# blockedBy must all pass first
blocked="$(jq -r --arg id "$id" '
  . as $r | .features[] | select(.id==$id) | .blockedBy[]
  | . as $b | ($r.features[] | select(.id==$b) | select(.passes==false and (.skipped // false)==false) | .id)' "$FEATURES")"
if [ -n "$blocked" ]; then echo "REFUSED: $id is blocked by unpassed: $blocked"; exit 1; fi

if ! "$ROOT/scripts/verify-features.sh" "$id"; then
  echo "REFUSED: tests for $id are not all passing. features.json unchanged."
  exit 1
fi

tmp="$(mktemp)"
jq --arg id "$id" --arg ts "$(date -u +%FT%TZ)" '
  .features |= map(if .id==$id then .passes=true | .passedAt=$ts else . end)' "$FEATURES" > "$tmp" \
  && mv "$tmp" "$FEATURES"
echo "PASSED $id"
