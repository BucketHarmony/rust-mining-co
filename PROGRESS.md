# PROGRESS — Rust Mining Co, Phase 1

Append-only handoff log. Each session adds one block (format in CLAUDE.md). The newest block is at the bottom; read the last 80 lines first.

## Standing notes

- Engine: Bevy 0.20.x pinned (fallback rule in docs/SAD.md §3).
- Core crates are Bevy-free; the client is Bevy.
- All Bevy tests go through rmc_testkit; E2E runs on lavapipe via scripts/e2e.sh.
- features.json is the source of truth for what is done. Only scripts/pass-feature.sh flips it.

## Decisions

(none yet)

## Blockers

(none yet)

---
