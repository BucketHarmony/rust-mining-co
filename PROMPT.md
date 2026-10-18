You are one iteration of an unattended overnight build of Rust Mining Co. Nobody will answer questions. Work on exactly one feature, verify it, commit it, record it, and stop.

Steps, in order:

1. Orient. Run `pwd`, `git status --short`, `git log --oneline -15`. Read the last 80 lines of `PROGRESS.md` and all of `features.json`. Read `CLAUDE.md`.
2. If `git status` shows uncommitted changes, a previous iteration was cut off. Run `cargo test --workspace`. If green and the changes belong to a single feature, finish that feature (continue at step 5). If the changes are broken beyond a few minutes of repair, `git stash` them with a message naming the feature, note it in `PROGRESS.md`, and continue at step 3.
3. Run `scripts/smoke.sh`. If it fails on code already committed, fixing that is this session's work: fix it, commit with message `fix: smoke <what>`, append a PROGRESS.md block titled `FIX-smoke`, and stop. Skip steps 9 and 11.
4. Pick the feature: the first entry in `features.json` with `"passes": false` and no `"skipped": true` whose every `blockedBy` id has `"passes": true` or `"skipped": true`. If the feature has `"optional": true` and the last PROGRESS.md block already reports a serious failed attempt at it, run `scripts/skip-feature.sh <id> "<reason>"`, commit, and pick the next feature instead. If none exists and `scripts/verify-features.sh --all` reports all passing, print the single line `ALL_FEATURES_PASS` and stop. If none exists but some are unpassed, print `BLOCKED: <ids>` with the reason and stop.
5. Read the SAD section named in the feature's `sad` field. Read any existing code the feature touches.
6. TDD. Write the test(s) listed in the feature's `tests` field so they compile and fail for the right reason. Run them and confirm the failure. Then implement until they pass. Then refactor with everything green. Do not touch unrelated code.
7. Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. If the feature has `"e2e": true`, also run `scripts/e2e.sh <scenario>` for each scenario listed in `tests`, then `scripts/screens.sh <scenario>`.
8. If the feature has `"review": true`, invoke the `reviewer` subagent with the feature id. Address every gap it reports that is a genuine SAD violation or a bug. Ignore style comments.
9. Run `scripts/pass-feature.sh <id>`. It runs the feature's tests and flips `passes`. If it refuses, the feature is not done; go back to step 6.
10. Commit with message `feat(<id>): <title>` (the commit gate runs the full check; fix anything it reports). Append the session block to `PROGRESS.md` and commit it with `chore(<id>): progress`.
11. If the feature's milestone is now complete (every feature with that `milestone` passes) and the milestone is M6, M7, M8, E2, E3b or E5, invoke the `playtester` subagent and commit its report.
12. Stop. Do not start another feature.

Rules that override everything else: never delete or weaken a test; never edit `features.json` by hand; never force-push or reset; never ask a question. If you cannot make progress after a serious attempt: `git stash push -m "<id> wip"` the broken code, write what you tried and the exact error under `Blockers:` in `PROGRESS.md`, commit PROGRESS.md, and stop.
