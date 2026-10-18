#!/usr/bin/env bash
# Verify feature test lists from features.json. Never edits features.json.
#   verify-features.sh <id>        run every test of one feature (cargo + e2e); exit 0 only if all pass
#   verify-features.sh --quick     run the cargo: tests of every feature marked passes=true (no e2e)
#   verify-features.sh --all       run every test of every non-skipped feature; print ALL_FEATURES_PASS when
#                                  every non-skipped feature has passes=true and all its tests are green
set -u
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FEATURES="$ROOT/features.json"
cd "$ROOT"
LOG=/tmp/rmc-verify.log

# A test run counts only if libtest reports exactly one test passed and none failed.
one_passed() { grep -qE "test result: ok\. 1 passed; 0 failed" "$LOG"; }

run_test() {
  local spec="$1"
  case "$spec" in
    cargo:*)
      local pkg filter
      pkg="$(echo "$spec" | cut -d: -f2)"
      filter="$(echo "$spec" | cut -d: -f3-)"
      # exact path first; fall back to substring (tolerates mod-name differences) but still demands exactly one match
      if cargo test -q -p "$pkg" --all-targets -- --exact "$filter" --include-ignored >"$LOG" 2>&1 && one_passed; then
        echo "PASS $spec"; return 0
      elif cargo test -q -p "$pkg" --all-targets -- "$filter" --include-ignored >"$LOG" 2>&1 && one_passed; then
        echo "PASS $spec (substring match)"; return 0
      else
        echo "FAIL $spec"; tail -n 30 "$LOG"; return 1
      fi ;;
    e2e:*)
      local scenario="${spec#e2e:}"
      if "$ROOT/scripts/e2e.sh" "$scenario" >"$LOG" 2>&1 && one_passed; then
        echo "PASS $spec"; return 0
      else
        echo "FAIL $spec"; tail -n 40 "$LOG"; return 1
      fi ;;
    *) echo "UNKNOWN test spec: $spec"; return 1 ;;
  esac
}

verify_feature() {
  local id="$1" mode="$2" rc=0 tests
  tests="$(jq -r --arg id "$id" '.features[] | select(.id==$id) | .tests[]' "$FEATURES")"
  [ -z "$tests" ] && { echo "no such feature: $id"; return 1; }
  while IFS= read -r t; do
    [ "$mode" = "quick" ] && [[ "$t" == e2e:* ]] && continue
    run_test "$t" || rc=1
  done <<< "$tests"
  return $rc
}

case "${1:-}" in
  --quick)
    rc=0
    for id in $(jq -r '.features[] | select(.passes==true) | .id' "$FEATURES"); do
      verify_feature "$id" quick || { echo "REGRESSION in $id"; rc=1; }
    done
    exit $rc ;;
  --all)
    rc=0
    unpassed="$(jq -r '.features[] | select(.passes==false and (.skipped // false)==false) | .id' "$FEATURES" | tr '\n' ' ')"
    for id in $(jq -r '.features[] | select((.skipped // false)==false) | .id' "$FEATURES"); do
      verify_feature "$id" full || rc=1
    done
    if [ $rc -eq 0 ] && [ -z "${unpassed// /}" ]; then echo ALL_FEATURES_PASS; else echo "NOT_ALL_PASS unpassed=[$unpassed] rc=$rc"; exit 1; fi ;;
  "") echo "usage: $0 <id> | --quick | --all"; exit 2 ;;
  *)  verify_feature "$1" full ;;
esac
