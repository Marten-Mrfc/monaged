# Changelog

All notable changes to Monaged. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Fix findings**: a button next to Review on the confidence card sends the review's findings and any failing tests to the agent as one message (it's asked to check each one first and say when one isn't real); the change is reviewed again when it's done.
- **Rule out a finding**: ✓ next to a finding says it isn't a problem, with an optional reason. The score is worked out again without it, and the project's next reviews are told, so a wrong finding doesn't keep coming back.
- **Show only this agent**: a pane's header can switch the middle to one agent at a time (and back to side by side); Ctrl K has both, and can turn the Changes column beside the agent off.

### Changed

- **Reviews know more about the project.** Besides the diff and the tests, the reviewer now gets what you said to the agent after the task, the project's `CLAUDE.md` and shared notes, the agent's own account of its work (as a claim to check), the changed files in full, and the findings you ruled out, and it's told to leave out what depends on code it can't see. Fewer findings that are only wrong for lack of context.

### Fixed

- **Low-scored work was integrated and merged anyway.** A builder that said it was done counted as done straight away, without waiting for its review, and merging into develop by itself only stopped at 1/5. Now builders and the integrator have to pass a quality gate: their work is reviewed when they say they're done and needs 4/5 (Settings → Reviews: 3, 4 or 5). Below that, Monaged sends the findings back to fix, twice; then it stops and asks you (Fix again, Review again, or Accept anyway, which is logged). Merging into develop by itself needs the same score. When reviews are off, the work waits for you instead of going through unchecked. A rerun builder replaces the old one instead of the pipeline waiting for both.
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
