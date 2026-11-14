# Changelog

All notable changes to Monaged. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

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

[0.3.0]: https://github.com/Marten-Mrfc/monaged/releases/tag/v0.3.0
[0.2.0]: https://github.com/Marten-Mrfc/monaged/commit/503e441
[0.1.0]: https://github.com/Marten-Mrfc/monaged/commit/6fee8a1
