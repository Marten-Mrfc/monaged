# Monaged

One central place to run Claude Code agents on your projects. It's a native Windows app built with [GPUI](https://crates.io/crates/gpui), Zed's UI framework. The agents run **inside WSL**, each in its own git worktree, and Monaged talks to them over the [Agent Client Protocol](https://agentclientprotocol.com).

```
Monaged (Windows, GPUI) ──ACP over wsl.exe──▶ claude-agent-acp (Ubuntu) ──▶ one Claude session per agent
  Project ▸ Subproject ▸ agent tabs                 each session's cwd = its own worktree
        ▲                                                     │ MCP (stdio)
        └──── localhost TCP ◀── monaged.exe --mcp-relay ◀─────┘ (via WSL interop)
```

The vision, decisions and phases are in [ROADMAP.md](ROADMAP.md).

## Run

```powershell
cargo run --manifest-path O:\Projects\projectwithoutaname\app\Cargo.toml
```

Options: `--distro Ubuntu`, and `--agent <path to claude-agent-acp in WSL>`.

## Using it

- **The window:** the sidebar tree on the left (with a **Needs you** inbox on top), agents in the middle, and the inspector (Pipeline, Board, Merge) on the right. The status bar shows the connection, weekly usage and Monaged's share.
- **Wide screens** show several agents side by side; Ctrl-click an agent in the tree to open it beside the current one.
- **Ctrl K** opens the command palette: jump anywhere, run an action, switch theme.
- **Right-click** a project, subproject or agent (or use its ⋯) for everything you can do with it.
- **Other shortcuts:** Ctrl B toggles the sidebar, Ctrl Alt B the inspector, Ctrl , opens Settings, Ctrl / lists every shortcut, Alt ↑/↓ moves between agents, Ctrl N starts an agent; Enter sends, Esc stops an agent or closes whatever is open.
- **Messages:** things that happened appear bottom right and fade (errors stay); things that are true right now (offline, not logged in, paused) sit in a bar at the top until fixed.
- **Themes** (Settings → Appearance): Carbon, Graphite, Clay, Paper, or follow Windows.

## How it works

1. **Add a project:** clone from GitLab/GitHub, use an existing WSL folder, or start a new empty repo. Monaged makes sure the repo has a commit. It uses `develop` as the base branch when it exists.
2. **New subproject:** a *feature* gets branch `feature/<name>`, a *standing* concern gets `standing/<name>`. The first agent starts automatically.
3. **+ Agent:** creates a worktree at `~/code/.monaged/<repo>/<subproject>/<agent>` on branch `agent/<subproject>/<agent>`, then opens a Claude session there. Agents work in parallel without touching each other's files or your main checkout.
4. **Restart-safe:** everything is saved in `%APPDATA%\Monaged\monaged.db`, and opening an agent reloads its conversation.
5. **Open in Zed:** opens the project or the selected agent's worktree in Zed through its WSL remote.

6. **Review an agent:** the **Changes** tab shows its diff, a summary written by Haiku, and a **1–5 confidence score**. The score comes from the project's tests plus a skeptical Haiku review, and always lists its reasons. Summaries and reviews run automatically after every turn, or on demand with **Review** and **Summarize**.
7. **Pipeline** (on by default for a new feature subproject, or **Run pipeline** on an existing one):
   - **The flow:** a researcher, two verifiers, a decision ("do we build this?"), a planner, plan approval, then 1–10 builders in parallel, and an integrator that merges their branches into the feature branch.
   - **Questions** appear as cards with options to click.
   - **Auto mode** answers questions with the recommended option, and lets small plans with confidence 4–5 and no risk flags start by themselves. Large, risky or uncertain plans always wait for you.
   - **Project switch:** "auto-start" is set per project in the project view.
8. **Merging:**
   - **Project view:** shows your branch chain (develop → acceptance → main) with a rule per target you can click to change, plus promote buttons with commit counts.
   - **Merge card** (in each subproject): merge, push, open a GitLab/GitHub merge or pull request (via `glab`/`gh`), resolve conflicts with an agent, and clean up worktrees.
   - **Auto-merge:** finished pipelines merge into develop by themselves when the score allows it.
9. **Coordination** (the **Board** tab in each subproject):
   - Every agent has Monaged's tools: `board_read`, `board_post` (contracts, decisions, notes), `claim_files` (warns about overlapping edits) and `ask` (another agent, by name or `lead`).
   - Questions go to a Haiku **librarian** first, which answers from the board and plan. Only if it can't is the other agent asked; its answer comes back to the asker.
   - New contracts and decisions are attached to each agent's next message, so agents don't poll or chat.
   - The Board tab shows open questions, who owns which files, and every entry.
10. **Usage** (sidebar meter, Settings → Usage and budget, Settings → Models):
   - The sidebar shows your week (all your Claude use) and Monaged's estimated share of it. The agent bar shows each agent's model and API-equivalent cost; the subproject header shows this week's cost.
   - **Budget:** Monaged may use a share of your week (default 25%), and pauses automatic work when the whole week reaches a ceiling (default 90%). Paused work waits and continues by itself; your own messages always go through.
   - **Models per role:** research, verification and integration default to Sonnet 5.5, everything else to your default model.
   - Plans show an estimated cost, and the plan gate asks you when a plan won't fit in what's left of the budget.
11. **Settings** (sidebar footer):
   - paste your Anthropic API key so the Haiku work is paid from your API credit; it's stored in Windows Credential Manager;
   - without a key, Monaged can use your Max plan through the Claude CLI instead;
   - shows the Haiku spend so far.

Keys: **Enter** sends (or submits a form), **Esc** stops the agent (or closes a form).

## Setup (already done on this PC)

- Adapter in Ubuntu: `~/.local/share/claude-agent-acp/0.107.0`, with `current` symlinked to it. It bundles the Claude CLI.
- Log in once: `wsl -d Ubuntu -- bash -lc "~/.local/share/claude-agent-acp/current/claude"`, then `/login`.
- Git identity inside WSL: copied from Windows (Settings → "Copy my Windows git identity into WSL").
- pytest is **not** installed in WSL, so Python projects' tests can't run there (`sudo apt install python3-pytest` fixes that).
- Start Monaged normally (Start menu, Explorer or `cargo run`). A copy started from inside a sandboxed tool shell may not write to the real `%APPDATA%\Monaged\monaged.db`.

## Code

| File | What it does |
|---|---|
| `src/main.rs` | The `Monaged` state, ACP routing by session id, forms' logic, actions and key bindings |
| `src/shell.rs` | The window: title bar, sidebar tree and inbox, agent panes, inspector, status bar, overview, forms |
| `src/palette.rs` | The Ctrl K command palette |
| `src/menu.rs` | Right-click and ⋯ menus |
| `src/feedback.rs` | Toasts, confirm dialogs, the keyboard sheet, automatic reconnects |
| `src/ui.rs` | The component kit: buttons, labels, status dots, meters, rows, cards, switches, fields |
| `src/theme.rs` | Design tokens and the four themes |
| `src/icons.rs`, `assets/icons/` | Monaged's own icon set, embedded |
| `src/dots.rs` | Dot-matrix numerals |
| `src/agent.rs` | One agent's conversation and review state, built from `session/update`s |
| `src/coord.rs` | Coordination plumbing: the MCP relay (`--mcp-relay`), the localhost bridge, tool definitions, board text, path/glob overlap (unit-tested) |
| `src/usage.rs` | Usage logic: rate-limit parsing, Monaged's share and its calibration, budget checks, models per role, cost estimates (unit-tested) |
| `src/usage_ui.rs` | Recording turn costs, the meter, pausing and resuming, model switching, plan estimates, and the usage and model settings |
| `src/coord_ui.rs` | The board, claims, the librarian, routing `ask` between agents, board digests, and the Board tab |
| `src/merge_ui.rs` | Branch chain and rules, merging, promotions, push, merge/pull requests, cleanup, auto-merge |
| `src/hosting.rs` | GitHub/GitLab remote parsing and the `gh`/`glab` calls (unit-tested parsing) |
| `src/pipeline.rs` | Roles, stages, briefs, `monaged-*` block parsing, and the gate rules (unit-tested) |
| `src/pipeline_ui.rs` | Running the pipeline after each turn, gate, plan and question cards, the pipeline strip |
| `src/review_ui.rs` | The Changes tab, the summary and confidence flows, Markdown rendering, Settings |
| `src/review.rs` | Summary and review prompts, test detection, and the 1–5 scoring rules |
| `src/llm.rs` | Claude Haiku 5.5 calls: raw HTTP with your API key, or `claude -p --json-schema` on the Max plan |
| `src/diff.rs` | Collects and parses `git diff` for an agent's worktree |
| `src/markdown.rs` | Turns Markdown into blocks (via pulldown-cmark) |
| `src/secrets.rs` | API key storage in Windows Credential Manager |
| `src/tests.rs` | Unit tests for diff parsing, Markdown, scoring, gates, branch rules, coordination and usage (`cargo test`) |
| `src/acp.rs` | One `claude-agent-acp` process through `wsl.exe`, newline-delimited JSON-RPC |
| `src/git.rs` | Repos, branches and worktrees inside WSL |
| `src/store.rs` | SQLite storage |
| `src/wsl.rs` | Runs scripts in WSL via `bash -s` over stdin (wsl.exe drops quotes from arguments) |
| `src/zed.rs` | `zed --wsl user@distro path` |
| `src/text_input.rs` | Text input adapted from GPUI's example |

## Measured

| What | Time |
|---|---|
| Warm connect | about 1 s |
| New agent (worktree and session) | about 3–5 s |
| Simple prompt, first words | 0.7–3.7 s |

Two agents ran in parallel in separate worktrees in testing.

| Coordination (measured on the demo) | Time |
|---|---|
| `board_post`, `claim_files`, `board_read` round trip (relay ⇄ window) | well under a second each |
| `ask` answered by the librarian (Haiku) | a few seconds |
| `ask` routed to an idle agent and answered | about 2 s for the answer turn |
| Full pipeline with two parallel builders (small feature) | about 5 minutes |
