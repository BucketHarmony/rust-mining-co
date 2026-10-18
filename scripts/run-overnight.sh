#!/usr/bin/env bash
# Overnight driver: fresh Claude Code session per feature until all features pass or limits are hit.
#
#   MODEL=opus MAX_ITER=40 scripts/run-overnight.sh
#
# Run as a non-root user inside WSL2 (bypassPermissions refuses to start as root).
# Run `claude` once interactively in this directory beforehand so the folder is trusted.
# Logs: logs/run-N.json (session result; claude -p emits an array of messages, the result is the last), logs/run-N.txt (driver notes), logs/driver.log
set -u
# jq.exe on Windows ends lines with CRLF; strip CR so ids compare and files stay LF.
jq() { command jq "$@" | tr -d '\r'; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
mkdir -p logs

MODEL="${MODEL:-opus}"
FALLBACK="${FALLBACK:-sonnet}"
MAX_ITER="${MAX_ITER:-40}"
MAX_TURNS="${MAX_TURNS:-200}"
EFFORT="${EFFORT:-high}"
RATE_SLEEP="${RATE_SLEEP:-1200}"        # seconds to wait on a rate/usage limit
STALL_LIMIT="${STALL_LIMIT:-3}"         # iterations with no new commit before stopping
PROMPT_FILE="${PROMPT_FILE:-PROMPT.md}"

log() { echo "$(date -u +%FT%TZ) $*" | tee -a logs/driver.log; }

stall=0
heavy=0
for i in $(seq 1 "$MAX_ITER"); do
  before="$(git rev-parse HEAD 2>/dev/null || echo none)"
  log "iteration $i model=$MODEL head=$before"

  claude -p "$(cat "$PROMPT_FILE")" \
    --permission-mode bypassPermissions \
    --permission-prompts none \
    --model "$MODEL" --fallback-model "$FALLBACK" \
    --effort "$EFFORT" \
    --max-turns "$MAX_TURNS" \
    --output-format json \
    > "logs/run-$i.json" 2> "logs/run-$i.err"
  rc=$?

  subtype="$(jq -r '(if type=="array" then .[-1] else . end) | .subtype // "unknown"' "logs/run-$i.json" 2>/dev/null)"
  turns="$(jq -r '(if type=="array" then .[-1] else . end) | .num_turns // 0' "logs/run-$i.json" 2>/dev/null)"
  cost="$(jq -r '(if type=="array" then .[-1] else . end) | .total_cost_usd // 0' "logs/run-$i.json" 2>/dev/null)"
  result="$(jq -r '(if type=="array" then .[-1] else . end) | .result // ""' "logs/run-$i.json" 2>/dev/null)"
  after="$(git rev-parse HEAD 2>/dev/null || echo none)"
  log "iteration $i done rc=$rc subtype=$subtype turns=$turns cost=$cost head=$after"

  # The marker must be a line of its own (prose that mentions it does not count),
  # and features.json must agree that nothing unskipped is still unpassed.
  if echo "$result" | tr -d '\r' | grep -qxE '[[:space:]]*ALL_FEATURES_PASS[[:space:]]*'; then
    left="$(jq -r '[.features[] | select(.passes != true and (.skipped // false) != true)] | length' features.json)"
    if [ "$left" = "0" ]; then
      log "all features pass"; break
    fi
    log "ALL_FEATURES_PASS printed but $left features are unpassed; continuing"
  fi

  errtext="$(jq -r '(if type=="array" then .[-1] else . end) | [.result // "", .error // "", (.errors // [] | join(" "))] | join(" ")' "logs/run-$i.json" 2>/dev/null; cat "logs/run-$i.err" 2>/dev/null)"
  if echo "$errtext" | grep -qiE "rate.?limit|usage limit|overloaded|too many requests|status 429"; then
    log "rate/usage limit hit; sleeping ${RATE_SLEEP}s"; sleep "$RATE_SLEEP"; continue
  fi

  if [ "$after" = "$before" ]; then
    stall=$((stall+1)); log "no new commit (stall=$stall)"
    if [ "$stall" -ge "$STALL_LIMIT" ]; then
      log "stalled $STALL_LIMIT times; running unstick prompt once"
      claude -p "The last $STALL_LIMIT sessions produced no commit. Read PROGRESS.md Blockers and logs/run-$i.err. Either resolve the blocker with a conservative decision recorded under Decisions in PROGRESS.md, or write under Blockers in PROGRESS.md exactly what a human must decide, commit PROGRESS.md, and stop. Then follow PROMPT.md." \
        --permission-mode bypassPermissions --permission-prompts none --model "$MODEL" --effort "$EFFORT" \
        --max-turns "$MAX_TURNS" --output-format json > "logs/run-$i-unstick.json" 2> "logs/run-$i-unstick.err"
      if [ "$(git rev-parse HEAD)" = "$after" ]; then log "still stalled; stopping"; break; fi
      stall=0
    fi
  else
    stall=0
  fi

  # Budget guard: if the first two iterations are both heavy, drop to the cheaper model.
  if [ "$i" -le 2 ] && [ "${turns:-0}" -ge 150 ]; then heavy=$((heavy+1)); fi
  if [ "$heavy" -ge 2 ] && [ "$MODEL" != "$FALLBACK" ]; then MODEL="$FALLBACK"; log "switching MODEL to $FALLBACK for the rest of the night"; fi

  sleep 15
done

log "writing report"
"$ROOT/scripts/report.sh" || true
git add -A docs/REPORT.md docs/screens 2>/dev/null
git commit -qm "chore: overnight report" 2>/dev/null || true
git push -q 2>/dev/null || log "push failed or no remote"
log "driver finished"
