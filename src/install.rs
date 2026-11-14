//! Installing Monaged just for you, without admin rights. The setup file (the
//! same exe, named Monaged-Setup-<version>.exe) copies itself to
//! %LOCALAPPDATA%\Programs\Monaged, adds Start menu shortcuts (one of them to
//! uninstall) and lists itself under Settings → Apps for your user only
//! (HKCU). Projects and settings live in %APPDATA%\Monaged and are never
//! touched by installing, updating or uninstalling.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result};
use gpui::{Context, IntoElement, Render, Window, div, prelude::*, px, rgb};

use crate::icons::Icon;
use crate::ui::{self, Kind};
use crate::theme;

pub fn install_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Programs").join("Monaged")
}

pub fn installed_exe() -> PathBuf {
    install_dir().join("monaged.exe")
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Monaged runs from its installed place (updates replace this file).
pub fn running_installed() -> bool {
    std::env::current_exe().is_ok_and(|exe| same_file(&exe, &installed_exe()))
}

/// The exe was started as the setup file.
pub fn is_setup_exe() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .is_some_and(|n| n.starts_with("monaged-setup"))
}

/// Removes the copy an update or install moved aside (it was still running
/// then), and a staged copy an interrupted update left.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join("monaged.old.exe"));
        let _ = std::fs::remove_file(dir.join("monaged.new.exe"));
    }
}

/// Puts `new` where `target` is, even while `target` is running (Windows lets
/// a running exe be renamed, not overwritten). The new file is copied next to
/// it first, so a failed copy leaves the old one untouched, and a failed swap
/// puts it back.
pub fn replace_exe(new: &Path, target: &Path) -> Result<()> {
    let staged = target.with_file_name("monaged.new.exe");
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(new, &staged).with_context(|| format!("couldn't write {}", staged.display()))?;
    let old = target.with_file_name("monaged.old.exe");
    let moved = target.exists();
    if moved {
        let _ = std::fs::remove_file(&old);
        if let Err(err) = std::fs::rename(target, &old) {
            let _ = std::fs::remove_file(&staged);
            return Err(err).with_context(|| format!("couldn't move {} aside", target.display()));
        }
    }
    if let Err(err) = std::fs::rename(&staged, target) {
        if moved {
            let _ = std::fs::rename(&old, target);
        }
        let _ = std::fs::remove_file(&staged);
        return Err(err).with_context(|| format!("couldn't put the new {} in place", target.display()));
    }
    Ok(())
}

fn start_menu() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Microsoft").join("Windows").join("Start Menu").join("Programs")
}

/// A .lnk through Windows Script Host (PowerShell is always there).
fn shortcut(link: &Path, target: &Path, args: &str, description: &str) -> Result<()> {
    let ps = |s: &str| s.replace('\'', "''");
    let script = format!(
        "$s = New-Object -ComObject WScript.Shell; $l = $s.CreateShortcut('{link}'); $l.TargetPath = '{target}'; $l.Arguments = '{args}'; \
         $l.WorkingDirectory = '{dir}'; $l.IconLocation = '{target},0'; $l.Description = '{description}'; $l.Save()",
        link = ps(&link.display().to_string()),
        target = ps(&target.display().to_string()),
        args = ps(args),
        dir = ps(&target.parent().map(|p| p.display().to_string()).unwrap_or_default()),
        description = ps(description),
    );
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let output = command.output().context("couldn't run PowerShell to make a shortcut")?;
    if !output.status.success() {
        anyhow::bail!("couldn't make the shortcut: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

fn desktop() -> PathBuf {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Desktop")
}

const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Monaged";

fn reg(args: &[&str]) -> Result<()> {
    let mut command = Command::new("reg.exe");
    command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let status = command.status().context("couldn't run reg.exe")?;
    if !status.success() {
        anyhow::bail!("reg.exe {} failed", args.first().copied().unwrap_or_default());
    }
    Ok(())
}

/// Lists Monaged under Settings → Apps (for this user), with its version,
/// icon and uninstaller.
pub fn register(exe: &Path) -> Result<()> {
    let exe_text = exe.display().to_string();
    let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let size_kb = std::fs::metadata(exe).map(|m| m.len() / 1024).unwrap_or(0).to_string();
    let uninstall = format!("\"{exe_text}\" --uninstall");
    let icon = format!("{exe_text},0");
    let strings = [
        ("DisplayName", "Monaged"),
        ("DisplayVersion", env!("CARGO_PKG_VERSION")),
        ("Publisher", "Monaged"),
        ("DisplayIcon", icon.as_str()),
        ("InstallLocation", dir.as_str()),
        ("UninstallString", uninstall.as_str()),
        ("URLInfoAbout", "https://github.com/Marten-Mrfc/monaged"),
        ("HelpLink", "https://github.com/Marten-Mrfc/monaged/blob/main/docs/troubleshooting.md"),
        ("Comments", "Run Claude Code agents on your projects, inside WSL, from one window"),
    ];
    for (name, value) in strings {
        reg(&["add", UNINSTALL_KEY, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])?;
    }
    for (name, value) in [("NoModify", "1"), ("NoRepair", "1"), ("EstimatedSize", size_kb.as_str())] {
        reg(&["add", UNINSTALL_KEY, "/v", name, "/t", "REG_DWORD", "/d", value, "/f"])?;
    }
    Ok(())
}

/// After an update the installed copy is a new version: keep Settings → Apps in step.
pub fn refresh_registration(store: &crate::store::Store) {
    if !running_installed() || store.setting("install:registered").as_deref() == Some(env!("CARGO_PKG_VERSION")) {
        return;
    }
    if register(&installed_exe()).is_ok() {
        let _ = store.set_setting("install:registered", env!("CARGO_PKG_VERSION"));
    }
}

/// Installs the running exe; returns where it went.
pub fn install(desktop_shortcut: bool) -> Result<PathBuf> {
    let src = std::env::current_exe().context("couldn't find this program's file")?;
    let dst = installed_exe();
    std::fs::create_dir_all(install_dir()).with_context(|| format!("couldn't create {}", install_dir().display()))?;
    if !same_file(&src, &dst) {
        replace_exe(&src, &dst)?;
    }
    std::fs::create_dir_all(start_menu()).ok();
    shortcut(&start_menu().join("Monaged.lnk"), &dst, "", "Run Claude agents on your projects")?;
    shortcut(&start_menu().join("Uninstall Monaged.lnk"), &dst, "--uninstall", "Remove Monaged (your projects and settings stay)")?;
    if desktop_shortcut {
        shortcut(&desktop().join("Monaged.lnk"), &dst, "", "Run Claude agents on your projects")?;
    }
    // Settings → Apps lists it; failing that only loses the listing.
    let _ = register(&dst);
    Ok(dst)
}

/// Removes the shortcuts and, once this process ends, the program folder.
pub fn uninstall() -> Result<()> {
    for link in [start_menu().join("Monaged.lnk"), start_menu().join("Uninstall Monaged.lnk"), desktop().join("Monaged.lnk")] {
        let _ = std::fs::remove_file(link);
    }
    let _ = reg(&["delete", UNINSTALL_KEY, "/f"]);
    crate::onboarding::reopen_after_restart(false);
    // The folder can only go once this window (running from it) closes: try
    // every second or so for five minutes. ping waits where `timeout` can't
    // (it needs a console's input).
    let dir = install_dir().display().to_string();
    let line = format!("/C for /l %i in (1,1,300) do (ping -n 2 127.0.0.1 >nul & rmdir /s /q \"{dir}\" 2>nul & if not exist \"{dir}\" exit)");
    let mut command = Command::new("cmd.exe");
    // Not from inside the folder it deletes.
    command.current_dir(std::env::temp_dir());
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
        command.raw_arg(&line);
    }
    #[cfg(not(windows))]
    command.arg(&line);
    command.spawn().context("couldn't schedule removing the program folder")?;
    Ok(())
}

// ---- The setup window ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum SetupMode {
    Install,
    Uninstall,
}

enum SetupState {
    Ready,
    Working,
    Done(Option<PathBuf>),
    Failed(String),
}

pub struct SetupView {
    mode: SetupMode,
    desktop: bool,
    state: SetupState,
}

impl SetupView {
    pub fn new(mode: SetupMode) -> Self {
        Self { mode, desktop: false, state: SetupState::Ready }
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.state = SetupState::Working;
        let (mode, desktop) = (self.mode, self.desktop);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match mode {
                        SetupMode::Install => install(desktop).map(Some),
                        SetupMode::Uninstall => uninstall().map(|_| None),
                    }
                })
                .await;
            let removed = matches!(mode, SetupMode::Uninstall) && result.is_ok();
            view.update(cx, |this, cx| {
                this.state = match result {
                    Ok(path) => SetupState::Done(path),
                    Err(err) => SetupState::Failed(format!("{err:#}")),
                };
                cx.notify();
            })
            .ok();
            // The program folder can only be removed once this window is gone.
            if removed {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                cx.update(|cx| cx.quit()).ok();
            }
        })
        .detach();
    }
}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let version = env!("CARGO_PKG_VERSION");
        let (title, body) = match self.mode {
            SetupMode::Install => (
                format!("Install Monaged {version}"),
                format!(
                    "Monaged goes into {} (just for you, no admin needed) with Start menu shortcuts. When it opens, a setup guide checks WSL, Ubuntu and the Claude agent and installs what's missing, asking first. Your projects and settings in %APPDATA%\\Monaged stay as they are.",
                    install_dir().display()
                ),
            ),
            SetupMode::Uninstall => (
                "Uninstall Monaged".to_string(),
                "Removes the program and its shortcuts. Your projects, settings and agent worktrees stay, so installing again picks up where you were.".to_string(),
            ),
        };
        let actions = match &self.state {
            SetupState::Ready => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(self.mode == SetupMode::Install, |d| {
                    let on = self.desktop;
                    d.child(ui::switch("setup-desktop", "Desktop shortcut", on).on_click(cx.listener(move |this, _, _, cx| {
                        this.desktop = !on;
                        cx.notify();
                    })))
                })
                .child(div().flex_1())
                .child(ui::button("setup-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                .child(
                    ui::button("setup-go", if self.mode == SetupMode::Install { Kind::Primary } else { Kind::Danger }, None, if self.mode == SetupMode::Install { "Install" } else { "Uninstall" })
                        .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
                ),
            SetupState::Working => div().flex().child(div().flex_1()).child(ui::busy_button("setup-busy", Kind::Primary, if self.mode == SetupMode::Install { "Installing…" } else { "Removing…" })),
            SetupState::Done(path) => {
                let path = path.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(13.)).text_color(rgb(theme::text_2())).child(if self.mode == SetupMode::Install { "Installed. Monaged's setup guide takes it from here." } else { "Removed. This window closes by itself." }))
                    .child(ui::button("setup-close", Kind::Ghost, None, "Close").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                    .children(path.map(|p| {
                        ui::button("setup-open", Kind::Primary, Some(Icon::Play), "Open Monaged").on_click(cx.listener(move |_, _, _, cx| {
                            let _ = Command::new(&p).spawn();
                            cx.quit();
                        }))
                    }))
            }
            SetupState::Failed(err) => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(ui::callout(crate::agent::Tone::Error, err.clone()))
                .child(div().flex().gap(px(8.)).child(div().flex_1()).child(ui::button("setup-retry", Kind::Secondary, None, "Try again").on_click(cx.listener(|this, _, _, cx| this.run(cx))))),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(28.))
            .bg(rgb(theme::bg()))
            .text_color(rgb(theme::text()))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .child(div().flex().items_center().gap(px(14.)).child(crate::onboarding::logo_mark(40.)).child(ui::title(title)))
            .child(div().line_height(px(20.)).text_color(rgb(theme::text_2())).child(body))
            .child(div().flex_1())
            .child(actions)
    }
}
