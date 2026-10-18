# Rust Mining Co — overnight kit

Drop the contents of this folder into an empty `rust-mining-co` git repo. Claude Code builds the game from here.

## Files

| File | Purpose |
|---|---|
| `docs/SAD.md` | Architecture, test strategy, workflow. Read-only for Claude Code (hook-protected) |
| `CLAUDE.md` | Build rules Claude Code loads every session |
| `PROMPT.md` | The per-iteration prompt the driver feeds to `claude -p` |
| `features.json` | Ordered feature list with test names and pass state. Only scripts change it |
| `PROGRESS.md` | Append-only handoff log |
| `.claude/settings.json` | Deny rules and hooks |
| `.claude/hooks/*.sh` | protect-files, cargo-check, gate-commit, stop-guard |
| `.claude/agents/*.md` | `reviewer`, `playtester` subagents |
| `scripts/*.sh` | smoke, e2e, screens, soak, verify-features, pass-feature, skip-feature, check-no-bevy, report, run-overnight |
| `rust-toolchain.toml` | pinned stable toolchain with clippy/rustfmt |
| `.github/workflows/ci.yml` | Linux tests + E2E (lavapipe), Windows build + E2E (WARP) + release artifact |

## One-time setup (WSL2 Ubuntu on the 4090 box)

```bash
# as a dedicated non-root user
sudo apt update && sudo apt install -y build-essential pkg-config libasound2-dev libudev-dev libwayland-dev \
  libxkbcommon-dev mesa-vulkan-drivers vulkan-tools jq ripgrep git gh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env && rustup component add clippy rustfmt rust-analyzer
vulkaninfo --summary | grep -i llvmpipe        # must print a line

# Claude Code
curl -fsSL https://claude.ai/install.sh | bash   # or npm i -g @anthropic-ai/claude-code
claude                                            # sign in once, then /exit
claude plugin install rust-analyzer-lsp@claude-plugins-official

# Repo
mkdir -p ~/src && cd ~/src
gh repo create rust-mining-co --private --clone && cd rust-mining-co
cp -r /path/to/rmc-kit/. .
chmod +x scripts/*.sh .claude/hooks/*.sh
printf 'target/\nlogs/\n.claude/worktrees/\nsave/\n' > .gitignore
git add -A && git commit -m "chore: overnight kit" && git push -u origin main

# Trust the folder and accept the bypass disclaimer once, interactively:
claude --permission-mode bypassPermissions      # answer the prompts, then /exit
```

## Start the run

```bash
cd ~/src/rust-mining-co
mkdir -p logs
MODEL=opus MAX_ITER=40 nohup scripts/run-overnight.sh > logs/nohup.out 2>&1 &
tail -f logs/driver.log
```

Expect iteration 1 (F001) to take the longest; it creates the workspace and resolves Bevy 0.20 dependencies.

## Morning

1. `cat docs/REPORT.md` and open `docs/screens/`.
2. `git log --oneline` and the Decisions/Blockers sections in `PROGRESS.md`.
3. Play: on Windows, `git clone` the repo to a Windows path (not `\\wsl$`) and `cargo run --release -p rmc_app`, or download the `windows-release` artifact from the latest CI run.

## Knobs

- `MODEL`, `FALLBACK`, `MAX_ITER`, `MAX_TURNS`, `EFFORT`, `RATE_SLEEP`, `STALL_LIMIT` on `run-overnight.sh`.
- First night on a Max 20x plan: `MODEL=opus` is the intent; the driver drops to `sonnet` if the first two iterations each exceed 150 turns.
- Native Windows instead of WSL2: install Git for Windows and `winget install jqlang.jq`; set `"env": {"CLAUDE_CODE_GIT_BASH_PATH": "C:\\Program Files\\Git\\bin\\bash.exe"}` in `~/.claude/settings.json`; the scripts run under Git Bash; E2E uses WARP automatically. The Bash sandbox is not available on native Windows.
