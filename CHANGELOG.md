# Changelog

All notable changes to Monaged. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **A subproject's dashboard drew its sections on top of each other on wide screens**: once the column reached its full width, the request card, the "Needs you" cards and the agent cards were laid out shorter than their text, so the next section started over them. The column now has an exact width, like the agent cards.

## [0.8.0] - 2026-11-16

Git work that knows about origin and can go through pull and merge requests, conversations you can read at a glance, a dock that says what's going on, and a sidebar that shows which agents have news for you.

### Added

- **New replies stand out in the sidebar**: an agent that answered something you asked while you were elsewhere is bold with a white dot until you open it, and so are its subproject and a folded project. Briefs, review rounds and lead updates from Monaged don't count, so pipelines don't light up the tree.
- **The sidebar says more without getting longer**: a working agent shows how long it's been at it (so a stuck dot can't pass for work), one that needs you says so, subprojects count what needs you or is working, a folded project sums up what's inside, and pointing at an agent gives its role, state and score. The Needs you header shows F8, and "N more" when the list is longer than six.
- **Conversations you can read at a glance**: an agent's steps (files read, commands run, edits, its thinking) fold into one line that says what they did ("Read 4 files, searched twice and ran a command · 7 steps", failures in red) and opens on click; Settings → General shows every step instead. Each turn ends with how long it took, how many steps and how many files it changed (click for the changes), with **Copy reply**. While an agent works, the line under the conversation names the running step, the step number and the time, with a Stop button.
- **Formatted agent replies**: tables, task lists and strikethrough; code blocks say their language and have a Copy button, and unlabelled JSON and shell are recognised and highlighted. Your own messages (and Monaged's to agents) are formatted when they use Markdown.
- **Tool calls you can read**: a call's input is laid out field by field instead of raw JSON (a command as a shell line, an edit as a diff, file contents highlighted in their language), and JSON output is highlighted.
- **The dock tells you what's going on**: each tab has a badge (needs you, running, ready to merge, board posts, uncommitted files), Alt 1–5 switch tabs (again hides the dock), and a button hides it.
- **How merges reach a branch**, per project: here then push (the default with an origin), here only, or through GitHub pull requests or GitLab merge requests, merged on the host once its reviews and checks allow it (protected branches work). Pick it from the menu in the project view or the Merge tab.
- **Update from develop**: when develop has commits a feature branch doesn't, one button brings them in (after catching develop up with origin), so conflicts show up early and can go to an agent.
- **Better pull and merge requests**: the description says what each agent did, how it was checked, the score with its reasons and any open findings; below the quality bar the request opens as a draft and is marked ready once the score reaches the bar.

### Changed

- **Merges know about origin**: Monaged fetches first, catches the target up when origin moved on, and stops (instead of making a branch that can't be pushed) when the target went its own way both here and on origin.
- **Fetching is on by default** (every 15 minutes), and open requests and running CI are re-checked about once a minute while you're in the project.
- **The dock, reorganised**: five tabs (Pipeline, Merge, Git, Run, Activity). The board lives in Pipeline (the latest three entries, all on request), next to who owns which files; plan and budget are one section; the steps' details line up.
- **Worktree menus**: Git and Run pick a worktree from a menu (it was a wall of chips) and follow the agent you open until you choose one.
- **Activity** (was Timeline) hides each turn's entry unless you ask for them, so reviews, merges and the pipeline's steps stand out.
- **The Merge tab, redone**: one line says where the work stands, one button does the next thing (merge, or resolve conflicts), and the other actions are a list that says what each would act on ("6 new commits", "1 to push", "as a draft"). The pull or merge request shows its CI and why the host is waiting; the rules sit under **How it merges**.
- **Rules and the merge method open a menu** with every choice explained, instead of changing each time you click.
- **Branches against origin**: the project view says under each branch whether it's in sync, or how many commits there are to push or pull, and pushes or pulls it in one click. The Git tab says "5 to push" instead of "↑5 ↓0" and can push any branch that's ahead (it could only push the feature branch).
- The project view's refresh button fetches from origin; the Git tab says how often fetching happens.
- The merge queue folds subprojects with nothing new into one line.

### Fixed

- The merge queue counted a feature as ready at 3/5; it now uses the quality bar.
- The Git tab showed origin's address as "…", and branch labels in its history ran over the times; `origin/x` next to `x` is one label now.
- Long lines in a subproject's Recent list were cut off without an ellipsis, and so were branch names and commit subjects in the Git tab.
- A debug build crashed when an agent started working (the status bar styled one item twice).
- "Reopened the saved conversation." no longer repeats after every restart.
- Pointing at a subproject in the sidebar cut its "merged" label in half behind the ⋯ button; the label steps aside now. Long folded rows ("4 done: …") end in an ellipsis.

## [0.7.0] - 2026-11-15

Claude can look at Monaged now (an export to read or paste into a chat, and an MCP server for Claude Desktop and Claude Code), a security review and a bug hunt are fixed, and a round of quality-of-life features saves time and keeps work from being lost.

### Added

- **Export for analysis** (Ctrl K, a project's menu, Settings → Connect Claude): a Markdown report to read or paste into a chat with Claude, and a zip with every conversation in full, the pipeline data as JSON, git's view of each project, Monaged's log and a screenshot. Secret values and token-shaped text are masked.
- **Connect Claude**: an MCP server (`monaged.exe --mcp-debug`) for Claude Desktop or Claude Code, so Claude can look at the running app: its status, projects, subprojects, agents and their conversations, diffs, what waits for you, the log, screenshots, and exports; with your permission it can also message, stop and review agents. One click adds it to Claude Desktop or to Claude Code in WSL.
- **The dashboard's Recent list**, and agents ordered by what matters now (needing you, working, then the agents doing the work, then the ones whose part is done).
- **F8: the next thing that needs you.** Goes to the next subproject's dashboard with something waiting (Ctrl K → "Go to what needs you" too).
- **While you were away**: after 10 minutes or more away from Monaged, a card says what happened meanwhile (agents that worked and the cost, reviews, merges, finished pipelines, failures, what waits for you). Settings → General turns it off.
- **The PC stays awake while agents work** or reviews run, so Windows doesn't sleep and pause WSL mid-task (the screen can still turn off).
- **Closing asks first while agents work**: closing Monaged stops them, so it says so and lets you keep it open.
- **Agents you start are named after your first message** (`fix-failing-coupon` instead of `agent-3`).
- **Quick replies** when an agent ends on a question: "Yes, go ahead" or "No, that's all for now" in one click.
- **Open a terminal** in an agent's worktree or a project's repo, from their menus or Ctrl K.
- **Copy the conversation** of one agent as Markdown (secrets masked), to paste into a chat.
- **The sidebar folds pipeline agents whose part is done** (research, verification, the plan) into one row.

### Changed

- **Agents get no git tokens by default.** Git in agents' commands has no credential helpers and Monaged's helper refuses agents, so an agent can't ask git for your GitHub or GitLab token. Settings → Git accounts can allow it; Monaged's own pushes and your terminal are unaffected.
- **Sandboxed projects' tests run fenced**: when a project uses Claude Code's sandbox, the review runs the tests in bubblewrap without Windows drives or WSL interop.
- The merge rule chip says what it does: "auto from 4/5".
- The message box's hint follows a renamed agent; the timeline leaves out an unknown model and $0.00 instead of showing "· ·".
- "N working" in the status bar goes to the next working agent; Esc closes the "while you were away" card.

### Fixed

- **Security**: a command that merely mentioned one of Monaged's tools was approved without asking (and "always"); any agent could act as another on the local bridge (each now has its own key); vault values could reach Haiku through the review's full files, notes and test output, or a commit-message diff (everything sent is masked now, and the review reads no symlinks or files outside the worktree); links from servers or agents could start programs (only web pages open now); the updater installed a release whose checksums didn't download; the elevated WSL install could run a wsl.exe planted next to Monaged; scripts that carry secrets found `timeout` and `bash` through a PATH an agent can write to; agent output reached the log unmasked.
- **Crashes**: HTML tag names with non-ASCII letters in a diff, and long output with non-ASCII text from `run_with_env`.
- **Pipelines**: an agent outside the current stage or run could move it (a planner asked "why?" during Build re-planned and started new builders; an old integrator could finish a new run); Build never finished after a rerun with other workstreams; a second integration failed on the feature branch's worktree; Approve and build twice started two sets of builders; a builder that reported done again stayed "passed"; reruns matched builders by name prefix; renaming an agent lost its place in the quality gate; cleaning up worktrees archived a running pipeline's agents and the lead.
- **Auto-merge** waits for the integrator's latest work to be reviewed, never falls back to a builder's score when there's an integrator, and a ruled-out finding no longer merges by itself.
- **The quality gate**: reviews it was waiting for when Monaged closed run again; a review that can't start answers it instead of leaving it waiting; Fix again without a score sends a message.
- **The dashboard**: agent cards were far taller than their contents; the lead's box lost the cursor after dialogs and kept the keyboard when it wasn't on screen; updates for a busy lead swallowed what you typed; "Send anyway" sent every paused agent's message; the lead could be started twice.
- Failing tests no longer run twice in full: the base branch's result is cached per commit and borrows the worktree's installed dependencies.

## [0.6.0] - 2026-11-15

Each subproject gets a dashboard where everything that needs you can be answered, with a lead agent to talk to, and pipeline work now has to pass review before it's integrated or merged.

### Added

- **A dashboard per subproject.** Clicking a subproject opens it: the request, where it stands, and everything that waits for you, answerable in place (agents' questions with their options or your own reply, permissions, forms, secret requests, the pipeline's decisions and plan, work below the bar, failed sessions, the merge), then every agent with its state, score and findings, with Open, Changes, Review, Fix findings and Stop. The inbox opens it too, and agents have a button back to it.
- **The lead.** Every pipeline gets an agent that oversees the others, and you talk to it on the dashboard: ask how things stand, or tell it what should happen next and it passes it on. It has its own tools (the team's status, messaging an agent, starting one, reviews and fixes), Monaged keeps it posted, and it can't write code or accept work below the bar for you. Settings → Agents turns it off; Ctrl K or the dashboard starts one anywhere. Other agents' `ask` with "lead" reaches it.
- **Fix findings**: a button next to Review on the confidence card sends the review's findings and any failing tests to the agent as one message (it's asked to check each one first and say when one isn't real); the change is reviewed again when it's done.
- **Rule out a finding**: ✓ next to a finding says it isn't a problem, with an optional reason. The score is worked out again without it, and the project's next reviews are told, so a wrong finding doesn't keep coming back.
- **Show only this agent**: a pane's header can switch the middle to one agent at a time (and back to side by side); Ctrl K has both, and can turn the Changes column beside the agent off.

### Changed

- **Reviews know more about the project.** Besides the diff and the tests, the reviewer now gets what you said to the agent after the task, the project's `CLAUDE.md` and shared notes, the agent's own account of its work (as a claim to check), the changed files in full, and the findings you ruled out, and it's told to leave out what depends on code it can't see. Fewer findings that are only wrong for lack of context.

### Fixed

- **Low-scored work was integrated and merged anyway.** A builder that said it was done counted as done straight away, without waiting for its review, and merging into develop by itself only stopped at 1/5. Now builders and the integrator have to pass a quality gate: their work is reviewed when they say they're done and needs 4/5 (Settings → Reviews: 3, 4 or 5). Below that, Monaged sends the findings back to fix, twice; then it stops and asks you (Fix again, Review again, or Accept anyway, which is logged). Merging into develop by itself needs the same score. When reviews are off, the work waits for you instead of going through unchecked. A rerun builder replaces the old one instead of the pipeline waiting for both.
- **Reviews no longer blame agents for tests that were already broken.** When the tests fail, Monaged runs them on the branch the agent started from too; when every failing test failed there as well, the score isn't capped at 2 and the agent isn't asked to fix them.
- **Closing a pane** didn't stick: the agent you closed came straight back because it was still the focused agent. A closed pane now stays closed until you open that agent again.

## [0.5.0] - 2026-11-15

### Added

- **Sounds.** A few short, soft sounds for moments you might miss because you're looking elsewhere: an agent needs you (a question, a permission, a plan to approve), an agent is done, a pipeline's feature is complete, something long you started finished (an install, a clone, a merge, a push, a sign-in, an update), or something went wrong. Nothing plays for messages, tool calls or clicks, or for the agent you're looking at. Several at once play as one, the most important wins, and they stay quiet while you present, play full screen or are away. The sounds are made by Monaged itself (rising for good news, falling for trouble), not Windows' own. Settings → Sounds turns them off or one by one, sets the volume and plays each; Ctrl K → Mute sounds.

### Changed

- While Monaged's sounds are on, its Windows notifications come without Windows' own ding, so nothing plays twice.

## [0.4.1] - 2026-11-15

### Fixed

- **Endpoint security (Sophos "Lockdown") stopped Monaged as malicious.** Monaged started PowerShell to make its Start menu shortcuts and, hidden and with an encoded script, to install WSL as administrator; a hidden cmd.exe to remove its program folder after uninstalling; and reg.exe for the registry. Security software rightly treats an app that starts script hosts and shells as hijacked. Monaged now starts none of them: shortcuts come from Windows' own Shell API, the registry is read and written directly, installing WSL asks Windows to run Monaged itself as administrator (the same one prompt), and the uninstaller moves itself out of the program folder so the folder can go at once. The programs Monaged still starts are wsl.exe, git, and the tools you ask for (winget, gh, Zed).

## [0.4.0] - 2026-11-15

Setting up happens entirely inside Monaged now, git signs in to GitHub and GitLab by itself, and uninstalling can take everything with it.

### Added

- **Git accounts** (Settings → Git accounts): sign in to GitHub, GitLab (Enterprise and self-hosted too) or any other host, with nothing to register and no server in between. In the browser (the host's token page opens with the right permissions ticked; Monaged picks the token up from the clipboard, checks it and empties the clipboard), by pasting a token, or by importing what the GitHub CLI, the GitLab CLI or Git Credential Manager already has. Tokens live in Windows Credential Manager, expiring ones are flagged, and Monaged is git's credential helper in WSL (`monaged.exe --git-credential`), so Git Credential Manager is optional. Several accounts per host, per-project accounts, SSH keys (create, add to your account, test, trust), and "commit as this account".
- **Pull and merge requests and CI without `gh` or `glab`**: through the hosts' REST APIs with your account.
- **A fuller Git tab**: commit only the files you tick, amend, take back a commit that isn't pushed, pull with a merge when a branch moved on both sides, and an Origin row (account, connection test, HTTPS ⇄ SSH).
- **Install buttons** in the setup guide's extras: Zed and the GitHub CLI with winget (hidden; Windows still asks for permission when an installer needs it, and the new PATH is picked up without restarting Monaged), Claude Code's sandbox tools, pytest and curl with apt as root in WSL (no Linux password), each with its progress. The GitHub CLI signs in with GitHub's one-time code shown in the guide. When winget is missing, a **Website** button offers the download instead.
- **A full uninstaller.** Uninstalling (Settings → Apps, or Start menu → Uninstall Monaged) shows what Monaged put on your PC, with sizes, and lets you choose what goes. The program always goes, and with it git's sign-in setting in WSL (git would otherwise call a Monaged that's gone). The Claude agent is ticked; your projects and settings, saved sign-ins, agents' working folders, the playground and Claude's sign-in in WSL are choices. The Ubuntu the setup guide installed can go too: never ticked by itself, and only after a warning that lists what's lost. WSL itself, your other distros and apps like Zed stay. An open Monaged window is closed first, each part shows its progress, and the last screen says what was removed and what wasn't. For scripts: `monaged.exe --uninstall --quiet` (add `--all` for everything).

### Changed

- **No console windows in the setup guide.** WSL installs in the background after one administrator prompt, with what its installer says shown live (and kept in `%LOCALAPPDATA%\Monaged\wsl-install.log`).
- **Ubuntu installs inside the guide with real progress** instead of in a console that says "This might take a while…": the download (megabytes, speed, checked against WSL's checksum), unpacking, the first start (cloud-init's stages) and your user. You type your Linux username (suggested from your Windows name) and password in the guide; Monaged creates the user in the usual groups and makes it the default. A distro that only has root gets the same form.
- **Signing in to Claude** happens in the guide: Claude's sign-in page opens in your browser, and you paste the code it shows into a box (Enter works). Subscription or Anthropic Console account. The "not signed in" bar and the health check open this instead of a terminal.
- The guide's extras link to Git accounts instead of turning on Git Credential Manager, and no longer list commands to copy.

### Fixed

- Progress bars never filled: downloads, clones and installs showed an empty track. They fill now, and a segment slides along while progress isn't known.
- The name and email fields in the setup guide's git step and in the Git tab collapsed to nothing.
- With the extras open, the setup guide's own buttons (Close, Try the playground, Add your first project) could end up out of reach below the list.

## [0.3.0] - 2026-11-14

The first release meant for other people's PCs: Monaged now sets up everything it needs, asking first.

### Added

- **Setup guide.** On a fresh PC Monaged opens a step-by-step guide instead of connecting, and installs what's missing, asking first each time:
  - WSL itself (one administrator prompt). When Windows needs a restart, Monaged offers it and opens again afterwards to carry on.
  - Ubuntu, in its own window, where you pick your Linux username and password. Monaged notices when you're done.
  - The Claude agent (claude-agent-acp with the Claude CLI), downloaded from GitHub into `~/.local/share/claude-agent-acp`. No sudo; an **Update** button fetches the newest one later.
  - Signing in to Claude (`claude auth login` in a terminal window), git's name and email inside WSL, and a checklist of extras (Zed, gh, glab, Git Credential Manager, sandbox tools, pytest, an API key) with the command for each.
  - Open it again any time from Ctrl K → Setup guide, the health check, or the bar that appears when Claude can't be reached.
- **Checkpoints and replay.** Every turn snapshots the agent's worktree: roll back to before any message (and undo that), or step through an agent's turns with each turn's diff.
- **Project settings.** MCP servers per project (with a Test button), project knowledge (CLAUDE.md, skills, subagents, commands, shared notes), and run and test commands.
- **Run tab.** Start a worktree's dev server, run its tests once or three times to find flaky ones, and send failures to the agent.
- **Environment and secrets.** Variables per environment, encrypted with Windows DPAPI. Agents get names, never values; values are masked everywhere and block merges and pushes.
- **Spending caps** per subproject, **syntax colors** in code and diffs, images in conversations, a **side chat** with Haiku, keyboard shortcuts you can change, and moving through the sidebar with the keyboard.
- **A new Add project** with live checks, one-click fixes, step-by-step progress and cancellable clones.
- **The playground**: a small project with a bug, a flaky test and a merge conflict, plus a tour of every feature.
- Monaged lists itself under Windows Settings → Apps, with its version and an uninstaller.
- A **Sign in** button in the "not signed in" bar.
- Release builds write a log to `%APPDATA%\Monaged\monaged.log` (the previous run's is kept).

### Changed

- **Updates work without the GitHub CLI** once the repository is public: Monaged reads GitHub's public release API first and falls back to your `gh` login for a private repository. Every download is checked against the release's `SHA256SUMS.txt` before it replaces anything. A release now counts as newer than its own pre-releases.
- Monaged uses the WSL distribution the setup guide picked (for example `Ubuntu-24.04`) instead of assuming one called `Ubuntu`, and remembers it. `--distro` still overrides it.
- Confidence checks run the project's own test command: the one in Project settings → Run, else the one in `monaged.json` (as the Run tab does). Python projects use pytest only when it's installed, and `unittest` otherwise; tests that need a missing pytest count as checks that couldn't run, not as failures.
- Release builds no longer open a console window next to Monaged.
- In the sidebar, builders are tagged "build" instead of repeating their workstream's name.
- Releases are built and published by GitHub Actions on Windows, with checksums.
- Updates copy the new version next to the old one first and put the old one back if the swap fails. Uninstalling removes the program folder too, once its window has closed.

### Fixed

- On a PC without WSL, starting Monaged could make Windows' `wsl.exe` offer to install WSL by itself. Monaged now checks for WSL without running `wsl.exe` and installs nothing without a click.
- A missing pytest counted as failing tests, which capped every Python project's confidence score at 2.
- The plan card ran past the inspector's edge; it wraps now.

## [0.2.0] - 2026-11-14

### Added

- Session settings as chips (mode, model, effort and more), question forms, slash commands, and tool calls you can open up.
- Agent profiles (Settings → Agents) for other ACP agents.
- Attachments, queued messages, renaming, archiving, find in a conversation and search across them.
- Windows notifications, a timeline, comments on diff lines, a subproject's combined changes, plan editing, re-running a stage, and a merge queue with CI status.
- Git in Monaged: changes, commits, stashes, sync, finishing conflicted merges and the history graph.
- Resizable panes, saved layouts, zen mode and compact density; a health check.
- A per-user installer and updates from GitHub releases; Monaged's own icon.

## [0.1.0] - 2026-11-14

### Added

- Projects, subprojects and agents, each agent in its own git worktree inside WSL, over the Agent Client Protocol.
- Review: diffs, Haiku summaries and a 1–5 confidence score.
- The pipeline: research, two verifiers, a decision, a plan with a risk-based gate, parallel builders and an integrator.
- Coordination: a board with contracts and decisions, file claims, and questions answered by a Haiku librarian first.
- Merging by branch rules, promotions, pushes and GitHub/GitLab merge requests.
- Usage awareness: weekly usage, Monaged's share, budgets and models per role.
- A redesigned window with themes, a command palette and a UX pass.

[0.5.0]: https://github.com/Marten-Mrfc/monaged/releases/tag/v0.5.0
[0.4.1]: https://github.com/Marten-Mrfc/monaged/releases/tag/v0.4.1
[0.4.0]: https://github.com/Marten-Mrfc/monaged/releases/tag/v0.4.0
[0.3.0]: https://github.com/Marten-Mrfc/monaged/releases/tag/v0.3.0
[0.2.0]: https://github.com/Marten-Mrfc/monaged/commit/503e441
[0.1.0]: https://github.com/Marten-Mrfc/monaged/commit/6fee8a1
