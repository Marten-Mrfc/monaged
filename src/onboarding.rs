//! The setup guide: what a new PC needs before Monaged works, checked and
//! installed one step at a time, each one asked first.
//!
//! 1. WSL itself: one administrator prompt (`wsl --install --no-distribution`),
//!    usually followed by a restart; Monaged opens again afterwards.
//! 2. Ubuntu, installed in its own window (`wsl --install -d Ubuntu`), where
//!    you pick your Linux username and password.
//! 3. The Claude agent (claude-agent-acp, which bundles the Claude CLI),
//!    downloaded into `~/.local/share/claude-agent-acp` in WSL. No sudo.
//! 4. Signing in to Claude in a terminal window (`claude auth login`).
//! 5. Git's name and email inside WSL, so agents can commit.
//! 6. Extras that make some features work (Zed, gh, glab, pytest, the
//!    sandbox tools): checked, with the command to install each.
//!
//! Steps that wait for you in another window are watched, and the guide moves
//! on by itself when they're done.

use std::collections::HashSet;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, ClipboardItem, Context, Entity, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon_sized};
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Form, Monaged, theme, wsl};

/// Where the Claude agent (with the Claude CLI bundled) is published.
pub const AGENT_REPO: &str = "agentclientprotocol/claude-agent-acp";
/// Used when GitHub can't say which release is the newest.
const AGENT_FALLBACK_VERSION: &str = "0.107.0";
/// The distro Monaged installs and prefers.
pub const DEFAULT_DISTRO: &str = "Ubuntu";
/// Where the agent goes inside WSL; `current` points at the active version.
pub const AGENT_HOME: &str = "~/.local/share/claude-agent-acp";

#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

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
    pub glab: bool,
}

impl Probe {
    fn has(&self, tool: &str) -> bool {
        self.tools.contains(tool)
    }

    fn user_ok(&self, root_ok: bool) -> bool {
        self.user.as_deref().is_some_and(|u| !u.is_empty() && (u != "root" || root_ok))
    }
}

/// Something running for a step: an installer, or a window Monaged waits on.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Job {
    /// Monaged runs it and shows its output.
    Running { step: StepId, line: String, fraction: Option<f32> },
    /// A window of its own is open (Ubuntu's first start, the sign-in); the
    /// guide checks every few seconds whether it's done.
    Waiting { step: StepId, note: String },
}

impl Job {
    fn step(&self) -> StepId {
        match self {
            Job::Running { step, .. } | Job::Waiting { step, .. } => *step,
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
    /// The window Monaged opened for a step, while it's open.
    window: Option<Arc<Mutex<Child>>>,
    /// Bumped by every job so stale polls stop.
    generation: u64,
    /// The distro, the agent or the sign-in changed: reconnect when the guide closes.
    reconnect: bool,
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

/// `wsl.exe <args>`: (succeeded, what it printed). WSL_UTF8 makes wsl.exe's
/// own messages UTF-8 instead of UTF-16.
fn wsl_exe(args: &[&str]) -> Result<(bool, String)> {
    let out = quiet("wsl.exe").args(args).env("WSL_UTF8", "1").output().context("wsl.exe isn't on this Windows (WSL needs Windows 10 2004 or newer)")?;
    let mut text = wsl::decode(&out.stdout);
    let err = wsl::decode(&out.stderr);
    if !err.trim().is_empty() {
        text.push('\n');
        text.push_str(&err);
    }
    Ok((out.status.success(), text.replace('\0', "").trim().to_string()))
}

fn short(text: &str) -> String {
    text.lines().map(str::trim).filter(|l| !l.is_empty()).take(3).collect::<Vec<_>>().join(" ")
}

/// Whether WSL is installed, without running wsl.exe: on a PC without WSL,
/// Windows' wsl.exe stub may offer to install it by itself, and Monaged only
/// installs things when you click. Looks for the WSL package, its services,
/// and distros registered for this user.
pub fn wsl_present() -> bool {
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    if std::path::Path::new(&program_files).join("WSL").join("wsl.exe").exists() {
        return true;
    }
    let key_exists = |key: &str| quiet("reg.exe").args(["query", key]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    key_exists(r"HKLM\SYSTEM\CurrentControlSet\Services\WSLService")
        || key_exists(r"HKLM\SYSTEM\CurrentControlSet\Services\LxssManager")
        || !registered_distros().is_empty()
}

/// Distros registered for this user (from the registry), with their default user id.
pub fn registered_distros() -> Vec<(String, Option<u32>)> {
    let Ok(out) = quiet("reg.exe").args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss", "/s"]).output() else { return Vec::new() };
    parse_lxss(&String::from_utf8_lossy(&out.stdout))
}

/// `reg query …\Lxss /s` output: one block per distro key.
fn parse_lxss(text: &str) -> Vec<(String, Option<u32>)> {
    let mut found = Vec::new();
    let mut name: Option<String> = None;
    let mut uid: Option<u32> = None;
    let flush = |name: &mut Option<String>, uid: &mut Option<u32>, found: &mut Vec<(String, Option<u32>)>| {
        if let Some(n) = name.take() {
            found.push((n, uid.take()));
        }
        *uid = None;
    };
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            flush(&mut name, &mut uid, &mut found);
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            ["DistributionName", "REG_SZ", rest @ ..] if !rest.is_empty() => name = Some(rest.join(" ")),
            ["DefaultUid", "REG_DWORD", value] => uid = u32::from_str_radix(value.trim_start_matches("0x"), 16).ok(),
            _ => {}
        }
    }
    flush(&mut name, &mut uid, &mut found);
    found
}

/// Distros WSL has, without the ones container tools install for themselves.
pub fn list_distros() -> Vec<String> {
    match wsl_exe(&["--list", "--quiet"]) {
        Ok((true, text)) => text
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
    match agent_cmd.trim().rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/claude"),
        None => "claude".into(),
    }
}

/// Opens a visible terminal window running `wsl.exe <args>`.
fn open_window(args: &[String]) -> Result<Child> {
    let mut command = Command::new("wsl.exe");
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    command.spawn().context("couldn't open a window for WSL")
}

/// Runs a program as administrator (one UAC prompt) in its own window and
/// waits for it. Returns its exit code; 1223 means the prompt was declined.
fn run_elevated(program: &str, args: &[&str]) -> Result<i32> {
    let list = args.iter().map(|a| format!("'{}'", a.replace('\'', "''"))).collect::<Vec<_>>().join(",");
    let script = format!(
        "try {{ $p = Start-Process -FilePath '{program}' -ArgumentList {list} -Verb RunAs -Wait -PassThru -ErrorAction Stop; exit $p.ExitCode }} catch {{ exit 1223 }}"
    );
    let out = quiet("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().context("couldn't run PowerShell")?;
    Ok(out.status.code().unwrap_or(-1))
}

/// Makes Windows start Monaged once after the next sign-in (to carry on after a restart).
pub fn reopen_after_restart(on: bool) {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\RunOnce";
    let mut command = quiet("reg.exe");
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        command.args(["add", key, "/v", "Monaged", "/t", "REG_SZ", "/d", &format!("\"{}\" --setup", exe.display()), "/f"]);
    } else {
        command.args(["delete", key, "/v", "Monaged", "/f"]);
    }
    let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
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
    p.glab = works("glab");
    if !wsl_present() {
        p.wsl_error = Some("WSL isn't installed on this PC.".into());
        return p;
    }
    let status = wsl_exe(&["--status"]);
    let version = wsl_exe(&["--version"]);
    if let Ok((true, text)) = &version {
        p.wsl_version = text.lines().next().and_then(|l| l.split_once(':')).map(|(_, v)| v.trim().to_string());
    }
    p.distros = list_distros();
    p.wsl = matches!(status, Ok((true, _))) || matches!(version, Ok((true, _))) || !p.distros.is_empty();
    if !p.wsl {
        p.wsl_error = Some(match status {
            Ok((_, text)) => short(&text),
            Err(err) => format!("{err:#}"),
        });
    }
    p.distro = pick_distro(preferred, &p.distros);
    if let Some(distro) = p.distro.clone() {
        let script = format!(
            r#"echo "user: $(whoami)"
echo "os: $(. /etc/os-release 2>/dev/null; echo $PRETTY_NAME)"
for t in curl tar git bwrap socat pytest; do command -v $t >/dev/null 2>&1 && echo "tool: $t"; done
a={agent}; a="${{a/#\~/$HOME}}"
if [ -x "$a" ]; then echo "agent: $(basename "$(dirname "$(readlink -f "$a")")")"; elif command -v "$a" >/dev/null 2>&1; then echo "agent: installed"; fi
c={cli}; c="${{c/#\~/$HOME}}"
if [ -x "$c" ]; then if timeout 25 "$c" auth status </dev/null >/dev/null 2>&1; then echo "login: yes"; else echo "login: no"; fi; fi
echo "gitname: $(git config --global user.name 2>/dev/null)"
echo "gitemail: $(git config --global user.email 2>/dev/null)"
true"#,
            agent = wsl::quote(agent_cmd),
            cli = wsl::quote(&claude_cli(agent_cmd)),
        );
        match wsl::run(&distro, &script) {
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
mkdir -p "$root"; rm -rf "$dir.part"; mkdir -p "$dir.part"
echo "Downloading claude-agent-acp $v for $arch (about 220 MB)…"
curl -fL --progress-bar "https://github.com/{repo}/releases/download/v$v/claude-agent-acp-linux-$arch.tar.gz" | tar xz -C "$dir.part"
[ -x "$dir.part/claude-agent-acp" ] || {{ echo "The download didn't contain claude-agent-acp."; false; }}
rm -rf "$dir"; mv "$dir.part" "$dir"
ln -sfn "$dir" "$root/current"
for old in "$root"/*/; do old=${{old%/}}; [ "$old" = "$dir" ] || [ "$old" = "$root/current" ] || rm -rf "$old"; done
echo "Installed claude-agent-acp $v."
true"#,
        repo = AGENT_REPO,
        fallback = AGENT_FALLBACK_VERSION,
    )
}

/// What the sign-in window runs. No double quotes anywhere: wsl.exe drops
/// them from its arguments.
fn sign_in_script(cli: &str) -> String {
    let cli = match cli.strip_prefix("~/") {
        Some(rest) => format!("$HOME/{}", wsl::quote(rest)),
        None => wsl::quote(cli),
    };
    format!(
        "c={cli}; echo 'Sign in to Claude for Monaged. A browser opens; if it does not, open the link shown below.'; echo; $c auth login; echo; read -r -p 'Done? Press Enter to close this window. ' _"
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
        self.setup.restart = self.store.setting("setup:restart").is_some();
        if self.setup.name.is_none() {
            self.setup.name = Some(cx.new(|cx| TextInput::new("Your name", cx)));
            self.setup.email = Some(cx.new(|cx| TextInput::new("you@example.com", cx)));
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
        if found.login == Some(true) && !was_signed_in && self.setup.probe.is_some() {
            self.setup.reconnect = true;
        }
        if found.login == Some(true) && self.logged_in == Some(false) {
            self.logged_in = None;
        }
        self.setup.probe = Some(found);
        // A window we were waiting on did its job: stop waiting.
        if let Some(Job::Waiting { step, .. }) = &self.setup.job
            && self.step_done(*step)
        {
            let step = *step;
            self.setup.job = None;
            self.setup.window = None;
            self.setup.generation += 1;
            self.toast(Tone::Info, format!("{}: done.", step.title()), cx);
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
            StepId::Wsl => p.wsl,
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
            self.setup.window = None;
        }
        self.setup.job = job;
        self.setup.error = None;
        cx.notify();
    }

    fn fail_step(&mut self, step: StepId, message: impl Into<String>, cx: &mut Context<Self>) {
        self.setup.generation += 1;
        self.setup.job = None;
        self.setup.window = None;
        self.setup.error = Some((step, message.into()));
        cx.notify();
    }

    /// Checks every few seconds until the step is done, the window closes and
    /// a last check is in, or another job starts.
    fn watch_step(&mut self, cx: &mut Context<Self>) {
        let generation = self.setup.generation;
        cx.spawn(async move |view, cx| {
            let mut closed_checks = 0;
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                let go = view
                    .update(cx, |this, cx| {
                        if this.setup.generation != generation || this.setup.job.is_none() {
                            return false;
                        }
                        let closed = this.setup.window.as_ref().is_some_and(|w| w.lock().ok().is_none_or(|mut c| c.try_wait().ok().flatten().is_some()));
                        if closed {
                            closed_checks += 1;
                        }
                        if closed_checks > 2 {
                            // The window is gone and the step still isn't done.
                            let step = this.setup.job.as_ref().map(Job::step).unwrap_or(StepId::Wsl);
                            this.setup.job = None;
                            this.setup.window = None;
                            if !this.step_done(step) {
                                this.setup.error = Some((step, "The window closed before this step was done. Try again, or check again if you finished it.".into()));
                            }
                            cx.notify();
                            return false;
                        }
                        this.run_setup_probe(cx);
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        })
        .detach();
    }

    // ---- Actions --------------------------------------------------------------------------

    /// Step 1: WSL itself, as administrator, without a distro (that installs
    /// as you, in step 2).
    pub(crate) fn setup_install_wsl(&mut self, cx: &mut Context<Self>) {
        self.set_job(Some(Job::Running { step: StepId::Wsl, line: "Windows asks for permission, then installs WSL in its own window…".into(), fraction: None }), cx);
        let generation = self.setup.generation;
        cx.spawn(async move |view, cx| {
            let code = cx.background_spawn(async move { run_elevated("wsl.exe", &["--install", "--no-distribution"]) }).await;
            view.update(cx, |this, cx| {
                if this.setup.generation != generation {
                    return;
                }
                match code {
                    Ok(0) | Ok(3010) => {
                        this.set_job(None, cx);
                        this.setup.restart = true;
                        let _ = this.store.set_setting("setup:restart", "1");
                        reopen_after_restart(true);
                        this.run_setup_probe(cx);
                    }
                    Ok(1223) => this.fail_step(StepId::Wsl, "Windows didn't get permission (the administrator prompt was declined or closed).", cx),
                    Ok(code) => this.fail_step(StepId::Wsl, format!("wsl --install stopped with code {code}. Its window said why; if it mentions virtualization, turn it on in your PC's BIOS or UEFI settings."), cx),
                    Err(err) => this.fail_step(StepId::Wsl, format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Step 2: Ubuntu in a window of its own: it downloads, then asks for a
    /// Linux username and password.
    pub(crate) fn setup_install_distro(&mut self, cx: &mut Context<Self>) {
        let args: Vec<String> = ["--install", "-d", DEFAULT_DISTRO].iter().map(|s| s.to_string()).collect();
        match open_window(&args) {
            Ok(child) => {
                self.set_job(
                    Some(Job::Waiting {
                        step: StepId::Distro,
                        note: "Ubuntu is installing in its own window. When it asks, pick a Linux username and password (they're only for WSL), then close that window.".into(),
                    }),
                    cx,
                );
                self.setup.window = Some(Arc::new(Mutex::new(child)));
                self.watch_step(cx);
            }
            Err(err) => self.fail_step(StepId::Distro, format!("{err:#}"), cx),
        }
    }

    /// Step 2 when the distro is there but only has root: open it so its
    /// first start asks for a user.
    pub(crate) fn setup_open_distro(&mut self, cx: &mut Context<Self>) {
        let distro = self.distro_name();
        match open_window(&["-d".into(), distro.clone(), "--cd".into(), "~".into()]) {
            Ok(child) => {
                self.set_job(
                    Some(Job::Waiting { step: StepId::Distro, note: format!("{distro} opened in its own window. Create your user when it asks, then close the window.") }),
                    cx,
                );
                self.setup.window = Some(Arc::new(Mutex::new(child)));
                self.watch_step(cx);
            }
            Err(err) => self.fail_step(StepId::Distro, format!("{err:#}"), cx),
        }
    }

    /// Step 3: download claude-agent-acp into WSL, with its progress.
    pub(crate) fn setup_install_agent(&mut self, cx: &mut Context<Self>) {
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
                if this.setup.generation != generation {
                    return;
                }
                match result {
                    Ok(_) => {
                        this.set_job(None, cx);
                        this.toast(Tone::Info, "The Claude agent is installed.", cx);
                        this.run_setup_probe(cx);
                        // A connection that failed for want of the agent can work now.
                        if matches!(this.form, Some(Form::Setup)) {
                            this.setup.reconnect = true;
                        } else {
                            this.reconnect_now(cx);
                        }
                    }
                    Err(err) => this.fail_step(StepId::Agent, format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Step 4: `claude auth login` in a terminal window.
    pub(crate) fn setup_sign_in(&mut self, cx: &mut Context<Self>) {
        let distro = self.distro_name();
        let script = sign_in_script(&claude_cli(&self.config.agent_cmd));
        match open_window(&["-d".into(), distro, "--cd".into(), "~".into(), "--".into(), "bash".into(), "-lc".into(), script]) {
            Ok(child) => {
                self.set_job(
                    Some(Job::Waiting {
                        step: StepId::Login,
                        note: "A terminal window opened. Pick how you sign in (a Claude Pro or Max subscription, or an Anthropic Console account) and finish in your browser. Monaged notices by itself.".into(),
                    }),
                    cx,
                );
                self.setup.window = Some(Arc::new(Mutex::new(child)));
                self.watch_step(cx);
            }
            Err(err) => self.fail_step(StepId::Login, format!("{err:#}"), cx),
        }
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
        let _ = self.store.set_setting("setup:done", "1");
        let _ = self.store.delete_setting("setup:restart");
        reopen_after_restart(false);
        self.setup.job = None;
        self.setup.window = None;
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

    /// What `new` holds back while the guide is open on a fresh PC.
    pub(crate) fn after_setup(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.setup.reconnect) || !matches!(self.conn, crate::Conn::Ready | crate::Conn::Handshaking) {
            self.reconnect_now(cx);
        }
        self.check_sandbox(cx);
        self.check_credentials(cx);
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

    /// The login command for a terminal (the banner and the health check).
    pub(crate) fn login_command(&self) -> String {
        format!("wsl -d {} -- bash -lc \"{} auth login\"", self.config.distro, claude_cli(&self.config.agent_cmd))
    }

    /// Opens the sign-in window from outside the guide (the not-logged-in bar).
    pub(crate) fn sign_in_window(&mut self, cx: &mut Context<Self>) {
        self.setup_sign_in(cx);
        self.toast(Tone::Info, "Sign in in the terminal window, then press Reconnect.", cx);
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
            .gap(px(6.))
            .child(ui::meta(if checking { "Checking this PC…".to_string() } else { format!("{done_count} of {required} steps done") }))
            .child(ui::progress_bar("setup-progress", if checking { None } else { Some(done_count as f32 / required as f32) }));
        let steps = div()
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
            StepId::Extras => "Optional. Each one turns on a feature.".into(),
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
            .when(done && step == StepId::Agent, |d| {
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
            return match job {
                Job::Running { line, fraction, .. } => col
                    .child(div().text_size(px(12.5)).font_family(theme::MONO).text_color(rgb(theme::text_2())).truncate().child(line.clone()))
                    .child(div().max_w(px(420.)).child(ui::progress_bar(SharedString::from(format!("setup-job-{step:?}")), *fraction)))
                    .into_any_element(),
                Job::Waiting { note, .. } => col
                    .child(text(note.clone()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::spinner("setup-wait", 14., theme::text_3()))
                            .child(ui::meta("Waiting; this page moves on by itself."))
                            .child(div().flex_1())
                            .child(ui::button("setup-wait-check", Kind::Secondary, Some(Icon::Refresh), "Check now").on_click(cx.listener(|this, _, _, cx| this.run_setup_probe(cx))))
                            .child(ui::button("setup-wait-stop", Kind::Ghost, None, "Stop waiting").on_click(cx.listener(|this, _, _, cx| this.set_job(None, cx)))),
                    )
                    .into_any_element(),
            };
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
                    .when_some(p.wsl_error.clone().filter(|e| !e.is_empty()), |d, e| d.child(div().text_size(px(12.)).font_family(theme::MONO).text_color(rgb(theme::text_3())).child(format!("wsl.exe: {e}"))))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("setup-wsl", Kind::Primary, Some(Icon::Shield), "Install WSL").on_click(cx.listener(|this, _, _, cx| this.setup_install_wsl(cx))))
                            .child(ui::button("setup-wsl-docs", Kind::Ghost, Some(Icon::External), "About WSL").on_click(cx.listener(|_, _, _, cx| cx.open_url("https://learn.microsoft.com/windows/wsl/install")))),
                    );
            }
            StepId::Distro => {
                let others: Vec<String> = p.distros.iter().filter(|d| Some(*d) != p.distro.as_ref()).cloned().collect();
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
                                .child(ui::button("setup-distro-open", Kind::Ghost, Some(Icon::Terminal), format!("Open {distro}")).on_click(cx.listener(|this, _, _, cx| this.setup_open_distro(cx)))),
                        );
                } else if p.distro.is_none() {
                    col = col
                        .child(text(
                            "Ubuntu installs in its own window (a download of about 500 MB). When it's done it asks you to pick a Linux username and password: your account inside WSL, separate from Windows. Remember the password; installing Linux packages asks for it.".into(),
                        ))
                        .child(ui::button("setup-ubuntu", Kind::Primary, Some(Icon::Plus), "Install Ubuntu").on_click(cx.listener(|this, _, _, cx| this.setup_install_distro(cx))));
                } else if !p.user_ok(self.setup.root_ok) {
                    col = col
                        .child(text(format!("{distro} is installed but has no user of your own yet. Open it, pick a username and password when it asks, then close the window.")))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(ui::button("setup-open-distro", Kind::Primary, Some(Icon::Terminal), format!("Open {distro}")).on_click(cx.listener(|this, _, _, cx| this.setup_open_distro(cx))))
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
                if !p.has("curl") || !p.has("tar") {
                    let command = "sudo apt update && sudo apt install -y curl tar";
                    col = col
                        .child(text(format!("Downloading the agent needs curl and tar, and {distro} doesn't have them. Open {distro}, run this command (it asks for your Linux password), then check again:")))
                        .child(command_row("setup-curl", command, cx))
                        .child(ui::button("setup-curl-open", Kind::Secondary, Some(Icon::Terminal), format!("Open {distro}")).on_click(cx.listener(|this, _, _, cx| this.open_distro_terminal(cx))));
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
                                .child(ui::button("setup-agent-docs", Kind::Ghost, Some(Icon::External), "About it").on_click(cx.listener(|_, _, _, cx| cx.open_url(&format!("https://github.com/{AGENT_REPO}"))))),
                        );
                }
            }
            StepId::Login => {
                col = col
                    .child(text(
                        "Agents use your own Claude account: a Claude Pro or Max subscription, or an Anthropic Console account with API billing. Signing in happens in a terminal window and your browser; Monaged never sees your password.".into(),
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("setup-login", Kind::Primary, Some(Icon::Key), "Sign in").on_click(cx.listener(|this, _, _, cx| this.setup_sign_in(cx))))
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
                                .child(div().flex_1().child(ui::field("Name", None, name.clone())))
                                .child(div().flex_1().child(ui::field("Email", None, email.clone())))
                                .child(ui::button("setup-git-save", Kind::Primary, None, "Save").on_click(cx.listener(|this, _, _, cx| this.setup_save_git(cx))))
                                .child(skip("setup-git-skip", "Skip", StepId::Git, cx)),
                        );
                }
            }
            StepId::Extras => {
                let wsl_ready = self.step_done(StepId::Distro);
                let row = |ok: bool, title: &'static str, detail: &'static str, fix: Option<AnyElement>| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .py(px(6.))
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
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(13.)).text_color(rgb(theme::text())).child(title))
                                .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(detail)),
                        )
                        .children((!ok).then_some(fix).flatten())
                        .into_any_element()
                };
                col = col
                    .child(row(p.zed, "Zed", "Open any worktree or file in Zed with one click.", Some(copy_button("setup-x-zed", "winget install -e --id ZedIndustries.Zed", cx))))
                    .child(row(p.gh,
                        "GitHub CLI (gh)",
                        "Pull requests, CI status, and updates while Monaged's repo is private. Run gh auth login afterwards.",
                        Some(copy_button("setup-x-gh", "winget install -e --id GitHub.cli", cx)),
                    ))
                    .child(row(p.glab, "GitLab CLI (glab)", "Merge requests and pipelines for GitLab projects. Run glab auth login afterwards.", Some(copy_button("setup-x-glab", "winget install -e --id GLab.GLab", cx))))
                    .when(wsl_ready, |d| {
                        d.child(row(self.creds_ok == Some(true),
                            "Git logins from Windows",
                            "Push and clone from WSL with the logins Git Credential Manager keeps on Windows.",
                            Some(ui::button("setup-x-gcm", Kind::Secondary, None, "Turn on").on_click(cx.listener(|this, _, _, cx| this.enable_windows_credentials(cx))).into_any_element()),
                        ))
                        .child(row(p.has("bwrap") && p.has("socat"),
                            "Sandbox tools (bubblewrap, socat)",
                            "Let Claude Code's sandbox keep agents' commands away from Windows and your other files.",
                            Some(copy_button("setup-x-sandbox", "sudo apt install -y bubblewrap socat", cx)),
                        ))
                        .child(row(p.has("pytest"), "pytest", "Run Python projects' tests for the confidence score and the Run tab.", Some(copy_button("setup-x-pytest", "sudo apt install -y python3-pytest", cx))))
                    })
                    .child(row(self.api_key.is_some(),
                        "Anthropic API key",
                        "Optional: pay for Haiku summaries and reviews from API credit instead of your plan.",
                        Some(ui::button("setup-x-key", Kind::Secondary, None, "Settings").on_click(cx.listener(|this, _, window, cx| this.open_settings_at("Reviews", window, cx))).into_any_element()),
                    ))
                    .child(ui::meta("Commands starting with sudo run in WSL (open your distro's terminal); winget commands run in Windows Terminal or PowerShell."));
            }
        }
        col.into_any_element()
    }

    /// A terminal in the distro (for commands the guide asks you to run).
    pub(crate) fn open_distro_terminal(&mut self, cx: &mut Context<Self>) {
        let distro = self.distro_name();
        if let Err(err) = open_window(&["-d".into(), distro, "--cd".into(), "~".into()]) {
            self.toast_error(format!("{err:#}"), cx);
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

/// A copy button for an install command (shows the command in its tooltip).
fn copy_button(id: &'static str, command: &'static str, cx: &mut Context<Monaged>) -> AnyElement {
    ui::button(id, Kind::Ghost, Some(Icon::Copy), "Copy command")
        .tooltip(crate::shell::tip(command))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(command.to_string()));
            this.toast(Tone::Info, format!("Copied: {command}"), cx);
        }))
        .into_any_element()
}

/// A command in mono with a copy button.
fn command_row(id: &'static str, command: &'static str, cx: &mut Context<Monaged>) -> impl IntoElement {
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
        .child(div().flex_1().min_w(px(0.)).font_family(theme::MONO).text_size(px(12.)).child(command))
        .child(copy_button(id, command, cx))
}

/// Monaged's mark: a dark rounded square with a dot-matrix M and the red dot.
pub(crate) fn logo_mark(size: f32) -> impl IntoElement {
    let dot = size * 0.075;
    div()
        .size(px(size))
        .flex_none()
        .relative()
        .rounded(px(size * 0.22))
        .bg(rgb(0x0f0f0f))
        .border_1()
        .border_color(rgb(0x2f2f2f))
        .flex()
        .items_center()
        .justify_center()
        .child(div().pt(px(size * 0.1)).child(crate::dots::dot_text("M", dot, gpui::rgb(0xededed))))
        .child(div().absolute().top(px(size * 0.13)).left(px(size * 0.15)).size(px(size * 0.13)).rounded_full().bg(rgb(0xd71921)))
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
    fn reads_distros_from_the_registry() {
        let text = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\r\n    DefaultDistribution    REG_SZ    {a}\r\n\r\n\
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{a}\r\n    State    REG_DWORD    0x1\r\n    DistributionName    REG_SZ    Ubuntu\r\n    DefaultUid    REG_DWORD    0x3e8\r\n\r\n\
HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{b}\r\n    DistributionName    REG_SZ    docker-desktop\r\n    DefaultUid    REG_DWORD    0x0\r\n";
        assert_eq!(parse_lxss(text), vec![("Ubuntu".to_string(), Some(1000)), ("docker-desktop".to_string(), Some(0))]);
        assert!(parse_lxss("").is_empty());
    }

    #[test]
    fn finds_the_claude_cli_next_to_the_agent() {
        assert_eq!(claude_cli("~/.local/share/claude-agent-acp/current/claude-agent-acp"), "~/.local/share/claude-agent-acp/current/claude");
        assert_eq!(claude_cli("claude-agent-acp"), "claude");
    }

    #[test]
    fn the_sign_in_script_has_no_double_quotes() {
        let script = sign_in_script("~/.local/share/claude-agent-acp/current/claude");
        assert!(!script.contains('"'));
        assert!(script.starts_with("c=$HOME/'.local/share/claude-agent-acp/current/claude';"));
        assert!(script.contains("$c auth login"));
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
