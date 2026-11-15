//! The inspector's Git tab for a subproject:
//! - pick a worktree (an agent's, or the project's own checkout);
//! - its uncommitted changes: discard a file, stash, and commit with a
//!   message (Haiku can suggest one);
//! - sync: fetch, and each branch's distance from origin, with pull (fast-
//!   forward only) and push;
//! - conflicts from a merge: keep one side per file and merge, or hand them
//!   to an agent;
//! - history: the graph of the subproject's branches; a commit opens its diff;
//! - committing only the files you tick, amending, and taking back the last
//!   commit while it isn't pushed;
//! - pulling a branch that moved on both sides (a merge, never a rebase);
//! - how origin signs in: the account (see `accounts_ui`), a connection test,
//!   and switching between HTTPS and SSH.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git_ops::{self, CommitInfo, GraphLine, Side, Status, StatusFile};
use crate::icons::{Icon, icon, icon_sized};
use crate::shell::tip;
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GitTarget {
    Agent(i64),
    Project,
}

pub(crate) struct GitPanel {
    pub target: GitTarget,
    pub status: Option<Result<Status, String>>,
    pub stashes: usize,
    pub message: Entity<TextInput>,
    pub graph: Option<Vec<GraphLine>>,
    /// (branch, ahead/behind origin)
    pub remote: Vec<(String, Option<(usize, usize)>)>,
    pub busy: Option<String>,
    /// Conflict choices: path -> side.
    pub choices: HashMap<String, Side>,
    /// Files ticked for the next commit (none ticked: everything).
    pub selected: HashSet<String>,
    /// Add to the last commit instead of making a new one.
    pub amend: bool,
    /// The newest commit: (sha, subject, pushed).
    pub last: Option<(String, String, bool)>,
    /// The account origin signs in as: `Some(None)` is the host's default.
    pub login: Option<Option<String>>,
}

/// A commit opened in the main area.
pub(crate) struct CommitView {
    pub sha: String,
    pub head: Option<String>,
    pub files: Vec<crate::diff::FileDiff>,
    pub error: Option<String>,
}

impl Monaged {
    /// Where git commands for this panel run.
    fn git_dir(&self, sub_id: i64, target: GitTarget) -> Option<String> {
        match target {
            GitTarget::Agent(id) => self.agent(id).filter(|a| !a.record.archived).map(|a| a.record.worktree.clone()),
            GitTarget::Project => {
                let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
                self.projects.iter().find(|p| p.id == sub.project_id).map(|p| p.path.clone())
            }
        }
    }

    fn project_path_of(&self, sub_id: i64) -> Option<(i64, String)> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        self.projects.iter().find(|p| p.id == sub.project_id).map(|p| (p.id, p.path.clone()))
    }

    /// Creates the panel the first time the tab shows, and loads it.
    pub(crate) fn prepare_git_panel(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if self.git_panels.contains_key(&sub_id) {
            return;
        }
        let target = match self.sel_agent.and_then(|id| self.agent(id)) {
            Some(a) if a.record.subproject_id == sub_id && !a.record.archived => GitTarget::Agent(a.record.id),
            _ => GitTarget::Project,
        };
        let message = cx.new(|cx| TextInput::new("Commit message", cx));
        self.git_panels.insert(
            sub_id,
            GitPanel {
                target,
                status: None,
                stashes: 0,
                message,
                graph: None,
                remote: Vec::new(),
                busy: None,
                choices: HashMap::new(),
                selected: HashSet::new(),
                amend: false,
                last: None,
                login: None,
            },
        );
        self.refresh_git(sub_id, cx);
    }

    /// Reads status, stashes, the remote distances and the graph.
    pub(crate) fn refresh_git(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panels.get(&sub_id) else { return };
        let Some(dir) = self.git_dir(sub_id, panel.target) else { return };
        let Some((_, repo)) = self.project_path_of(sub_id) else { return };
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let mut branches: Vec<String> = vec![sub.branch.clone()];
        let project_id = sub.project_id;
        branches.extend(self.chain(project_id));
        let agent_branches: Vec<String> =
            self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.record.branch != sub.branch).map(|a| a.record.branch.clone()).collect();
        let mut graph_refs = vec![sub.branch.clone()];
        graph_refs.extend(agent_branches);
        let https_host = self.origin_https_host(project_id);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let status = git_ops::status(&distro, &dir).map_err(|e| format!("{e:#}"));
                    let stashes = git_ops::stash_count(&distro, &dir).unwrap_or(0);
                    let remote = git_ops::remote_counts(&distro, &repo, &branches).unwrap_or_default();
                    let graph = git_ops::graph(&distro, &repo, &graph_refs, 60).ok();
                    let last = git_ops::last_commit(&distro, &dir).ok().flatten();
                    let login = https_host.map(|host| crate::git_auth::project_login(&distro, &repo, &host));
                    (status, stashes, branches.into_iter().zip(remote).collect::<Vec<_>>(), graph, last, login)
                })
                .await;
            view.update(cx, |this, cx| {
                if let Some(panel) = this.git_panels.get_mut(&sub_id) {
                    let (status, stashes, remote, graph, last, login) = result;
                    // Ticks for files that are no longer changed go away.
                    if let Ok(status) = &status {
                        panel.selected.retain(|path| status.files.iter().any(|f| &f.path == path));
                    }
                    panel.status = Some(status);
                    panel.stashes = stashes;
                    panel.remote = remote;
                    panel.graph = graph;
                    // Amending only makes sense while the last commit isn't pushed.
                    if !matches!(last, Some((_, _, false))) {
                        panel.amend = false;
                    }
                    panel.last = last;
                    panel.login = login;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_git_target(&mut self, sub_id: i64, target: GitTarget, cx: &mut Context<Self>) {
        if let Some(panel) = self.git_panels.get_mut(&sub_id) {
            panel.target = target;
            panel.status = None;
        }
        self.refresh_git(sub_id, cx);
        cx.notify();
    }

    /// Runs a git job in the background, shows it as busy, then refreshes.
    pub(crate) fn git_job(&mut self, sub_id: i64, label: &str, job: impl FnOnce(String) -> anyhow::Result<String> + Send + 'static, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panels.get_mut(&sub_id) else { return };
        if panel.busy.is_some() {
            return;
        }
        panel.busy = Some(label.to_string());
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { job(distro) }).await;
            view.update(cx, |this, cx| {
                if let Some(panel) = this.git_panels.get_mut(&sub_id) {
                    panel.busy = None;
                }
                match result {
                    Ok(done) if !done.is_empty() => this.toast(Tone::Info, done, cx),
                    Ok(_) => {}
                    Err(err) => this.toast_error(format!("{err:#}"), cx),
                }
                this.refresh_git(sub_id, cx);
                if let Some(project_id) = this.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id) {
                    this.refresh_branches(project_id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn git_commit(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panels.get(&sub_id) else { return };
        let message = panel.message.read(cx).text().trim().to_string();
        let paths: Vec<String> = panel.selected.iter().cloned().collect();
        if panel.amend {
            let Some(dir) = self.git_dir(sub_id, panel.target) else { return };
            let input = panel.message.clone();
            self.git_job(sub_id, "Amending…", move |distro| git_ops::amend(&distro, &dir, &message, &paths).map(|sha| format!("Amended the last commit ({sha})")), cx);
            input.update(cx, |input, cx| input.clear(cx));
            if let Some(panel) = self.git_panels.get_mut(&sub_id) {
                panel.amend = false;
                panel.selected.clear();
            }
            return;
        }
        if message.is_empty() {
            self.toast(Tone::Warn, "Write a commit message first (or let Haiku suggest one).", cx);
            return;
        }
        let Some(dir) = self.git_dir(sub_id, panel.target) else { return };
        let input = panel.message.clone();
        let agent = match panel.target {
            GitTarget::Agent(id) => Some(id),
            GitTarget::Project => None,
        };
        let summary = message.lines().next().unwrap_or_default().to_string();
        let _ = self.store.log_event(sub_id, agent, "commit", &format!("Committed: {summary}"));
        self.git_job(sub_id, "Committing…", move |distro| git_ops::commit(&distro, &dir, &message, &paths).map(|sha| format!("Committed {sha}")), cx);
        input.update(cx, |input, cx| input.clear(cx));
        if let Some(panel) = self.git_panels.get_mut(&sub_id) {
            panel.selected.clear();
        }
    }

    /// Takes back the last commit (not pushed yet); its changes stay.
    fn git_undo_commit(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panels.get(&sub_id) else { return };
        let Some(dir) = self.git_dir(sub_id, panel.target) else { return };
        let input = panel.message.clone();
        let subject = panel.last.as_ref().map(|(_, s, _)| s.clone()).unwrap_or_default();
        // The message comes back so you can commit again with it (unless you're typing one).
        if input.read(cx).text().trim().is_empty() {
            input.update(cx, |input, cx| input.set_text(subject, cx));
        }
        self.git_job(sub_id, "Undoing the commit…", move |distro| git_ops::undo_commit(&distro, &dir).map(|s| format!("Took back \"{s}\"; its changes are uncommitted again.")), cx);
    }

    fn toggle_git_file(&mut self, sub_id: i64, path: String, cx: &mut Context<Self>) {
        if let Some(panel) = self.git_panels.get_mut(&sub_id)
            && !panel.selected.remove(&path)
        {
            panel.selected.insert(path);
        }
        cx.notify();
    }

    /// Haiku reads the uncommitted diff and writes a commit message.
    fn suggest_commit_message(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(backend) = self.backend() else {
            self.toast(Tone::Warn, "Suggestions need an API key or the Max plan fallback (Settings → API key).", cx);
            return;
        };
        let Some(target) = self.git_panels.get(&sub_id).map(|p| p.target) else { return };
        let Some(dir) = self.git_dir(sub_id, target) else { return };
        let Some(panel) = self.git_panels.get_mut(&sub_id) else { return };
        let input = panel.message.clone();
        panel.busy = Some("Haiku is writing a message…".into());
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let diff = crate::wsl::run(&distro, &format!("cd {} && git add -A -N . && git diff --stat && git diff | head -c 60000", crate::wsl::shell_path(&dir)))?;
                    let schema = serde_json::json!({
                        "type": "object",
                        "properties": { "message": { "type": "string" } },
                        "required": ["message"],
                        "additionalProperties": false
                    });
                    let system = "You write git commit messages: an imperative subject line under 60 characters, then (only if useful) a blank line and a short body saying why. No prefixes like feat: unless the diff's project uses them.";
                    let (value, _) = crate::llm::complete_json(&backend, system, &format!("Write the commit message for this change:\n\n{diff}"), &schema)?;
                    anyhow::Ok(value["message"].as_str().unwrap_or_default().to_string())
                })
                .await;
            view.update(cx, |this, cx| {
                if let Some(panel) = this.git_panels.get_mut(&sub_id) {
                    panel.busy = None;
                }
                match result {
                    Ok(message) => input.update(cx, |input, cx| input.set_text(message.lines().next().unwrap_or_default().to_string(), cx)),
                    Err(err) => this.toast_error(format!("Couldn't suggest a message: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn git_discard(&mut self, sub_id: i64, file: StatusFile, cx: &mut Context<Self>) {
        let Some(panel) = self.git_panels.get(&sub_id) else { return };
        let Some(dir) = self.git_dir(sub_id, panel.target) else { return };
        let path = file.path.clone();
        self.git_job(sub_id, "Discarding…", move |distro| git_ops::discard(&distro, &dir, &file).map(|_| format!("Discarded the changes to {path}")), cx);
    }

    /// Finishes a conflicted merge with the chosen side per file.
    fn merge_with_choices(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(target) = self.merge_target(sub_id) else { return };
        let Some((_, repo)) = self.project_path_of(sub_id) else { return };
        let conflict = self.merges.get(&sub_id).map(|m| m.conflict.clone()).unwrap_or_default();
        let choices: Vec<(String, Side)> = self.git_panels.get(&sub_id).map(|p| conflict.iter().filter_map(|f| p.choices.get(f).map(|s| (f.clone(), *s))).collect()).unwrap_or_default();
        if choices.len() < conflict.len() {
            self.toast(Tone::Warn, "Pick a side for every conflicted file first.", cx);
            return;
        }
        let message = format!("Merge {} into {target} (conflicts resolved in Monaged)", sub.branch);
        let (source, t) = (sub.branch.clone(), target.clone());
        let _ = self.store.log_event(sub_id, None, "merge", &format!("Merged {} into {target}, resolving {} conflict(s) by hand", sub.branch, choices.len()));
        self.git_job(
            sub_id,
            "Merging with your choices…",
            move |distro| git_ops::merge_with_choices(&distro, &repo, &source, &t, &message, &choices).map(|sha| format!("Merged into {t} ({sha})")),
            cx,
        );
        if let Some(state) = self.merges.get_mut(&sub_id) {
            state.conflict.clear();
        }
    }

    // ---- The commit page ---------------------------------------------------------------------

    pub(crate) fn open_commit(&mut self, project_id: i64, sha: String, cx: &mut Context<Self>) {
        let Some(repo) = self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone()) else { return };
        self.commit_view = Some(CommitView { sha: sha.clone(), head: None, files: Vec::new(), error: None });
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let s = sha.clone();
            let result = cx.background_spawn(async move { git_ops::show(&distro, &repo, &s) }).await;
            view.update(cx, |this, cx| {
                if let Some(v) = this.commit_view.as_mut().filter(|v| v.sha == sha) {
                    match result {
                        Ok((head, files)) => {
                            v.head = Some(head);
                            v.files = files;
                        }
                        Err(err) => v.error = Some(format!("{err:#}")),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_commit_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let v = self.commit_view.as_ref()?;
        let header = div()
            .flex_none()
            .h(px(54.))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .child(icon(Icon::Commit).text_color(rgb(theme::text_2())))
            .child(div().flex_1().flex().flex_col().gap(px(1.)).child(ui::heading(format!("Commit {}", v.sha))).child(ui::meta(format!("{} file{}", v.files.len(), if v.files.len() == 1 { "" } else { "s" }))))
            .child(ui::button("commit-close", Kind::Secondary, None, "Close").on_click(cx.listener(|this, _, _, cx| {
                this.commit_view = None;
                cx.notify();
            })));
        let body: AnyElement = match (&v.head, &v.error) {
            (_, Some(err)) => ui::callout(Tone::Error, err.clone()).into_any_element(),
            (None, None) => ui::skeleton_text("commit-sk", 6).into_any_element(),
            (Some(head), None) => div()
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(div().font_family(theme::MONO).text_size(px(12.5)).line_height(px(19.)).text_color(rgb(theme::text_2())).children(head.lines().map(|l| div().child(if l.is_empty() { " ".to_string() } else { l.to_string() }))))
                .children(v.files.iter().map(|f| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().flex().gap(px(8.)).child(div().flex_1().font_family(theme::MONO).text_size(px(12.5)).child(f.path.clone())).child(ui::mono(format!("+{} −{}", f.added, f.removed))))
                        .child(self.render_diff_body(None, f, cx))
                }))
                .into_any_element(),
        };
        Some(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(header)
                .child(div().id("commit-view").flex_1().min_h(px(0.)).overflow_y_scroll().child(div().max_w(px(1200.)).p(px(20.)).child(body)))
                .into_any_element(),
        )
    }

    // ---- Settings → Git ----------------------------------------------------------------------

    pub(crate) fn load_git_identity(&mut self, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new("Your name", cx));
        let email = cx.new(|cx| TextInput::new("you@example.com", cx));
        self.git_identity = Some((name.clone(), email.clone()));
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let found = cx.background_spawn(async move { git_ops::identity(&distro) }).await;
            view.update(cx, |_, cx| {
                if let Ok((n, e)) = found {
                    name.update(cx, |i, cx| i.set_text(n, cx));
                    email.update(cx, |i, cx| i.set_text(e, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn save_git_identity(&mut self, cx: &mut Context<Self>) {
        let Some((name, email)) = &self.git_identity else { return };
        let (n, e) = (name.read(cx).text().to_string(), email.read(cx).text().to_string());
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { git_ops::set_identity(&distro, &n, &e) }).await;
            view.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.identity_missing = false;
                    this.toast(Tone::Info, "Saved: git in WSL commits with this name and email.", cx);
                }
                Err(err) => this.toast_error(format!("{err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_git_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let autofetch: u64 = self.store.setting("git:autofetch").and_then(|v| v.parse().ok()).unwrap_or(0);
        let suggest = self.store.setting("git:suggest").is_none_or(|v| v == "1");
        let mut col = div().flex().flex_col().gap(px(14.));
        if let Some((name, email)) = &self.git_identity {
            col = col.child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(10.))
                    .child(div().flex_1().child(ui::field("Name", None, name.clone())))
                    .child(div().flex_1().child(ui::field("Email", None, email.clone())))
                    .child(ui::button("git-identity-save", Kind::Secondary, None, "Save").on_click(cx.listener(|this, _, _, cx| this.save_git_identity(cx)))),
            );
        }
        col = col
            .child(div().text_size(px(12.5)).text_color(rgb(theme::text_2())).child("Fetch from origin automatically"))
            .child(div().flex().gap(px(6.)).children([(0u64, "Off"), (5, "Every 5 min"), (15, "Every 15 min"), (30, "Every 30 min")].into_iter().map(|(minutes, label)| {
                ui::choice(SharedString::from(format!("autofetch-{minutes}")), label, autofetch == minutes).on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.store.set_setting("git:autofetch", &minutes.to_string());
                    this.fetched.clear();
                    cx.notify();
                }))
            })))
            .child(ui::switch("git-suggest", "Offer Haiku-written commit messages in the Git tab", suggest).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.store.set_setting("git:suggest", if suggest { "0" } else { "1" });
                cx.notify();
            })));
        col.into_any_element()
    }

    // ---- Auto-fetch --------------------------------------------------------------------------

    /// Fetches each project every few minutes when Settings → Git says so.
    pub(crate) fn maybe_autofetch(&mut self, cx: &mut Context<Self>) {
        let minutes: u64 = self.store.setting("git:autofetch").and_then(|v| v.parse().ok()).unwrap_or(0);
        if minutes == 0 {
            return;
        }
        let projects: Vec<(i64, String)> = self.projects.iter().map(|p| (p.id, p.path.clone())).collect();
        for (id, path) in projects {
            if self.fetched.get(&id).is_some_and(|at| at.elapsed() < Duration::from_secs(minutes * 60)) {
                continue;
            }
            self.fetched.insert(id, Instant::now());
            let distro = self.config.distro.clone();
            cx.spawn(async move |view, cx| {
                let _ = cx.background_spawn(async move { git_ops::fetch(&distro, &path) }).await;
                view.update(cx, |this, cx| {
                    let subs: Vec<i64> = this.git_panels.keys().copied().collect();
                    for sub in subs {
                        this.refresh_git(sub, cx);
                    }
                    this.refresh_branches(id, cx);
                })
                .ok();
            })
            .detach();
        }
    }

    // ---- The tab -----------------------------------------------------------------------------

    pub(crate) fn render_git_inspector(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.git_panels.get(&sub_id) else { return ui::panel_section("Git").child(ui::skeleton_text("git-sk", 3)).into_any_element() };
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return div().into_any_element() };
        let project_id = sub.project_id;
        let busy = panel.busy.clone();

        // Which worktree.
        let mut targets: Vec<(GitTarget, String)> =
            self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| (GitTarget::Agent(a.record.id), a.record.name.clone())).collect();
        targets.push((GitTarget::Project, "project checkout".into()));
        let picker = div().flex().flex_wrap().gap(px(4.)).children(targets.into_iter().enumerate().map(|(i, (target, name))| {
            let on = panel.target == target;
            div()
                .id(("git-target", i))
                .px(px(7.))
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(rgb(if on { theme::text_3() } else { theme::line() }))
                .when(on, |d| d.bg(rgb(theme::selected())))
                .font_family(theme::MONO)
                .text_size(px(11.))
                .text_color(rgb(if on { theme::text() } else { theme::text_3() }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme::hover())))
                .child(name)
                .on_click(cx.listener(move |this, _, _, cx| this.set_git_target(sub_id, target, cx)))
        }));

        // Status and committing.
        let status_section = match &panel.status {
            None => ui::panel_section("Worktree").child(picker).child(ui::skeleton_text("git-status-sk", 3)),
            Some(Err(err)) => ui::panel_section("Worktree").child(picker).child(ui::callout(Tone::Error, err.clone())),
            Some(Ok(status)) => {
                let files = status.files.clone();
                let mut section = ui::panel_section("Worktree")
                    .child(picker)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon_sized(Icon::Branch, 13.).text_color(rgb(theme::text_3())))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(12.)).child(status.branch.clone()))
                            .child(ui::mono(match (&status.upstream, status.ahead, status.behind) {
                                (None, _, _) => "no upstream".to_string(),
                                (Some(_), 0, 0) => "up to date".to_string(),
                                (Some(_), a, b) => format!("↑{a} ↓{b}"),
                            })),
                    );
                if files.is_empty() {
                    section = section.child(ui::meta("No uncommitted changes."));
                } else {
                    section = section
                        .child(ui::meta(format!("{} uncommitted file{}", files.len(), if files.len() == 1 { "" } else { "s" })))
                        .child(div().flex().flex_col().gap(px(1.)).children(files.iter().take(40).enumerate().map(|(i, f)| {
                            let file = f.clone();
                            let ticked = panel.selected.contains(&f.path);
                            let path = f.path.clone();
                            div()
                                .id(("git-file", i))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .h(px(24.))
                                .group("git-file")
                                .cursor_pointer()
                                .tooltip(tip("Tick files to commit only those"))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_git_file(sub_id, path.clone(), cx)))
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(13.))
                                        .rounded(px(3.))
                                        .border_1()
                                        .border_color(rgb(if ticked { theme::text_2() } else { theme::line_strong() }))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .children(ticked.then(|| icon_sized(Icon::Check, 10.).text_color(rgb(theme::text())))),
                                )
                                .child(div().w(px(52.)).flex_none().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(if f.label() == "conflict" { theme::red_text() } else { theme::text_3() })).child(f.label()))
                                .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.5)).child(f.path.clone()))
                                .child(
                                    div()
                                        .id(("git-discard", i))
                                        .invisible()
                                        .group_hover("git-file", |s| s.visible())
                                        .cursor_pointer()
                                        .tooltip(tip("Discard this file's changes"))
                                        .child(icon_sized(Icon::Trash, 13.).text_color(rgb(theme::text_3())))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.ask_confirm(crate::feedback::ConfirmAction::Discard(sub_id, file.clone()), window, cx)
                                        })),
                                )
                        })))
                        .child(panel.message.clone())
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .child(match &busy {
                                    Some(label) => ui::busy_button("git-commit", Kind::Primary, label.clone()).into_any_element(),
                                    None => ui::button("git-commit", Kind::Primary, Some(Icon::Commit), match (panel.amend, panel.selected.len()) {
                                        (true, 0) => "Amend last commit".to_string(),
                                        (true, n) => format!("Amend with {n} file{}", if n == 1 { "" } else { "s" }),
                                        (false, 0) => "Commit all".to_string(),
                                        (false, n) => format!("Commit {n} file{}", if n == 1 { "" } else { "s" }),
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| this.git_commit(sub_id, cx)))
                                    .into_any_element(),
                                })
                                .when(self.store.setting("git:suggest").is_none_or(|v| v == "1"), |d| {
                                    d.child(ui::button("git-suggest", Kind::Secondary, None, "Suggest message").on_click(cx.listener(move |this, _, _, cx| this.suggest_commit_message(sub_id, cx))))
                                })
                                .child(ui::button("git-stash", Kind::Ghost, None, "Stash").on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(dir) = this.git_panels.get(&sub_id).and_then(|p| this.git_dir(sub_id, p.target)) else { return };
                                    this.git_job(sub_id, "Stashing…", move |distro| git_ops::stash(&distro, &dir).map(|_| "Stashed your changes".to_string()), cx);
                                }))),
                        )
                        .when(matches!(&panel.last, Some((_, _, false))), |d| {
                            let amend = panel.amend;
                            d.child(ui::switch("git-amend", "Add to the last commit instead (it isn't pushed yet)", amend).on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = this.git_panels.get_mut(&sub_id) {
                                    p.amend = !amend;
                                }
                                cx.notify();
                            })))
                        });
                }
                if let Some((sha, subject, pushed)) = &panel.last {
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .min_h(px(24.))
                            .child(icon_sized(Icon::Commit, 12.).text_color(rgb(theme::text_4())))
                            .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(sha.clone()))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_2())).child(subject.clone()))
                            .when(!*pushed, |d| {
                                d.child(
                                    ui::button("git-undo-commit", Kind::Ghost, None, "Undo")
                                        .tooltip(tip("Take back this commit; its changes become uncommitted again"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.git_undo_commit(sub_id, cx))),
                                )
                            }),
                    );
                }
                if panel.stashes > 0 {
                    section = section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().child(ui::meta(format!("{} stash{}", panel.stashes, if panel.stashes == 1 { "" } else { "es" }))))
                            .child(ui::button("git-pop", Kind::Secondary, None, "Restore latest").on_click(cx.listener(move |this, _, _, cx| {
                                let Some(dir) = this.git_panels.get(&sub_id).and_then(|p| this.git_dir(sub_id, p.target)) else { return };
                                this.git_job(sub_id, "Restoring…", move |distro| git_ops::stash_pop(&distro, &dir).map(|_| "Restored the stash".to_string()), cx);
                            }))),
                    );
                }
                section
            }
        };

        // Sync with origin.
        let repo = self.project_path_of(sub_id).map(|(_, p)| p).unwrap_or_default();
        let has_origin = self.branch_info.get(&project_id).is_some_and(|i| i.state.remote_url.is_some());
        let mut sync = ui::panel_section("Origin");
        if !has_origin {
            sync = sync.child(ui::meta("This project has no origin remote."));
        } else {
            let r = repo.clone();
            sync = sync
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().child(ui::meta(match self.fetched.get(&project_id) {
                    Some(at) => format!("Fetched {} ago", humanize(at.elapsed())),
                    None => "Not fetched in this session".to_string(),
                })))
                .child(ui::button("git-fetch", Kind::Secondary, Some(Icon::Refresh), "Fetch").on_click(cx.listener(move |this, _, _, cx| {
                    this.fetched.insert(project_id, Instant::now());
                    let r = r.clone();
                    this.git_job(sub_id, "Fetching…", move |distro| git_ops::fetch(&distro, &r).map(|_| String::new()), cx);
                }))))
                .child(self.render_origin_signin(sub_id, project_id, &repo, panel, cx))
                .children(panel.remote.iter().enumerate().map(|(i, (branch, counts))| {
                    let (b, r) = (branch.clone(), repo.clone());
                    let is_sub = *branch == sub.branch;
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .h(px(26.))
                        .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.5)).child(branch.clone()))
                        .child(ui::mono(match counts {
                            None => "not on origin".to_string(),
                            Some((0, 0)) => "in sync".to_string(),
                            Some((a, b)) => format!("↑{a} ↓{b}"),
                        }))
                        .when(matches!(counts, Some((0, behind)) if *behind > 0), |d| {
                            d.child(ui::button(("git-pull", i), Kind::Secondary, None, "Pull").on_click(cx.listener(move |this, _, _, cx| {
                                let (b, r) = (b.clone(), r.clone());
                                this.git_job(sub_id, "Pulling…", move |distro| git_ops::pull_ff(&distro, &r, &b).map(|sha| format!("{b} is now at {sha}")), cx);
                            })))
                        })
                        .when(matches!(counts, Some((ahead, behind)) if *ahead > 0 && *behind > 0), |d| {
                            let (b, r) = (branch.clone(), repo.clone());
                            d.child(
                                ui::button(("git-pull-merge", i), Kind::Secondary, None, "Pull (merge)")
                                    .tooltip(tip("Both sides moved: merge origin's commits into this branch. Nothing gets rewritten; a conflict changes nothing."))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let (b, r) = (b.clone(), r.clone());
                                        this.git_job(sub_id, "Merging origin…", move |distro| git_ops::pull_merge(&distro, &r, &b).map(|sha| format!("Merged origin/{b} into {b} ({sha})")), cx);
                                    })),
                            )
                        })
                        .when(is_sub && (counts.is_none() || matches!(counts, Some((ahead, _)) if *ahead > 0)), |d| {
                            d.child(ui::button(("git-push", i), Kind::Secondary, Some(Icon::Push), "Push").on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, false, cx))))
                        })
                }));
        }

        // Conflicts from the last merge.
        let conflict = self.merges.get(&sub_id).map(|m| m.conflict.clone()).unwrap_or_default();
        let target = self.merge_target(sub_id).unwrap_or_default();
        let conflicts = (!conflict.is_empty()).then(|| {
            let all_chosen = conflict.iter().all(|f| panel.choices.contains_key(f));
            ui::panel_section("Conflicts")
                .child(ui::meta(format!("Merging {} into {target} conflicts in these files. Keep one side per file, or hand them to an agent.", sub.branch)))
                .children(conflict.iter().enumerate().map(|(i, path)| {
                    let chosen = panel.choices.get(path).copied();
                    let side_chip = |label: String, side: Side, path: String, cx: &mut Context<Self>| {
                        let on = chosen == Some(side);
                        div()
                            .id(SharedString::from(format!("conflict-{i}-{label}")))
                            .px(px(7.))
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(rgb(if on { theme::text_3() } else { theme::line() }))
                            .when(on, |d| d.bg(rgb(theme::selected())))
                            .text_size(px(11.5))
                            .cursor_pointer()
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(p) = this.git_panels.get_mut(&sub_id) {
                                    p.choices.insert(path.clone(), side);
                                }
                                cx.notify();
                            }))
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::red_text())).child(path.clone()))
                        .child(div().flex().gap(px(4.)).child(side_chip(format!("keep {target}"), Side::Ours, path.clone(), cx)).child(side_chip(format!("take {}", sub.branch), Side::Theirs, path.clone(), cx)))
                }))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(if all_chosen {
                            ui::button("conflict-merge", Kind::Primary, Some(Icon::Merge), "Merge with these choices").on_click(cx.listener(move |this, _, _, cx| this.merge_with_choices(sub_id, cx))).into_any_element()
                        } else {
                            ui::disabled_button("conflict-merge", Kind::Primary, Some(Icon::Merge), "Merge with these choices").into_any_element()
                        })
                        .child(ui::button("conflict-agent", Kind::Secondary, Some(Icon::Agent), "Hand to an agent").on_click(cx.listener(move |this, _, _, cx| this.show_inspector(sub_id, crate::shell::InspectorTab::Merge, cx)))),
                )
        });

        // History.
        let history = match &panel.graph {
            None => ui::panel_section("History").child(ui::skeleton_text("git-graph-sk", 4)),
            Some(lines) if lines.is_empty() => ui::panel_section("History").child(ui::meta("No commits on these branches yet.")),
            Some(lines) => ui::panel_section("History").child(div().flex().flex_col().children(lines.iter().enumerate().map(|(i, line)| render_graph_line(i, line, project_id, cx)))),
        };

        div().flex().flex_col().child(status_section).children(conflicts).child(sync).child(history).into_any_element()
    }
}

fn render_graph_line(i: usize, line: &GraphLine, project_id: i64, cx: &mut Context<Monaged>) -> AnyElement {
    let graph = div().flex_none().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_4())).child(if line.graph.is_empty() { " ".to_string() } else { line.graph.clone() });
    let Some(CommitInfo { sha, subject, author, when, refs }) = line.commit.clone() else {
        return div().flex().h(px(16.)).child(graph).into_any_element();
    };
    div()
        .id(("graph-line", i))
        .flex()
        .items_center()
        .gap(px(6.))
        .min_h(px(22.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme::hover())))
        .child(graph)
        .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(sha.clone()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .items_center()
                .gap(px(4.))
                .children(refs.iter().take(2).map(|r| {
                    div().flex_none().max_w(px(110.)).truncate().px(px(4.)).rounded(px(3.)).border_1().border_color(rgb(theme::line_strong())).font_family(theme::MONO).text_size(px(10.)).text_color(rgb(theme::text_2())).child(r.clone())
                }))
                .child(div().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_2())).child(subject)),
        )
        .child(div().flex_none().text_size(px(10.5)).text_color(rgb(theme::text_4())).child(when.replace(" ago", "")))
        .tooltip(crate::session_ui::tip_static(format!("{author} · {when} · click for the diff")))
        .on_click(cx.listener(move |this, _, _, cx| this.open_commit(project_id, sha.clone(), cx)))
        .into_any_element()
}

fn humanize(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}
