//! Updates from Monaged's GitHub releases: check (by hand, or once a day),
//! then "Update and restart" downloads the new setup exe, checks it against
//! the release's SHA256SUMS.txt, and swaps it in. Public releases come straight
//! from GitHub's API; while the repo is private, through the GitHub CLI's
//! login. Only the installed copy updates itself; a development build points
//! you to installing.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui::{AnyElement, Context, div, prelude::*, px, rgb};
use serde_json::Value;
use sha2::{Digest, Sha256};

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
    /// The setup exe's download link (public releases).
    pub asset: Option<String>,
    /// SHA256SUMS.txt's download link (public releases).
    pub sums: Option<String>,
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

/// "0.10.1" > "0.9.3"; a release beats its own pre-releases ("0.3.0" > "0.3.0-beta.1").
pub fn newer(candidate: &str, than: &str) -> bool {
    let parse = |v: &str| -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((core, _)) => (core, true),
            None => (v, false),
        };
        (core.split('.').take(3).map(|p| p.parse().unwrap_or(0)).collect(), !pre)
    };
    parse(candidate) > parse(than)
}

fn http(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(timeout))).build().into()
}

fn user_agent() -> String {
    format!("Monaged/{} (+https://github.com/{DEFAULT_REPO})", current())
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

/// A release as GitHub's API describes it.
fn release_from(v: &Value) -> Option<Release> {
    let tag = v["tag_name"].as_str().or_else(|| v["tagName"].as_str())?.to_string();
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let link = |want: &dyn Fn(&str) -> bool| {
        assets.iter().find(|a| a["name"].as_str().is_some_and(want)).and_then(|a| a["browser_download_url"].as_str().or_else(|| a["url"].as_str())).map(str::to_owned)
    };
    Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        notes: v["body"].as_str().unwrap_or_default().to_string(),
        url: v["html_url"].as_str().or_else(|| v["url"].as_str()).unwrap_or_default().to_string(),
        asset: link(&|n: &str| n.starts_with("Monaged-Setup-") && n.ends_with(".exe")),
        sums: link(&|n: &str| n == "SHA256SUMS.txt"),
        tag,
    })
}

/// Through GitHub's public API: Ok(None) when there's no release to see (none
/// yet, or the repo is private).
fn latest_public(repo: &str) -> Result<Option<Release>> {
    let mut response = http(30)
        .get(&format!("https://api.github.com/repos/{repo}/releases/latest"))
        .header("User-Agent", &user_agent())
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("couldn't reach GitHub")?;
    match response.status().as_u16() {
        200 => {
            let v: Value = response.body_mut().read_json().context("unexpected reply from GitHub")?;
            Ok(release_from(&v))
        }
        404 => Ok(None),
        403 | 429 => bail!("GitHub's rate limit was reached; try again in an hour"),
        code => bail!("GitHub answered {code}"),
    }
}

/// Through the GitHub CLI (private repos).
fn latest_gh(repo: &str) -> Result<Option<Release>> {
    match gh(&["release", "view", "--repo", repo, "--json", "tagName,body,url"]) {
        Ok(out) => {
            let v: Value = serde_json::from_str(&out).context("unexpected reply from gh")?;
            Ok(release_from(&v))
        }
        Err(err) if err.to_string().contains("release not found") => Ok(None),
        Err(err) => Err(err),
    }
}

/// The newest release, or None when the repo has none yet.
pub fn latest(repo: &str) -> Result<Option<Release>> {
    let public = match latest_public(repo) {
        Ok(Some(release)) => return Ok(Some(release)),
        other => other,
    };
    match (latest_gh(repo), public) {
        (Ok(found), _) => Ok(found),
        (Err(err), Ok(_)) if err.to_string().contains("couldn't run gh") => {
            // GitHub shows nothing publicly and gh isn't there: private, or no release yet.
            bail!("no public release found at github.com/{repo}. If the repo is private, install the GitHub CLI and run gh auth login")
        }
        (Err(err), Ok(_)) => Err(err),
        (Err(_), Err(err)) => Err(err),
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The hash SHA256SUMS.txt lists for `name`.
fn listed_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.trim().to_lowercase())
    })
}

fn download(url: &str, to: &Path) -> Result<()> {
    let mut response = http(900).get(url).header("User-Agent", &user_agent()).header("Accept", "application/octet-stream").call().context("couldn't reach GitHub")?;
    if response.status().as_u16() != 200 {
        bail!("GitHub answered {} for {url}", response.status().as_u16());
    }
    let mut file = std::fs::File::create(to).with_context(|| format!("couldn't write {}", to.display()))?;
    std::io::copy(&mut response.body_mut().with_config().limit(1 << 30).reader(), &mut file).context("the download broke off")?;
    Ok(())
}

/// Downloads the release's setup exe, checks it, and puts it in place of the running one.
pub fn download_and_swap(repo: &str, release: &Release) -> Result<()> {
    let dir = std::env::temp_dir().join("monaged-update");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    match &release.asset {
        Some(url) => {
            let name = url.rsplit('/').next().unwrap_or("Monaged-Setup.exe");
            download(url, &dir.join(name))?;
            if let Some(sums) = &release.sums {
                download(sums, &dir.join("SHA256SUMS.txt"))?;
            }
        }
        None => {
            let dest = dir.display().to_string();
            gh(&["release", "download", &release.tag, "--repo", repo, "--pattern", "Monaged-Setup-*.exe", "--dir", &dest, "--clobber"])?;
            let _ = gh(&["release", "download", &release.tag, "--repo", repo, "--pattern", "SHA256SUMS.txt", "--dir", &dest, "--clobber"]);
        }
    }
    let file: PathBuf = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "exe"))
        .context("the release has no Monaged-Setup exe")?;
    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    // A Windows program starts with "MZ"; anything else is an error page.
    let mut head = [0u8; 2];
    std::fs::File::open(&file)?.read_exact(&mut head).context("the download is empty")?;
    if &head != b"MZ" {
        bail!("the download isn't a Windows program");
    }
    if let Ok(sums) = std::fs::read_to_string(dir.join("SHA256SUMS.txt")) {
        let expected = listed_hash(&sums, &name).with_context(|| format!("SHA256SUMS.txt doesn't list {name}"))?;
        let actual = sha256_file(&file)?;
        if actual != expected {
            bail!("the download doesn't match its checksum (expected {expected}, got {actual}); nothing was changed");
        }
    }
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

    /// After an update: one note saying what's new (not on a fresh install).
    pub(crate) fn note_new_version(&mut self, cx: &mut Context<Self>) {
        let seen = self.store.setting("app:version");
        if seen.as_deref() == Some(current()) {
            return;
        }
        let _ = self.store.set_setting("app:version", current());
        if seen.is_some() || !self.projects.is_empty() {
            let url = format!("https://github.com/{}/releases/tag/v{}", self.update_repo(), current());
            self.toast_action(Tone::Info, format!("Monaged is updated to {}.", current()), "What's new", crate::feedback::ToastAction::OpenUrl(url), cx);
        }
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
            let wanted = release.clone();
            let result = cx.background_spawn(async move { download_and_swap(&repo, &wanted) }).await;
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
        col.child(ui::meta(format!(
            "Releases come from github.com/{}: straight from GitHub when the repo is public, otherwise through your GitHub CLI login. Each download is checked against the release's SHA256SUMS.txt.",
            self.update_repo()
        )))
        .into_any_element()
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
        assert!(super::newer("0.3.0", "0.3.0-beta.1"));
        assert!(!super::newer("0.3.0-beta.2", "0.3.0"));
        assert!(super::newer("0.3.1-beta.1", "0.3.0"));
    }

    #[test]
    fn reads_a_release_and_its_files() {
        let v = serde_json::json!({
            "tag_name": "v0.3.0",
            "body": "notes",
            "html_url": "https://github.com/o/r/releases/tag/v0.3.0",
            "assets": [
                { "name": "monaged-0.3.0-windows-x64.zip", "browser_download_url": "https://x/zip" },
                { "name": "Monaged-Setup-0.3.0.exe", "browser_download_url": "https://x/exe" },
                { "name": "SHA256SUMS.txt", "browser_download_url": "https://x/sums" }
            ]
        });
        let r = super::release_from(&v).unwrap();
        assert_eq!(r.version, "0.3.0");
        assert_eq!(r.asset.as_deref(), Some("https://x/exe"));
        assert_eq!(r.sums.as_deref(), Some("https://x/sums"));
    }

    #[test]
    fn finds_a_files_checksum() {
        let sums = "abc123  Monaged-Setup-0.3.0.exe\r\nDEF456  monaged-0.3.0-windows-x64.zip\n";
        assert_eq!(super::listed_hash(sums, "Monaged-Setup-0.3.0.exe").as_deref(), Some("abc123"));
        assert_eq!(super::listed_hash(sums, "monaged-0.3.0-windows-x64.zip").as_deref(), Some("def456"));
        assert_eq!(super::listed_hash(sums, "other.exe"), None);
    }
}
