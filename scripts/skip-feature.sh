#!/usr/bin/env bash
# Skip an optional feature with a recorded reason. Only features with "optional": true can be skipped.
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FEATURES="$ROOT/features.json"
id="${1:-}"; reason="${2:-}"
[ -z "$id" ] || [ -z "$reason" ] && { echo "usage: $0 <feature-id> \"<reason>\""; exit 2; }
optional="$(jq -r --arg id "$id" '.features[] | select(.id==$id) | .optional // false' "$FEATURES")"
[ "$optional" = "true" ] || { echo "REFUSED: $id is not optional"; exit 1; }
tmp="$(mktemp)"
jq --arg id "$id" --arg r "$reason" --arg ts "$(date -u +%FT%TZ)" '
  .features |= map(if .id==$id then .skipped=true | .skipReason=$r | .skippedAt=$ts else . end)' "$FEATURES" > "$tmp" && mv "$tmp" "$FEATURES"
printf '\nSkipped %s: %s\n' "$id" "$reason" >> "$ROOT/PROGRESS.md"
echo "SKIPPED $id"
