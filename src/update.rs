//! Updates from the GitHub releases of Monaged's (private) repo, through the
//! GitHub CLI's login: check (by hand, or once a day), then "Update and
//! restart" downloads the new setup exe and swaps it in. Only the installed
//! copy updates itself; a development build points you to installing.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use gpui::{AnyElement, Context, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::agent::Tone;
use crate::icons::Icon;
use crate::ui::{self, Kind};
use crate::{Monaged, install, theme};

pub const DEFAULT_REPO: &str = "Marten-Mrfc/monaged";
/// Checked again after a day.
const CHECK_EVERY: i64 = 24 * 60 * 60;

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub notes: String,
    pub url: String,
}

#[derive(Default)]
pub(crate) struct UpdateState {
    pub checking: bool,
    pub installing: bool,
    pub latest: Option<Release>,
    pub checked: bool,
    pub error: Option<String>,
    /// The new exe is in place; restart to use it.
    pub ready: bool,
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "0.10.1" > "0.9.3".
pub fn newer(candidate: &str, than: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.trim_start_matches('v').split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    parse(candidate) > parse(than)
}

fn gh(args: &[&str]) -> Result<String> {
    let mut command = Command::new("gh");
    command.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let out = command.output().context("couldn't run gh, the GitHub CLI; is it installed and logged in?")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("gh failed").trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The newest release, or None when the repo has none yet.
pub fn latest(repo: &str) -> Result<Option<Release>> {
    match gh(&["release", "view", "--repo", repo, "--json", "tagName,name,body,url"]) {
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).context("unexpected reply from gh")?;
            let tag = v["tagName"].as_str().unwrap_or_default().to_string();
            Ok(Some(Release {
                version: tag.trim_start_matches('v').to_string(),
                tag,
                notes: v["body"].as_str().unwrap_or_default().to_string(),
                url: v["url"].as_str().unwrap_or_default().to_string(),
            }))
        }
        Err(err) if err.to_string().contains("release not found") => Ok(None),
        Err(err) => Err(err),
    }
}

/// Downloads the release's setup exe and puts it in place of the running one.
pub fn download_and_swap(repo: &str, tag: &str) -> Result<()> {
    let dir = std::env::temp_dir().join("monaged-update");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    gh(&["release", "download", tag, "--repo", repo, "--pattern", "Monaged-Setup-*.exe", "--dir", &dir.display().to_string(), "--clobber"])?;
    let file: PathBuf = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "exe"))
        .context("the release has no Monaged-Setup exe")?;
    let target = std::env::current_exe()?;
    install::replace_exe(&file, &target)
}

impl Monaged {
    fn update_repo(&self) -> String {
        self.store.setting("updates:repo").filter(|r| !r.trim().is_empty()).unwrap_or_else(|| DEFAULT_REPO.into())
    }

    /// Checks GitHub for a newer release (in the background).
    pub(crate) fn check_updates(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if self.update.checking {
            return;
        }
        self.update.checking = true;
        self.update.error = None;
        let repo = self.update_repo();
        let _ = self.store.set_setting("updates:checked", &crate::usage::now_unix().to_string());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { latest(&repo) }).await;
            view.update(cx, |this, cx| {
                this.update.checking = false;
                this.update.checked = true;
                match result {
                    Ok(release) => {
                        let fresh = release.as_ref().is_some_and(|r| newer(&r.version, current()));
                        if fresh && let Some(r) = &release {
                            this.toast_action(Tone::Info, format!("Monaged {} is available.", r.version), "Update", crate::feedback::ToastAction::Update, cx);
                        } else if !quiet {
                            this.toast(Tone::Info, format!("Monaged {} is the newest version.", current()), cx);
                        }
                        this.update.latest = release;
                    }
                    Err(err) => {
                        if !quiet {
                            this.toast_error(format!("Couldn't check for updates: {err:#}"), cx);
                        }
                        this.update.error = Some(format!("{err:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Once a day at start, when Settings → Updates allows it.
    pub(crate) fn auto_check_updates(&mut self, cx: &mut Context<Self>) {
        if self.store.setting("updates:auto").is_some_and(|v| v == "0") {
            return;
        }
        let last: i64 = self.store.setting("updates:checked").and_then(|v| v.parse().ok()).unwrap_or(0);
        if crate::usage::now_unix() - last >= CHECK_EVERY {
            self.check_updates(true, cx);
        }
    }

    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        let Some(release) = self.update.latest.clone() else { return };
        if !install::running_installed() {
            self.toast(Tone::Warn, "This is a development build. Install Monaged first (Settings → Updates), then it updates itself.", cx);
            return;
        }
        if self.agents.iter().any(|a| a.is_working()) {
            self.toast(Tone::Warn, "Agents are working; update when they're done (their turns would stop).", cx);
            return;
        }
        self.update.installing = true;
        let repo = self.update_repo();
        self.progress("update", format!("Downloading Monaged {}…", release.version), cx);
        cx.spawn(async move |view, cx| {
            let tag = release.tag.clone();
            let result = cx.background_spawn(async move { download_and_swap(&repo, &tag) }).await;
            view.update(cx, |this, cx| {
                this.update.installing = false;
                match result {
                    Ok(()) => {
                        this.update.ready = true;
                        this.settle("update", Tone::Info, format!("Monaged {} is installed. Restart to use it.", release.version), cx);
                    }
                    Err(err) => this.settle("update", Tone::Error, format!("The update failed: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn restart(&mut self, cx: &mut Context<Self>) {
        if let Ok(exe) = std::env::current_exe() {
            // The new copy waits for this one to close (one Monaged at a time).
            let _ = Command::new(exe).arg("--restarted").spawn();
        }
        cx.quit();
    }

    /// Installs this build (from a development build), so updates can follow.
    fn install_this_build(&mut self, cx: &mut Context<Self>) {
        self.progress("install", "Installing this build…", cx);
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { install::install(false) }).await;
            view.update(cx, |this, cx| match result {
                Ok(path) => this.settle("install", Tone::Info, format!("Installed to {}. Start it from the Start menu.", path.display()), cx),
                Err(err) => this.settle("install", Tone::Error, format!("Couldn't install: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_update_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.update;
        let auto = self.store.setting("updates:auto").is_none_or(|v| v != "0");
        let installed = install::running_installed();
        let latest_line = match (&state.latest, state.checked, &state.error) {
            (_, _, Some(err)) => format!("Couldn't check: {err}"),
            (Some(r), _, _) if newer(&r.version, current()) => format!("Monaged {} is available.", r.version),
            (Some(_), _, _) => "You have the newest version.".to_string(),
            (None, true, _) => "No releases yet on GitHub.".to_string(),
            (None, false, _) => "Not checked yet.".to_string(),
        };
        let available = state.latest.as_ref().is_some_and(|r| newer(&r.version, current()));
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(ui::heading(format!("Monaged {}", current())))
                    .child(ui::mono(if installed { "installed".to_string() } else { "development build".to_string() })),
            )
            .child(div().text_size(px(13.)).text_color(rgb(if state.error.is_some() { theme::red_text() } else { theme::text_2() })).child(latest_line))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(if state.checking {
                        ui::busy_button("update-check", Kind::Secondary, "Checking…").into_any_element()
                    } else {
                        ui::button("update-check", Kind::Secondary, Some(Icon::Refresh), "Check now").on_click(cx.listener(|this, _, _, cx| this.check_updates(false, cx))).into_any_element()
                    })
                    .when(available && !state.ready, |d| {
                        d.child(if state.installing {
                            ui::busy_button("update-install", Kind::Primary, "Updating…").into_any_element()
                        } else if installed {
                            ui::button("update-install", Kind::Primary, Some(Icon::ArrowDown), "Update").on_click(cx.listener(|this, _, _, cx| this.install_update(cx))).into_any_element()
                        } else {
                            ui::disabled_button("update-install", Kind::Primary, Some(Icon::ArrowDown), "Update").into_any_element()
                        })
                    })
                    .when(state.ready, |d| d.child(ui::button("update-restart", Kind::Primary, Some(Icon::Refresh), "Restart now").on_click(cx.listener(|this, _, _, cx| this.restart(cx)))))
                    .when(!installed, |d| {
                        d.child(ui::button("install-this", Kind::Ghost, None, "Install this build").on_click(cx.listener(|this, _, _, cx| this.install_this_build(cx))))
                    }),
            )
            .child(ui::switch("update-auto", "Check for updates once a day", auto).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.store.set_setting("updates:auto", if auto { "0" } else { "1" });
                cx.notify();
            })));
        if let Some(r) = state.latest.as_ref().filter(|_| available) {
            let url = r.url.clone();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .child(ui::label(format!("What's new in {}", r.version)))
                    .child(crate::review_ui::render_markdown(&r.notes.chars().take(3000).collect::<String>()))
                    .child(div().flex().child(ui::button("update-notes", Kind::Ghost, Some(Icon::External), "Open on GitHub").on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url))))),
            );
        }
        col.child(ui::meta(format!("Releases come from github.com/{} through your GitHub CLI login.", self.update_repo()))).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare() {
        assert!(super::newer("0.2.0", "0.1.9"));
        assert!(super::newer("v0.10.0", "0.9.3"));
        assert!(!super::newer("0.2.0", "0.2.0"));
        assert!(!super::newer("0.1.0", "0.2.0"));
    }
}
