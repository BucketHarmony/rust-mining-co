#!/usr/bin/env bash
# PreToolUse on Edit/Write/MultiEdit: deny edits to protected files and edits that remove tests.
# Hooks run before permission-mode checks, so this holds even under bypassPermissions.
INPUT="$(cat)"
FILE="$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')"
[ -z "$FILE" ] && exit 0
PROJ="${CLAUDE_PROJECT_DIR:-$(echo "$INPUT" | jq -r '.cwd // "."')}"
PROJ="$(cd "$PROJ" 2>/dev/null && pwd || echo "$PROJ")"
case "$FILE" in /*) ABS="$FILE" ;; *) ABS="$(echo "$INPUT" | jq -r '.cwd // "."')/$FILE" ;; esac
REL="${ABS#"$PROJ"/}"

deny() { jq -nc --arg r "$1" '{hookSpecificOutput:{hookEventName:"PreToolUse",permissionDecision:"deny",permissionDecisionReason:$r}}'; exit 0; }

case "$REL" in
  features.json|docs/SAD.md|.claude/*|PROMPT.md|CLAUDE.md|scripts/verify-features.sh|scripts/pass-feature.sh|scripts/skip-feature.sh)
    deny "Protected file: $REL. features.json changes only via scripts/pass-feature.sh or scripts/skip-feature.sh. Record anything else under Decisions in PROGRESS.md." ;;
esac

case "$REL" in *.rs) ;; *) exit 0 ;; esac
TOOL="$(echo "$INPUT" | jq -r '.tool_name // empty')"
count_tests() { printf '%s' "$1" | grep -c '#\[test\]'; }
count_ignores() { printf '%s' "$1" | grep -c '#\[ignore'; }

old_tests=0; new_tests=0; old_ign=0; new_ign=0
case "$TOOL" in
  Write)
    NEW="$(echo "$INPUT" | jq -r '.tool_input.content // empty')"
    if [ -f "$ABS" ]; then OLD="$(cat "$ABS")"; else OLD=""; fi
    old_tests=$(count_tests "$OLD"); new_tests=$(count_tests "$NEW")
    old_ign=$(count_ignores "$OLD"); new_ign=$(count_ignores "$NEW") ;;
  Edit)
    OLD="$(echo "$INPUT" | jq -r '.tool_input.old_string // empty')"
    NEW="$(echo "$INPUT" | jq -r '.tool_input.new_string // empty')"
    old_tests=$(count_tests "$OLD"); new_tests=$(count_tests "$NEW")
    old_ign=$(count_ignores "$OLD"); new_ign=$(count_ignores "$NEW") ;;
  MultiEdit)
    OLD="$(echo "$INPUT" | jq -r '[.tool_input.edits[]?.old_string] | join("\n")')"
    NEW="$(echo "$INPUT" | jq -r '[.tool_input.edits[]?.new_string] | join("\n")')"
    old_tests=$(count_tests "$OLD"); new_tests=$(count_tests "$NEW")
    old_ign=$(count_ignores "$OLD"); new_ign=$(count_ignores "$NEW") ;;
esac

if [ "$new_tests" -lt "$old_tests" ]; then
  deny "This edit removes a #[test] from $REL. Tests may not be deleted. If a test is wrong, fix its body in a test-fix: commit and explain in PROGRESS.md."
fi
if [ "$new_ign" -gt "$old_ign" ]; then
  case "$REL" in tests/e2e/*) ;; *) deny "Adding #[ignore] outside tests/e2e is not allowed." ;; esac
fi
exit 0
