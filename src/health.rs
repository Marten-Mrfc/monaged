//! The health check (Ctrl K → Health check): everything Monaged depends on,
//! checked in one go, each with what's wrong and the button that fixes it.

use gpui::{AnyElement, ClipboardItem, Context, FontWeight, IntoElement, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon_sized};
use crate::ui::{self, Kind};
use crate::{Conn, Form, LOGIN_COMMAND, Monaged, theme};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Check {
    Ok,
    Warn,
    Fail,
    Running,
}

#[derive(Clone)]
pub(crate) enum HealthFix {
    Reconnect,
    CopyLogin,
    CopyIdentity,
    GitAccounts,
    Settings,
}

#[derive(Clone)]
pub(crate) struct HealthItem {
    pub title: &'static str,
    pub state: Check,
    pub detail: String,
    pub fix: Option<(&'static str, HealthFix)>,
}

/// What the WSL side reports, one `key: value` per line.
fn parse_report(text: &str) -> std::collections::HashMap<String, String> {
    text.lines().filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect()
}

impl Monaged {
    pub(crate) fn open_health(&mut self, cx: &mut Context<Self>) {
        self.form = Some(Form::Health);
        self.run_health(cx);
        cx.notify();
    }

    /// Checks that need WSL run in the background; the rest are known already.
    pub(crate) fn run_health(&mut self, cx: &mut Context<Self>) {
        self.health_wsl = None;
        let distro = self.config.distro.clone();
        let agent_cmd = self.config.agent_cmd.clone();
        let worktree_roots: Vec<String> = self
            .projects
            .iter()
            .map(|p| {
                let repo = p.path.trim_end_matches('/');
                let (parent, name) = repo.rsplit_once('/').unwrap_or(("", repo));
                format!("{parent}/.monaged/{name}")
            })
            .collect();
        let started = std::time::Instant::now();
        cx.spawn(async move |view, cx| {
            let script = format!(
                r#"echo "os: $(. /etc/os-release 2>/dev/null; echo $PRETTY_NAME)"
a={agent}; a="${{a/#\~/$HOME}}"
if [ -x "$a" ]; then echo "adapter: $(readlink -f "$a" | sed 's#.*/claude-agent-acp/\([^/]*\)/.*#\1#')"; else echo "adapter: missing"; fi
echo "gitname: $(git config --global user.name)"
echo "gitemail: $(git config --global user.email)"
echo "gcm: $(git config --global --get-all credential.helper | tail -n 1)"
echo "monaged: $(git config --global --get-regexp '^credential\..*\.helper$' 2>/dev/null | grep -c -- '--git-credential')"
echo "disk: $(df -h --output=avail,pcent "$HOME" | tail -1)"
total=0
for d in {roots}; do d="${{d/#\~/$HOME}}"; [ -d "$d" ] && total=$((total + $(du -sk "$d" 2>/dev/null | cut -f1))); done
echo "worktrees: $total"
echo "nproc: $(nproc)"
echo "mem: $(free -m | awk '/Mem:/ {{print $2" "$7}}')"
"#,
                agent = crate::wsl::quote(&agent_cmd),
                roots = worktree_roots.iter().map(|r| crate::wsl::quote(r)).collect::<Vec<_>>().join(" "),
            );
            let report = cx.background_spawn(async move { crate::wsl::run(&distro, &script) }).await;
            let windows = cx
                .background_spawn(async move {
                    let probe = |cmd: &str, args: &[&str]| {
                        let mut command = std::process::Command::new(cmd);
                        command.args(args);
                        #[cfg(windows)]
                        {
                            use std::os::windows::process::CommandExt;
                            command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
                        }
                        command.output().map(|o| o.status.success()).unwrap_or(false)
                    };
                    (probe("zed", &["--version"]), probe("gh", &["auth", "status"]), probe("glab", &["auth", "status"]))
                })
                .await;
            view.update(cx, |this, cx| {
                this.health_wsl = Some((report.map(|t| parse_report(&t)).map_err(|e| format!("{e:#}")), windows, started.elapsed()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn health_items(&self) -> Vec<HealthItem> {
        let mut items = Vec::new();
        let running = |title: &'static str| HealthItem { title, state: Check::Running, detail: "Checking…".into(), fix: None };
        // The connection, from what Monaged already knows.
        items.push(match &self.conn {
            Conn::Ready if self.logged_in == Some(false) => HealthItem {
                title: "Claude login",
                state: Check::Fail,
                detail: "Claude isn't logged in inside WSL.".into(),
                fix: Some(("Copy the login command", HealthFix::CopyLogin)),
            },
            Conn::Ready => HealthItem { title: "Claude connection", state: Check::Ok, detail: "Connected; sessions open on demand.".into(), fix: None },
            Conn::Failed(reason) => HealthItem { title: "Claude connection", state: Check::Fail, detail: reason.clone(), fix: Some(("Reconnect", HealthFix::Reconnect)) },
            _ => running("Claude connection"),
        });
        items.push(HealthItem {
            title: "Coordination tools",
            state: if self.bridge.is_some() { Check::Ok } else { Check::Warn },
            detail: if self.bridge.is_some() {
                "Agents can post to the board, ask each other and claim files.".into()
            } else {
                "The local bridge didn't start, so agents run without the board and asks.".into()
            },
            fix: None,
        });
        items.push(HealthItem {
            title: "Haiku summaries and reviews",
            state: if self.api_key.is_some() || self.cli_fallback { Check::Ok } else { Check::Warn },
            detail: match (&self.api_key, self.cli_fallback) {
                (Some(_), _) => "Using your API key.".into(),
                (None, true) => "No API key: they run on your Max plan through the Claude CLI.".into(),
                (None, false) => "Off: no API key and the Max plan fallback is off.".into(),
            },
            fix: (self.api_key.is_none() && !self.cli_fallback).then_some(("Open Settings", HealthFix::Settings)),
        });
        let Some((report, (zed, gh, glab), took)) = &self.health_wsl else {
            for title in ["WSL", "Agent adapter", "Git identity", "Git logins", "Disk", "Zed", "GitHub and GitLab"] {
                items.push(running(title));
            }
            return items;
        };
        match report {
            Err(err) => items.push(HealthItem {
                title: "WSL",
                state: Check::Fail,
                detail: format!("Can't run commands in the {} distro: {err}", self.config.distro),
                fix: None,
            }),
            Ok(r) => {
                let get = |k: &str| r.get(k).cloned().unwrap_or_default();
                let mem: Vec<String> = get("mem").split_whitespace().map(str::to_owned).collect();
                items.push(HealthItem {
                    title: "WSL",
                    state: Check::Ok,
                    detail: format!(
                        "{} · {} cores · {} MB memory, {} MB free · answered in {} ms",
                        get("os"),
                        get("nproc"),
                        mem.first().cloned().unwrap_or_default(),
                        mem.get(1).cloned().unwrap_or_default(),
                        took.as_millis()
                    ),
                    fix: None,
                });
                let adapter = get("adapter");
                items.push(HealthItem {
                    title: "Agent adapter",
                    state: if adapter == "missing" { Check::Fail } else { Check::Ok },
                    detail: if adapter == "missing" { format!("Not found at {}", self.config.agent_cmd) } else { format!("claude-agent-acp {adapter}") },
                    fix: None,
                });
                let (name, email) = (get("gitname"), get("gitemail"));
                items.push(if name.is_empty() || email.is_empty() {
                    HealthItem {
                        title: "Git identity",
                        state: Check::Fail,
                        detail: "Git in WSL has no name or email, so builders can't commit.".into(),
                        fix: Some(("Copy it from Windows", HealthFix::CopyIdentity)),
                    }
                } else {
                    HealthItem { title: "Git identity", state: Check::Ok, detail: "Set (name and email).".into(), fix: None }
                });
                let gcm = get("gcm").contains("git-credential-manager");
                let monaged = get("monaged").parse::<usize>().unwrap_or(0);
                let accounts = self.git_auth.accounts.len();
                let ok = monaged > 0 || gcm;
                let expiring: Vec<String> = self.git_auth.accounts.iter().filter(|a| a.days_left().is_some_and(|d| d < 14)).map(|a| a.key()).collect();
                items.push(HealthItem {
                    title: "Git logins",
                    state: if ok && expiring.is_empty() { Check::Ok } else { Check::Warn },
                    detail: if !expiring.is_empty() {
                        format!("These tokens have expired or expire within two weeks: {}. Sign in again in Settings → Git accounts.", expiring.join(", "))
                    } else { match (monaged > 0, gcm) {
                        (true, true) => format!("{accounts} Monaged account(s) sign git in to {monaged} host(s); Git Credential Manager covers the rest."),
                        (true, false) => format!("{accounts} Monaged account(s) sign git in to {monaged} host(s)."),
                        (false, true) => "Git in WSL uses Windows' Git Credential Manager for push and clone.".into(),
                        (false, false) => "Git in WSL can't sign in anywhere, so pushes and private clones fail.".into(),
                    } },
                    fix: (!ok || !expiring.is_empty()).then_some(("Sign in", HealthFix::GitAccounts)),
                });
                let disk = get("disk");
                let pct: u32 = disk.split_whitespace().nth(1).and_then(|p| p.trim_end_matches('%').parse().ok()).unwrap_or(0);
                let worktrees_kb = get("worktrees").parse::<u64>().unwrap_or(0);
                let worktrees = match worktrees_kb {
                    kb if kb < 1024 => format!("{kb} KB"),
                    kb if kb < 1024 * 1024 => format!("{} MB", kb / 1024),
                    kb => format!("{:.1} GB", kb as f64 / 1024. / 1024.),
                };
                items.push(HealthItem {
                    title: "Disk",
                    state: if pct >= 95 { Check::Fail } else if pct >= 85 { Check::Warn } else { Check::Ok },
                    detail: format!(
                        "{} free in WSL ({}% used) · agent worktrees take {}",
                        disk.split_whitespace().next().unwrap_or("?"),
                        pct,
                        worktrees
                    ),
                    fix: None,
                });
            }
        }
        items.push(HealthItem {
            title: "Zed",
            state: if *zed { Check::Ok } else { Check::Warn },
            detail: if *zed { "The zed command works, so Open in Zed does too.".into() } else { "The zed command isn't on PATH; Open in Zed won't work.".into() },
            fix: None,
        });
        let api_accounts = self.git_auth.accounts.iter().filter(|a| a.provider.is_some()).count();
        items.push(HealthItem {
            title: "GitHub and GitLab",
            state: if *gh || *glab || api_accounts > 0 { Check::Ok } else { Check::Warn },
            detail: format!(
                "{}GitHub CLI {} · GitLab CLI {}",
                if api_accounts > 0 { format!("{api_accounts} account(s) for pull and merge requests · ") } else { String::new() },
                if *gh { "logged in" } else { "not logged in" },
                if *glab { "logged in" } else { "not logged in" }
            ),
            fix: (!*gh && !*glab && api_accounts == 0).then_some(("Sign in", HealthFix::GitAccounts)),
        });
        items
    }

    fn run_health_fix(&mut self, fix: HealthFix, cx: &mut Context<Self>) {
        match fix {
            HealthFix::Reconnect => self.reconnect_now(cx),
            HealthFix::CopyLogin => {
                cx.write_to_clipboard(ClipboardItem::new_string(LOGIN_COMMAND.to_string()));
                self.toast(Tone::Info, "Copied. Paste it in a terminal, then type /login.", cx);
            }
            HealthFix::CopyIdentity => self.copy_git_identity(cx),
            HealthFix::GitAccounts => self.open_settings(cx),
            HealthFix::Settings => self.open_settings(cx),
        }
    }

    pub(crate) fn render_health(&self, cx: &mut Context<Self>) -> AnyElement {
        let items = self.health_items();
        let problems = items.iter().filter(|i| matches!(i.state, Check::Fail | Check::Warn)).count();
        let checking = items.iter().any(|i| i.state == Check::Running);
        let rows = items.into_iter().enumerate().map(|(n, item)| {
            let glyph: AnyElement = match item.state {
                Check::Running => ui::spinner(("health-spin", n), 15., theme::text_3()),
                Check::Ok => icon_sized(Icon::Check, 15.).text_color(rgb(theme::text_2())).into_any_element(),
                Check::Warn => icon_sized(Icon::Alert, 15.).text_color(rgb(theme::text_2())).into_any_element(),
                Check::Fail => icon_sized(Icon::Alert, 15.).text_color(rgb(theme::red_text())).into_any_element(),
            };
            let fix = item.fix.clone();
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .px(px(16.))
                .py(px(13.))
                .when(n > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                .child(div().w(px(18.)).flex_none().child(glyph))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(item.title))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(rgb(if item.state == Check::Fail { theme::red_text() } else { theme::text_3() }))
                                .child(item.detail),
                        ),
                )
                .children(fix.map(|(label, fix)| {
                    ui::button(("health-fix", n), Kind::Secondary, None, label).on_click(cx.listener(move |this, _, _, cx| {
                        this.run_health_fix(fix.clone(), cx);
                        this.run_health(cx);
                    }))
                }))
        });
        div()
            .id("health")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div().flex().justify_center().px(px(32.)).py(px(36.)).child(
                    div()
                        .w(px(820.))
                        .flex()
                        .flex_col()
                        .gap(px(20.))
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(16.))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(ui::title("Health check"))
                                        .child(ui::meta(match (checking, problems) {
                                            (true, _) => "Checking WSL, the agent, git, logins and disk…".to_string(),
                                            (false, 0) => "Everything Monaged needs is in order.".to_string(),
                                            (false, n) => format!("{n} thing{} to look at.", if n == 1 { "" } else { "s" }),
                                        })),
                                )
                                .child(if checking {
                                    ui::busy_button("health-rerun", Kind::Secondary, "Checking…")
                                } else {
                                    ui::button("health-rerun", Kind::Secondary, Some(Icon::Refresh), "Check again").on_click(cx.listener(|this, _, _, cx| this.run_health(cx)))
                                }),
                        )
                        .child(div().rounded(px(8.)).border_1().border_color(rgb(theme::line())).bg(rgb(theme::panel())).children(rows)),
                ),
            )
            .into_any_element()
    }
}
