# Security

## Reporting a vulnerability

Please don't open a public issue for security problems. Use GitHub's private reporting instead: on the repository page, **Security → Report a vulnerability**. If that isn't available, open an issue that only says you have something to report privately, without details, and you'll be contacted.

Include what you found, how to reproduce it, and what an attacker could do with it. You'll get an answer within a week, and a fix or a plan as soon as the problem is understood. Credit goes in the changelog unless you'd rather not.

## Supported versions

Only the newest release gets fixes. Monaged updates itself (Settings → Updates), so staying current is one click.

## How Monaged protects you

- **Agents run inside WSL** in their own git worktrees, through Claude Code with its permission prompts. Monaged's own tools (the board, claims, asks) are the only ones it approves by itself.
- **Secrets**: environment variables are encrypted with Windows DPAPI. Agents see names, not values; `run_with_env` runs a command with values only after you approve it, and its output comes back masked. Values are masked everywhere Monaged shows text (including Base64, hex and percent-encoded forms), flagged in diffs, and merges and pushes that contain one are refused.
- **Agents' rules**: every session gets a settings layer the agent can't edit that denies reading `.env` files and pushing; Claude Code's sandbox can hide other processes, `/mnt` and Windows programs from agents' commands.
- **Updates** are only installed from the configured repository's releases and are checked against the release's `SHA256SUMS.txt` before anything is replaced.
- **The coordination bridge** listens on localhost only, and every call needs a per-run token.
- **The installer** asks for no administrator rights. Only installing WSL itself (in the setup guide, after you click) asks Windows for one administrator prompt.

## Known limitations

- The installer and app aren't code-signed yet, so Windows SmartScreen warns about them. Compare the file with the release's `SHA256SUMS.txt` if you want to be sure.
- Anything you send to an agent goes to Anthropic through Claude Code; see [PRIVACY.md](PRIVACY.md).
