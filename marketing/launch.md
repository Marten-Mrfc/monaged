# Launch kit

Ready-to-use copy for announcing Monaged. Swap in the real links when the repository is public; screenshots are in `site/img/`, the logo in `assets/brand/`, the link card in `assets/brand/social-preview.png`.

## Names and lines

- **Name:** Monaged
- **Tagline:** One window for every Claude Code agent.
- **One line (≤ 80 characters):** Run Claude Code agents side by side, each in its own worktree, from one window.
- **Short (≤ 160 characters):** Monaged runs Claude Code agents on your projects in parallel, each in its own git worktree inside WSL, and shows what they did and what needs you.
- **Category:** Developer tools · AI coding · Windows

## Description (about 100 words)

Monaged is a native Windows app for running Claude Code agents on your projects. Projects split into subprojects, subprojects into agents, and every agent works in its own git worktree inside WSL, so several can work in parallel without touching each other's files. A pipeline takes a feature from a sentence to a merged branch: research, two verifiers, a plan with risk flags, parallel builders and an integrator, stopping for your approval when it matters. Every change comes with a diff, a summary and a confidence score that lists its reasons. A setup guide installs WSL, Ubuntu and the Claude agent for you. Free; uses your own Claude account.

## Key points

1. **Parallel without conflicts.** Each agent gets its own branch and worktree.
2. **A pipeline that asks first.** Research, verify, plan, build, integrate; risky plans wait for you.
3. **Review with reasons.** Tests plus a skeptical review give a 1–5 score with the why.
4. **Merges on your rules.** develop → acceptance → main, each with its own rule.
5. **Budget-aware.** Your weekly usage, Monaged's share and a cap.
6. **Native and fast.** GPUI (Zed's UI framework), keyboard-first.
7. **Ready in minutes.** The setup guide installs everything, asking first.

## Show HN

**Title:** Show HN: Monaged – run Claude Code agents side by side on Windows, each in its own worktree

**Text:**

> I kept losing track of Claude Code sessions: every chat for every project in one list, two agents editing the same files, and big tasks ending in a wall of text I had to trust.
>
> Monaged is a native Windows app (Rust, GPUI) that organizes agents into projects and subprojects and gives each one its own git worktree inside WSL. It talks to Claude Code through the Agent Client Protocol (claude-agent-acp), so it shows everything the agent reports: tool calls, questions, modes, models.
>
> On top of that: a pipeline (research → two verifiers → plan → parallel builders → integrator) that stops for approval on risky plans, a 1–5 confidence score per change from your tests plus a skeptical Haiku review, a small board where agents post contracts instead of chatting, merge rules per branch, checkpoints with undo, and a weekly usage budget.
>
> It's free and uses your own Claude subscription or API account. Windows 10/11 only for now; a setup guide installs WSL, Ubuntu and the agent. Not affiliated with Anthropic. I'd love feedback on the pipeline's gates and the scoring.

## Product Hunt

- **Tagline (60 chars):** One window for every Claude Code agent
- **Description (260 chars):** Run Claude Code agents in parallel on your projects, each in its own git worktree. A pipeline researches, plans and builds features, stopping for your approval when it matters. Every change gets a diff, a summary and a confidence score with reasons.
- **First comment:** Why it exists (the long list of chats), what's different (worktrees, gates, scores with reasons), what it needs (Windows, WSL, your Claude account), and what's next (from ROADMAP.md).

## Social posts

**X / Bluesky / Mastodon**

> Monaged 0.3.0 is out: one window for every Claude Code agent.
> Each agent gets its own git worktree, a pipeline takes a feature from a sentence to a merged branch, and every change comes with a score and its reasons.
> Windows + WSL, free, uses your own Claude account. [link]

**LinkedIn**

> I built Monaged because running several Claude Code agents at once got messy: one long list of chats, agents editing the same files, and no quick way to know whether a change was any good.
>
> Monaged gives every agent its own git worktree, runs a research → verify → plan → build → integrate pipeline that waits for approval when a plan is risky, and scores every change from 1 to 5 based on your tests and a skeptical review, with the reasons spelled out.
>
> It's a native Windows app, free, and it uses your own Claude account. Version 0.3.0 adds a setup guide that installs WSL, Ubuntu and the Claude agent for you. [link]

**Reddit (r/ClaudeAI, r/ClaudeCode)**

> **I made a Windows app to run Claude Code agents side by side, each in its own worktree**
>
> Short description, two screenshots (`pipeline-build.png`, `agent.png`), what it needs, and an honest list of limits (Windows only, not code-signed yet, independent project). Ask what people would want the pipeline to do differently.

## Release announcement template

> **Monaged {version}**
>
> {two or three sentences on the headline change}
>
> - {highlight}
> - {highlight}
> - {highlight}
>
> Download: {release link} · What's new: {changelog link}

## Assets checklist

- [ ] Logo: `assets/brand/monaged-logo-dark.svg` / `-light.svg`
- [ ] Icon: `assets/brand/monaged-mark.svg`, `assets/monaged-256.png`
- [ ] Link card: `assets/brand/social-preview.png` (also set it in the repository's Settings → Social preview)
- [ ] Screenshots: `site/img/pipeline-build.png` (hero), `agent.png`, `pipeline-plan.png`, `setup.png`, `palette.png`, `light.png`
- [ ] Website: `site/` (publish with the Website workflow once Pages is on)
