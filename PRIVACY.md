# Privacy

Monaged has no servers, no accounts and no analytics. It doesn't collect or send anything about you or your work to its authors.

## What stays on your PC

- **Your projects, settings, conversations' metadata, reviews, the board and usage numbers**: in a SQLite database in `%APPDATA%\Monaged` (with backups next to it).
- **Your Anthropic API key**, if you add one: in Windows Credential Manager. It's never written to the database or shown again.
- **Environment variables you store per project**: encrypted with Windows DPAPI for your Windows account, plus a per-install key in Credential Manager. The database only holds encrypted values.
- **A log** of the current and the previous run: `%APPDATA%\Monaged\monaged.log`. It holds errors and the agent's log lines, never secret values (they're masked).
- **Agent worktrees** inside WSL, next to each project (`~/code/.monaged/...`).

## What leaves your PC, and to where

| What | Where it goes | When |
|---|---|---|
| Your prompts, the code agents read, and their replies | Anthropic, through Claude Code (claude-agent-acp) running in WSL with your own login | Whenever an agent works. This is the same as using Claude Code directly; Anthropic's terms and privacy policy apply. |
| Diffs and conversation excerpts for summaries, reviews, the librarian and the side chat | Anthropic: through the Messages API with your API key, or through the Claude CLI on your plan | After agents' turns (you can turn automatic reviews off in Settings). Secret values are masked first. |
| A request for the newest release | GitHub (`api.github.com`, or your `gh` login for a private repository) | Once a day, if "Check for updates" is on, and when you check by hand. It sends Monaged's version in the User-Agent, nothing else. |
| The Claude agent download | GitHub (`github.com/agentclientprotocol/claude-agent-acp` releases) | When you install or update the agent in the setup guide. |
| Pushes, pull requests, merge requests and CI status | Your own git remotes, GitHub or GitLab, through git, `gh` and `glab` | When you push or open a request, or the merge queue checks CI. |

Monaged never reads your Claude credentials. Signing in happens in the Claude CLI, in a terminal window.

## Removing everything

Uninstalling (Start menu → Uninstall Monaged, or Settings → Apps) removes the program and its shortcuts but keeps your data, so reinstalling picks up where you were. To remove the data too, delete `%APPDATA%\Monaged`, the `monaged` entries in Windows Credential Manager (the API key and the vault key), and the `.monaged` folders inside WSL.
