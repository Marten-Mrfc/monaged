# Troubleshooting

Start with the **health check** (Ctrl K → Health check). It tests WSL, the agent, git, your logins and disk space, and most problems come with a button that fixes them. The **setup guide** (Ctrl K → Setup guide) re-checks everything Monaged needs to run.

Monaged writes a log to `%APPDATA%\Monaged\monaged.log` (the previous run's is `monaged.old.log`). Attach its end to bug reports.

## Installing

**Windows SmartScreen says it protected my PC.** The installer isn't code-signed yet. Click **More info → Run anyway**. To check the file, compare `Get-FileHash .\Monaged-Setup-<version>.exe` in PowerShell with the release's `SHA256SUMS.txt`.

**Where did it install?** `%LOCALAPPDATA%\Programs\Monaged`. Your projects and settings are in `%APPDATA%\Monaged`, and stay there when you update or uninstall.

## WSL

**Install WSL does nothing, or says permission was declined.** It needs one administrator prompt (UAC). If your account isn't an administrator, ask someone who is to run `wsl --install --no-distribution` in an administrator terminal, then click **Check again**.

**Ubuntu installed but "didn't start".** Right after installing WSL, Windows usually needs a restart. If it still fails after one, virtualization is probably off: turn on *Intel VT-x / AMD-V* (often called *SVM* or *Virtualization Technology*) in your PC's BIOS or UEFI settings. In Windows, *Virtual Machine Platform* must be on (Turn Windows features on or off).

**I closed the Ubuntu window before creating my user.** Click **Open Ubuntu** in the setup guide; it asks again. If you'd rather keep using root, choose **Keep using root** (not recommended).

**I use another distro.** The setup guide lists the distros you have; click one to use it. Monaged remembers it. You can also start Monaged with `--distro <name>`.

## The Claude agent

**"Can't reach Claude inside WSL."** The bar at the top has the reason; **Copy details** puts the whole error on the clipboard. Common causes:

- *The agent isn't installed*: open the setup guide and click **Install the Claude agent**.
- *curl is missing in WSL*: open your distro and run `sudo apt update && sudo apt install -y curl`.
- *GitHub can't be reached from WSL* (a proxy or firewall): download `claude-agent-acp-linux-x64.tar.gz` from the [claude-agent-acp releases](https://github.com/agentclientprotocol/claude-agent-acp/releases) yourself and unpack it into `~/.local/share/claude-agent-acp/<version>`, then point `~/.local/share/claude-agent-acp/current` at that folder.

**"Claude isn't signed in."** Click **Sign in** in the bar (or in the setup guide), finish in the browser, then **Reconnect**. In a terminal it's: `wsl -- ~/.local/share/claude-agent-acp/current/claude auth login`.

**Agents can't commit.** Git in WSL needs a name and email: Settings → Git, or the setup guide's git step.

**Pushing asks for a password, or fails.** Turn on "Use my Windows Git credentials in WSL" (Settings → Git, or the setup guide's extras); it makes git in WSL use the logins Git Credential Manager keeps on Windows.

## Reviews and tests

**The confidence score says checks failed, but my tests pass.** Reviews run the test command from Project settings → Run when you set one, else the `test` in a `monaged.json` in the project root (`{"test": "..."}`), else one Monaged detects (cargo, npm, go, pytest or unittest, make). Set your real test command in one of the first two places; the Run tab uses the same one.

**Open in Zed does nothing.** Install Zed and make sure `zed` works in a terminal (`winget install -e --id ZedIndustries.Zed`).

## Updates

**Check for updates says "no public release found".** While Monaged's repository is private, updates come through the GitHub CLI: install it (`winget install -e --id GitHub.cli`) and run `gh auth login`.

**"This is a development build."** Only an installed copy updates itself. Install it from Settings → Updates → Install this build, or run the setup file.

## Starting over

- Re-run setup: Ctrl K → Setup guide, or start Monaged with `--setup`.
- Reset Monaged completely: quit it and delete `%APPDATA%\Monaged` (your projects in WSL aren't touched). Monaged keeps the last five database backups in `%APPDATA%\Monaged\backups`, and goes back to the newest good one by itself if the database is ever damaged.
