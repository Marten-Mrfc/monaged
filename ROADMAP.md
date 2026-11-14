# Monaged: vision and roadmap

Decided with the user on 2026-11-14 through four rounds of questions. This file is the source of truth for what Monaged should do.

## Vision

**Monaged is one central place to run Claude Code agents on your projects.** You click a project, then a subproject, and agents work on it in parallel inside WSL, each in its own git worktree. They coordinate with each other, summarise their progress cheaply, rate how confident they are, and merge according to rules you set per branch. It's a native GPUI app on Windows, fast like Zed, with Zed one click away.

The problem it solves: in the Claude desktop app, every chat for every project lands in one long, mixed list.

## Core concepts

| Concept | Meaning |
|---|---|
| **Project** | One git repo inside WSL, for example an HR application. |
| **Subproject** | Always in the same repo. Either a **feature** ("company-car reservation") with its own branch, or a **standing** concern ("security") that keeps running and checking. Later: an **external** subproject that links to another project (for example shared auth). |
| **Agent** | One Claude Code session (over ACP) with a role, running in its own worktree on a branch of its subproject. |
| **Pipeline** | Research → 2 verifiers → questions and decision → plan → build (1 to 10 agents) → integrate → merge. |
| **Board** | A per-subproject record of contracts and decisions that agents share. See Coordination. |

## Decisions

**Agents.** Claude Code only for now (ACP keeps Codex, Gemini and Copilot cheap to add later).

**Review.** An in-app diff view, plus **live summaries** written by a cheap model (Haiku 5.5) and a **confidence score from 1 to 5** that always shows *why*. Research shows agents are systematically overconfident, so the score combines:
- build and test results (the strongest signal);
- a short adversarial Haiku review framed as "find what's broken";
- how much of the plan was covered, plus risk flags.

The agent's own opinion is only one input.

**Stop-and-wait points.**
- **Questions to the user** come up often during research and planning, because answering them raises confidence. **Auto mode** picks the most logical answer itself.
- **Plan approval depends on risk.** A small task or bug at confidence 4-5 starts automatically. A large feature, or anything risky (database migrations, auth, data), needs you to read the plan first. Configurable per project.
- **Before merging finished work:** "Is this OK?"

**Branch policy** (configurable per project):

| Merge | Rule |
|---|---|
| work → `develop` | automatic, unless confidence is 1 |
| `develop` → `acceptance` | always asks you |
| `acceptance` → `main` | your explicit confirmation; usually you do it yourself |

**Merging.** The agents decide whether an integrator agent is needed, within the branch rules.

**Git hosting.** GitLab (work) and GitHub (private) from the start, plus local-only git. Gitea later.

**Usage.**
- Agents run on the **Max subscription** and stay **budget-aware**: you set a cap such as "at most 25% of my weekly usage", and Monaged plans around it (sizing work, warning before limits).
- The **€200 API credit** pays for the Haiku work (summaries, reviews, librarian answers) so it doesn't eat into Max usage.
- A Team plan may be used later.

**Zed.**
- Open any file or worktree in Zed in one click (`zed --wsl user@distro path`).
- Review diffs in Zed (`zed --diff`).
- An in-app, read-only file viewer styled like Zed. Editing happens in Zed.

## Coordination design (token-frugal)

Coordination overhead grows quickly with the number of agents (about n² paths), and in published estimates multi-agent setups cost several times the tokens of a single agent. So Monaged avoids chatter:

1. **One brief per subproject.** The planner writes the plan, the contracts and who owns which files once. Every agent starts from it, and the shared text is prompt-cached.
2. **Contracts board.** Agents post small structured entries (API shapes, data models, decisions) through a Monaged MCP tool. There is no broadcast and no polling: Monaged hands each agent the new contracts and decisions, plus notes that touch its files, with its next message.
3. **File ownership.** The plan assigns paths per agent, and Monaged warns about overlapping edits. Most conflicts are prevented instead of discussed.
4. **Questions, in order of cost.** A cheap Haiku **librarian** answers from the board and summaries first. Only if it can't does Monaged wake the other agent (as a steering message) and route its short answer back.
5. **Across subprojects** (for example car reservation ↔ dashboard): agents can read another subproject's board and ask through the same channel.
6. **A token meter** per subproject and per agent, checked against your budget.

## Phases

### Phase 1: Projects and parallel agents (done 2026-11-14)
- [x] Rename to Monaged
- [x] Local store (SQLite, `%APPDATA%\Monaged\monaged.db`) for projects, subprojects and agents
- [x] Sidebar: projects, with subprojects inside them, plus activity dots
- [x] Add a project: new empty repo and existing folder tested; **clone from GitLab/GitHub built but untested**
- [x] Subprojects: a feature (`feature/<slug>`) or standing (`standing/<slug>`), off `develop` when it exists
- [x] Several agents per subproject, each in its own **git worktree** (`~/code/.monaged/<repo>/<sub>/<agent>`, branch `agent/<sub>/<agent>`), all through one ACP connection. Two agents ran in parallel in testing.
- [x] Resume agents after a restart (ACP `session/load` replays the conversation; sessions reopen only when you open the agent)
- [x] "Open in Zed" for the project or an agent's worktree (`zed --wsl user@distro path`)

Found while testing:
- `wsl.exe` drops embedded double quotes from its arguments. Monaged now pipes scripts to `bash -s` over stdin.
- Git inside WSL has no `user.name`/`user.email` on this PC (no `~/.gitconfig`), so agents can't commit until it's set. Monaged warns about this.

### Phase 2: Review (done 2026-11-14, except where noted)
- [x] **Changes tab** per agent: file list and a colored diff with line numbers against where the agent's branch started (new files included), plus "Open in Zed" per file. *Not done: a combined diff for the whole subproject.*
- [x] **Markdown rendering** of agent replies: headings, lists, code blocks, bold, italic, inline code.
- [x] **Haiku summaries** (Claude Haiku 5.5, structured JSON, effort `low`): status, what was done, and the next step. Written automatically after every turn and saved.
- [x] **Confidence score v1:**
  - runs the project's tests (detects cargo, npm, go, pytest and make);
  - asks Haiku for an adversarial review ("find what's broken");
  - combines the two into 1–5 with reasons and concrete issues.
  - Tests that fail cap the score at 2; no tests caps it at 4; a "partly" verdict caps it at 3 and "broken" at 2.
  - *Tested only on the no-tests path so far.*
- [x] **Settings:**
  - API key pasted from the clipboard into Windows Credential Manager (never shown, never in the database);
  - a fallback to the Max plan through the Claude CLI when there's no key;
  - auto-review on/off;
  - Haiku spend so far, split between API credit and Max.
  - *The API-key path is built but untested until a key is added.*
- [ ] In-app file viewer with syntax highlighting (the diff view and Zed cover this for now).

### Phase 3: Pipeline and gates (done 2026-11-14)
- [x] **Roles:** researcher, verifier ×2 (in parallel), planner, builders (one per workstream, up to 10, in parallel), integrator. Each gets a **brief** as its first message, shown collapsed in the transcript.
- [x] **Reporting back:** agents end their turn with fenced JSON blocks (`monaged-question`, `-research`, `-verification`, `-plan`, `-done`), which Monaged renders as cards and uses to move the pipeline on.
  - A missing block gets one automatic reminder.
  - An unreadable block is sent back to the agent to fix.
- [x] **Question cards** with clickable options (the recommended one is marked). The answers are sent together once every question is answered. You can also type your own answer.
- [x] **Auto mode** per subproject. Questions get the agent's recommended option, the decide gate passes when no verifier found major issues, and plans follow the plan gate.
- [x] **Plan gate:**
  - **Auto-starts** only for small plans with confidence 4–5 and no risk flags, with auto mode on and the project switch on.
  - Large, risky (migration, auth, data, public API, infra, security) or low-confidence plans always wait.
- [x] **Plan card:** size, risk flags, confidence and its reason, workstreams with owned paths, contracts and tests, with Approve / Stop.
- [x] **Pipeline strip:** Research → Verify → Decide → Plan → Approve → Build → Integrate → Done, with notes saying what it's waiting for. Saved across restarts.
- [x] **Git identity check** before building, with a one-click "Copy my Windows git identity into WSL" (also in Settings).
- [x] **Tested end-to-end** on a small feature ("Health check") with auto mode: research, 2 verifiers, auto decide, plan (small, 5/5, auto-start), identity gate, builder (committed, scored 4/5), integrator (merged into `feature/health-check`, no conflicts), done. `main` stayed untouched.

**Not exercised live yet:**
- question cards (that agent had no questions);
- the manual decide gate;
- several builders in parallel;
- revising a plan by replying to the planner.

**Known gaps:**
- The read-only roles' worktrees stay around (cleanup comes with Phase 5).
- ~~Every role uses your default model~~: models per role came with Phase 6 (research, verification and integration default to Sonnet 5.5).

### Phase 4: Coordination (done 2026-11-14, except where noted)
- [x] **How agents reach Monaged:** every agent session gets a `monaged` MCP server. The Claude adapter starts `monaged.exe --mcp-relay` inside WSL (through WSL's Windows interop). The relay speaks MCP on stdio and forwards each tool call over a token-protected localhost connection to the running window. Windows-to-Windows localhost always works, so WSL's NAT networking doesn't matter.
- [x] **Four tools:**
  - `board_read`: the subproject's agents (role, busy or idle, owned files) plus the entries the agent hasn't seen. `all=true` shows everything, including the plan's contracts. `subproject` reads another subproject's board in the same project.
  - `board_post`: a contract, decision or note, with optional paths.
  - `claim_files`: records a claim and warns when a path overlaps another agent's owned or claimed files, in any subproject of the project. Ownership comes from the plan's workstreams (`owns`). Globs (`**`, `*`) are unit-tested.
  - `ask`: the **Haiku librarian** answers from the board and plan first. Only if it can't is the question sent to the agent (by name, workstream, or `lead` for the integrator or planner), and its short answer comes back as the tool result. A busy agent gets the question after its current turn. Two agents waiting on each other are told to post an assumption instead. A call gives up after 8 minutes; a late answer still lands on the board.
- [x] **No chatter:** new contracts and decisions (and notes that touch an agent's files) are attached to that agent's next message as a short digest, shown folded in the transcript. Answers an agent already got directly aren't repeated.
- [x] **Monaged's tools never ask you for permission;** other tools still do.
- [x] **Board tab** per subproject: open questions with how long they've waited, who owns which files, and every entry (newest first) with a Remove link.
- [x] **Briefs:** builders are told how and when to use the tools; the integrator starts with `board_read`.
- [x] **Answer turns** (`monaged-answer` block) don't count as pipeline results, don't trigger the "deliver your result" reminder, and skip the automatic review.

**Tested:**
- Two free agents, step by step: post a contract, claim files, read the board. The librarian answered a question from the board. A question it couldn't answer went to the other agent, and its answer came back. An overlapping claim got a warning.
- The full pipeline "Car reservations CLI" in auto mode, with two builders in parallel (core and cli). It ran research, 2 verifiers, the plan (auto-started) and both builders. build-core posted the details the plan had left open. The integrator read the board, then merged into the feature branch, which was auto-merged into `develop`. The merged CLI works: reserve, overlap rejection, list and cancel. Its pytest tests couldn't run because pytest isn't installed in WSL.

**Not done or not tested:**
- An agent in the middle of a long turn only sees new entries when it calls one of Monaged's tools; builders are told to `board_read` again before finishing. Claude Code has no way to push a message into a running turn.
- Cross-subproject `board_read`/`ask` are built but not exercised live.
- Standing subprojects on a schedule (for example security) moved to Later.

### Phase 5: Merging and git hosting (done 2026-11-14)
- [x] **Branch chain per project:** `develop → acceptance → main`, from whichever branches exist. **"Set up develop and acceptance"** creates them, after which new subprojects branch off `develop`.
- [x] **Rules per target** (click to change): develop **auto unless confidence 1/5**, acceptance **asks you**, the top branch (main) **you confirm twice**. With only `main`, features merge into it and that asks. Unit-tested.
- [x] **Safe merges** (always `--no-ff`):
  - in the worktree where the target is checked out, only when it has no uncommitted changes;
  - otherwise in Monaged's own merge worktree, which releases the branch afterwards.
  - Conflicts are aborted cleanly and their files listed.
- [x] **Merge card** per subproject: target, rule, confidence (integrator, else the lowest builder score), commits ready, Merge, Push, open merge/pull request, Clean up.
- [x] **Auto-merge** after a finished pipeline, once a score is in and the rule allows it. It never fires on unreviewed commits or while conflicts are open.
- [x] **Resolve conflicts with an agent:** an agent on the feature branch merges the target in and resolves the conflicts; then you merge again.
- [x] **Promotions** in the project view, with commit counts per hop and the two-step confirmation for main.
- [x] **Push** (any remote) and **GitLab/GitHub merge/pull requests** through `glab`/`gh` on Windows, which hold your logins, plus request status on the card.
- [x] **Settings:** "Use my Windows Git credentials in WSL" (Git Credential Manager), so push and clone from WSL use your logins.
- [x] **Clean up worktrees:** removes agent worktrees that have no uncommitted work, deletes branches only when everything on them is in the feature branch, and archives those agents.

**Tested on the demo:**
- set up develop and acceptance;
- feature → develop → acceptance → main, including the two-step confirmation; `main` was merged in its own checkout, which was clean;
- cleanup: 6 worktrees and 5 branches removed;
- a real conflict: detected and aborted, an agent resolved it, then merged;
- push to a local test remote.

**Not tested live:**
- creating merge/pull requests (that needs a real GitHub/GitLab repo; the `gh`/`glab` flags were checked with read-only queries);
- the credentials switch (it changes your WSL git config, so it's your click).

### Phase 6: Usage awareness (done 2026-11-14, except where noted)
- [x] **What's measured,** all from the Claude adapter, with no extra API calls and no access to your credentials:
  - **per turn:** the API-equivalent cost (`usage_update.cost`, cumulative per session; Monaged stores the difference), tokens (input, output, cache reads and writes) and the model. Saved per agent and subproject.
  - **your week:** the plan's weekly usage for *all* your Claude use (`_meta["_claude/rateLimit"]`, roughly 1% steps) and when it resets. A 5-hour window is shown too if your plan has one. This account's plan only reports the weekly window.
- [x] **Monaged's share of your week** (estimated, because it isn't reported):
  - weekly-usage increases while Monaged was busy the whole time count as Monaged's;
  - those stretches calibrate how much of a week $1 of agent work is;
  - across idle gaps (you may have used Claude yourself), only what Monaged's cost explains counts.
  - Your own parallel use can only make the estimate too high, which is the safe side for a budget. Before calibration it assumes $1 ≈ 0.2% of a week.
- [x] **Live meter** in the sidebar ("Week 3% · Monaged ≈0.1% of 25%", with a bar), cost per agent in the agent bar ("Sonnet 5.5 · $0.11"), and cost this week per subproject in its header.
- [x] **Budget** (Settings → Usage and budget):
  - Monaged may use 10/25/50/75% of your week, or no cap (default 25%);
  - automatic work also pauses when the whole week reaches 80/90/95% (default 90%);
  - a plan-limit refusal pauses it too.
  - Paused agents keep their next message, a bar shows why with **Resume anyway**, and work continues on its own when a new week starts or you raise the budget. Messages you type, and answers you click, always go through.
  - `ask` doesn't wake another agent while over budget. Haiku reviews on the Max plan (no API key) count towards the share and pause with it.
- [x] **Models per role** (Settings → Models), applied when a session opens or reopens, before its first message. Defaults: researcher, verifiers and integrator use **Sonnet 5.5**; planner, builders and your own agents use the default (Fable 5.2).
- [x] **Cost estimates on plans:** "about $X API-equivalent, ≈Y% of your week", from earlier pipelines of the same size (a rough guess before there are any). The plan gate asks you instead of auto-starting when the estimate won't fit in what's left of the budget.
- [x] **Haiku spend** (API credit vs Max) stays in Settings.

**Tested:**
- Settings, the sidebar meter and the cost labels with real data.
- A role switched to Sonnet before its first message, and a reopened agent switched as well.
- Pausing with a simulated 12% share against a 10% budget: the researcher's brief waited with the paused bar, then resumed by itself when the budget went to 25%.
- 14 unit tests, including attribution, calibration, week rollover, budget checks, estimates and rate-limit parsing.

**Not tested yet:**
- Calibration against real weekly-usage steps. It needs more Monaged work than this session's few dollars; until then the safe default applies.
- A week rolling over, and the 5-hour window (this plan doesn't report one).

**Known limits:**
- Switching an existing conversation to another model rebuilds its prompt cache once. It's cheap for new agents, more for long conversations.
- The share is an estimate. Your own Claude use at the same time pushes it up, never down.

### Design overhaul (done 2026-11-14)
Researched against Claude desktop, Zed, Nothing OS, IntelliJ (New UI), Raycast and Resend, plus UI-psychology basics (pre-attentive cues, von Restorff, Gestalt grouping, progressive disclosure, 50–75 character lines, motion as feedback). The mockup is in `design/mockup.html` next to the app folder.

- [x] **Rules:**
  - monochrome first, with one accent, red, that only ever means "needs you";
  - two font weights;
  - hairlines instead of boxes, and shadows only on floating layers;
  - small radii (4–8);
  - the machine's real data (branches, IDs, costs) in mono;
  - dot-matrix numerals only for the few numbers that matter.
- [x] **Themes:** Carbon (near-black, default), Graphite (dim grey), Clay (warm, like the Claude app), Paper (light), or follow Windows' light/dark mode. Every colour is a role in `theme.rs`, switched live from Settings → Appearance or the palette.
- [x] **Shell:**
  - **Title bar:** Monaged's own, with breadcrumbs, a centred search that opens the palette, and native minimize, maximize and close (snap layouts work).
  - **Sidebar tree:** projects › subprojects › agents with status dots and score meters, under a "Needs you" inbox.
  - **Inspector:** Pipeline, Board and Merge tabs.
  - **Status bar:** connection, distro, agents working, branch, weekly usage and Monaged's share.
- [x] **Wide screens:** the agent area splits into panes of at least 600px:
  - the focused agent, plus agents that are working or waiting on you;
  - Ctrl-click in the tree opens one beside it;
  - spare room shows the focused agent's Changes next to its conversation.
  - 3440px at 125% scaling gives three panes plus the inspector. A narrow window shows one agent with a Conversation/Changes switch.
- [x] **Reading:** a 680px column (about 72 characters), Markdown at 14/22, your messages as quiet bubbles, tool calls as one mono line with an icon each.
- [x] **Command palette (Ctrl K):** jump to any project, subproject or agent, or run an action or switch theme; arrows to move, Enter to run.
- [x] **Shortcuts:** Ctrl B sidebar, Ctrl Alt B inspector, Ctrl , settings.
- [x] **Icons:** 40 line icons drawn for Monaged (16px, 1.4px stroke), embedded in the binary.
- [x] **Motion:** working agents and the current pipeline stage pulse slowly, and the palette fades in over 140ms. Everything else is instant, and nothing moves when Windows' "Show animations" is off.

**Not done yet:** a combined diff for a whole subproject.

### UX pass (done 2026-11-14)
One rule for where a message goes:
- an **event** (something happened) is a toast, bottom right: information fades after a few seconds, errors stay until closed and have a Copy button;
- a **condition** (offline, not logged in, paused by the budget) is a bar above the content until it's fixed;
- a **problem with a field** shows under that field.

- [x] **Toasts** replace the single banner. Long jobs (merging, copying the git identity) show a spinner toast that turns into the result. A toast can carry one action: Open, Retry, Reconnect, Fix in Settings, Copy.
- [x] **Background agents tell you:** a free agent you aren't looking at says when it finished; any hidden agent says when it asks for permission. A failed session offers Retry.
- [x] **Connection:** when the Claude process dies, Monaged reconnects by itself after 2, 6 and 15 seconds, with a countdown in a bar and the status bar, then waits for **Reconnect now**. The bar has **Copy details**.
- [x] **Loading:**
  - a spinner wherever something runs for seconds (making a worktree, reviewing, reading a diff, summaries);
  - skeletons where content is about to appear (reopening a conversation, the diff, the Merge tab), so nothing jumps;
  - in agent panes, the session's own state: waiting for Claude, starting, reopening, failed with **Try again**.
- [x] **Working line:** while a turn runs, the end of the conversation says what the agent is doing (thinking, writing, running a tool, waiting for your answer) and for how long, with Esc to stop.
- [x] **Latest:** scrolled up while an agent writes? A **Latest** button takes you back down.
- [x] **Menus:** right-click any project, subproject or agent in the tree, or use the ⋯ that appears when you point at a row or in an agent's header: open, open beside, show changes, review, open in Zed, copy path or branch, stop, restart the session, pipeline/board/merge, clean up. Arrows and Enter work in menus.
- [x] **Confirm dialogs** for steps that can't be undone: stopping a pipeline, removing the API key, cleaning up worktrees, promoting into the top branch. Enter confirms, Esc cancels.
- [x] **Buttons:** pressed states, a busy state with a spinner that keeps the button's size, a disabled state with the reason in a tooltip (for example "Merge" with nothing to merge), and primary and danger colours that hold in every theme.
- [x] **Forms:** the field that's wrong turns red with the problem under it, and both clear as you type. Submitting shows "Cloning…" or "Setting up…" on the button.
- [x] **Settings:** a section list on the left that jumps to a section and follows your scrolling.
- [x] **Keyboard:** Ctrl / shows every shortcut, Alt ↑/↓ moves between agents, Ctrl N makes an agent in the current subproject. Tooltips show the shortcut for their button.
- [x] **Window:** Monaged remembers its size, position and maximized state, and whether the sidebar was open.
- [x] **Fixed:** long replies were measured narrower than they're drawn, so their last lines (and the notes under them) sat past the end of the scroll area. The conversation column now gets an exact width.

### Agent UI, Git and the rest of the ideas list (done 2026-11-14, version 0.2.0)
**The agent protocol, as a UI.** Monaged shows what the agent reports instead of assuming Claude, so another ACP agent's options appear the same way.
- [x] **Session settings as chips** under the message box, from the agent's `configOptions`: mode (Manual, Accept edits, Plan, Auto, Bypass), model, effort, fast mode, ultracode, persona. Each opens a menu; changes go through `session/set_config_option`.
- [x] **Question forms:** Monaged announces `elicitation` support, so Claude's question cards and MCP input or sign-in requests appear in the conversation as forms (single choice, multiple choice, own words, yes/no, numbers, links). The answer goes back; Esc cancels. Off-screen ones raise a toast and a Windows notification.
- [x] **Slash commands:** typing `/` lists Monaged's own (`/review`, `/stop`, `/model`, `/mode`, `/effort`, `/changes`, `/restart`, `/rename`) and the agent's (from `available_commands_update`); ↑↓ and Tab or Enter pick.
- [x] **Tool calls open up:** their input, the files they touched (click opens Zed), output and diffs. Notices, compaction and the session's title are shown.
- [x] **Agent profiles** (Settings → Agents): the ACP command Monaged runs, what the agent reports it can do, and profiles for other agents. Switching reconnects.

**Messages.**
- [x] Multi-line message box: Shift+Enter adds a line (or Ctrl+Enter sends, Settings → Message box). ↑ at the top walks back through what you sent.
- [x] Attachments: paste an image, drop files from Explorer, or pick them; images and text files go as ACP content blocks.
- [x] Messages typed while an agent works are queued for after its turn; queued messages survive a restart.

**Agents and search.**
- [x] Rename, archive (with undo) and restore agents from their menu; archived ones open read-only.
- [x] Ctrl F finds in a conversation; Ctrl Shift F and Ctrl K search every loaded conversation; Ctrl K also lists the worktree's files (opens Zed).

**Seeing what happened.**
- [x] A subproject's combined changes: the feature branch plus every agent's worktree, file by file, with overlaps marked.
- [x] Comments on diff lines, sent to the agent as one message.
- [x] Timeline tab: turns (model, cost), reviews, board posts, agents joining, merges, pushes, pipeline stages.
- [x] Windows notifications through a tray icon (no registration needed), with a flashing taskbar button; clicking one opens the agent.

**Pipeline and merging.**
- [x] Edit the plan (title, summary, workstreams, owned files) before approving.
- [x] Run one stage, or one builder, again.
- [x] Merge queue on the overview: ready first, confidence, conflicts, the merge/pull request, the branch's latest CI run (GitHub Actions or GitLab pipelines), "Merge N ready" one at a time.

**Git.**
- [x] Git tab: a worktree's changes (discard asks first), commit with a suggested message, stash; origin distances with fetch, fast-forward pull and push; finishing a conflicted merge with a side per file; the history graph, and any commit's diff.
- [x] Settings → Git: the identity git uses in WSL, auto-fetch, suggestions on/off.

**Layout.**
- [x] Drag the sidebar, inspector and pane edges (double-click resets); saved layouts per project; zen mode (F11); compact density.

**Reliability and shipping.**
- [x] Health check page with fixes; open where you left off; the data folder.
- [x] Unsigned per-user installer (`Monaged-Setup-<version>.exe`, no admin, no registry), updates from the private repo's GitHub releases, `scripts/release.ps1`, Monaged's own icon.

**Not done in this batch:** conversations of agents whose sessions aren't open yet aren't searchable (their history loads with the session); a signed installer.

### Next ideas
- Fork an agent: a new worktree from its branch, with a summary of the conversation as the brief.
- A morning summary: what finished overnight, what waits for you, what it cost.
- Drag an agent into a pane; an accent per project (never red).
- Search conversations that aren't open (store transcripts, or ask the agent for `session/list`).
- Several agents at once (Claude and another ACP agent side by side, one connection each).
- The agent's terminal output (ACP `terminal/*`), and reading and writing files through Monaged (`fs/*`) so edits can be reviewed before they land.
- A signed installer (a code-signing certificate), and a delta updater.

### Later
- Standing subprojects on a schedule (for example a weekly security pass)
- Recording conversations into Delta threads (the Delta CLI has a producer API: `thread create/append/capture`)
- External subprojects (links between projects), Gitea, phone notifications

## Technical spikes to resolve
- ~~How the Monaged MCP server is reached from WSL~~: solved with a stdio relay (`monaged.exe --mcp-relay`) started through WSL interop, see Phase 4.
- ~~Reading Max-plan usage and limits programmatically~~: the adapter reports weekly usage and per-session cost in `usage_update`, see Phase 6.
- Resource use of about 10 concurrent Claude processes in WSL (the WSL VM has 31 GB).
