#!/usr/bin/env bash
# Stop hook: a session may end only with a clean working tree (everything committed or stashed).
# If Rust changes are uncommitted and tests fail, the reason includes the failure tail.
INPUT="$(cat)"
[ "$(echo "$INPUT" | jq -r '.stop_hook_active // false')" = "true" ] && exit 0
PROJ="${CLAUDE_PROJECT_DIR:-$(echo "$INPUT" | jq -r '.cwd // "."')}"
cd "$PROJ" || exit 0
[ -f Cargo.toml ] || exit 0
dirty="$(git status --porcelain 2>/dev/null | grep -vE '^\?\? (logs/|target/)' | head -n 30)"
[ -z "$dirty" ] && exit 0
extra=""
if echo "$dirty" | grep -qE '\.rs$|Cargo\.toml$'; then
  if ! PROPTEST_CASES="${PROPTEST_CASES:-64}" cargo test --workspace -q >/tmp/rmc-stop.log 2>&1; then
    extra="Tests currently fail:
$(tail -n 30 /tmp/rmc-stop.log)"
  fi
fi
jq -nc --arg d "$dirty" --arg e "$extra" '{decision:"block", reason:("Working tree is not clean. Finish the feature and commit (the commit gate must pass), or git stash with a message naming the feature and record it in PROGRESS.md. Dirty files:\n" + $d + "\n" + $e)}'
exit 0
