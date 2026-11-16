# User guide

New here? Start with [Getting started](getting-started.md). The playground project (Ctrl K → Create the playground project) has a hands-on tour of everything below.

## The window

- **Sidebar** (left): your projects › subprojects › agents, always in the same order. Each agent has a status dot (working, done, needs you); on the right, how long it's been working, "needs you", or its score once it's been reviewed. **New replies are bold** with a white dot: an agent that answered something you asked while you were looking elsewhere, until you open it (the subproject and a folded project are bold too). Subprojects count what needs you or is working ("2 working"), and a folded project still says what's inside. Point at an agent for its role, state and score. A **Needs you** inbox sits on top (F8 goes through it): questions, plans waiting for approval, permission requests, failed sessions.
- **Agents** (middle): the focused agent's conversation. Wide screens show several agents side by side (Ctrl-click an agent to open it beside the current one), and spare room shows the focused agent's **Changes** next to its conversation. A pane's **×** closes it (it stays closed until you open that agent again), and **Show only this agent** (the maximize button in its header, or Ctrl K → "Show one agent at a time") keeps it to one agent until you switch back; Ctrl K → "Don't show changes beside the agent" turns the extra Changes column off.
- **The dock** (right): **Pipeline** (with the agents' board), **Merge**, **Git**, **Run** and **Activity** for the subproject you're in (Alt 1–5, or click). Each tab says what's going on before you open it: a red dot when something needs you there (a decision, conflicts, failing tests), a pulsing dot while something runs, a white dot on Merge when work at the quality bar is ready, and counts for board posts and uncommitted files. The button at its right hides it (Ctrl Alt B brings it back). Git and Run show the worktree of the agent you open, until you pick one from the **Worktree** menu.
- **Status bar**: the connection, the distro, how many agents are working, the branch, your weekly usage and Monaged's share of it.

Messages follow one rule: things that *happened* appear bottom right and fade (errors stay until closed); things that are *true right now* (offline, not signed in, paused by the budget) sit in a bar at the top until they're fixed.

## Projects, subprojects and agents

- **A project** is one git repository inside WSL. Add one with **+** next to Projects: clone, pick a folder, or start empty. Monaged makes sure it has a commit and uses `develop` as the base branch when it exists.
- **A subproject** is a *feature* (branch `feature/<name>`) or a *standing* concern such as security (branch `standing/<name>`). Its first agent starts by itself. Clicking a subproject opens its **dashboard** (below).
- **An agent** (**Ctrl N**, or **+ New agent**) gets its own worktree at `~/code/.monaged/<repo>/<subproject>/<agent>` on branch `agent/<subproject>/<agent>`, then a Claude session there. Agents work in parallel without touching each other's files or your own checkout.
- Everything is saved in `%APPDATA%\Monaged\monaged.db`; opening an agent after a restart reopens its conversation.
- Right-click a project, subproject or agent (or use its **⋯**) for everything you can do with it: open beside, show changes, review, open in Zed, copy path or branch, stop, restart the session, rename, archive, clean up.

## The dashboard

A subproject's dashboard is the one place for what you need to do there:

- **The request** you started it with, and one line on where it stands. With a pipeline, its stages and **Auto mode** sit on top, with **Stop pipeline** or **Run pipeline**, **All changes** and **New agent**.
- **Needs you**: every agent's open questions (answered right there, the same card as in the conversation), permission request, form and secret request; the pipeline's "build this?" and the plan to approve; work held below the bar (see the quality gate); a failed session to restart; and the merge, when it waits for you. You answer here; **Open** takes you to the agent when you want the whole conversation.
- **Agents**: each one's state, score, findings and what it's doing, with Open, Changes, Review, **Fix findings** and Stop.
- **The lead**, on the right: an agent that oversees the others. Ask it how things stand, or tell it what should happen next ("tell the API builder to use UUIDs", "start an agent on the README"); it passes it on. It doesn't write code itself, and it can't accept work below the bar for you. Monaged tells it what happens (a builder passed, was sent back or waits for you), and it answers in a sentence. Every pipeline starts one (Settings → Agents turns that off); elsewhere **Start the lead** does. On a narrow window it opens as a pane.

The inbox opens the dashboard too, and the dashboard button in an agent's header goes back to it.

## Small things that save time

- **F8** goes to the next subproject that waits for you (its dashboard, where you answer it); Ctrl K → "Go to what needs you" too.
- **While you were away**: come back to Monaged after 10 minutes or more and a card says what happened: how many agents worked and what it cost, reviews, merges, pipelines that finished, what failed and what waits for you.
- **Names**: an agent you start gets its name from your first message ("Please fix the failing coupon test" → `fix-failing-coupon`); rename it any time.
- **Quick replies**: when an agent ends on a question ("Want me to look at it next?"), "Yes, go ahead" and "No, that's all for now" sit above the message box.
- **The PC stays awake** while agents work or reviews run (the screen can still turn off), and **closing Monaged asks first** when agents are working, since that stops them. Settings → General turns either off.
- **The sidebar folds** a pipeline's research, verification and planning agents into one row once their part is done.
- An agent's menu: **Open a terminal in the worktree** (projects and subprojects have one for the repo) and **Copy the conversation** (Markdown, secrets masked, to paste into a chat).

## Talking to an agent

- **Enter** sends, **Shift Enter** adds a line (swap them in Settings → Message box). **↑** on the first line brings back what you sent.
- **/** lists commands: Monaged's own (`/review`, `/stop`, `/model`, `/mode`, `/effort`, `/changes`, `/restart`, `/rename`) and the agent's, including your project's `.claude/commands`.
- **The chips** under the message box set the session's mode (Manual, Accept edits, Plan, Auto…), model and effort.
- **Attachments**: paste an image, drop files from Explorer, or use the paperclip.
- **Queue**: messages typed while the agent works wait for its turn to end, and survive a restart.
- **Questions**: an agent's questions come as one card, numbered, each with why it matters and its options (what each one means, the recommended one marked; checkboxes when several can apply). **Something else…** on any question opens a box for your own words. Nothing goes out until you press **Send** (or Enter in a box), so you can change your mind; **Use the recommended answers** fills in the rest. Afterwards the card shows what you answered. You can still reply in the message box instead. Forms a tool asks you to fill in appear in the conversation too, and wait for you.
- **Steps fold into one line**: a run of tool calls (and the agent's thinking) shows as one line that says what it did, such as "Read 4 files, searched twice and ran a command · 7 steps", with failures in red; click it to see each step. Settings → General (or Ctrl K → "Show every step of an agent") shows every step instead.
- **While it works**, the line under the conversation names the step that's running (the command, the file), the step number and the time, with **Stop** (or Esc).
- **When a turn ends**: how long it took, how many steps, and how many files it changed (click to open its changes); **Copy reply** appears when you point at it.
- **Esc** stops the agent.
- **Undo**: every turn is checkpointed. Point at one of your messages and **↶** rolls the worktree back to before it (with Undo). Right-click an agent → **Replay** steps through its turns with each turn's diff.
- **Side chat** (**Ctrl J**): ask Haiku about an agent's work beside its conversation; the agent never sees it.
- **What agents write is formatted**: headings, lists, task lists, tables, ~~strikethrough~~, and code with its language and a **Copy** button (JSON and shell are recognised when the agent doesn't say). Your own messages are formatted when they use Markdown. Click a tool call to see it: its input laid out field by field (a command as a shell line, an edit as a diff, file contents highlighted), and JSON output highlighted.

## Reviewing what an agent did

The **Changes** tab shows the agent's diff against where its branch started, a summary written by Haiku, and a **1–5 confidence score** that always lists its reasons:

- the project's tests run first (from Project settings → Run, `monaged.json`, or detected); failing tests cap the score at 2, no tests at 4;
- then a short Haiku review asked to find what's broken; "partly" caps the score at 3, "broken" at 2.

The reviewer doesn't only see the diff: it gets the task and what you said to the agent after it, the project's `CLAUDE.md` and the shared notes for its agents (so it follows the project's conventions instead of flagging them), the agent's own account of its work (as a claim to check), the changed files in full (so it can see what the diff calls), and the findings you ruled out before.

- **Fix findings** (next to Review) sends the findings and any failing tests to the agent as one message. It's asked to check each one first and say so when one isn't a real problem; when it's done, the change is reviewed again.
- **✓** next to a finding rules it out: say why if you like (optional). The score is worked out again without it, and the next reviews in this project are told, so the same wrong finding doesn't come back.

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

**The quality gate.** A builder or the integrator saying it's done isn't enough: Monaged reviews the work first, and it needs **4/5** (Settings → Reviews: 3, 4 or 5) to count. Below that, the findings and failing tests go back to the agent to fix, twice; then it waits for you on the dashboard with **Fix again**, **Review again**, **Changes** and **Accept anyway** (which is logged). Integration starts when every builder is through, and merging into `develop` by itself needs the same score. When reviews are off, the work waits for you instead of going through unchecked. Tests that already failed on the branch the agent started from don't count against it: when the tests fail, Monaged runs them on that branch too, and only failures that are new are the agent's.

Questions from pipeline agents show as one card with their options (the recommended ones marked) and room for your own answers; auto mode answers with the recommended options. Any stage can be run again from the Pipeline tab.

## Agents working together

Every agent has Monaged's tools: `board_read`, `board_post` (contracts, decisions, notes), `claim_files` (warns about overlapping edits) and `ask` (another agent, by name, or `lead`).

- Questions go to a Haiku **librarian** first, which answers from the board and the plan. Only if it can't is the other agent asked, and its answer comes back to the asker.
- New contracts and decisions are attached to each agent's next message, so agents don't poll or chat.
- The **Pipeline** tab shows the board: open questions, the latest entries (all of them on request) and who owns which files.

## Merging

- **The project view** shows your branch chain (develop → acceptance → main) with a rule on each hop; click it for a menu: **Automatically** (develop's default: once the integrator's latest work is reviewed at the quality bar, 4/5 by default), **Ask me** (acceptance's default), **Confirm twice** (the top branch's). Under each branch it says how it stands against origin: "in sync", or "5 to push" / "2 to pull", which push or pull it when you click. **Set up develop and acceptance** creates them. The refresh button fetches from origin and reads the branches again.
- **How merges reach a branch** (**Merges** next to origin in the project view, **How** in the Merge tab; both open a menu):
  - **Here, then push** (the default with an origin): Monaged fetches first and catches the target up with origin, merges, then pushes the target. When the target moved both here and on origin, it stops and asks you to pull it first, so it never pushes over someone else's work. A push that's refused (a protected branch) says so and suggests the next method.
  - **Here only**: the same merge, without the push.
  - **Through pull requests** (GitHub) or **merge requests** (GitLab): Monaged pushes the branch, opens the request with a description from the work (what each agent did, the checks, the score and open findings), and merges it on GitHub or GitLab once the host allows it (required reviews and checks); then it catches the local target up. Below the quality bar the request opens as a draft, and becomes ready once the score reaches the bar. Promotions (develop → acceptance → main) go through requests too.
- **The Merge tab** says where the subproject's work stands in one line ("1 commit ready to merge", "Conflicts with develop in 1 file", "Merged into develop at 3f2a1c9"), then offers the one thing to do next as its button: merge (by the project's method), or with conflicts, **Resolve conflicts with an agent**. Under it, what else you can do and what it would act on: **Update from develop** when develop has commits the branch doesn't (conflicts show up then and go to an agent, instead of at merge time), choose sides yourself (Git tab), push the branch, open or view the pull or merge request (through the REST API with your Git account, or `gh`/`glab` when you have them), and clean up worktrees. Below: the conflicted files, the request with its CI and why the host is waiting, and **How it merges** (the rule, the method, the confidence and the branch against origin).
- **Fetching**: every 15 minutes by default (Settings → Git), and before every merge; open requests and running CI are re-checked about once a minute while you're in the project.
- **The merge queue** (project overview): ready first, with confidence, conflicts, the request and its CI status; **Merge N ready** merges them one at a time, and only work at the quality bar counts as ready. Subprojects with nothing new fold into one line under it.
- **The Git tab**: a worktree's changes; commit everything or only the files you tick, with a suggested message, or amend the last commit; take back a commit that isn't pushed yet; stash, fetch, pull (a merge when both sides moved, never a rebase) and push any branch that's ahead of origin (develop after merging here, say); finish a conflicted merge; and the history graph. Its **Origin** row says which account the project signs in as, switches account, tests the connection, and flips origin between HTTPS and SSH.

## Git accounts

Settings → **Git accounts** signs git in to GitHub, GitLab (Enterprise and self-hosted too) or any other host, with nothing to register and no server in between:

- **In the browser**: Monaged opens the host's own token page with the right permissions ticked. Click Generate and Copy; Monaged picks the token up from the clipboard (only something with the host's token shape, copied after you started), checks it with the host and empties the clipboard.
- **By pasting a token**, with a username and app password for other hosts, or by **importing** a login the GitHub CLI, the GitLab CLI or Git Credential Manager already has.

Tokens are checked with the host (who you are, your email, the token's scopes and expiry; tokens that expire within two weeks are flagged here and in the health check), kept in Windows Credential Manager, and never written to the database, a file in WSL or a command line. Monaged becomes git's credential helper in WSL for each host with an account (`monaged.exe --git-credential`), so Git Credential Manager isn't needed; if you do use it, one switch makes it the helper for other hosts. Several accounts per host work: one is the default, and a project can sign in as another from its Git tab. **Commit as this account** makes its name and email git's identity in WSL. Tokens are masked like secrets and block merges and pushes that would contain one.

**SSH keys** in WSL are listed here too: create one, add it to your GitHub or GitLab account in one click, test it per host, and trust a host's key.

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
- **Summaries and reviews** run on Haiku: with an API key (Settings → API key, stored in Windows Credential Manager) they're paid from API credit; without one they use your plan through the Claude CLI.

## Keyboard

| Keys | Does |
|---|---|
| Ctrl K | Command palette: jump anywhere, run any action, switch theme, search files and conversations |
| Ctrl N | New agent in this subproject |
| Ctrl B / Ctrl Alt B | Toggle the sidebar / the dock |
| Alt 1 … Alt 5 | The dock's Pipeline, Merge, Git, Run, Activity (again hides it) |
| Ctrl , | Settings |
| Ctrl / | Every shortcut |
| Alt ↑ / Alt ↓ | Previous / next agent |
| Ctrl J | Side chat |
| Ctrl F / Ctrl Shift F | Find in this conversation / search all of them |
| Ctrl 0 | Move through the sidebar with the arrow keys |
| F11 | Zen mode |
| Esc | Stop the agent, or close whatever is open |

Change any of them in Settings → Keyboard.

## Sounds

Monaged plays a few short sounds, only for moments you might miss because you're looking elsewhere:

| Sound | When |
|---|---|
| **Needs you**: two quick, bright notes going up | An agent asks permission or a question, or a plan or decision waits for you |
| **An agent is done**: a soft rising pair | An agent finished its turn while you were looking at something else |
| **A feature is complete**: a rising run of four notes | A pipeline merged its feature together, or setting up is done |
| **Finished**: a short, quiet lift | Something you started that takes a while worked: an install, a clone, a merge, a push, a sign-in, an update |
| **Something went wrong**: two low notes going down | A session failed, the connection to Claude dropped, or something you started failed |

Nothing plays for messages, tool calls, typing or clicks, or for the agent you're looking at (unless you want that too). Several things at once play as one sound, and the most important one wins. Sounds stay off while you present, while a full-screen game or video runs, and while you're away from the PC. While they're on, Monaged's Windows notifications come without Windows' own ding, so nothing plays twice.

Settings → Sounds turns them off or each one separately, sets the volume (quiet, normal, loud) and plays each one. **Ctrl K → Mute sounds** silences them in one go.

## Letting Claude look at Monaged

Settings → Connect Claude, or Ctrl K:

- **Export for analysis** (also in a project's menu): a Markdown report and a zip in your Downloads folder. The report is made to read, or to paste or upload into any chat with Claude: what waits for you, each subproject's request, pipeline, plan and quality gate, every agent's state, summary, score and findings, the board, the timeline and the settings. The zip adds every conversation in full (from Claude's session files when one isn't open), the data as JSON, git's view of each project, Monaged's log and a screenshot of the window. Secret values and anything that looks like a token or key are masked.
- **Connect Claude** (an MCP server): turn on "Let Claude Desktop or Claude Code read Monaged", then **Add to Claude Desktop** (restart it after), **Add to Claude Code in WSL**, or copy the command for Claude Code on Windows. Claude then has these tools: `status`, `projects`, `subproject`, `agent`, `changes`, `inbox`, `log`, `screenshot`, `show` (switches what Monaged shows, so a screenshot can follow) and `export`. With "Let Claude act too" it can also send agents messages, stop them and run reviews. It works only while Monaged runs and the switch is on; agents run as you, so they could reach it too: leave actions off unless you're debugging with Claude.

## Security

- Agents get **no git tokens** by default: Monaged pushes and opens pull requests itself, so git in an agent's commands has no credential helpers (Settings → Git accounts can allow it).
- With Claude Code's **sandbox** on for a project (Project settings), agents' commands can't see Windows drives, Windows programs or other processes, and the review runs the tests in the same kind of fence.
- Everything sent to Haiku (summaries, reviews, commit messages, the side chat) is masked first; links only open when they're web pages; each agent's connection to Monaged has its own key.
- Without the sandbox, agents run as you inside WSL: Monaged keeps its own doors shut, but can't fence an agent off from everything you can reach.

## Appearance

Settings → Appearance: **Carbon** (near-black, the default), **Graphite**, **Clay**, **Paper** (light), or follow Windows. Drag the sidebar, inspector and pane edges (double-click resets), save layouts per project, and switch to compact density in Settings → General.

## Updates

Monaged checks for a new release once a day (Settings → Updates). **Update** downloads it, checks it against the release's checksums and swaps it in; **Restart now** uses it. Only an installed copy updates itself.
