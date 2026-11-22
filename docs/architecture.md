# Architecture

Monaged is a native Windows app built with [GPUI](https://crates.io/crates/gpui), Zed's UI framework. Agents run **inside WSL**, each in its own git worktree, and Monaged talks to them over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP).

```
Monaged (Windows, GPUI) ──ACP over wsl.exe──▶ claude-agent-acp (WSL) ──▶ one Claude session per agent
  Project ▸ Subproject ▸ agents                    each session's cwd = its own worktree
        ▲                                                     │ MCP (stdio)
        └──── localhost TCP ◀── monaged.exe --mcp-relay ◀─────┘ (via WSL interop)
```

- **One agent process, many sessions.** Monaged starts `claude-agent-acp` once through `wsl.exe` and speaks newline-delimited JSON-RPC over its stdin and stdout. Every agent is a session with its own working directory.
- **Scripts go over stdin.** `wsl.exe` drops double quotes from its arguments, so scripts are piped to `bash -s` instead (`src/wsl.rs`). Every `wsl.exe` starts in the Linux home (`--cd ~`), and git and ssh never wait for prompts nobody can answer.
- **Agents reach Monaged through MCP.** Each session gets a `monaged` MCP server: the adapter starts `monaged.exe --mcp-relay` inside WSL through WSL's Windows interop. The relay speaks MCP on stdio and forwards each tool call over a token-protected localhost TCP connection to the running window, so WSL's networking mode doesn't matter.
- **State** lives in SQLite at `%APPDATA%\Monaged\monaged.db` (backed up on every start, the last five kept). Secrets are encrypted with DPAPI; the API key is in Windows Credential Manager.
- **Release builds** are windowed apps (no console); their stderr goes to `%APPDATA%\Monaged\monaged.log`. The relay still gets its stdio from WSL interop.
- **Installing** copies the exe to `%LOCALAPPDATA%\Programs\Monaged`, adds shortcuts and a per-user Settings → Apps entry. **Updates** read GitHub's release API (or `gh` for a private repository), verify the setup exe against `SHA256SUMS.txt`, and rename the running exe aside to replace it.
- **The setup guide** looks for WSL without running `wsl.exe` (its stub on a PC without WSL may offer to install it by itself), then checks the distro, the agent, the sign-in and git in one script.

## Source files

| File | What it does |
|---|---|
| `src/main.rs` | The `Monaged` state, ACP routing by session id, forms' logic, actions and key bindings, startup |
| `src/onboarding.rs` | The setup guide: WSL (hidden elevated install, its log shown live), Ubuntu, the Claude agent, sign-in, git identity, extras; Monaged's logo mark |
| `src/distro.rs` | Installing Ubuntu in the guide: WSL's catalog, the checked download, `wsl --install --from-file` (or `--import`), the first start with cloud-init's stages, and creating your user as root |
| `src/signin.rs` | Sign-ins without a terminal: `claude auth login` hidden in WSL (its link shown, the pasted code typed in for you) and `gh auth login`'s one-time code |
| `src/extras.rs` | The guide's Install buttons: winget for Windows programs (and picking up the new PATH), apt as root in WSL, and reading their progress |
| `src/shell.rs` | The window: title bar, sidebar tree and inbox, agent panes, inspector, status bar, overview, forms |
| `src/palette.rs` | The Ctrl K command palette (with file and conversation search) |
| `src/session_ui.rs` | The agent session UI: message box, settings chips, slash commands, question forms, tool details |
| `src/attach.rs` | Attachments (images, text files) as ACP content blocks |
| `src/search.rs` | Find in a conversation, search across conversations, the file index |
| `src/layout.rs` | Resizable panes, saved layouts, zen mode, density |
| `src/changes_ui.rs` | The diff view with line comments, and a subproject's combined changes |
| `src/turns.rs` | The conversation turn by turn: steps folded into a summary line, turn footers, the live working line |
| `src/timeline.rs` | The Activity tab (the timeline) |
| `src/pipeline_edit.rs` | Editing the plan, running a stage or builder again |
| `src/queue_ui.rs` | The merge queue with CI status |
| `src/git_ops.rs`, `src/git_ui.rs` | Git: status, commit (selected files, amend, undo), stash, sync (fast-forward or merge), conflicts, history; the Git tab and its settings |
| `src/git_auth.rs` | Signing git in: accounts, tokens in Credential Manager, the browser sign-in (the host's token page, picked up from the clipboard), token expiry, importing gh/glab/GCM logins, `monaged.exe --git-credential` as git's helper in WSL, Git Credential Manager setup, SSH keys, per-project accounts |
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
| `src/install.rs`, `src/update.rs` | The per-user installer and its Settings → Apps entry; updates from GitHub releases, verified by checksum |
| `src/sound.rs` | Sounds: five synthesized cues (soft bell-like notes, rising for good news, falling for trouble), played from memory with PlaySound; when they may play (settings, Windows' busy and away states, bursts as one, the important one wins) |
| `src/native.rs` | Windows' own APIs where Monaged used to start helper programs: the registry, Start menu shortcuts (IShellLink), running itself as administrator (ShellExecuteEx "runas"), moving the running exe out of its folder. Monaged starts no script hosts or shells |
| `src/uninstall.rs` | The uninstaller: what Monaged put on the PC (database, registry, Credential Manager, folders, each distro it used), a choice per part, removing them in a safe order with progress, and `--uninstall --quiet` |
| `src/add_project.rs` | Adding a project: live checks, one-click fixes, step-by-step progress, cancelling a clone |
| `src/env.rs` | Environment variables: the DPAPI vault, masking, .env import, the agents' `env_list`/`run_with_env` tools and approvals, repo checks |
| `src/instance.rs` | One Monaged at a time |
| `src/menu.rs` | Right-click and ⋯ menus |
| `src/feedback.rs` | Toasts, confirm dialogs, the keyboard sheet, automatic reconnects |
| `src/ui.rs` | The component kit: buttons, labels, status dots, meters, rows, cards, switches, fields |
| `src/theme.rs` | Design tokens and the themes |
| `src/icons.rs`, `assets/icons/` | Monaged's own icon set, embedded |
| `src/dots.rs` | Dot-matrix numerals (and the M of the logo) |
| `src/agent.rs` | One agent's conversation and review state, built from `session/update`s |
| `src/coord.rs` | Coordination plumbing: the MCP relay (`--mcp-relay`), the localhost bridge, tool definitions, board text, path/glob overlap |
| `src/usage.rs` | Usage logic: rate-limit parsing, Monaged's share and its calibration, budget checks, models per role, cost estimates |
| `src/usage_ui.rs` | Recording turn costs, the meter, pausing and resuming, model switching, plan estimates, the usage and model settings |
| `src/coord_ui.rs` | The board, claims, the librarian, routing `ask` between agents, board digests, the board sections of the Pipeline tab |
| `src/merge_ui.rs` | Branch chain and rules, merging, promotions, push, merge/pull requests, cleanup, auto-merge |
| `src/hosting.rs` | GitHub/GitLab remote parsing; pull/merge requests and CI through the REST API with your account, or the `gh`/`glab` CLIs |
| `src/pipeline.rs` | Roles, stages, briefs, `monaged-*` block parsing, and the gate rules |
| `src/autopilot.rs` | The project chat in a project's view, and autopilot: what it decides itself, the lead's autopilot brief, fixing and retrying the merge into develop |
| `src/plan.rs`, `src/plan_view.rs` | Plans as documents: versions, comments and the message that sends them to the planner; the plan page (outline, document with comments, at a glance, approve) |
| `src/diagram.rs` | ```mermaid flowcharts and sequence diagrams: parsing, a layered layout, drawn natively |
| `src/pipeline_ui.rs` | Running the pipeline after each turn; gate, plan and question cards; the pipeline strip |
| `src/review_ui.rs` | The Changes tab, the summary and confidence flows, Markdown rendering, Settings |
| `src/review.rs` | Summary and review prompts, running the project's tests, and the 1–5 scoring rules |
| `src/llm.rs` | Claude Haiku calls: raw HTTP with your API key, or `claude -p --json-schema` on your plan |
| `src/diff.rs` | Collects and parses `git diff` for an agent's worktree |
| `src/markdown.rs` | Turns Markdown into blocks (via pulldown-cmark) |
| `src/secrets.rs` | API key storage in Windows Credential Manager |
| `src/tests.rs` | Unit tests for diff parsing, Markdown, scoring, gates, branch rules, coordination and usage |
| `src/acp.rs` | One `claude-agent-acp` process through `wsl.exe`, newline-delimited JSON-RPC |
| `src/git.rs` | Repos, branches and worktrees inside WSL |
| `src/store.rs` | SQLite storage and backups |
| `src/stall.rs` | The stall guard: quiet turns stopped and continued, failed turns asked again, interrupted turns resumed after a reconnect, sessions that don't open reopened, an hourly cap |
| `src/wsl_health.rs` | When WSL itself breaks: checking WSL after the Claude process stops, `wsl --shutdown` and reconnecting, Restart WSL (and its service, as administrator) |
| `src/wsl.rs` | Runs scripts in WSL via `bash -s` over stdin, a few `wsl.exe` at a time, and holds them while WSL's service is broken |
| `src/zed.rs` | `zed --wsl user@distro path` |
| `src/text_input.rs` | Text input adapted from GPUI's example |
| `build.rs` | Embeds the icon and version info in the Windows exe (needs `rc.exe` from the Windows SDK) |
| `scripts/` | `release.ps1` (build and package), `package.ps1`, `release-notes.ps1`, `make_icon.py`, `make_brand.py` |

## Measured

| What | Time |
|---|---|
| Warm connect | about 1 s |
| New agent (worktree and session) | about 3–5 s |
| Simple prompt, first words | 0.7–3.7 s |
| `board_post`, `claim_files`, `board_read` round trip | well under a second each |
| `ask` answered by the librarian | a few seconds |
| Full pipeline with two parallel builders (small feature) | about 5 minutes |
