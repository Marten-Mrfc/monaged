//! Installing Monaged just for you, without admin rights. The setup file (the
//! same exe, named Monaged-Setup-<version>.exe) copies itself to
//! %LOCALAPPDATA%\Programs\Monaged, adds Start menu shortcuts (one of them to
//! uninstall) and lists itself under Settings → Apps for your user only
//! (HKCU). Projects and settings live in %APPDATA%\Monaged: installing and
//! updating never touch them, and uninstalling removes them only when you
//! tick them (see `uninstall.rs`).

use std::path::{Path, PathBuf};

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

/// A .lnk through the Shell's own ShellLink object (no PowerShell: security
/// software rightly stops apps that start script hosts).
fn shortcut(link: &Path, target: &Path, args: &str, description: &str) -> Result<()> {
    crate::native::create_shortcut(link, target, args, description)
}

fn desktop() -> PathBuf {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Desktop")
}

/// Settings → Apps' entry for Monaged, under HKCU.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Monaged";

/// Lists Monaged under Settings → Apps (for this user), with its version,
/// icon and uninstaller.
pub fn register(exe: &Path) -> Result<()> {
    let exe_text = exe.display().to_string();
    let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
    let size_kb = u32::try_from(std::fs::metadata(exe).map(|m| m.len() / 1024).unwrap_or(0)).unwrap_or(u32::MAX);
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
    use crate::native::{Hive, reg_set_dword, reg_set_string};
    for (name, value) in strings {
        reg_set_string(Hive::CurrentUser, UNINSTALL_KEY, name, value)?;
    }
    for (name, value) in [("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", size_kb)] {
        reg_set_dword(Hive::CurrentUser, UNINSTALL_KEY, name, value)?;
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
    shortcut(&start_menu().join("Uninstall Monaged.lnk"), &dst, "--uninstall", "Remove Monaged, choosing what goes with it")?;
    if desktop_shortcut {
        shortcut(&desktop().join("Monaged.lnk"), &dst, "", "Run Claude agents on your projects")?;
    }
    // Settings → Apps lists it; failing that only loses the listing.
    let _ = register(&dst);
    Ok(dst)
}

/// The shortcuts `install` makes, whether or not they're there.
pub fn shortcut_paths() -> [PathBuf; 3] {
    [start_menu().join("Monaged.lnk"), start_menu().join("Uninstall Monaged.lnk"), desktop().join("Monaged.lnk")]
}

/// Settings → Apps lists Monaged (for this user).
pub fn listed() -> bool {
    crate::native::reg_key_exists(crate::native::Hive::CurrentUser, UNINSTALL_KEY)
}

/// What updates and the setup guide leave behind: the update download in
/// %TEMP% and the Ubuntu download and WSL install log in %LOCALAPPDATA%\Monaged.
fn leftovers() -> Vec<PathBuf> {
    let mut list = vec![std::env::temp_dir().join("monaged-update")];
    if let Some(local) = crate::uninstall::local_root() {
        list.push(local.join("downloads"));
        list.push(local.join("wsl-install.log"));
    }
    list
}

/// Removes the shortcuts, the Settings → Apps entry, the start-after-restart
/// entry, setup leftovers and the program folder. Says what it removed.
pub fn remove_program() -> Result<String> {
    let mut removed: Vec<String> = Vec::new();
    let links = shortcut_paths().into_iter().filter(|link| link.exists() && std::fs::remove_file(link).is_ok()).count();
    if links > 0 {
        removed.push(format!("{links} shortcut{}", if links == 1 { "" } else { "s" }));
    }
    if listed() && crate::native::reg_delete_tree(crate::native::Hive::CurrentUser, UNINSTALL_KEY).is_ok() {
        removed.push("the entry in Settings → Apps".into());
    }
    crate::onboarding::reopen_after_restart(false);
    let mut cleared = 0;
    for path in leftovers() {
        let gone = if path.is_dir() { std::fs::remove_dir_all(&path).is_ok() } else { path.is_file() && std::fs::remove_file(&path).is_ok() };
        cleared += usize::from(gone);
    }
    if cleared > 0 {
        removed.push("leftover downloads and logs".into());
    }
    // %LOCALAPPDATA%\Monaged itself, if nothing else is in it now.
    if let Some(local) = crate::uninstall::local_root() {
        let _ = std::fs::remove_dir(local);
    }
    let dir = install_dir();
    let mut text = if removed.is_empty() { String::new() } else { format!("Removed {}.", crate::uninstall::and_list(&removed)) };
    if dir.exists() {
        anyhow::ensure!(crate::uninstall::program_folder_ok(&dir), "{} doesn't look like Monaged's program folder, so it was left alone", dir.display());
        remove_program_folder(&dir)?;
        text = format!("{text} Removed the program folder.").trim().to_string();
    }
    Ok(if text.is_empty() { "Nothing was left to remove.".into() } else { text })
}

/// Removes the program folder now. This uninstaller may be running from it:
/// Windows lets a running program be moved but not deleted, so it moves
/// itself to %TEMP% first (no helper process waiting for it to exit; a
/// hidden cmd.exe doing that is what security software flags). A Monaged
/// window that just closed may hold on for a moment, so it tries for a while.
fn remove_program_folder(dir: &Path) -> Result<()> {
    crate::native::move_running_exe_out(dir)?;
    let mut last = None;
    for _ in 0..20 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(_) if !dir.exists() => return Ok(()),
            Err(err) => last = Some(err),
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last.map(anyhow::Error::from).unwrap_or_else(|| anyhow::anyhow!("it's still in use"))).with_context(|| format!("couldn't remove {}; close Monaged and delete it by hand", dir.display()))
}

// ---- The setup window ---------------------------------------------------------------------

/// Which small window `monaged.exe` opens instead of the main one.
#[derive(Clone, Copy, PartialEq)]
pub enum SetupMode {
    Install,
    /// `--uninstall`: see `uninstall::UninstallView`.
    Uninstall,
}

enum SetupState {
    Ready,
    Working,
    Done(PathBuf),
    Failed(String),
}

/// The install window (the setup file, or `--install`).
pub struct SetupView {
    desktop: bool,
    state: SetupState,
}

impl SetupView {
    pub fn new() -> Self {
        Self { desktop: false, state: SetupState::Ready }
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        self.state = SetupState::Working;
        let desktop = self.desktop;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { install(desktop) }).await;
            view.update(cx, |this, cx| {
                this.state = match result {
                    Ok(path) => SetupState::Done(path),
                    Err(err) => SetupState::Failed(format!("{err:#}")),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for SetupView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let version = env!("CARGO_PKG_VERSION");
        let title = format!("Install Monaged {version}");
        let body = format!(
            "Monaged goes into {} (just for you, no admin needed) with Start menu shortcuts. When it opens, a setup guide checks WSL, Ubuntu and the Claude agent and installs what's missing, asking first. Your projects and settings in %APPDATA%\\Monaged stay as they are.",
            install_dir().display()
        );
        let actions = match &self.state {
            SetupState::Ready => {
                let on = self.desktop;
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::switch("setup-desktop", "Desktop shortcut", on).on_click(cx.listener(move |this, _, _, cx| {
                        this.desktop = !on;
                        cx.notify();
                    })))
                    .child(div().flex_1())
                    .child(ui::button("setup-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                    .child(ui::button("setup-go", Kind::Primary, None, "Install").on_click(cx.listener(|this, _, _, cx| this.run(cx))))
            }
            SetupState::Working => div().flex().child(div().flex_1()).child(ui::busy_button("setup-busy", Kind::Primary, "Installing…")),
            SetupState::Done(path) => {
                let path = path.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(13.)).text_color(rgb(theme::text_2())).child("Installed. Monaged's setup guide takes it from here."))
                    .child(ui::button("setup-close", Kind::Ghost, None, "Close").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                    .child(ui::button("setup-open", Kind::Primary, Some(Icon::Play), "Open Monaged").on_click(cx.listener(move |_, _, _, cx| {
                        let _ = std::process::Command::new(&path).spawn();
                        cx.quit();
                    })))
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
