//! The playground: a small project to try every part of Monaged on (Ctrl K →
//! Create the playground project, or the button on the welcome screen).
//!
//! It writes Lunchbox (a Python standard-library app with a bug, a flaky test,
//! an MCP server and project knowledge) to ~/code/monaged-playground in WSL,
//! makes `develop`, `acceptance` and a branch that conflicts with `develop`,
//! pushes them to a local bare remote, and adds the project with three
//! subprojects. PLAYGROUND.md is the tour; Monaged shows it too.
//!
//! Running it again opens the playground; if you deleted its folder, it's
//! rebuilt (the project entry stays, its old agents are archived).

use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, Context, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon};
use crate::ui::{self, Kind, StepState, StepView};
use crate::{Form, Monaged, git, theme};

pub(crate) const DIR: &str = "~/code/monaged-playground";
pub(crate) const NAME: &str = "Monaged playground";
pub(crate) const GUIDE: &str = include_str!("../playground/files/PLAYGROUND.md");

/// (name, kind, branch)
const SUBS: [(&str, &str, &str); 3] = [
    ("Order history", "feature", "feature/order-history"),
    ("Price format", "feature", "feature/price-format"),
    ("Security pass", "standing", "standing/security-pass"),
];

macro_rules! files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_bytes!(concat!("../playground/files/", $path)) as &[u8])),*]
    };
}

const FILES: &[(&str, &[u8])] = files![
    ".gitignore",
    "README.md",
    "PLAYGROUND.md",
    "CLAUDE.md",
    "monaged.json",
    "lunchbox/__init__.py",
    "lunchbox/menu.py",
    "lunchbox/orders.py",
    "lunchbox/server.py",
    "tests/__init__.py",
    "tests/test_menu.py",
    "tests/test_orders.py",
    "tests/test_kitchen.py",
    "tools/menu_mcp.py",
    "docs/architecture.md",
    "docs/screenshot.png",
    ".claude/skills/add-menu-item/SKILL.md",
    ".claude/agents/menu-reviewer.md",
    ".claude/commands/smoke.md",
];

const MENU_DEVELOP: &[u8] = include_bytes!("../playground/variants/menu_develop.py");
const TEST_DEVELOP: &[u8] = include_bytes!("../playground/variants/test_menu_develop.py");
const MENU_PRICE_FORMAT: &[u8] = include_bytes!("../playground/variants/menu_price_format.py");
const TEST_PRICE_FORMAT: &[u8] = include_bytes!("../playground/variants/test_menu_price_format.py");

/// Text files go to Linux with Linux line endings, whatever git did on Windows.
fn encoded(path: &str, bytes: &[u8]) -> String {
    if path.ends_with(".png") {
        return crate::attach::base64(bytes);
    }
    let text: Vec<u8> = bytes.iter().copied().filter(|b| *b != b'\r').collect();
    crate::attach::base64(&text)
}

fn put(path: &str, bytes: &[u8]) -> String {
    format!("put '{path}' '{}'\n", encoded(path, bytes))
}

/// The bash script that makes the repository (nothing when the folder exists).
fn script() -> String {
    // In a subshell: with `set -e`, a login shell's exit runs ~/.bash_logout,
    // whose clear_console fails without a terminal and turns success into 1.
    let mut s = String::from(
        r#"(
set -e
DIR="$HOME/code/monaged-playground"
REMOTE="$HOME/code/.remotes/monaged-playground.git"
if [ -e "$DIR" ]; then echo "MONAGED_EXISTS"; exit 0; fi
mkdir -p "$DIR" "$(dirname "$REMOTE")"
cd "$DIR"
put() { mkdir -p "$(dirname "$1")"; printf '%s' "$2" | base64 -d > "$1"; }
echo "MONAGED_STEP 0"
"#,
    );
    for (path, bytes) in FILES {
        s.push_str(&put(path, bytes));
    }
    s.push_str(
        r#"chmod +x tools/menu_mcp.py
echo "MONAGED_STEP 1"
git init -q -b main
git config user.name >/dev/null || git config user.name "Monaged playground"
git config user.email >/dev/null || git config user.email "playground@localhost"
git add .gitignore README.md lunchbox/__init__.py lunchbox/menu.py lunchbox/orders.py tests
git commit -q -m "Lunchbox: the menu, orders and tests"
git add lunchbox/server.py monaged.json docs
git commit -q -m "The web app"
git add -A
git commit -q -m "Project knowledge and the menu MCP server"
echo "MONAGED_STEP 2"
git branch develop
git branch acceptance
git checkout -q -b feature/price-format
"#,
    );
    s.push_str(&put("lunchbox/menu.py", MENU_PRICE_FORMAT));
    s.push_str(&put("tests/test_menu.py", TEST_PRICE_FORMAT));
    s.push_str("git commit -qam \"Prices in other currencies\"\ngit checkout -q develop\n");
    s.push_str(&put("lunchbox/menu.py", MENU_DEVELOP));
    s.push_str(&put("tests/test_menu.py", TEST_DEVELOP));
    s.push_str(
        r#"git commit -qam "Dutch price notation: € 7,95"
git checkout -q main
echo "MONAGED_STEP 3"
[ -d "$REMOTE" ] || git init -q --bare "$REMOTE"
git remote add origin "$REMOTE"
git push -q -f -u origin main develop acceptance feature/price-format 2>&1
echo "MONAGED_CREATED"
)
"#,
    );
    s
}

/// What making the playground shows, step by step.
const STEPS: [&str; 5] = [
    "Write Lunchbox's files",
    "Make its git history",
    "Make the branches and a merge conflict",
    "Push to a local remote",
    "Add the project and its subprojects",
];

/// Makes the repository if it isn't there, reporting each step. True when it was made now.
fn make(distro: &str, on_step: &dyn Fn(usize)) -> Result<bool> {
    let out = crate::wsl::stream(distro, &script(), |line| {
        if let Some(step) = line.strip_prefix("MONAGED_STEP ").and_then(|n| n.trim().parse().ok()) {
            on_step(step);
        }
    })?;
    if out.contains("MONAGED_EXISTS") {
        return Ok(false);
    }
    anyhow::ensure!(out.contains("MONAGED_CREATED"), "the playground script stopped early: {out}");
    Ok(true)
}

impl Monaged {
    pub(crate) fn create_playground(&mut self, cx: &mut Context<Self>) {
        if self.playground_busy {
            return;
        }
        self.playground_busy = true;
        self.playground_steps = Some(STEPS.iter().map(|l| StepView { label: (*l).into(), detail: None, state: StepState::Waiting, progress: None }).collect());
        self.form = Some(Form::Guide);
        cx.notify();
        let distro = self.config.distro.clone();
        let (tx, mut rx) = unbounded::<usize>();
        cx.spawn(async move |view, cx| {
            while let Some(step) = rx.next().await {
                view.update(cx, |this, cx| this.playground_step(step, None, cx)).ok();
            }
        })
        .detach();
        cx.spawn(async move |view, cx| {
            let d = distro.clone();
            let result = cx
                .background_spawn(async move {
                    let made = make(&d, &|step| {
                        let _ = tx.unbounded_send(step);
                    })
                    .context("making ~/code/monaged-playground")?;
                    let _ = tx.unbounded_send(4);
                    let prepared = git::prepare_project(&d, DIR)?;
                    for (_, _, branch) in SUBS {
                        git::ensure_branch(&d, &prepared.path, branch, &prepared.base_branch)?;
                    }
                    anyhow::Ok((made, prepared))
                })
                .await;
            let ok = view
                .update(cx, |this, cx| {
                    this.playground_busy = false;
                    let ok = result.is_ok();
                    match result {
                        Ok((made, prepared)) => {
                            if !made {
                                this.playground_step(4, Some("Already in ~/code/monaged-playground"), cx);
                            }
                            this.playground_step(STEPS.len(), None, cx);
                            this.finish_playground(made, prepared, &distro, cx);
                        }
                        Err(err) => {
                            let failed = this.playground_steps.as_mut().and_then(|s| {
                                let at = s.iter().position(|s| s.state == StepState::Running).or_else(|| s.iter().position(|s| s.state == StepState::Waiting))?;
                                s.get_mut(at)
                            });
                            if let Some(step) = failed {
                                step.state = StepState::Failed;
                                step.detail = Some(format!("{err:#}").into());
                            }
                            this.toast_error(format!("Couldn't make the playground: {err:#}"), cx);
                        }
                    }
                    cx.notify();
                    ok
                })
                .unwrap_or(false);
            // A moment on the finished steps, then just the tour.
            if ok {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                view.update(cx, |this, cx| {
                    this.playground_steps = None;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn finish_playground(&mut self, made: bool, prepared: git::Prepared, distro: &str, cx: &mut Context<Self>) {
        let existing = self.projects.iter().find(|p| p.path == prepared.path).map(|p| p.id);
        let project_id = match existing {
            Some(id) => id,
            None => match self.store.add_project(NAME, distro, &prepared.path, &prepared.base_branch) {
                Ok(project) => {
                    let id = project.id;
                    self.projects.push(project);
                    self.projects.sort_by_key(|p| p.name.to_lowercase());
                    id
                }
                Err(err) => {
                    self.toast_error(format!("Couldn't add the playground: {err:#}"), cx);
                    return;
                }
            },
        };
        for (name, kind, branch) in SUBS {
            if self.subprojects.iter().any(|s| s.project_id == project_id && s.name == name) {
                continue;
            }
            match self.store.add_subproject(project_id, name, kind, branch) {
                Ok(sub) => self.subprojects.push(sub),
                Err(err) => self.toast_error(format!("Couldn't add {name}: {err:#}"), cx),
            }
        }
        // A rebuilt playground: the old agents' worktrees went with the folder.
        if made && existing.is_some() {
            let subs: Vec<i64> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.id).collect();
            for agent in self.agents.iter_mut().filter(|a| subs.contains(&a.record.subproject_id) && !a.record.archived) {
                let _ = self.store.archive_agent(agent.record.id);
                agent.record.archived = true;
            }
        }
        let mut servers = crate::project_ui::load_servers(&self.store, project_id);
        if !servers.iter().any(|s| s.name == "lunchbox-menu") {
            servers.push(crate::project_ui::McpServer {
                name: "lunchbox-menu".into(),
                target: format!("python3 {}/tools/menu_mcp.py", prepared.path),
                env: String::new(),
                enabled: true,
            });
            crate::project_ui::save_servers(&self.store, project_id, &servers);
        }
        self.collapsed.remove(&project_id);
        self.select_project(project_id, cx);
        self.form = Some(Form::Guide);
        let what = match (made, existing.is_some()) {
            (true, false) => "The playground is ready in ~/code/monaged-playground. The tour is below.",
            (true, true) => "Rebuilt the playground; its old agents are archived.",
            (false, false) => "Added the playground folder that was already in WSL.",
            (false, true) => "Here's the playground and its tour.",
        };
        self.toast(Tone::Info, what, cx);
    }

    /// Marks steps before `step` done and `step` running. When the script
    /// skipped making files (the folder was there), the earlier ones say so.
    fn playground_step(&mut self, step: usize, skipped: Option<&str>, cx: &mut Context<Self>) {
        if let Some(steps) = &mut self.playground_steps {
            for (i, s) in steps.iter_mut().enumerate() {
                if i < step {
                    if let Some(why) = skipped {
                        s.state = StepState::Skipped;
                        s.detail = Some(why.to_string().into());
                    } else if s.state != StepState::Skipped {
                        s.state = StepState::Done;
                    }
                } else if i == step {
                    s.state = StepState::Running;
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_guide(&mut self, cx: &mut Context<Self>) {
        self.form = Some(Form::Guide);
        cx.notify();
    }

    pub(crate) fn render_guide(&self, cx: &mut Context<Self>) -> AnyElement {
        let project = self.projects.iter().find(|p| p.path.ends_with("/monaged-playground")).map(|p| p.id);
        div()
            .id("guide")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .px(px(32.))
            .py(px(36.))
            .child(
                div()
                    .w_full()
                    .max_w(px(820.))
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(icon(Icon::Flow).text_color(rgb(theme::text_2())))
                            .child(div().flex_1().child(ui::meta("~/code/monaged-playground/PLAYGROUND.md")))
                            .when_some(project, |d, pid| {
                                d.child(ui::button("guide-settings", Kind::Secondary, Some(Icon::Sliders), "Project settings").on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_project_settings(pid, window, cx)
                                })))
                            })
                            .when(project.is_none(), |d| {
                                d.child(ui::button("guide-create", Kind::Primary, Some(Icon::Plus), "Create the playground").on_click(cx.listener(|this, _, _, cx| this.create_playground(cx))))
                            }),
                    )
                    .children(self.playground_steps.as_ref().map(|steps| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(14.))
                            .p(px(16.))
                            .rounded(px(10.))
                            .border_1()
                            .border_color(rgb(theme::line()))
                            .bg(rgb(theme::panel()))
                            .child(div().text_size(px(13.5)).text_color(rgb(theme::text())).child("Making the playground in ~/code/monaged-playground"))
                            .child(ui::steps("playground-steps", steps))
                    }))
                    .child(crate::review_ui::render_markdown(GUIDE)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn script_has_every_file_and_no_carriage_returns() {
        let script = super::script();
        for (path, _) in super::FILES {
            assert!(script.contains(&format!("put '{path}' ")), "{path}");
        }
        assert!(!script.contains('\r'));
        // The decoded text files have Linux line endings.
        let menu = crate::media::base64_decode(&super::encoded("x.py", super::MENU_DEVELOP)).unwrap();
        assert!(!menu.contains(&b'\r'));
        // To try the script by hand: MONAGED_DUMP_PLAYGROUND=path cargo test playground
        if let Ok(path) = std::env::var("MONAGED_DUMP_PLAYGROUND") {
            std::fs::write(path, script).unwrap();
        }
    }
}
