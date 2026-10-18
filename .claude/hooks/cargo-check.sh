#!/usr/bin/env bash
# PostToolUse on Edit/Write of a .rs file: clippy the touched crate and feed failures back.
INPUT="$(cat)"
# Native Windows: jq.exe ends lines with CRLF, and tool paths arrive as E:\...; normalize both.
jq() { command jq "$@" | tr -d '\r'; }
posix_path() { local p="${1//\\//}"; case "$p" in [A-Za-z]:/*) p="/$(printf '%s' "${p:0:1}" | tr 'A-Z' 'a-z')${p:2}" ;; esac; printf '%s' "$p"; }
FILE="$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')"
FILE="$(posix_path "$FILE")"
case "$FILE" in *.rs) ;; *) exit 0 ;; esac
PROJ="${CLAUDE_PROJECT_DIR:-$(echo "$INPUT" | jq -r '.cwd // "."')}"
PROJ="$(posix_path "$PROJ")"
cd "$PROJ" || exit 0
[ -f Cargo.toml ] || exit 0
case "$FILE" in /*) ABS="$FILE" ;; *) ABS="$(posix_path "$(echo "$INPUT" | jq -r '.cwd // "."')")/$FILE" ;; esac
REL="${ABS#"$PROJ"/}"
PKG=""
case "$REL" in
  crates/*/*) PKG="$(echo "$REL" | cut -d/ -f2)" ;;
  tests/e2e/*) PKG="rmc_e2e" ;;
esac
[ -z "$PKG" ] && exit 0
OUT="$(cargo clippy -q -p "$PKG" --all-targets -- -D warnings 2>&1 | tail -c 6000)"
if echo "$OUT" | grep -qE '^(error|warning)'; then
  jq -nc --arg r "clippy failed for $PKG after editing $REL:
$OUT" '{decision:"block", reason:$r}'
fi
exit 0
