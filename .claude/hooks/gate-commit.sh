#!/usr/bin/env bash
# PreToolUse on Bash: when the command contains `git commit`, block it unless fmt, clippy and tests are green.
# rmc_e2e is left out of `cargo test`: all its scenarios are #[ignore] (run by scripts/e2e.sh), so the
# workspace test run would only link 20+ Bevy binaries for nothing; clippy --all-targets still compiles them.
INPUT="$(cat)"
# Native Windows: jq.exe ends lines with CRLF, and tool paths arrive as E:\...; normalize both.
jq() { command jq "$@" | tr -d '\r'; }
posix_path() { local p="${1//\\//}"; case "$p" in [A-Za-z]:/*) p="/$(printf '%s' "${p:0:1}" | tr 'A-Z' 'a-z')${p:2}" ;; esac; printf '%s' "$p"; }
CMD="$(echo "$INPUT" | jq -r '.tool_input.command // empty')"
echo "$CMD" | grep -qE '(^|[;&|[:space:]])git([[:space:]]+-C[[:space:]]+\S+)?[[:space:]]+commit([[:space:]]|$)' || exit 0
echo "$CMD" | grep -qE -- '--no-verify|[[:space:]]-n([[:space:]]|$)' && { echo "COMMIT BLOCKED: --no-verify is not allowed" >&2; exit 2; }
PROJ="${CLAUDE_PROJECT_DIR:-$(echo "$INPUT" | jq -r '.cwd // "."')}"
cd "$PROJ" || { echo "gate: cannot cd" >&2; exit 2; }
[ -f Cargo.toml ] || exit 0
LOG=/tmp/rmc-gate.log
: > "$LOG"
fail() { echo "COMMIT BLOCKED: $1" >&2; tail -n 50 "$LOG" >&2; exit 2; }

cargo fmt --all --check >>"$LOG" 2>&1 || fail "cargo fmt --all --check failed (run cargo fmt --all)"
cargo clippy --workspace --all-targets -q -- -D warnings >>"$LOG" 2>&1 || fail "clippy failed"
PROPTEST_CASES="${PROPTEST_CASES:-64}" cargo test --workspace --exclude rmc_e2e -q >>"$LOG" 2>&1 || fail "cargo test --workspace --exclude rmc_e2e failed"
[ -x scripts/check-no-bevy.sh ] && { scripts/check-no-bevy.sh >>"$LOG" 2>&1 || fail "a core crate depends on bevy"; }
exit 0
