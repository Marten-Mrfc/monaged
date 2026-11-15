//! The setup guide: what a new PC needs before Monaged works, checked and
//! installed one step at a time, each one asked first. Everything happens in
//! this window, with its progress; no console windows, no commands to copy.
//!
//! 1. WSL itself: one administrator prompt (`wsl --install --no-distribution`,
//!    hidden; its output shows here), usually followed by a restart; Monaged
//!    opens again afterwards.
//! 2. Ubuntu: downloaded from WSL's own catalog with megabytes and speed,
//!    installed, started for the first time, and your Linux user created from
//!    the username and password you type here (see `distro`).
//! 3. The Claude agent (claude-agent-acp, which bundles the Claude CLI),
//!    downloaded into `~/.local/share/claude-agent-acp` in WSL. No sudo.
//! 4. Signing in to Claude: the sign-in page opens in your browser and you
//!    paste the code it shows into the guide (see `signin`).
//! 5. Git's name and email inside WSL, so agents can commit.
//! 6. Extras that make some features work (Zed, gh, pytest, the sandbox
//!    tools), each with an Install button, gh's sign-in, and a way to Git
//!    accounts (Settings) for pushes and pull requests.

use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, ClipboardItem, Context, Entity, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::extras::Extra;
use crate::icons::{Icon, icon_sized};
use crate::signin::{Account, Live};
use crate::text_input::TextInput;
use crate::ui::{self, Kind, StepState, StepView};
use crate::{Form, Monaged, distro, signin, theme, wsl};

/// Where the Claude agent (with the Claude CLI bundled) is published.
pub const AGENT_REPO: &str = "agentclientprotocol/claude-agent-acp";
/// Used when GitHub can't say which release is the newest.
const AGENT_FALLBACK_VERSION: &str = "0.107.0";
/// Where the agent goes inside WSL; `current` points at the active version.
pub const AGENT_HOME: &str = "~/.local/share/claude-agent-acp";
/// The setting naming the WSL distro the setup guide installed (absent when
/// you brought your own).
pub const INSTALLED_DISTRO_SETTING: &str = "setup:installed_distro";

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum StepId {
    Wsl,
    Distro,
    Agent,
    Login,
    Git,
    Extras,
}

pub(crate) const STEPS: [StepId; 6] = [StepId::Wsl, StepId::Distro, StepId::Agent, StepId::Login, StepId::Git, StepId::Extras];

impl StepId {
    fn title(self) -> &'static str {
        match self {
            StepId::Wsl => "Windows Subsystem for Linux",
            StepId::Distro => "Ubuntu and your Linux user",
            StepId::Agent => "The Claude agent",
            StepId::Login => "Sign in to Claude",
            StepId::Git => "Your name for git commits",
            StepId::Extras => "Extras",
        }
    }

    fn number(self) -> usize {
        STEPS.iter().position(|s| *s == self).unwrap_or(0) + 1
    }
}

/// What the PC has, as last checked.
#[derive(Clone, Default, Debug)]
pub(crate) struct Probe {
    /// WSL is installed and answers.
    pub wsl: bool,
    pub wsl_error: Option<String>,
    pub wsl_version: Option<String>,
    /// Installed distros (Docker's and Rancher's own left out).
    pub distros: Vec<String>,
    /// The one Monaged uses.
    pub distro: Option<String>,
    /// The distro is there but didn't run a command.
    pub distro_error: Option<String>,
    pub user: Option<String>,
    pub os: Option<String>,
    /// Commands found in WSL: curl, tar, git, bwrap, socat, pytest.
    pub tools: HashSet<String>,
    /// The agent's version (or "installed" when it isn't where Monaged puts it).
    pub agent: Option<String>,
    /// From `claude auth status`; None when there's no Claude CLI to ask.
    pub login: Option<bool>,
    pub git_name: String,
    pub git_email: String,
    /// Git's identity on Windows, to offer in WSL.
    pub win_git: Option<(String, String)>,
    pub zed: bool,
    pub gh: bool,
    pub gh_auth: bool,
}

impl Probe {
    fn has(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    fn user_ok(&self, root_ok: bool) -> bool {
        self.user.as_deref().is_some_and(|u| !u.is_empty() && (u != "root" || root_ok))
    }
}

/// Signing in to Claude: where it is.
#[derive(Clone, Debug)]
pub(crate) struct SignIn {
    pub account: Account,
    /// The sign-in page, once the CLI has said it.
    pub url: Option<String>,
    /// The last thing it said (a wrong code, a warning).
    pub note: Option<String>,
    /// A code was sent and the CLI is checking it.
    pub checking: bool,
}

/// Something running for a step, shown in place of the step's buttons.
#[derive(Clone, Debug)]
pub(crate) enum Job {
    /// One line of what it's doing, with a bar.
    Running { step: StepId, line: String, fraction: Option<f32> },
    /// A longer job as a list of steps (installing Ubuntu, creating the user).
    Steps { step: StepId, items: Vec<StepView> },
    /// Signing in to Claude: the link and a box for the code.
    SignIn(SignIn),
    /// An extra installing.
    Installing { extra: Extra, line: String, fraction: Option<f32> },
    /// GitHub's sign-in: the code to enter on its page.
    GitHub { code: Option<String>, url: Option<String>, note: Option<String> },
}

impl Job {
    fn step(&self) -> StepId {
        match self {
            Job::Running { step, .. } | Job::Steps { step, .. } => *step,
            Job::SignIn(_) => StepId::Login,
            Job::Installing { extra: Extra::Curl, .. } => StepId::Agent,
            Job::Installing { .. } | Job::GitHub { .. } => StepId::Extras,
        }
    }
}

#[derive(Default)]
pub(crate) struct Setup {
    pub probe: Option<Probe>,
    pub probing: bool,
    pub job: Option<Job>,
    /// The last thing that failed, per step.
    pub error: Option<(StepId, String)>,
    /// WSL was just installed: Windows wants a restart.
    pub restart: bool,
    /// Steps you chose to skip.
    pub skipped: HashSet<StepId>,
    /// You chose to keep using root in the distro.
    pub root_ok: bool,
    pub name: Option<Entity<TextInput>>,
    pub email: Option<Entity<TextInput>>,
    /// Your Linux username and password, for the user Monaged creates.
    pub linux_user: Option<Entity<TextInput>>,
    pub linux_password: Option<Entity<TextInput>>,
    pub linux_password_again: Option<Entity<TextInput>>,
    /// The code Claude's sign-in page shows.
    pub code: Option<Entity<TextInput>>,
    /// A sign-in running in the background (Claude's or GitHub's).
    live: Option<Live>,
    /// The Ubuntu install's steps as they were when it failed.
    failed_steps: Option<Vec<StepView>>,
    /// The extra that last failed to install (its row offers its website).
    failed_extra: Option<Extra>,
    /// The running distro job is a full install (not just creating the user).
    installing_distro: bool,
    /// Bumped by every job so stale updates stop.
    generation: u64,
    /// The distro, the agent or the sign-in changed: reconnect when the guide closes.
    reconnect: bool,
    /// The guide opened instead of connecting (first start): connect once it's left,
    /// however that happens (its buttons, Esc, or another page from Ctrl K).
    pub pending: bool,
}

// ---- Talking to Windows and WSL --------------------------------------------------------------

/// A Windows program without a console window.
fn quiet(program: &str) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(wsl::CREATE_NO_WINDOW);
    }
    command
}

/// Runs `f` on its own thread and gives up after `secs` (a hung WSL service
/// mustn't freeze the guide; the thread finishes on its own).
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).unwrap_or_else(|_| bail!("WSL didn't answer within {secs} seconds"))
}

/// `wsl.exe <args>`: (succeeded, stdout, stderr). WSL_UTF8 makes wsl.exe's
/// own messages UTF-8 instead of UTF-16.
fn wsl_exe(args: &[&str]) -> Result<(bool, String, String)> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    within(30, move || {
        let out = quiet("wsl.exe").args(&args).env("WSL_UTF8", "1").output().context("wsl.exe isn't on this Windows (WSL needs Windows 10 2004 or newer)")?;
        let clean = |b: &[u8]| wsl::decode(b).replace('\0', "").trim().to_string();
        Ok((out.status.success(), clean(&out.stdout), clean(&out.stderr)))
    })
}

/// What wsl.exe said, stdout and stderr together.
fn said(stdout: &str, stderr: &str) -> String {
    format!("{stdout}\n{stderr}").trim().to_string()
}

fn short(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect::<Vec<_>>().join(" ")
}

/// Whether WSL is installed, without running wsl.exe: on a PC without WSL,
/// Windows' wsl.exe stub may offer to install it by itself, and Monaged only
/// installs things when you click. Looks for the WSL package, its services,
/// and distros registered for this user.
pub fn wsl_present() -> bool {
    use crate::native::{Hive, reg_key_exists};
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    if std::path::Path::new(&program_files).join("WSL").join("wsl.exe").exists() {
        return true;
    }
    reg_key_exists(Hive::LocalMachine, r"SYSTEM\CurrentControlSet\Services\WSLService")
        || reg_key_exists(Hive::LocalMachine, r"SYSTEM\CurrentControlSet\Services\LxssManager")
        || !registered_distros().is_empty()
}

/// Distros registered for this user (from the registry), with their default user id.
pub fn registered_distros() -> Vec<(String, Option<u32>)> {
    crate::native::wsl_distros().into_iter().map(|d| (d.name, d.default_uid)).collect()
}

/// Distros WSL has, without the ones container tools install for themselves.
pub fn list_distros() -> Vec<String> {
    // Only stdout: a warning on stderr must never be taken for a distro.
    match wsl_exe(&["--list", "--quiet"]) {
        Ok((true, text, _)) => text
            .lines()
            .map(|l| l.trim().trim_start_matches('\u{feff}').to_string())
            .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop") && !l.starts_with("rancher-desktop") && !l.starts_with("podman-machine"))
            .collect(),
        _ => Vec::new(),
    }
}

/// The distro to use: the one asked for if it's there, else an Ubuntu, else the first.
pub fn pick_distro(preferred: &str, distros: &[String]) -> Option<String> {
    distros
        .iter()
        .find(|d| d.eq_ignore_ascii_case(preferred))
        .or_else(|| distros.iter().find(|d| d.to_lowercase().starts_with("ubuntu")))
        .or_else(|| distros.first())
        .cloned()
}

/// The Claude CLI next to the agent (`…/current/claude`).
pub fn claude_cli(agent_cmd: &str) -> String {
    let program = agent_cmd.split_whitespace().next().unwrap_or_default();
    match program.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/claude"),
        None => "claude".into(),
    }
}

/// Where the WSL install writes what it says, for the guide to show.
fn wsl_install_log() -> std::path::PathBuf {
    let base = std::env::var("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    base.join("Monaged").join("wsl-install.log")
}

/// What Monaged asks Windows to run as administrator to install WSL: itself,
/// with `--install-wsl <log>` (see `install_wsl_here`). Not PowerShell: an
/// app that starts script hosts (let alone hidden, encoded ones) is what
/// security software like Sophos stops as malicious.
pub fn install_wsl_args(log: &std::path::Path) -> String {
    format!("--install-wsl {}", crate::native::quote_arg(&log.display().to_string()))
}

/// `monaged.exe --install-wsl <log>`, running as administrator: installs WSL
/// without a distro (Ubuntu comes in the next step), or on older Windows 10
/// builds that don't know --no-distribution, the classic install without
/// launching a distro. What wsl.exe says goes to `log` as it says it, for the
/// guide to show. Returns wsl.exe's exit code (3010: restart needed).
pub fn install_wsl_here(log: Option<std::path::PathBuf>) -> i32 {
    let run = |args: &[&str]| -> i32 {
        let file = log.as_ref().and_then(|l| std::fs::OpenOptions::new().create(true).append(true).open(l).ok());
        let (out, err) = match file.as_ref().and_then(|f| f.try_clone().ok().map(|c| (c, f))) {
            Some((a, b)) => (Stdio::from(a), b.try_clone().map(Stdio::from).unwrap_or_else(|_| Stdio::null())),
            None => (Stdio::null(), Stdio::null()),
        };
        // Running as administrator: Windows' own wsl.exe by its full path, never
        // one found next to monaged.exe (Windows looks in the program's folder first).
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = std::path::Path::new(&system).join("System32").join("wsl.exe");
        quiet(&exe.to_string_lossy()).args(args).env("WSL_UTF8", "1").stdout(out).stderr(err).status().ok().and_then(|s| s.code()).unwrap_or(1)
    };
    let code = run(&["--install", "--no-distribution"]);
    if code == 0 || code == 3010 {
        return code;
    }
    run(&["--install", "--no-launch"])
}

/// The last thing a growing log says, and the percentage in it if any.
fn log_tail(bytes: &[u8]) -> Option<(String, Option<f32>)> {
    let text = wsl::decode(bytes).replace('\0', "");
    let line = text.split(['\n', '\r']).map(str::trim).rfind(|l| l.chars().any(char::is_alphanumeric))?.to_string();
    let fraction = distro::percent_in(&line);
    // A progress bar alone ("[=====   42.0%   ]") reads better as words.
    let shown = if line.starts_with('[') { "Installing…".to_string() } else { line };
    Some((shown, fraction))
}

/// Makes Windows start Monaged once after the next sign-in (to carry on after a restart).
pub fn reopen_after_restart(on: bool) {
    use crate::native::{Hive, reg_delete_value, reg_set_string};
    let key = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let _ = reg_set_string(Hive::CurrentUser, key, "Monaged", &format!("\"{}\" --setup", exe.display()));
    } else {
        let _ = reg_delete_value(Hive::CurrentUser, key, "Monaged");
    }
}

/// When Windows last started, in Unix seconds.
fn booted_at() -> Option<i64> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetTickCount64() -> u64;
        }
        let up = unsafe { GetTickCount64() } / 1000;
        Some(crate::usage::now_unix() - up as i64)
    }
    #[cfg(not(windows))]
    {
        let up: f64 = std::fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
        Some(crate::usage::now_unix() - up as i64)
    }
}

/// WSL was installed and Windows hasn't restarted since.
fn restart_pending(installed_at: Option<String>) -> bool {
    let Some(at) = installed_at.and_then(|v| v.parse::<i64>().ok()) else { return false };
    // Without a boot time, assume it's still pending until WSL shows it works.
    booted_at().is_none_or(|boot| boot < at)
}

/// Restarts Windows in a few seconds.
pub fn restart_windows() -> Result<()> {
    let status = quiet("shutdown.exe")
        .args(["/r", "/t", "5", "/c", "Restarting to finish installing WSL. Monaged opens again afterwards."])
        .status()
        .context("couldn't run shutdown.exe")?;
    if !status.success() {
        bail!("Windows didn't start the restart; restart it from the Start menu.");
    }
    Ok(())
}

fn windows_git(key: &str) -> Option<String> {
    let out = quiet("git").args(["config", "--global", key]).output().ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn works(program: &str) -> bool {
    quiet(program).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Checks everything the guide needs to know. Slow (WSL may have to start).
pub fn probe(preferred: &str, agent_cmd: &str) -> Probe {
    let mut p = Probe::default();
    p.win_git = windows_git("user.name").zip(windows_git("user.email"));
    p.zed = works("zed");
    p.gh = works("gh");
    p.gh_auth = p.gh && quiet("gh").args(["auth", "status", "--hostname", "github.com"]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !wsl_present() {
        p.wsl_error = Some("WSL isn't installed on this PC.".into());
        return p;
    }
    let status = wsl_exe(&["--status"]);
    let version = wsl_exe(&["--version"]);
    if let Ok((true, text, _)) = &version {
        p.wsl_version = text.lines().next().and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string());
    }
    p.distros = list_distros();
    p.wsl = matches!(status, Ok((true, _, _))) || matches!(version, Ok((true, _, _))) || !p.distros.is_empty();
    if !p.wsl {
        p.wsl_error = Some(match status {
            Ok((_, out, err)) => short(&said(&out, &err)),
            Err(err) => format!("{err:#}"),
        });
    }
    p.distro = pick_distro(preferred, &p.distros);
    if let Some(distro) = p.distro.clone() {
        let script = format!(
            r#"echo "user: $(whoami)"
echo "os: $(. /etc/os-release 2>/dev/null; echo $PRETTY_NAME)"
for t in curl tar git bwrap socat pytest; do command -v $t >/dev/null 2>&1 && echo "tool: $t"; done
a={agent}; a="${{a/#\~/$HOME}}"; a="${{a%% *}}"
if [ -x "$a" ]; then echo "agent: $(basename "$(dirname "$(readlink -f "$a")")")"; elif command -v "$a" >/dev/null 2>&1; then echo "agent: installed"; fi
c={cli}; c="${{c/#\~/$HOME}}"
if [ -x "$c" ]; then if timeout 25 "$c" auth status </dev/null >/dev/null 2>&1; then echo "login: yes"; else echo "login: no"; fi; fi
echo "gitname: $(git config --global user.name 2>/dev/null)"
echo "gitemail: $(git config --global user.email 2>/dev/null)"
true"#,
            agent = wsl::quote(agent_cmd),
            cli = wsl::quote(&claude_cli(agent_cmd)),
        );
        let d = distro.clone();
        match within(90, move || wsl::run(&d, &script)) {
            Ok(out) => {
                for line in out.lines() {
                    let Some((key, value)) = line.split_once(':') else { continue };
                    let value = value.trim().to_string();
                    match key.trim() {
                        "user" => p.user = Some(value),
                        "os" => p.os = (!value.is_empty()).then_some(value),
                        "tool" => {
                            p.tools.insert(value);
                        }
                        "agent" => p.agent = Some(value),
                        "login" => p.login = Some(value == "yes"),
                        "gitname" => p.git_name = value,
                        "gitemail" => p.git_email = value,
                        _ => {}
                    }
                }
            }
            Err(err) => p.distro_error = Some(format!("{err:#}")),
        }
    }
    p
}

/// Downloads the newest claude-agent-acp for this CPU into AGENT_HOME/<version>
/// and points `current` at it.
fn agent_install_script() -> String {
    format!(
        r#"set -eo pipefail
case "$(uname -m)" in x86_64|amd64) arch=x64 ;; aarch64|arm64) arch=arm64 ;; *) echo "This CPU ($(uname -m)) isn't supported by claude-agent-acp."; false ;; esac
command -v curl >/dev/null || {{ echo "curl is missing. Open the distro and run: sudo apt install -y curl"; false; }}
echo "Looking up the newest claude-agent-acp…"
url=$(curl -fsSLI -o /dev/null -w '%{{url_effective}}' https://github.com/{repo}/releases/latest || true)
v=${{url##*/tag/}}; v=${{v#v}}
case "$v" in [0-9]*) ;; *) v={fallback}; echo "GitHub didn't say; using $v." ;; esac
root="$HOME/.local/share/claude-agent-acp"; dir="$root/$v"
mkdir -p "$root"
exec 9>"$root/.install.lock"
flock -n 9 || {{ echo "Another install of the agent is running."; false; }}
prev=$(readlink -f "$root/current" 2>/dev/null || true)
rm -rf "$dir.part"; mkdir -p "$dir.part"
echo "Downloading claude-agent-acp $v for $arch (about 220 MB)…"
curl -fL --progress-bar "https://github.com/{repo}/releases/download/v$v/claude-agent-acp-linux-$arch.tar.gz" | tar xz -C "$dir.part"
[ -x "$dir.part/claude-agent-acp" ] || {{ echo "The download didn't contain claude-agent-acp."; false; }}
rm -rf "$dir"; mv "$dir.part" "$dir"
ln -sfn "$dir" "$root/current"
# Older versions go, except the one a running agent may still use.
for old in "$root"/*/; do old=${{old%/}}; case "$old" in "$dir"|"$root/current"|"$prev") ;; *) rm -rf "$old" ;; esac; done
echo "Installed claude-agent-acp $v."
true"#,
        repo = AGENT_REPO,
        fallback = AGENT_FALLBACK_VERSION,
    )
}

/// curl's progress bar ends in a percentage ("####   42.7%").
fn percent(line: &str) -> Option<f32> {
    let token = line.split_whitespace().last()?;
    let value: f32 = token.strip_suffix('%')?.parse().ok()?;
    Some((value / 100.0).clamp(0.0, 1.0))
}

// ---- The guide's logic ----------------------------------------------------------------------

impl Monaged {
    /// First start (nothing set up and no projects), or `--setup`: the guide
    /// instead of connecting. Existing installs count as set up.
    pub(crate) fn setup_needed(&self, asked: bool) -> bool {
        if asked {
            return true;
        }
        if self.store.setting("setup:done").is_some() {
            return false;
        }
        if !self.projects.is_empty() {
            let _ = self.store.set_setting("setup:done", "1");
            return false;
        }
        true
    }

    pub(crate) fn open_setup(&mut self, cx: &mut Context<Self>) {
        self.form = Some(Form::Setup);
        self.palette = None;
        self.setup.root_ok = self.store.setting("setup:root_ok").is_some();
        self.setup.restart = restart_pending(self.store.setting("setup:restart"));
        if !self.setup.restart {
            let _ = self.store.delete_setting("setup:restart");
        }
        if self.setup.name.is_none() {
            self.setup.name = Some(cx.new(|cx| TextInput::new("Your name", cx)));
            self.setup.email = Some(cx.new(|cx| TextInput::new("you@example.com", cx)));
            let suggested = distro::suggest_username(&std::env::var("USERNAME").unwrap_or_default());
            self.setup.linux_user = Some(cx.new(|cx| {
                let mut input = TextInput::new("username", cx);
                input.set_text(suggested, cx);
                input
            }));
            self.setup.linux_password = Some(cx.new(|cx| TextInput::new("Password", cx).masked()));
            self.setup.linux_password_again = Some(cx.new(|cx| TextInput::new("The same password again", cx).masked()));
            self.setup.code = Some(cx.new(|cx| TextInput::new("Paste the code here", cx)));
        }
        self.run_setup_probe(cx);
        cx.notify();
    }

    /// Checks everything again in the background.
    pub(crate) fn run_setup_probe(&mut self, cx: &mut Context<Self>) {
        if self.setup.probing {
            return;
        }
        self.setup.probing = true;
        let preferred = self.config.distro.clone();
        let agent_cmd = self.config.agent_cmd.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let found = cx.background_spawn(async move { probe(&preferred, &agent_cmd) }).await;
            view.update(cx, |this, cx| this.on_setup_probe(found, cx)).ok();
        })
        .detach();
    }

    fn on_setup_probe(&mut self, found: Probe, cx: &mut Context<Self>) {
        self.setup.probing = false;
        let was_ready = self.setup.probe.is_some() && self.setup_ready();
        // Use the distro that's there (and remember it) when the preferred one isn't.
        if let Some(distro) = &found.distro
            && *distro != self.config.distro
        {
            self.use_distro(distro.clone(), cx);
        }
        if found.wsl && !found.distros.is_empty() && found.distro_error.is_none() {
            // WSL works, so whatever restart it wanted has happened.
            self.setup.restart = false;
            let _ = self.store.delete_setting("setup:restart");
        }
        // Fill in the git identity once: WSL's own, else Windows'.
        if let (Some(name), Some(email)) = (self.setup.name.clone(), self.setup.email.clone())
            && name.read(cx).text().is_empty()
            && email.read(cx).text().is_empty()
        {
            let (n, e) = if !found.git_name.is_empty() {
                (found.git_name.clone(), found.git_email.clone())
            } else {
                found.win_git.clone().unwrap_or_default()
            };
            name.update(cx, |i, cx| i.set_text(n, cx));
            email.update(cx, |i, cx| i.set_text(e, cx));
        }
        let was_signed_in = self.setup.probe.as_ref().is_some_and(|p| p.login == Some(true));
        if found.login == Some(true) && !was_signed_in && self.setup.probe.is_some() && matches!(self.form, Some(Form::Setup)) {
            self.setup.reconnect = true;
        }
        if found.login == Some(true) && self.logged_in == Some(false) {
            self.logged_in = None;
        }
        let signed_in_now = found.login == Some(true) && !was_signed_in && self.setup.probe.is_some();
        let had_probe = self.setup.probe.is_some();
        self.setup.probe = Some(found);
        // The last required step just got done in the guide: that's the moment.
        if !was_ready && had_probe && self.setup_ready() && matches!(self.form, Some(Form::Setup)) {
            self.chime(crate::sound::Cue::Complete);
        }
        if let Some((step, _)) = &self.setup.error
            && self.step_done(*step)
        {
            self.setup.error = None;
        }
        // Signed in from the bar outside the guide: the connection can use it now.
        if signed_in_now && !matches!(self.form, Some(Form::Setup)) {
            self.setup.reconnect = false;
            self.reconnect_when_idle(cx);
        }
        cx.notify();
    }

    /// Switches Monaged to another distro (remembered for the next start).
    pub(crate) fn use_distro(&mut self, distro: String, cx: &mut Context<Self>) {
        let _ = self.store.set_setting("wsl:distro", &distro);
        self.config.distro = distro;
        self.linux_user = None;
        if matches!(self.form, Some(Form::Setup)) {
            self.setup.reconnect = true;
        } else {
            self.reconnect_now(cx);
        }
    }

    pub(crate) fn step_done(&self, step: StepId) -> bool {
        let Some(p) = &self.setup.probe else { return false };
        match step {
            // Installed but waiting for a restart: the restart prompt stays up.
            StepId::Wsl => p.wsl && !self.setup.restart,
            StepId::Distro => p.distro.is_some() && p.distro_error.is_none() && p.user_ok(self.setup.root_ok),
            StepId::Agent => p.agent.is_some(),
            StepId::Login => p.login == Some(true) || (p.agent.as_deref() == Some("installed") && p.login.is_none()),
            StepId::Git => !p.git_name.is_empty() && !p.git_email.is_empty(),
            StepId::Extras => false,
        }
    }

    /// The first step that still needs something.
    fn current_step(&self) -> StepId {
        STEPS.iter().copied().find(|s| *s != StepId::Extras && !self.step_done(*s) && !self.setup.skipped.contains(s)).unwrap_or(StepId::Extras)
    }

    /// The steps Monaged can't work without are done.
    fn setup_ready(&self) -> bool {
        [StepId::Wsl, StepId::Distro, StepId::Agent, StepId::Login].iter().all(|s| self.step_done(*s))
    }

    fn distro_name(&self) -> String {
        self.setup.probe.as_ref().and_then(|p| p.distro.clone()).unwrap_or_else(|| self.config.distro.clone())
    }

    fn set_job(&mut self, job: Option<Job>, cx: &mut Context<Self>) {
        self.setup.generation += 1;
        if job.is_none() {
            self.stop_live();
        } else {
            self.setup.failed_steps = None;
            self.setup.failed_extra = None;
        }
        self.setup.job = job;
        self.setup.error = None;
        cx.notify();
    }

    /// Stops a sign-in running in the background.
    fn stop_live(&mut self) {
        if let Some(live) = self.setup.live.take() {
            live.stop();
        }
    }

    /// Another step is running: one at a time.
    fn setup_busy(&mut self, cx: &mut Context<Self>) -> bool {
        if self.setup.job.is_some() {
            self.toast(Tone::Info, "Another setup step is still running; wait for it, or cancel it first.", cx);
            return true;
        }
        false
    }

    fn fail_step(&mut self, step: StepId, message: impl Into<String>, cx: &mut Context<Self>) {
        self.chime(crate::sound::Cue::Error);
        self.setup.generation += 1;
        self.setup.job = None;
        self.stop_live();
        self.setup.error = Some((step, message.into()));
        cx.notify();
    }

    /// Updates the running job if it's still the one started at `generation`.
    /// False when it isn't (cancelled, or the guide was closed).
    fn update_job(&mut self, generation: u64, cx: &mut Context<Self>, f: impl FnOnce(&mut Job)) -> bool {
        if self.setup.generation != generation {
            return false;
        }
        if let Some(job) = &mut self.setup.job {
            f(job);
            cx.notify();
        }
        true
    }

    // ---- Actions --------------------------------------------------------------------------

    /// Step 1: WSL itself, as administrator, hidden; what wsl.exe says shows
    /// in the guide as it says it.
    pub(crate) fn setup_install_wsl(&mut self, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        self.set_job(Some(Job::Running { step: StepId::Wsl, line: "Windows asks for permission first…".into(), fraction: None }), cx);
        let generation = self.setup.generation;
        let log = wsl_install_log();
        if let Some(dir) = log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::remove_file(&log);
        let args = install_wsl_args(&log);
        let worker = cx.background_spawn(async move {
            let exe = std::env::current_exe().context("couldn't find Monaged's own program file")?;
            crate::native::run_elevated(&exe, &args).map(|code| code as i32)
        });
        // Shows the log's last line while the install runs.
        cx.spawn(async move |view, cx| {
            let started = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let said = std::fs::read(&log).ok().and_then(|b| log_tail(&b));
                let alive = view
                    .update(cx, |this, cx| {
                        this.update_job(generation, cx, |job| {
                            if let Job::Running { line, fraction, .. } = job {
                                let secs = started.elapsed().as_secs();
                                let time = format!("{}:{:02}", secs / 60, secs % 60);
                                match &said {
                                    Some((text, f)) => {
                                        *line = format!("{text} · {time}");
                                        if f.is_some() {
                                            *fraction = *f;
                                        }
                                    }
                                    None if secs > 3 => *line = format!("Installing WSL… · {time}"),
                                    None => {}
                                }
                            }
                        })
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |view, cx| {
            let code = worker.await;
            view.update(cx, |this, cx| {
                if this.setup.generation != generation {
                    return;
                }
                let said = std::fs::read(wsl_install_log()).ok().and_then(|b| log_tail(&b)).map(|(t, _)| t).unwrap_or_default();
                match code {
                    Ok(0) | Ok(3010) => {
                        this.set_job(None, cx);
                        this.chime(crate::sound::Cue::Success);
                        this.setup.restart = true;
                        let _ = this.store.set_setting("setup:restart", &crate::usage::now_unix().to_string());
                        reopen_after_restart(true);
                        this.run_setup_probe(cx);
                    }
                    Ok(1223) => this.fail_step(StepId::Wsl, "Windows didn't get permission (the administrator prompt was declined or closed).", cx),
                    Ok(code) => this.fail_step(
                        StepId::Wsl,
                        format!(
                            "WSL didn't install (code {code}){}. If it mentions virtualization, turn it on in your PC's BIOS or UEFI settings, then try again.",
                            if said.is_empty() { String::new() } else { format!(": {}", said.trim_end_matches('.')) }
                        ),
                        cx,
                    ),
                    Err(err) => this.fail_step(StepId::Wsl, format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// The username and password typed in the Ubuntu step, checked.
    fn linux_account(&mut self, cx: &mut Context<Self>) -> Option<(String, String)> {
        let (Some(user), Some(password), Some(again)) = (&self.setup.linux_user, &self.setup.linux_password, &self.setup.linux_password_again) else { return None };
        let user_text = user.read(cx).text().trim().to_string();
        let (pw, pw2) = (password.read(cx).text().to_string(), again.read(cx).text().to_string());
        let problem = distro::check_username(&user_text).err().or_else(|| distro::check_password(&pw, &pw2).err());
        if let Some(problem) = problem {
            self.setup.error = Some((StepId::Distro, problem.into()));
            cx.notify();
            return None;
        }
        Some((user_text, pw))
    }

    /// Step 2: downloads Ubuntu from WSL's catalog, installs it, starts it and
    /// creates your user, all in the guide with its progress.
    pub(crate) fn setup_install_distro(&mut self, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        if distro::INSTALLING.load(std::sync::atomic::Ordering::SeqCst) {
            self.toast(Tone::Info, "Ubuntu is still installing in the background; this step updates when it's done.", cx);
            return;
        }
        let Some((user, password)) = self.linux_account(cx) else { return };
        let taken: Vec<String> = registered_distros().into_iter().map(|(name, _)| name).collect();
        self.setup.installing_distro = true;
        self.run_distro_job(&distro::STEPS, move |send| distro::install(&user, &password, &taken, send), cx);
    }

    /// Step 2 when the distro is there but only has root: its first start and
    /// your user, without a console.
    pub(crate) fn setup_create_user(&mut self, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        let Some((user, password)) = self.linux_account(cx) else { return };
        let name = self.distro_name();
        self.setup.installing_distro = false;
        self.run_distro_job(&distro::USER_STEPS, move |send| distro::provision(&name, &user, &password, 0, send).map(|()| name.clone()), cx);
    }

    /// Runs a distro job on its own thread, showing its steps; uses the distro
    /// it names when it worked.
    fn run_distro_job(&mut self, labels: &[&str], work: impl FnOnce(&(dyn Fn(distro::Event) + Sync)) -> Result<String> + Send + 'static, cx: &mut Context<Self>) {
        let items = labels.iter().map(|l| StepView { label: SharedString::from(l.to_string()), detail: None, state: StepState::Waiting, progress: None }).collect();
        self.set_job(Some(Job::Steps { step: StepId::Distro, items }), cx);
        // The passwords aren't needed any more.
        for input in [self.setup.linux_password.clone(), self.setup.linux_password_again.clone()].into_iter().flatten() {
            input.update(cx, |i, cx| i.set_text(String::new(), cx));
        }
        let generation = self.setup.generation;
        let (tx, mut rx) = unbounded::<Result<distro::Event, Result<String, String>>>();
        distro::INSTALLING.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let events = tx.clone();
            let send = move |event: distro::Event| {
                let _ = events.unbounded_send(Ok(event));
            };
            let result = work(&send).map_err(|e| format!("{e:#}"));
            distro::INSTALLING.store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.unbounded_send(Err(result));
        });
        cx.spawn(async move |view, cx| {
            while let Some(message) = rx.next().await {
                let finished = message.is_err();
                view.update(cx, |this, cx| match message {
                    Ok(event) => {
                        this.update_job(generation, cx, |job| {
                            if let Job::Steps { items, .. } = job {
                                apply_distro_event(items, event);
                            }
                        });
                    }
                    Err(result) => this.on_distro_job(generation, result, cx),
                })
                .ok();
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_distro_job(&mut self, generation: u64, result: Result<String, String>, cx: &mut Context<Self>) {
        let current = self.setup.generation == generation;
        match result {
            Ok(name) => {
                if current {
                    self.set_job(None, cx);
                }
                // Remember a distro Monaged installed itself: the uninstaller
                // offers to remove that one (and only that one).
                if std::mem::take(&mut self.setup.installing_distro) {
                    let _ = self.store.set_setting(INSTALLED_DISTRO_SETTING, &name);
                }
                // Left the guide meanwhile? The install still counts.
                self.root_ok_off();
                if name != self.config.distro {
                    self.use_distro(name.clone(), cx);
                }
                self.toast(Tone::Info, format!("{name} is ready, with your user."), cx);
                self.chime(crate::sound::Cue::Success);
                self.run_setup_probe(cx);
            }
            Err(message) if current => {
                // Keep the steps on screen (where it stopped) with the reason under them.
                let items = match self.setup.job.take() {
                    Some(Job::Steps { mut items, .. }) => {
                        for item in items.iter_mut().filter(|i| i.state == StepState::Running) {
                            item.state = StepState::Failed;
                            item.progress = None;
                        }
                        Some(items)
                    }
                    _ => None,
                };
                self.fail_step(StepId::Distro, message, cx);
                self.setup.failed_steps = items;
                self.run_setup_probe(cx);
            }
            Err(message) => self.toast_error(format!("Installing Ubuntu failed: {message}"), cx),
        }
    }

    fn root_ok_off(&mut self) {
        self.setup.root_ok = false;
        let _ = self.store.delete_setting("setup:root_ok");
    }

    /// Step 3: download claude-agent-acp into WSL, with its progress.
    pub(crate) fn setup_install_agent(&mut self, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        let distro = self.distro_name();
        self.set_job(Some(Job::Running { step: StepId::Agent, line: "Starting…".into(), fraction: None }), cx);
        let generation = self.setup.generation;
        let (tx, mut rx) = unbounded::<String>();
        let script = agent_install_script();
        let worker = cx.background_spawn(async move {
            wsl::stream(&distro, &script, |line| {
                let _ = tx.unbounded_send(line.to_string());
            })
        });
        cx.spawn(async move |view, cx| {
            while let Some(line) = rx.next().await {
                let alive = view
                    .update(cx, |this, cx| {
                        if this.setup.generation != generation {
                            return false;
                        }
                        if let Some(Job::Running { line: shown, fraction, .. }) = &mut this.setup.job {
                            match percent(&line) {
                                Some(f) => *fraction = Some(f),
                                None if !line.trim().is_empty() && !line.trim_start().starts_with('#') => *shown = line.trim().to_string(),
                                None => {}
                            }
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
            let result = worker.await;
            view.update(cx, |this, cx| {
                // Left the guide meanwhile? The install still counts.
                let current = this.setup.generation == generation;
                match result {
                    Ok(_) => {
                        if current {
                            this.set_job(None, cx);
                            this.run_setup_probe(cx);
                        }
                        this.toast(Tone::Info, "The Claude agent is installed.", cx);
                        this.chime(crate::sound::Cue::Success);
                        // A connection that failed for want of the agent can work now.
                        if matches!(this.form, Some(Form::Setup)) {
                            this.setup.reconnect = true;
                        } else {
                            this.reconnect_when_idle(cx);
                        }
                    }
                    Err(err) if current => this.fail_step(StepId::Agent, format!("{err:#}"), cx),
                    Err(err) => this.toast_error(format!("Installing the Claude agent failed: {err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Step 4: `claude auth login`, hidden in WSL. The guide shows the
    /// sign-in page's link (and opens it) and a box for the code it shows.
    pub(crate) fn setup_sign_in(&mut self, account: Account, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        let distro = self.distro_name();
        let cli = claude_cli(&self.config.agent_cmd);
        self.set_job(Some(Job::SignIn(SignIn { account, url: None, note: None, checking: false })), cx);
        if let Some(code) = &self.setup.code {
            code.update(cx, |i, cx| i.set_text(String::new(), cx));
        }
        let generation = self.setup.generation;
        let (tx, mut rx) = unbounded::<signin::Event>();
        match signin::start_claude(&distro, &cli, account, move |event| {
            let _ = tx.unbounded_send(event);
        }) {
            Ok(live) => self.setup.live = Some(live),
            Err(err) => return self.fail_step(StepId::Login, format!("{err:#}"), cx),
        }
        cx.spawn(async move |view, cx| {
            while let Some(event) = rx.next().await {
                let alive = view.update(cx, |this, cx| this.on_sign_in_event(generation, event, cx)).unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_sign_in_event(&mut self, generation: u64, event: signin::Event, cx: &mut Context<Self>) -> bool {
        if self.setup.generation != generation {
            return false;
        }
        match event {
            signin::Event::Url(url) => {
                let first = matches!(&self.setup.job, Some(Job::SignIn(s)) if s.url.is_none());
                if let Some(Job::SignIn(s)) = &mut self.setup.job {
                    s.url = Some(url.clone());
                }
                if first {
                    crate::ui::open_web(cx, &url);
                }
            }
            signin::Event::Note(note) => {
                if let Some(Job::SignIn(s)) = &mut self.setup.job {
                    s.note = Some(note);
                    s.checking = false;
                }
            }
            signin::Event::Code(_) => {}
            signin::Event::Finished(Ok(())) => {
                self.setup.live = None;
                self.set_job(None, cx);
                self.logged_in = None;
                self.toast(Tone::Info, "Signed in to Claude.", cx);
                self.chime(crate::sound::Cue::Success);
                if !matches!(self.form, Some(Form::Setup)) {
                    self.reconnect_when_idle(cx);
                }
                self.run_setup_probe(cx);
                return false;
            }
            signin::Event::Finished(Err(why)) => {
                self.setup.live = None;
                self.fail_step(StepId::Login, format!("Signing in didn't finish: {why}"), cx);
                return false;
            }
        }
        cx.notify();
        true
    }

    /// Sends the code from Claude's page to the waiting sign-in.
    pub(crate) fn setup_submit_code(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.setup.code.clone() else { return };
        let code = match signin::check_claude_code(input.read(cx).text()) {
            Ok(code) => code,
            Err(problem) => {
                if let Some(Job::SignIn(s)) = &mut self.setup.job {
                    s.note = Some(problem.into());
                }
                cx.notify();
                return;
            }
        };
        let sent = self.setup.live.as_ref().map(|live| live.send_line(&code));
        if let Some(Job::SignIn(s)) = &mut self.setup.job {
            match sent {
                Some(Ok(())) => {
                    s.checking = true;
                    s.note = None;
                }
                Some(Err(err)) => s.note = Some(format!("{err:#}")),
                None => s.note = Some("The sign-in stopped; start it again.".into()),
            }
        }
        cx.notify();
    }

    /// Enter in one of the guide's fields does what its step's main button does.
    pub(crate) fn setup_enter(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        use gpui::Focusable as _;
        if !matches!(self.form, Some(Form::Setup)) {
            return false;
        }
        let focused = |inputs: &[&Option<Entity<TextInput>>], cx: &Context<Self>| inputs.iter().any(|i| i.as_ref().is_some_and(|i| i.focus_handle(cx).is_focused(window)));
        if focused(&[&self.setup.code], cx) {
            self.setup_submit_code(cx);
        } else if focused(&[&self.setup.linux_user, &self.setup.linux_password, &self.setup.linux_password_again], cx) {
            if self.setup.probe.as_ref().is_some_and(|p| p.distro.is_none()) {
                self.setup_install_distro(cx);
            } else {
                self.setup_create_user(cx);
            }
        } else if focused(&[&self.setup.name, &self.setup.email], cx) {
            self.setup_save_git(cx);
        }
        true
    }

    /// Stops whatever is running (a sign-in, or waiting on one).
    pub(crate) fn setup_cancel(&mut self, cx: &mut Context<Self>) {
        self.set_job(None, cx);
    }

    /// Step 5: git's name and email inside WSL.
    pub(crate) fn setup_save_git(&mut self, cx: &mut Context<Self>) {
        let (Some(name), Some(email)) = (&self.setup.name, &self.setup.email) else { return };
        let (n, e) = (name.read(cx).text().to_string(), email.read(cx).text().to_string());
        if n.trim().is_empty() || !e.contains('@') {
            self.setup.error = Some((StepId::Git, "Enter a name and an email address.".into()));
            cx.notify();
            return;
        }
        let distro = self.distro_name();
        self.set_job(Some(Job::Running { step: StepId::Git, line: "Saving…".into(), fraction: None }), cx);
        let generation = self.setup.generation;
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { crate::git_ops::set_identity(&distro, &n, &e) }).await;
            view.update(cx, |this, cx| {
                if this.setup.generation != generation {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.identity_missing = false;
                        this.set_job(None, cx);
                        this.run_setup_probe(cx);
                    }
                    Err(err) => this.fail_step(StepId::Git, format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Installs an extra: Windows programs with winget, Linux tools with apt
    /// as root in WSL. Its progress shows in its row.
    pub(crate) fn setup_install_extra(&mut self, extra: Extra, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        let distro = self.distro_name();
        self.set_job(Some(Job::Installing { extra, line: "Starting…".into(), fraction: None }), cx);
        let generation = self.setup.generation;
        let (tx, mut rx) = unbounded::<String>();
        let worker = cx.background_spawn(async move {
            let on_line = |line: &str| {
                let _ = tx.unbounded_send(line.to_string());
            };
            match extra.winget_id() {
                Some(id) => crate::extras::winget_install(id, on_line),
                None => crate::extras::apt_install(&distro, extra.apt_packages(), on_line),
            }
        });
        cx.spawn(async move |view, cx| {
            while let Some(line) = rx.next().await {
                let (text, fraction) = crate::extras::read_progress(&line);
                let alive = view
                    .update(cx, |this, cx| {
                        this.update_job(generation, cx, |job| {
                            if let Job::Installing { line, fraction: shown, .. } = job {
                                if let Some(text) = text {
                                    *line = text;
                                }
                                if fraction.is_some() {
                                    *shown = fraction;
                                }
                            }
                        })
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
            let result = worker.await;
            view.update(cx, |this, cx| {
                let current = this.setup.generation == generation;
                match result {
                    Ok(()) => {
                        if current {
                            this.set_job(None, cx);
                        }
                        this.toast(Tone::Info, format!("Installed {}.", extra.title()), cx);
                        this.chime(crate::sound::Cue::Success);
                        this.run_setup_probe(cx);
                        if extra == Extra::Sandbox {
                            this.check_sandbox(cx);
                        }
                    }
                    Err(err) if current => {
                        let step = if extra == Extra::Curl { StepId::Agent } else { StepId::Extras };
                        this.fail_step(step, format!("Installing {} failed: {err:#}", extra.title()), cx);
                        this.setup.failed_extra = Some(extra);
                    }
                    Err(err) => this.toast_error(format!("Installing {} failed: {err:#}", extra.title()), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// `gh auth login` in the browser: shows GitHub's one-time code and opens
    /// the page to enter it on.
    pub(crate) fn setup_github_login(&mut self, cx: &mut Context<Self>) {
        if self.setup_busy(cx) {
            return;
        }
        self.set_job(Some(Job::GitHub { code: None, url: None, note: None }), cx);
        let generation = self.setup.generation;
        let (tx, mut rx) = unbounded::<signin::Event>();
        match signin::start_github(move |event| {
            let _ = tx.unbounded_send(event);
        }) {
            Ok(live) => self.setup.live = Some(live),
            Err(err) => return self.fail_step(StepId::Extras, format!("{err:#}"), cx),
        }
        cx.spawn(async move |view, cx| {
            while let Some(event) = rx.next().await {
                let alive = view
                    .update(cx, |this, cx| {
                        if this.setup.generation != generation {
                            return false;
                        }
                        match event {
                            signin::Event::Code(code) => {
                                if let Some(Job::GitHub { code: c, .. }) = &mut this.setup.job {
                                    *c = Some(code);
                                }
                            }
                            signin::Event::Url(url) => {
                                let first = matches!(&this.setup.job, Some(Job::GitHub { url: None, .. }));
                                if let Some(Job::GitHub { url: u, .. }) = &mut this.setup.job {
                                    *u = Some(url.clone());
                                }
                                if first {
                                    crate::ui::open_web(cx, &url);
                                }
                            }
                            signin::Event::Note(note) => {
                                if let Some(Job::GitHub { note: n, .. }) = &mut this.setup.job {
                                    *n = Some(note);
                                }
                            }
                            signin::Event::Finished(result) => {
                                this.setup.live = None;
                                match result {
                                    Ok(()) => {
                                        this.set_job(None, cx);
                                        this.toast(Tone::Info, "Signed in to GitHub.", cx);
                                        this.chime(crate::sound::Cue::Success);
                                        this.run_setup_probe(cx);
                                    }
                                    Err(why) => this.fail_step(StepId::Extras, format!("GitHub's sign-in didn't finish: {why}"), cx),
                                }
                                return false;
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn setup_skip(&mut self, step: StepId, cx: &mut Context<Self>) {
        if step == StepId::Distro && self.setup.probe.as_ref().is_some_and(|p| p.user.as_deref() == Some("root")) {
            self.setup.root_ok = true;
            let _ = self.store.set_setting("setup:root_ok", "1");
        } else {
            self.setup.skipped.insert(step);
        }
        self.setup.error = None;
        cx.notify();
    }

    /// Leaves the guide: remembers it's done, connects to Claude.
    pub(crate) fn finish_setup(&mut self, then: SetupThen, window: &mut Window, cx: &mut Context<Self>) {
        self.setup.pending = false;
        let _ = self.store.set_setting("setup:done", "1");
        // A restart still pending keeps the guide reopening afterwards.
        if !self.setup.restart {
            reopen_after_restart(false);
        }
        self.setup.job = None;
        self.stop_live();
        self.setup.generation += 1;
        self.form = None;
        window.focus(&self.focus);
        self.after_setup(cx);
        match then {
            SetupThen::Close => {}
            SetupThen::Playground => self.create_playground(cx),
            SetupThen::AddProject => self.open_project_form(window, cx),
        }
        cx.notify();
    }

    /// The guide was left some other way than its own buttons: carry on as if
    /// it had been closed (called while rendering).
    pub(crate) fn left_setup(&mut self, cx: &mut Context<Self>) {
        if !self.setup.pending || matches!(self.form, Some(Form::Setup)) {
            return;
        }
        self.setup.pending = false;
        let _ = self.store.set_setting("setup:done", "1");
        if !self.setup.restart {
            reopen_after_restart(false);
        }
        self.setup.job = None;
        self.stop_live();
        self.setup.generation += 1;
        self.after_setup(cx);
    }

    /// What `new` holds back while the guide is open on a fresh PC.
    pub(crate) fn after_setup(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.conn, crate::Conn::Ready | crate::Conn::Handshaking) {
            self.setup.reconnect = false;
            self.reconnect_now(cx);
        } else if std::mem::take(&mut self.setup.reconnect) {
            self.reconnect_when_idle(cx);
        }
        // Nothing below may run wsl.exe on a PC without WSL.
        if !wsl::installed() {
            return;
        }
        self.check_sandbox(cx);
        // Reviews the quality gate was waiting for when Monaged closed.
        self.resume_gates(cx);
        // Accounts, git's helper setup in WSL (repaired if Monaged moved), SSH keys.
        self.refresh_git_auth(true, cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let user = cx.background_spawn(async move { crate::git::linux_user(&distro) }).await;
            view.update(cx, |this, cx| {
                this.linux_user = user.ok();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reconnects to use a new agent or sign-in, unless agents are working
    /// (a reconnect would stop their turns): then it says so instead.
    pub(crate) fn reconnect_when_idle(&mut self, cx: &mut Context<Self>) {
        if self.agents.iter().any(|a| a.is_working()) {
            self.toast_action(Tone::Info, "Reconnect when the agents are done to use it (a reconnect stops their turns).", "Reconnect", crate::feedback::ToastAction::Reconnect, cx);
        } else {
            self.reconnect_now(cx);
        }
    }

    /// Signs in from outside the guide (the not-signed-in bar, the health
    /// check): opens the guide at its sign-in step and starts it.
    pub(crate) fn sign_in_window(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.form, Some(Form::Setup)) {
            self.open_setup(cx);
        }
        self.setup.skipped.remove(&StepId::Login);
        if self.setup.job.is_none() {
            self.setup_sign_in(Account::Subscription, cx);
        }
    }

    // ---- Rendering ------------------------------------------------------------------------

    pub(crate) fn render_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.current_step();
        let ready = self.setup_ready();
        let done_count = STEPS.iter().filter(|s| **s != StepId::Extras && (self.step_done(**s) || self.setup.skipped.contains(s))).count();
        let required = STEPS.len() - 1;
        let checking = self.setup.probe.is_none();
        let header = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .child(logo_mark(48.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().font_family(theme::DISPLAY).text_size(px(22.)).font_weight(FontWeight::MEDIUM).child(if ready { "Monaged is ready" } else { "Welcome to Monaged" }))
                    .child(ui::meta(if ready {
                        "Everything agents need is in place. Add a project, or try the playground first."
                    } else {
                        "Let's get this PC ready for Claude agents. Each step checks first and asks before installing anything."
                    })),
            )
            .child(if self.setup.probing {
                ui::busy_button("setup-recheck", Kind::Ghost, "Checking…")
            } else {
                ui::button("setup-recheck", Kind::Ghost, Some(Icon::Refresh), "Check again").on_click(cx.listener(|this, _, _, cx| this.run_setup_probe(cx)))
            });
        let progress = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().child(ui::meta(if checking { "Checking this PC…".to_string() } else { format!("{done_count} of {required} steps done") })))
                    // Ready: the next step is right here, not below the extras.
                    .when(ready, |d| {
                        d.child(
                            ui::button("setup-playground-top", Kind::Secondary, Some(Icon::Flow), "Try the playground")
                                .tooltip(crate::shell::tip("A small app with a bug, a flaky test and a merge conflict, to try every feature on"))
                                .on_click(cx.listener(|this, _, window, cx| this.finish_setup(SetupThen::Playground, window, cx))),
                        )
                        .child(
                            ui::button("setup-add-top", Kind::Primary, Some(Icon::Plus), "Add your first project")
                                .on_click(cx.listener(|this, _, window, cx| this.finish_setup(SetupThen::AddProject, window, cx))),
                        )
                    }),
            )
            .child(ui::progress_bar("setup-progress", if checking { None } else { Some(done_count as f32 / required as f32) }));
        let steps = div()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(theme::line()))
            .bg(rgb(theme::panel()))
            .children(STEPS.iter().enumerate().map(|(i, step)| self.render_setup_step(*step, i, *step == current && !checking, cx)));
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                ui::button("setup-skip-all", Kind::Ghost, None, if ready { "Close" } else { "Skip setup for now" })
                    .tooltip(crate::shell::tip("Reopen it any time: Ctrl K → Setup guide, or Settings → General"))
                    .on_click(cx.listener(|this, _, window, cx| this.finish_setup(SetupThen::Close, window, cx))),
            )
            .child(div().flex_1())
            .when(ready, |d| {
                d.child(
                    ui::button("setup-playground", Kind::Secondary, Some(Icon::Flow), "Try the playground")
                        .tooltip(crate::shell::tip("A small app with a bug, a flaky test and a merge conflict, to try every feature on"))
                        .on_click(cx.listener(|this, _, window, cx| this.finish_setup(SetupThen::Playground, window, cx))),
                )
                .child(ui::button("setup-add", Kind::Primary, Some(Icon::Plus), "Add your first project").on_click(cx.listener(|this, _, window, cx| this.finish_setup(SetupThen::AddProject, window, cx))))
            });
        div()
            .id("setup")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div().flex().justify_center().px(px(32.)).py(px(36.)).child(
                    div()
                        .w_full()
                        .max_w(px(760.))
                        .flex()
                        .flex_col()
                        .gap(px(20.))
                        .child(header)
                        .child(progress)
                        .child(steps)
                        .child(footer)
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(theme::text_4()))
                                .child("Monaged never installs anything without your click. Your projects and settings stay in %APPDATA%\\Monaged."),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_setup_step(&self, step: StepId, index: usize, current: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.setup.probe.clone().unwrap_or_default();
        let checking = self.setup.probe.is_none();
        let done = self.step_done(step);
        let skipped = self.setup.skipped.contains(&step);
        let job = self.setup.job.clone().filter(|j| j.step() == step);
        let error = self.setup.error.clone().filter(|(s, _)| *s == step).map(|(_, e)| e);
        let distro = self.distro_name();
        let marker: AnyElement = if job.is_some() {
            ui::spinner(("setup-spin", index), 15., theme::text_2())
        } else if checking {
            div().size(px(9.)).rounded_full().border_1().border_color(rgb(theme::line_strong())).into_any_element()
        } else if done {
            icon_sized(Icon::Check, 15.).text_color(rgb(theme::text_2())).into_any_element()
        } else if error.is_some() {
            icon_sized(Icon::Alert, 15.).text_color(rgb(theme::red_text())).into_any_element()
        } else {
            div()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(if current { theme::text_2() } else { theme::line_strong() }))
                .text_size(px(11.))
                .text_color(rgb(if current { theme::text() } else { theme::text_3() }))
                .child(step.number().to_string())
                .into_any_element()
        };
        // One line under the title: what's there, or what this step is for.
        let summary: String = match step {
            StepId::Wsl if done => format!("Installed{}", p.wsl_version.as_ref().map(|v| format!(" · version {v}")).unwrap_or_default()),
            StepId::Wsl if self.setup.restart => "Installed. Windows needs a restart to finish.".into(),
            StepId::Wsl => "Windows' built-in Linux, where agents run.".into(),
            StepId::Distro if done => format!("{distro} · {} · user {}", p.os.clone().unwrap_or_else(|| "Linux".into()), p.user.clone().unwrap_or_default()),
            StepId::Distro => "A Linux distribution with a user of your own.".into(),
            StepId::Agent if done => match p.agent.as_deref() {
                Some("installed") => format!("Found {}", self.config.agent_cmd),
                Some(v) => format!("claude-agent-acp {v}, with the Claude CLI, in {distro}"),
                None => String::new(),
            },
            StepId::Agent => "claude-agent-acp, which Monaged talks to, with the Claude CLI.".into(),
            StepId::Login if done => "Signed in.".into(),
            StepId::Login => "With your Claude subscription or an Anthropic Console account.".into(),
            StepId::Git if done => format!("{} <{}>", p.git_name, p.git_email),
            StepId::Git => "Agents commit their work under this name inside WSL.".into(),
            StepId::Extras => "Each one turns on a feature; Monaged works without them.".into(),
        };
        let open = (current || error.is_some() || job.is_some() || step == StepId::Extras || (step == StepId::Git && !done && self.step_done(StepId::Distro))) && !checking;
        let title_row = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .child(div().w(px(20.)).flex_none().flex().justify_center().child(marker))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(if done || current || step == StepId::Extras { theme::text() } else { theme::text_3() }))
                                    .when(skipped && !done, |d| d.line_through())
                                    .child(step.title()),
                            )
                            .when(step == StepId::Extras, |d| d.child(tag("optional"))),
                    )
                    .child(div().text_size(px(12.5)).text_color(rgb(theme::text_3())).child(summary)),
            )
            .when(done && step == StepId::Agent && self.setup.job.is_none(), |d| {
                d.child(
                    ui::button("setup-agent-update", Kind::Ghost, Some(Icon::Refresh), "Update")
                        .tooltip(crate::shell::tip("Download the newest claude-agent-acp again"))
                        .on_click(cx.listener(|this, _, _, cx| this.setup_install_agent(cx))),
                )
            });
        let mut body = div().flex().flex_col().gap(px(12.)).pl(px(34.));
        if open && (!done || step == StepId::Extras) {
            body = body.child(self.render_setup_body(step, &p, job.as_ref(), cx));
        }
        if let Some(error) = &error {
            body = body.child(ui::callout(Tone::Error, error.clone()));
        }
        div()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .gap(px(12.))
            .px(px(18.))
            .py(px(16.))
            .when(index > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
            .when(current && !checking, |d| d.bg(rgb(theme::raised())))
            .child(title_row)
            .child(body)
            .into_any_element()
    }

    fn render_setup_body(&self, step: StepId, p: &Probe, job: Option<&Job>, cx: &mut Context<Self>) -> AnyElement {
        let distro = self.distro_name();
        let text = |t: String| div().text_size(px(13.)).line_height(px(20.)).text_color(rgb(theme::text_2())).child(t);
        let mut col = div().flex().flex_col().gap(px(12.));
        if let Some(job) = job {
            match job {
                Job::Running { line, fraction, .. } => {
                    return col
                        .child(one_line(line.clone(), 12.5))
                        .child(div().flex().flex_col().max_w(px(420.)).child(ui::progress_bar(SharedString::from(format!("setup-job-{step:?}")), *fraction)))
                        .into_any_element();
                }
                Job::Steps { items, .. } => {
                    return col
                        .child(ui::steps("setup-distro-steps", items))
                        .child(ui::meta("You can keep using Windows meanwhile; this takes a few minutes."))
                        .into_any_element();
                }
                Job::SignIn(sign_in) => return self.render_claude_sign_in(sign_in, cx),
                // An extra's progress shows in its own row.
                Job::Installing { .. } | Job::GitHub { .. } => {}
            }
        }
        let skip = |id: &'static str, label: &'static str, step: StepId, cx: &mut Context<Self>| {
            ui::button(id, Kind::Ghost, None, label).on_click(cx.listener(move |this, _, _, cx| this.setup_skip(step, cx)))
        };
        match step {
            StepId::Wsl if self.setup.restart => {
                col = col.child(text("WSL is installed. Windows finishes setting it up when it restarts; Monaged opens again afterwards and carries on here.".into())).child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(ui::button("setup-restart", Kind::Primary, Some(Icon::Refresh), "Restart Windows now").on_click(cx.listener(|this, _, window, cx| {
                            this.ask_confirm(crate::feedback::ConfirmAction::RestartWindows, window, cx);
                        })))
                        .child(ui::button("setup-restart-later", Kind::Ghost, None, "I'll restart later").on_click(cx.listener(|this, _, _, cx| {
                            this.toast(Tone::Info, "Monaged opens this guide again after the restart.", cx);
                        })))
                        .child(ui::button("setup-restart-skip", Kind::Ghost, None, "Continue without restarting").on_click(cx.listener(|this, _, _, cx| {
                            this.setup.restart = false;
                            let _ = this.store.delete_setting("setup:restart");
                            this.run_setup_probe(cx);
                        }))),
                );
            }
            StepId::Wsl => {
                col = col
                    .child(text(
                        "Agents run inside WSL, the Linux that comes with Windows. Installing it takes a few minutes and one administrator prompt; Windows usually wants a restart afterwards, and Monaged opens again by itself to carry on.".into(),
                    ))
                    .when_some(p.wsl_error.clone().filter(|e| !e.is_empty()), |d, e| d.child(div().text_size(px(12.)).font_family(theme::MONO).text_color(rgb(theme::text_3())).child(e)))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("setup-wsl", Kind::Primary, Some(Icon::Shield), "Install WSL").on_click(cx.listener(|this, _, _, cx| this.setup_install_wsl(cx))))
                            .child(ui::button("setup-wsl-docs", Kind::Ghost, Some(Icon::External), "About WSL").on_click(cx.listener(|_, _, _, cx| crate::ui::open_web(cx, "https://learn.microsoft.com/windows/wsl/install")))),
                    );
            }
            StepId::Distro => {
                let others: Vec<String> = p.distros.iter().filter(|d| Some(*d) != p.distro.as_ref()).cloned().collect();
                if let Some(items) = &self.setup.failed_steps {
                    col = col.child(ui::steps("setup-distro-failed", items));
                }
                if let Some(err) = &p.distro_error {
                    col = col
                        .child(text(format!(
                            "{distro} is installed but didn't start. Right after installing WSL, Windows usually needs a restart; otherwise virtualization may be off in your PC's BIOS or UEFI settings."
                        )))
                        .child(div().text_size(px(12.)).font_family(theme::MONO).text_color(rgb(theme::text_3())).child(err.clone()))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(ui::button("setup-distro-restart", Kind::Secondary, Some(Icon::Refresh), "Restart Windows").on_click(cx.listener(|this, _, window, cx| {
                                    this.ask_confirm(crate::feedback::ConfirmAction::RestartWindows, window, cx);
                                })))
                                .child(ui::button("setup-distro-retry", Kind::Ghost, Some(Icon::Refresh), "Try again").on_click(cx.listener(|this, _, _, cx| this.run_setup_probe(cx)))),
                        );
                } else if p.distro.is_none() {
                    col = col
                        .child(text(
                            "Monaged downloads Ubuntu from WSL's own catalog (about 420 MB) and sets it up right here. Pick a Linux username and password: your account inside WSL, separate from Windows. Installing Linux packages with sudo asks for this password.".into(),
                        ))
                        .child(self.render_linux_account(cx))
                        .child(div().flex().child(ui::button("setup-ubuntu", Kind::Primary, Some(Icon::ArrowDown), "Install Ubuntu").on_click(cx.listener(|this, _, _, cx| this.setup_install_distro(cx)))));
                } else if !p.user_ok(self.setup.root_ok) {
                    col = col
                        .child(text(format!("{distro} is installed but has no user of your own yet. Pick a Linux username and password, and Monaged creates the user.")))
                        .child(self.render_linux_account(cx))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(ui::button("setup-create-user", Kind::Primary, Some(Icon::Plus), "Create my user").on_click(cx.listener(|this, _, _, cx| this.setup_create_user(cx))))
                                .child(skip("setup-root", "Keep using root", StepId::Distro, cx)),
                        );
                }
                if !others.is_empty() {
                    col = col.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .child(ui::meta("Or use another distro you have:"))
                            .children(others.into_iter().enumerate().map(|(i, d)| {
                                let name = d.clone();
                                ui::choice(("setup-distro", i), d, false).on_click(cx.listener(move |this, _, _, cx| {
                                    this.use_distro(name.clone(), cx);
                                    this.run_setup_probe(cx);
                                }))
                            })),
                    );
                }
            }
            StepId::Agent => {
                if let Some(Job::Installing { extra: Extra::Curl, line, fraction }) = &self.setup.job {
                    col = col.child(install_progress("setup-curl-progress", line, *fraction));
                } else if !p.has("curl") || !p.has("tar") {
                    col = col
                        .child(text(format!("Downloading the agent needs curl and tar, and {distro} doesn't have them yet. Monaged installs them in {distro} for you (no password needed).")))
                        .child(div().flex().child(ui::button("setup-curl", Kind::Primary, Some(Icon::ArrowDown), "Install curl and tar").on_click(cx.listener(|this, _, _, cx| this.setup_install_extra(Extra::Curl, cx)))));
                } else {
                    col = col
                        .child(text(format!(
                            "Monaged runs Claude Code through claude-agent-acp, which comes with the Claude CLI. It downloads from GitHub ({AGENT_REPO}, about 220 MB) into {AGENT_HOME} in {distro}: just for your user, no sudo."
                        )))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(ui::button("setup-agent", Kind::Primary, Some(Icon::ArrowDown), "Install the Claude agent").on_click(cx.listener(|this, _, _, cx| this.setup_install_agent(cx))))
                                .child(ui::button("setup-agent-docs", Kind::Ghost, Some(Icon::External), "About it").on_click(cx.listener(|_, _, _, cx| crate::ui::open_web(cx, &format!("https://github.com/{AGENT_REPO}"))))),
                        );
                }
            }
            StepId::Login => {
                col = col
                    .child(text(
                        "Agents use your own Claude account. Claude's sign-in page opens in your browser; afterwards it shows a code, which you paste here. Monaged never sees your password.".into(),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(
                                ui::button("setup-login", Kind::Primary, Some(Icon::Key), "Sign in with Claude")
                                    .tooltip(crate::shell::tip("A Claude Pro, Max, Team or Enterprise subscription"))
                                    .on_click(cx.listener(|this, _, _, cx| this.setup_sign_in(Account::Subscription, cx))),
                            )
                            .child(
                                ui::button("setup-login-console", Kind::Secondary, None, "Use an Anthropic Console account")
                                    .tooltip(crate::shell::tip("Pay per use with API billing from console.anthropic.com"))
                                    .on_click(cx.listener(|this, _, _, cx| this.setup_sign_in(Account::Console, cx))),
                            )
                            .child(skip("setup-login-skip", "Skip", StepId::Login, cx)),
                    );
            }
            StepId::Git => {
                if let (Some(name), Some(email)) = (&self.setup.name, &self.setup.email) {
                    let windows = p.win_git.is_some() && p.git_name.is_empty();
                    col = col
                        .child(text(format!(
                            "Agents commit their work, so git in {distro} needs a name and email.{}",
                            if windows { " These are the ones git uses on Windows." } else { "" }
                        )))
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(10.))
                                .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(ui::field("Name", None, name.clone())))
                                .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(ui::field("Email", None, email.clone())))
                                .child(ui::button("setup-git-save", Kind::Primary, None, "Save").on_click(cx.listener(|this, _, _, cx| this.setup_save_git(cx))))
                                .child(skip("setup-git-skip", "Skip", StepId::Git, cx)),
                        );
                }
            }
            StepId::Extras => {
                let wsl_ready = self.step_done(StepId::Distro);
                let running = self.setup.job.clone();
                // One extra: a flat flex row, with what it's doing (progress, a
                // sign-in) as a sibling under it. Keep this shape. A row nested in a
                // column, a row with vertical padding, or a text column with
                // `min_w(0)` made the layout misjudge the list's height (pushing the
                // guide's own buttons out of reach), and a block `div()` around the
                // row collapsed its text to one letter per line.
                let row = |ok: bool, title: &'static str, detail: &'static str, fix: Option<AnyElement>, below: Option<AnyElement>| -> Vec<AnyElement> {
                    let line = div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(
                            div().w(px(16.)).flex_none().child(if ok {
                                icon_sized(Icon::Check, 14.).text_color(rgb(theme::text_2())).into_any_element()
                            } else {
                                div().size(px(7.)).rounded_full().border_1().border_color(rgb(theme::line_strong())).into_any_element()
                            }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(13.)).text_color(rgb(theme::text())).child(title))
                                .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(detail)),
                        )
                        .children(fix)
                        .into_any_element();
                    std::iter::once(line).chain(below.map(|b| div().pl(px(28.)).pb(px(6.)).flex().flex_col().child(b).into_any_element())).collect()
                };
                // The Install button, or this extra's progress while it installs.
                let install = |extra: Extra, id: &'static str, cx: &mut Context<Self>| -> (Option<AnyElement>, Option<AnyElement>) {
                    match &running {
                        Some(Job::Installing { extra: e, line, fraction }) if *e == extra => (None, Some(install_progress(id, line, *fraction))),
                        _ => {
                            let website = extra.website().filter(|_| self.setup.failed_extra == Some(extra));
                            let buttons = div()
                                .flex()
                                .gap(px(6.))
                                .when_some(website, |d, site| {
                                    d.child(
                                        ui::button(SharedString::from(format!("{id}-site")), Kind::Ghost, Some(Icon::External), "Website")
                                            .tooltip(crate::shell::tip("Download it from its website instead"))
                                            .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, site))),
                                    )
                                })
                                .child(ui::button(id, Kind::Secondary, Some(Icon::ArrowDown), "Install").on_click(cx.listener(move |this, _, _, cx| this.setup_install_extra(extra, cx))));
                            (Some(buttons.into_any_element()), None)
                        }
                    }
                };
                let (zed_fix, zed_below) = if p.zed { (None, None) } else { install(Extra::Zed, "setup-x-zed", cx) };
                // gh: install, then sign in (GitHub's one-time code shows here).
                let (gh_fix, gh_below): (Option<AnyElement>, Option<AnyElement>) = if !p.gh {
                    install(Extra::Gh, "setup-x-gh", cx)
                } else if !p.gh_auth {
                    match &running {
                        Some(Job::GitHub { code, url, note }) => (None, Some(self.render_github_code(code.clone(), url.clone(), note.clone(), cx))),
                        _ => (
                            Some(ui::button("setup-x-gh-login", Kind::Secondary, Some(Icon::Key), "Sign in").on_click(cx.listener(|this, _, _, cx| this.setup_github_login(cx))).into_any_element()),
                            None,
                        ),
                    }
                } else {
                    (None, None)
                };
                let gh_ok = p.gh && p.gh_auth;
                col = col
                    .children(row(p.zed, "Zed", "Open any worktree or file in Zed with one click.", zed_fix, zed_below))
                    .children(row(
                        gh_ok,
                        "GitHub CLI (gh)",
                        if p.gh && !p.gh_auth { "Installed; sign in so Monaged can update itself while its repository is private." } else { "Optional: Monaged's updates while its repository is private. Pull requests work through Git accounts." },
                        gh_fix,
                        gh_below,
                    ))
                    .children(row(
                        self.creds_ok == Some(true) || !self.git_auth.accounts.is_empty(),
                        "Git accounts",
                        "Sign in to GitHub, GitLab or another host so agents' work can be pushed and pull requests opened.",
                        (self.creds_ok != Some(true) && self.git_auth.accounts.is_empty())
                            .then(|| ui::button("setup-x-accounts", Kind::Secondary, Some(Icon::User), "Sign in").on_click(cx.listener(|this, _, window, cx| this.open_settings_at("Git accounts", window, cx))).into_any_element()),
                        None,
                    ))
                    .when(wsl_ready, |d| {
                        let (sandbox_fix, sandbox_below) = if p.has("bwrap") && p.has("socat") { (None, None) } else { install(Extra::Sandbox, "setup-x-sandbox", cx) };
                        let (pytest_fix, pytest_below) = if p.has("pytest") { (None, None) } else { install(Extra::Pytest, "setup-x-pytest", cx) };
                        d.children(row(
                            p.has("bwrap") && p.has("socat"),
                            "Sandbox tools (bubblewrap, socat)",
                            "Let Claude Code's sandbox keep agents' commands away from Windows and your other files.",
                            sandbox_fix,
                            sandbox_below,
                        ))
                        .children(row(p.has("pytest"), "pytest", "Run Python projects' tests for the confidence score and the Run tab.", pytest_fix, pytest_below))
                    })
                    .children(row(
                        self.api_key.is_some(),
                        "Anthropic API key",
                        "Optional: pay for Haiku summaries and reviews from API credit instead of your plan.",
                        self.api_key
                            .is_none()
                            .then(|| ui::button("setup-x-key", Kind::Secondary, None, "Settings").on_click(cx.listener(|this, _, window, cx| this.open_settings_at("API key", window, cx))).into_any_element()),
                        None,
                    ))
                    .child(ui::meta("Linux tools install in WSL as root, without asking for your Linux password. Windows programs install with winget; Windows may ask for permission."));
            }
        }
        col.into_any_element()
    }

    /// The Linux username and password fields.
    fn render_linux_account(&self, cx: &mut Context<Self>) -> AnyElement {
        let (Some(user), Some(password), Some(again)) = (self.setup.linux_user.clone(), self.setup.linux_password.clone(), self.setup.linux_password_again.clone()) else {
            return div().into_any_element();
        };
        let _ = cx;
        let column = || div().flex_1().min_w(px(0.)).flex().flex_col();
        div()
            .flex()
            .items_start()
            .gap(px(10.))
            .child(column().child(ui::field("Linux username", Some("Lowercase letters, digits, _ and -"), user)))
            .child(column().child(ui::field("Password", None, password)))
            .child(column().child(ui::field("Repeat password", None, again)))
            .into_any_element()
    }

    /// Signing in to Claude: the page's link, and the box for its code.
    fn render_claude_sign_in(&self, sign_in: &SignIn, cx: &mut Context<Self>) -> AnyElement {
        let text = |t: String| div().text_size(px(13.)).line_height(px(20.)).text_color(rgb(theme::text_2())).child(t);
        let mut col = div().flex().flex_col().gap(px(12.));
        let Some(url) = sign_in.url.clone() else {
            return col
                .child(div().flex().items_center().gap(px(8.)).child(ui::spinner("setup-signin-start", 14., theme::text_3())).child(ui::meta("Starting the sign-in…")))
                .child(div().flex().child(ui::button("setup-signin-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| this.setup_cancel(cx)))))
                .into_any_element();
        };
        let account = match sign_in.account {
            Account::Subscription => "your Claude account",
            Account::Console => "your Anthropic Console account",
        };
        let link = url.clone();
        let copy = url.clone();
        col = col
            .child(text(format!("1. Sign in with {account} on the page that opened in your browser. If it didn't open, open it from here:")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .bg(rgb(theme::bg()))
                    .child(div().flex_1().min_w(px(0.)).font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::text_2())).truncate().child(url.clone()))
                    .child(ui::button("setup-signin-open", Kind::Secondary, Some(Icon::External), "Open the page").on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &link))))
                    .child(ui::button("setup-signin-copy", Kind::Ghost, Some(Icon::Copy), "Copy link").on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        this.toast(Tone::Info, "Copied the sign-in link.", cx);
                    }))),
            )
            .child(text("2. When the page shows a code, copy it and paste it here:".into()));
        if let Some(code) = self.setup.code.clone() {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w(px(0.)).flex().flex_col().child(code))
                    .child(if sign_in.checking {
                        ui::busy_button("setup-signin-send", Kind::Primary, "Checking…").into_any_element()
                    } else {
                        ui::button("setup-signin-send", Kind::Primary, Some(Icon::Check), "Sign in").on_click(cx.listener(|this, _, _, cx| this.setup_submit_code(cx))).into_any_element()
                    }),
            );
        }
        col.when_some(sign_in.note.clone(), |d, note| d.child(div().text_size(px(12.5)).text_color(rgb(theme::red_text())).child(note)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::meta("Monaged never sees your password; the code only lets this PC's Claude CLI sign in."))
                    .child(div().flex_1())
                    .child(ui::button("setup-signin-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| this.setup_cancel(cx)))),
            )
            .into_any_element()
    }

    /// GitHub's sign-in: the one-time code to type on its page.
    fn render_github_code(&self, code: Option<String>, url: Option<String>, note: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let Some(code) = code else {
            return div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ui::spinner("setup-gh-start", 14., theme::text_3()))
                .child(ui::meta("Asking GitHub for a code…"))
                .child(div().flex_1())
                .child(ui::button("setup-gh-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| this.setup_cancel(cx))))
                .into_any_element();
        };
        let page = url.unwrap_or_else(|| "https://github.com/login/device".into());
        let copy = code.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child("Enter this code on GitHub's page (it opened in your browser):"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().px(px(10.)).py(px(4.)).rounded(px(6.)).border_1().border_color(rgb(theme::line_strong())).font_family(theme::MONO).text_size(px(16.)).child(code))
                    .child(ui::button("setup-gh-copy", Kind::Ghost, Some(Icon::Copy), "Copy code").on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        this.toast(Tone::Info, "Copied the code.", cx);
                    })))
                    .child(ui::button("setup-gh-open", Kind::Secondary, Some(Icon::External), "Open GitHub").on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &page))))
                    .child(div().flex_1())
                    .child(ui::button("setup-gh-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| this.setup_cancel(cx)))),
            )
            .child(div().flex().items_center().gap(px(8.)).child(ui::spinner("setup-gh-wait", 13., theme::text_3())).child(ui::meta(note.unwrap_or_else(|| "Waiting for GitHub…".into()))))
            .into_any_element()
    }
}

/// Moves the steps of a distro job along.
fn apply_distro_event(items: &mut [StepView], event: distro::Event) {
    match event {
        distro::Event::Running(index, detail, fraction) => {
            for (i, item) in items.iter_mut().enumerate() {
                if i < index && item.state != StepState::Done {
                    item.state = StepState::Done;
                    item.progress = None;
                }
            }
            if let Some(item) = items.get_mut(index) {
                item.state = StepState::Running;
                item.detail = Some(detail.into());
                item.progress = Some(fraction);
            }
        }
        distro::Event::Done(index, detail) => {
            if let Some(item) = items.get_mut(index) {
                item.state = StepState::Done;
                item.detail = Some(detail.into());
                item.progress = None;
            }
        }
    }
}

/// Where to go once the guide is done.
#[derive(Clone, Copy)]
pub(crate) enum SetupThen {
    Close,
    Playground,
    AddProject,
}

/// A small outlined label.
fn tag(text: &'static str) -> impl IntoElement {
    div().px(px(6.)).rounded(px(4.)).border_1().border_color(rgb(theme::line_strong())).text_size(px(10.5)).text_color(rgb(theme::text_3())).child(text)
}

/// One line of output in mono, cut off with … when it's too long.
fn one_line(text: String, size: f32) -> impl IntoElement {
    div().flex().child(div().flex_1().min_w(px(0.)).text_size(px(size)).font_family(theme::MONO).text_color(rgb(theme::text_2())).truncate().child(text))
}

/// A line of what's installing, with its bar.
fn install_progress(id: &'static str, line: &str, fraction: Option<f32>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(one_line(line.to_string(), 12.))
        .child(div().flex().flex_col().max_w(px(420.)).child(ui::progress_bar(SharedString::from(format!("{id}-bar")), fraction)))
        .into_any_element()
}

/// Monaged's mark, as in the app icon: a dark rounded square with a
/// dot-matrix M and the red dot top left.
pub(crate) fn logo_mark(size: f32) -> impl IntoElement {
    div()
        .size(px(size))
        .flex_none()
        .relative()
        .rounded(px(size * 0.22))
        .bg(rgb(0x0f0f0f))
        .border_1()
        .border_color(rgb(0x2f2f2f))
        .child(div().absolute().left(px(size * 0.335)).top(px(size * 0.245)).child(crate::dots::dot_text("M", size * 0.05, gpui::rgb(0xededed))))
        .child(div().absolute().left(px(size * 0.125)).top(px(size * 0.105)).size(px(size * 0.13)).rounded_full().bg(rgb(0xd71921)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_preferred_distro_then_ubuntu_then_the_first() {
        let list = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_distro("Ubuntu", &list(&["Debian", "Ubuntu"])).as_deref(), Some("Ubuntu"));
        assert_eq!(pick_distro("ubuntu", &list(&["Ubuntu"])).as_deref(), Some("Ubuntu"));
        assert_eq!(pick_distro("Ubuntu", &list(&["Debian", "Ubuntu-24.04"])).as_deref(), Some("Ubuntu-24.04"));
        assert_eq!(pick_distro("Ubuntu", &list(&["Debian"])).as_deref(), Some("Debian"));
        assert_eq!(pick_distro("Ubuntu", &[]), None);
    }

    #[test]
    fn finds_the_claude_cli_next_to_the_agent() {
        assert_eq!(claude_cli("~/.local/share/claude-agent-acp/current/claude-agent-acp"), "~/.local/share/claude-agent-acp/current/claude");
        assert_eq!(claude_cli("claude-agent-acp"), "claude");
    }

    #[test]
    fn the_wsl_install_runs_monaged_itself_with_the_log() {
        assert_eq!(
            install_wsl_args(std::path::Path::new(r"C:\Users\Sam Rivera\AppData\Local\Monaged\wsl-install.log")),
            r#"--install-wsl "C:\Users\Sam Rivera\AppData\Local\Monaged\wsl-install.log""#
        );
    }

    #[test]
    fn shows_the_last_thing_the_wsl_install_said() {
        assert_eq!(log_tail(b"Downloading: WSL 2.6.1\r\n[====    42.0%    ]\r"), Some(("Installing…".to_string(), Some(0.42))));
        assert_eq!(log_tail(b"Installing: Windows Subsystem for Linux\r\n\r\n"), Some(("Installing: Windows Subsystem for Linux".to_string(), None)));
        assert_eq!(log_tail(b"  \r\n"), None);
    }

    #[test]
    fn a_restart_counts_once_windows_started_after_the_install() {
        assert!(!restart_pending(None));
        assert!(!restart_pending(Some("garbage".into())));
        // Installed long before this boot: done. Installed in the future: still pending.
        assert!(!restart_pending(Some("1".into())));
        assert!(restart_pending(Some((crate::usage::now_unix() + 3600).to_string())));
    }

    #[test]
    fn reads_curls_progress() {
        assert_eq!(percent("######################                     42.0%"), Some(0.42));
        assert_eq!(percent("Downloading claude-agent-acp 0.107.0"), None);
        assert_eq!(percent("100.0%"), Some(1.0));
    }

    #[test]
    fn the_install_script_has_no_unexpanded_placeholders() {
        let script = agent_install_script();
        assert!(script.contains("agentclientprotocol/claude-agent-acp/releases/latest"));
        assert!(script.contains("claude-agent-acp-linux-$arch.tar.gz"));
        assert!(!script.contains("{repo}") && !script.contains("{fallback}"));
    }
}
