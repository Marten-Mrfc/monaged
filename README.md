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

- **Try it:** Ctrl K → **Create the playground project** (or **Try the playground** on the welcome screen) makes Lunchbox in `~/code/monaged-playground`: a small Python app with a bug, a flaky test, a merge conflict, an MCP server and project knowledge, plus a tour of every feature (`PLAYGROUND.md`, also shown in Monaged).
- **The window:** the sidebar tree on the left (with a **Needs you** inbox on top), agents in the middle, and the inspector (Pipeline, Board, Merge, Git, Run, Timeline) on the right. The status bar shows the connection, weekly usage and Monaged's share.
- **Wide screens** show several agents side by side; Ctrl-click an agent in the tree to open it beside the current one.
- **Ctrl K** opens the command palette: jump anywhere, run an action, switch theme.
- **Right-click** a project, subproject or agent (or use its ⋯) for everything you can do with it.
- **Other shortcuts:** Ctrl B toggles the sidebar, Ctrl Alt B the inspector, Ctrl , opens Settings, Ctrl / lists every shortcut, Alt ↑/↓ moves between agents, Ctrl N starts an agent, Ctrl J opens the side chat, Ctrl F finds in a conversation, Ctrl Shift F searches all of them, F11 is zen mode; Enter sends, Shift+Enter adds a line, Esc stops an agent or closes whatever is open. Ctrl 0 puts a cursor in the sidebar (↑ ↓ → ← Enter Esc). Change any of them in Settings → Keyboard.
- **The message box:** `/` lists commands, ↑ brings back what you sent, paste or drop images and files (images show as thumbnails; click one for full size). The chips under it set the agent's mode, model and effort.
- **Undo and replay:** every turn is checkpointed; point at one of your messages and ↶ rolls the worktree back to before it (with Undo). Right-click an agent → Replay steps through its turns with each turn's diff.
- **Side chat** (Ctrl J): ask Haiku about an agent's work beside its conversation; the agent never sees it.
- **Project settings** (right-click a project): MCP servers for its agents (with a Test button), its knowledge (CLAUDE.md, skills, subagents, commands, shared notes) and its run and test commands.
- **Run tab:** start a worktree's dev server (output, port, Open), run its tests once or three times to find flaky ones, and send failures to the agent.
- **Spending cap:** right-click a subproject → Set a spending cap; its agents pause when they reach it.
- **Environment** (Project settings): variables per environment (Development, Test…), encrypted with Windows DPAPI. The Run tab and project MCP servers (`${NAME}`) get them as environment variables; agents only get names, and run commands with them through `run_with_env` after you approve (output masked). Secret values are masked everywhere Monaged shows text, flagged in diffs, and block merges and pushes.
- **Questions:** when an agent asks something, a form appears in its conversation; it waits for your answer.
- **The inspector** has Pipeline, Board, Merge, Git, Run and Timeline tabs. The Git tab commits (all files or the ones you tick, or amends), takes back a commit that isn't pushed, syncs (pull, or a merge when both sides moved), resolves conflicts and shows the history. Its Origin section says who the project signs in as, switches accounts, tests the connection, and flips origin between HTTPS and SSH.
- **Git accounts** (Settings → Git accounts): sign in to GitHub, GitLab (also Enterprise and self-hosted) or any other host, with nothing to register and no server involved. In the browser (Monaged opens the host's token page with the right permissions ticked; click Generate and Copy, and Monaged picks the token up from the clipboard, checks it and empties the clipboard), by pasting a token, or by importing a login the GitHub CLI, the GitLab CLI or Git Credential Manager already has. Monaged checks it with the host's API, keeps the token in Windows Credential Manager, and becomes git's credential helper in WSL for that host (`monaged.exe --git-credential`), so Git Credential Manager isn't needed. If you do want it for other hosts, Monaged finds it and sets it up. Several accounts per host: one is the default, a project can sign in as another. Pull and merge requests and CI status then go through the REST API, so `gh` and `glab` are optional. Tokens are masked like secrets and block pushes. SSH keys in WSL: create one, add it to your account in one click, test, trust a host.
- **Installing:** run `scripts/release.ps1` to build `dist/Monaged-Setup-<version>.exe`; running that installs Monaged for you (no admin). With `-Publish` it also makes the GitHub release that installed copies update from.
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
   - shows the Haiku spend so far;
   - Git accounts: your sign-ins for GitHub, GitLab and other hosts, Git Credential Manager (optional) and SSH keys.

Keys: **Enter** sends (or submits a form), **Esc** stops the agent (or closes a form).

## Setup (already done on this PC)

- Adapter in Ubuntu: `~/.local/share/claude-agent-acp/0.107.0`, with `current` symlinked to it. It bundles the Claude CLI.
- Log in once: `wsl -d Ubuntu -- bash -lc "~/.local/share/claude-agent-acp/current/claude"`, then `/login`.
- Git identity inside WSL: copied from Windows (Settings → "Copy my Windows git identity into WSL"), or from an account (Settings → Git accounts → Commit as this account).
- Git logins: Settings → Git accounts. Nothing to set up first: no OAuth app, client ID or secret, and no server. Everything stays between your PC and the git host.
- pytest is **not** installed in WSL, so Python projects' tests can't run there (`sudo apt install python3-pytest` fixes that).
- Start Monaged normally (Start menu, Explorer or `cargo run`). A copy started from inside a sandboxed tool shell may not write to the real `%APPDATA%\Monaged\monaged.db`.

## Code

| File | What it does |
|---|---|
| `src/main.rs` | The `Monaged` state, ACP routing by session id, forms' logic, actions and key bindings |
| `src/shell.rs` | The window: title bar, sidebar tree and inbox, agent panes, inspector, status bar, overview, forms |
| `src/palette.rs` | The Ctrl K command palette (with file and conversation search) |
| `src/session_ui.rs` | The agent session UI: message box, settings chips, slash commands, question forms, tool details |
| `src/attach.rs` | Attachments (images, text files) as ACP content blocks |
| `src/search.rs` | Find in a conversation, search across conversations, the file index |
| `src/layout.rs` | Resizable panes, saved layouts, zen mode, density |
| `src/changes_ui.rs` | The diff view with line comments, and a subproject's combined changes |
| `src/timeline.rs` | The Timeline tab |
| `src/pipeline_edit.rs` | Editing the plan, running a stage or builder again |
| `src/queue_ui.rs` | The merge queue with CI status |
| `src/git_ops.rs`, `src/git_ui.rs` | Git: status, commit (selected files, amend, undo), stash, sync (fast-forward or merge), conflicts, history; the Git tab and its settings |
| `src/git_auth.rs` | Signing git in: accounts, tokens in Credential Manager, the browser sign-in (the host's token page, picked up from the clipboard), token expiry, importing gh/glab/GCM logins, `monaged.exe --git-credential` as git's helper, Git Credential Manager setup, SSH keys, per-project accounts (unit-tested parts) |
| `src/accounts_ui.rs` | Settings → Git accounts, and the Git tab's "signs in with" row |
| `src/syntax.rs` | Syntax colors for code blocks and diffs (a small tokenizer per language family) |
| `src/checkpoints.rs` | Checkpoints per turn (git objects under `refs/monaged/`), rollback with undo, the turn-by-turn replay |
| `src/project_ui.rs` | Project settings: MCP servers, knowledge files, run and test commands |
| `src/run_ui.rs` | The Run tab: dev servers in their own process group, tests, flaky detection |
| `src/media.rs` | Image thumbnails and the full-size viewer |
| `src/side_chat.rs` | The side chat with Haiku |
| `src/keys.rs` | Shortcuts you can change, and moving through the sidebar with the keyboard |
| `src/playground.rs`, `playground/` | The playground project and its files |
| `src/notify.rs` | Windows notifications through a tray icon, the flashing taskbar button |
| `src/health.rs` | The health check |
| `src/settings_ui.rs` | Settings: agents, message box, notifications, general |
| `src/install.rs`, `src/update.rs` | The per-user installer and updates from GitHub releases |
| `src/add_project.rs` | Adding a project: live checks, one-click fixes, step-by-step progress, cancelling a clone |
| `src/env.rs` | Environment variables: the DPAPI vault, masking, .env import, the agents' `env_list`/`run_with_env` tools and approvals, repo checks |
| `src/instance.rs` | One Monaged at a time |
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
| `src/hosting.rs` | GitHub/GitLab remote parsing; pull/merge requests and CI through the REST API with your account, or the `gh`/`glab` CLIs (unit-tested parsing) |
| `src/pipeline.rs` | Roles, stages, briefs, `monaged-*` block parsing, and the gate rules (unit-tested) |
| `src/pipeline_ui.rs` | Running the pipeline after each turn, gate, plan and question cards, the pipeline strip |
| `src/review_ui.rs` | The Changes tab, the summary and confidence flows, Markdown rendering, Settings |
| `src/review.rs` | Summary and review prompts, test detection, and the 1–5 scoring rules |
| `src/llm.rs` | Claude Haiku 5.5 calls: raw HTTP with your API key, or `claude -p --json-schema` on the Max plan |
| `src/diff.rs` | Collects and parses `git diff` for an agent's worktree |
| `src/markdown.rs` | Turns Markdown into blocks (via pulldown-cmark) |
| `src/secrets.rs` | Anthropic API key storage in Windows Credential Manager |
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
