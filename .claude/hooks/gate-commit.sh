#!/usr/bin/env bash
# PreToolUse on Bash: when the command contains `git commit`, block it unless fmt, clippy and tests are green.
INPUT="$(cat)"
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
PROPTEST_CASES="${PROPTEST_CASES:-64}" cargo test --workspace -q >>"$LOG" 2>&1 || fail "cargo test --workspace failed"
[ -x scripts/check-no-bevy.sh ] && { scripts/check-no-bevy.sh >>"$LOG" 2>&1 || fail "a core crate depends on bevy"; }
exit 0
