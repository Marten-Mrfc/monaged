# Troubleshooting

Start with the **health check** (Ctrl K → Health check). It tests WSL, the agent, git, your logins and disk space, and most problems come with a button that fixes them. The **setup guide** (Ctrl K → Setup guide) re-checks everything Monaged needs to run.

Monaged writes a log to `%APPDATA%\Monaged\monaged.log` (the previous run's is `monaged.old.log`). Attach its end to bug reports.

## Installing

**Windows SmartScreen says it protected my PC.** The installer isn't code-signed yet. Click **More info → Run anyway**. To check the file, compare `Get-FileHash .\Monaged-Setup-<version>.exe` in PowerShell with the release's `SHA256SUMS.txt`.

**Where did it install?** `%LOCALAPPDATA%\Programs\Monaged`. Your projects and settings are in `%APPDATA%\Monaged`; they stay there when you update, and when you uninstall unless you tick them.

**How do I uninstall?** Settings → Apps → Monaged → Uninstall, or Start menu → Uninstall Monaged. A checklist shows what Monaged put on your PC. The program and git's sign-in setting in WSL always go, and the Claude agent in WSL is ticked. The rest is up to you: your projects and settings (`%APPDATA%\Monaged`), sign-ins saved in Windows Credential Manager, agents' working folders (their branches stay in your repos), the playground, Claude's sign-in in WSL, and the Ubuntu the setup guide installed, which takes everything inside it, including code you haven't pushed. Keep your projects and settings and installing again picks up where you were. WSL itself, other distros and apps like Zed or gh are never removed. If something couldn't be removed, the last screen says why; run the uninstaller again once it's fixed (Monaged stays installed while git in WSL still depends on it). Git tokens removed from Credential Manager keep working on GitHub or GitLab until you revoke them there. For scripts, `monaged.exe --uninstall --quiet` removes the default choice without a window (`--all` removes everything, the Ubuntu Monaged installed included) and exits with 0 when everything went.

## WSL

**Install WSL does nothing, or says permission was declined.** It needs one administrator prompt (UAC). If your account isn't an administrator, ask someone who is to run `wsl --install --no-distribution` in an administrator terminal, then click **Check again**. What WSL's installer said is in `%LOCALAPPDATA%\Monaged\wsl-install.log`.

**Ubuntu installed but "didn't start".** Right after installing WSL, Windows usually needs a restart. If it still fails after one, virtualization is probably off: turn on *Intel VT-x / AMD-V* (often called *SVM* or *Virtualization Technology*) in your PC's BIOS or UEFI settings. In Windows, *Virtual Machine Platform* must be on (Turn Windows features on or off).

**Installing Ubuntu stopped halfway.** The guide shows the step it stopped at and why; click **Install Ubuntu** again. A finished download is kept in `%LOCALAPPDATA%\Monaged\downloads` and reused (it's checked again first), and deleted once Ubuntu is installed. The download comes from the address in WSL's own list of distributions (`raw.githubusercontent.com` and `releases.ubuntu.com`); a proxy that blocks those stops it.

**My distro only has root.** The setup guide offers the same username and password form; **Create my user** makes the user, adds it to the usual groups (sudo among them) and makes it the default. If you'd rather keep using root, choose **Keep using root** (not recommended).

**I use another distro.** The setup guide lists the distros you have; click one to use it. Monaged remembers it. You can also start Monaged with `--distro <name>`.

## The Claude agent

**"Can't reach Claude inside WSL."** The bar at the top has the reason; **Copy details** puts the whole error on the clipboard. Common causes:

- *The agent isn't installed*: open the setup guide and click **Install the Claude agent**.
- *curl is missing in WSL*: the setup guide's agent step has **Install curl and tar**.
- *GitHub can't be reached from WSL* (a proxy or firewall): download `claude-agent-acp-linux-x64.tar.gz` from the [claude-agent-acp releases](https://github.com/agentclientprotocol/claude-agent-acp/releases) yourself and unpack it into `~/.local/share/claude-agent-acp/<version>`, then point `~/.local/share/claude-agent-acp/current` at that folder.

**"Claude isn't signed in."** Click **Sign in** in the bar (or the health check): the setup guide opens at its sign-in step and Claude's page opens in your browser. Sign in there, copy the code the page shows, paste it into the guide and press Enter.

- *The page didn't open*: click **Open the page** (or **Copy link** and paste it into a browser).
- *"That's only part of the code"*: the code has two halves joined by `#`; use the page's copy button so you get all of it.
- *It says the sign-in stopped*: the code expired or was used once already; click **Sign in** again for a new one.

**Agents can't commit.** Git in WSL needs a name and email: Settings → Git, or the setup guide's git step.

**Pushing asks for a password, or fails.** Sign in to the host in Settings → Git accounts (the setup guide's extras link there too). Monaged then answers git's credential questions in WSL for that host. Already using Git Credential Manager on Windows? Settings → Git accounts can make it the helper for other hosts instead.

## Reviews and tests

**The confidence score says checks failed, but my tests pass.** Reviews run the test command from Project settings → Run when you set one, else the `test` in a `monaged.json` in the project root (`{"test": "..."}`), else one Monaged detects (cargo, npm, go, pytest or unittest, make). Set your real test command in one of the first two places; the Run tab uses the same one.

**Open in Zed does nothing.** Install Zed (the setup guide's extras have an **Install** button) and make sure `zed` works in a terminal.

**An Install button says winget isn't on this PC.** Windows programs install through winget, which comes with "App Installer" from the Microsoft Store. Install that, or use the **Website** button next to **Install** to download the program yourself.

## Updates

**Check for updates says "no public release found".** While Monaged's repository is private, updates come through the GitHub CLI: in the setup guide's extras, **Install** it and **Sign in** (GitHub shows a one-time code to enter on its page).

**"This is a development build."** Only an installed copy updates itself. Install it from Settings → Updates → Install this build, or run the setup file.

## Starting over

- Re-run setup: Ctrl K → Setup guide, or start Monaged with `--setup`.
- Reset Monaged completely: uninstall it and tick **Projects and settings** (or quit it and delete `%APPDATA%\Monaged`); your repositories in WSL aren't touched. Monaged keeps the last five database backups in `%APPDATA%\Monaged\backups`, and goes back to the newest good one by itself if the database is ever damaged.
