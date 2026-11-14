# User guide

New here? Start with [Getting started](getting-started.md). The playground project (Ctrl K → Create the playground project) has a hands-on tour of everything below.

## The window

- **Sidebar** (left): your projects › subprojects › agents, with a status dot per agent (working, done, needs you) and a score meter once an agent has been reviewed. A **Needs you** inbox sits on top: questions, plans waiting for approval, permission requests, failed sessions.
- **Agents** (middle): the focused agent's conversation. Wide screens show several agents side by side (Ctrl-click an agent to open it beside the current one), and spare room shows the focused agent's **Changes** next to its conversation.
- **Inspector** (right): Pipeline, Board, Merge, Git, Run and Timeline for the subproject you're in.
- **Status bar**: the connection, the distro, how many agents are working, the branch, your weekly usage and Monaged's share of it.

Messages follow one rule: things that *happened* appear bottom right and fade (errors stay until closed); things that are *true right now* (offline, not signed in, paused by the budget) sit in a bar at the top until they're fixed.

## Projects, subprojects and agents

- **A project** is one git repository inside WSL. Add one with **+** next to Projects: clone, pick a folder, or start empty. Monaged makes sure it has a commit and uses `develop` as the base branch when it exists.
- **A subproject** is a *feature* (branch `feature/<name>`) or a *standing* concern such as security (branch `standing/<name>`). Its first agent starts by itself.
- **An agent** (**Ctrl N**, or **+ New agent**) gets its own worktree at `~/code/.monaged/<repo>/<subproject>/<agent>` on branch `agent/<subproject>/<agent>`, then a Claude session there. Agents work in parallel without touching each other's files or your own checkout.
- Everything is saved in `%APPDATA%\Monaged\monaged.db`; opening an agent after a restart reopens its conversation.
- Right-click a project, subproject or agent (or use its **⋯**) for everything you can do with it: open beside, show changes, review, open in Zed, copy path or branch, stop, restart the session, rename, archive, clean up.

## Talking to an agent

- **Enter** sends, **Shift Enter** adds a line (swap them in Settings → Message box). **↑** on the first line brings back what you sent.
- **/** lists commands: Monaged's own (`/review`, `/stop`, `/model`, `/mode`, `/effort`, `/changes`, `/restart`, `/rename`) and the agent's, including your project's `.claude/commands`.
- **The chips** under the message box set the session's mode (Manual, Accept edits, Plan, Auto…), model and effort.
- **Attachments**: paste an image, drop files from Explorer, or use the paperclip.
- **Queue**: messages typed while the agent works wait for its turn to end, and survive a restart.
- **Questions**: when an agent asks something, a form appears in its conversation; it waits for your answer.
- **Esc** stops the agent.
- **Undo**: every turn is checkpointed. Point at one of your messages and **↶** rolls the worktree back to before it (with Undo). Right-click an agent → **Replay** steps through its turns with each turn's diff.
- **Side chat** (**Ctrl J**): ask Haiku about an agent's work beside its conversation; the agent never sees it.

## Reviewing what an agent did

The **Changes** tab shows the agent's diff against where its branch started, a summary written by Haiku, and a **1–5 confidence score** that always lists its reasons:

- the project's tests run first (from Project settings → Run, `monaged.json`, or detected); failing tests cap the score at 2, no tests at 4;
- then a short Haiku review asked to find what's broken; "partly" caps the score at 3, "broken" at 2.

Summaries and reviews run after every turn (Settings → Reviews turns that off), or on demand with **Review** and **Summarize**. Click a diff line to comment; comments go to the agent as one message. A subproject's **All changes** shows the feature branch plus every agent's worktree, with overlaps marked.

## The pipeline

**Run pipeline** (in a new feature subproject, or Ctrl K) takes a sentence and runs:

1. **Research**: one agent reads the code and reports.
2. **Verify**: two agents check the report against the code.
3. **Decide**: "do we build this?" You decide, or auto mode does when no verifier found major issues.
4. **Plan**: workstreams that can run in parallel, each owning its files, with contracts between them, a size, risk flags, a confidence score and an estimated cost.
5. **Approve**: small plans with confidence 4–5 and no risk flags start by themselves in auto mode (when the project allows it). Large, risky (migrations, auth, data, public APIs, infra, security) or uncertain plans always wait for you. **Edit plan** changes it before approving.
6. **Build**: 1–10 builders in parallel, each on its own branch.
7. **Integrate**: an integrator merges the builders' branches into the feature branch and runs the tests.
8. **Done**: when the branch rules and the score allow it, the feature merges into `develop` by itself.

Questions from pipeline agents show as cards with options (the recommended one is marked). Any stage can be run again from the Pipeline tab.

## Agents working together

Every agent has Monaged's tools: `board_read`, `board_post` (contracts, decisions, notes), `claim_files` (warns about overlapping edits) and `ask` (another agent, by name, or `lead`).

- Questions go to a Haiku **librarian** first, which answers from the board and the plan. Only if it can't is the other agent asked, and its answer comes back to the asker.
- New contracts and decisions are attached to each agent's next message, so agents don't poll or chat.
- The **Board** tab shows open questions, who owns which files, and every entry.

## Merging

- **The project view** shows your branch chain (develop → acceptance → main) with a rule per target you can click to change: develop merges automatically unless the score is 1, acceptance asks you, the top branch asks twice. **Set up develop and acceptance** creates them.
- **The Merge tab**: merge, push, open a GitHub pull request or GitLab merge request (through `gh`/`glab`), resolve conflicts with an agent, and clean up worktrees.
- **The merge queue** (project overview): ready first, with confidence, conflicts, the request and its CI status; **Merge N ready** merges them one at a time.
- **The Git tab**: a worktree's changes, commit with a suggested message, stash, fetch, pull and push, finish a conflicted merge, and the history graph.

## Running and testing

The **Run** tab starts a worktree's dev server in its own process group (Stop ends all of it), shows its output and port with an **Open** button, runs the tests once or three times to find flaky ones, and sends failures to the agent. Commands come from Project settings → Run, else `monaged.json`, else detection (package.json, Cargo, go.mod, Python, Makefile).

## Project settings

Right-click a project → **Project settings**:

- **MCP servers** for its agents (stdio commands or HTTP URLs), each with a **Test** that lists the server's tools.
- **Knowledge**: CLAUDE.md, skills, subagents and commands in the repo (save, or save and commit), plus shared notes above every worktree that aren't committed.
- **Run**: dev and test commands and the port.
- **Environment**: variables per environment (Development, Test…), encrypted with Windows DPAPI. The Run tab and MCP servers (`${NAME}`) get them; agents only see names and run commands with them through `run_with_env` after you approve. Values are masked everywhere, flagged in diffs, and block merges and pushes.

## Usage and budget

- The sidebar meter shows your week (all your Claude use) and Monaged's estimated share of it. Agent headers show each agent's model and API-equivalent cost; subproject headers show this week's cost.
- **Budget** (Settings → Usage and budget): Monaged may use a share of your week (25% by default) and pauses automatic work when the whole week reaches a ceiling (90% by default). Paused work continues by itself; your own messages always go through.
- **Spending cap**: right-click a subproject → Set a spending cap; its agents pause when they reach it.
- **Models per role** (Settings → Models): research, verification and integration default to Sonnet; everything else to your default model.
- **Summaries and reviews** run on Haiku: with an API key (Settings → Reviews, stored in Windows Credential Manager) they're paid from API credit; without one they use your plan through the Claude CLI.

## Keyboard

| Keys | Does |
|---|---|
| Ctrl K | Command palette: jump anywhere, run any action, switch theme, search files and conversations |
| Ctrl N | New agent in this subproject |
| Ctrl B / Ctrl Alt B | Toggle the sidebar / the inspector |
| Ctrl , | Settings |
| Ctrl / | Every shortcut |
| Alt ↑ / Alt ↓ | Previous / next agent |
| Ctrl J | Side chat |
| Ctrl F / Ctrl Shift F | Find in this conversation / search all of them |
| Ctrl 0 | Move through the sidebar with the arrow keys |
| F11 | Zen mode |
| Esc | Stop the agent, or close whatever is open |

Change any of them in Settings → Keyboard.

## Appearance

Settings → Appearance: **Carbon** (near-black, the default), **Graphite**, **Clay**, **Paper** (light), or follow Windows. Drag the sidebar, inspector and pane edges (double-click resets), save layouts per project, and switch to compact density in Settings → General.

## Updates

Monaged checks for a new release once a day (Settings → Updates). **Update** downloads it, checks it against the release's checksums and swaps it in; **Restart now** uses it. Only an installed copy updates itself.
