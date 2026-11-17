//! CI pipelines in Monaged: each branch's latest run (GitLab pipeline or
//! GitHub Actions run), kept fresh, shown where you look, and acted on.
//!
//! - **Where:** a CI badge on develop, acceptance and main in the project
//!   view's branches (click it for the jobs by stage and what you can do), a CI
//!   card on a subproject's dashboard (its branch, and develop after it merged),
//!   and the Merge tab.
//! - **Fresh:** read with the branches, every 15 s while a run is going, and
//!   followed after Monaged pushes a branch (the host takes a moment to start it).
//! - **Told:** when a run you're following finishes you get a toast (with Open)
//!   and a sound; a failure also goes to the subproject's timeline and its lead.
//! - **Acted on:** open it, retry what failed, cancel it, start a new run
//!   (GitLab), or send the failed jobs' logs to the agent on the branch.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{AnyElement, AppContext as _, Context, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::ci::{self, Run, Status};
use crate::feedback::ToastAction;
use crate::icons::Icon;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

/// What Monaged knows about one branch's CI.
#[derive(Clone, Debug, Default)]
pub struct CiEntry {
    /// The latest run, when the host has one.
    pub run: Option<Run>,
    /// When it was last read.
    pub at: Option<Instant>,
    pub loading: bool,
    pub error: Option<String>,
    /// Followed until then (Monaged pushed it): its next run's end is announced.
    pub watch_until: Option<Instant>,
    /// The run whose end was announced, so it's said once.
    pub announced: Option<u64>,
}

/// How often runs that are going are read again.
const POLL: Duration = Duration::from_secs(15);
/// Branches nothing is running on are read again this often (for the project on screen).
const STALE: Duration = Duration::from_secs(300);
/// After a push: how long to follow the branch for its new run.
const WATCH: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug)]
pub enum CiAction {
    Retry,
    Cancel,
    Start,
}

fn state_of(status: Status) -> ui::State {
    match status {
        Status::Waiting | Status::Running => ui::State::Working,
        Status::Passed => ui::State::Done,
        Status::Failed => ui::State::NeedsYou,
        Status::Canceled | Status::Skipped | Status::Manual => ui::State::Idle,
    }
}

/// "12m", "3h", "2d" since an RFC 3339 time.
fn age(rfc: &str) -> String {
    let Some(at) = ci::parse_time(rfc) else { return String::new() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(at);
    let s = (now - at).max(0);
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

fn duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

/// The badge's words: "CI passed · 12m", "CI running 3/7", "CI failed · test".
pub fn badge_text(run: &Run) -> String {
    match run.status {
        Status::Running | Status::Waiting => {
            let (done, total) = run.progress();
            if total == 0 { format!("CI {}", run.status.word()) } else { format!("CI {} {done}/{total}", run.status.word()) }
        }
        Status::Failed => {
            let failed: Vec<&str> = run.failed_jobs().iter().map(|j| j.name.as_str()).collect();
            match failed.first() {
                Some(first) if failed.len() == 1 => format!("CI failed · {first}"),
                Some(first) => format!("CI failed · {first} +{}", failed.len() - 1),
                None => format!("CI failed · {}", age(&run.updated)),
            }
        }
        other => format!("CI {} · {}", other.word(), age(&run.updated)),
    }
}

/// What a failure looks like for the agent that fixes it.
pub fn failure_message(host: &str, run: &Run, logs: &[(String, String, String)]) -> String {
    let mut text = format!(
        "The CI pipeline on `{}` failed ({host}, run #{}, commit {}): {}.\n",
        run.branch,
        run.id,
        run.sha,
        run.summary()
    );
    for (name, stage, tail) in logs {
        let stage = if stage.is_empty() { String::new() } else { format!(" (stage {stage})") };
        text.push_str(&format!("\n### {name}{stage}: the end of its log\n```\n{tail}\n```\n"));
    }
    text.push_str(
        "\nFind the cause in this branch's code and fix it. Run what that job runs (see the CI config) locally to check, \
         then commit. Don't push: say in a sentence what was wrong and what you changed, and Monaged takes it from there.",
    );
    text
}

impl Monaged {
    /// The branches whose CI a project shows: its chain, and its subprojects' branches that are on origin.
    pub(crate) fn ci_branches(&self, project_id: i64) -> Vec<String> {
        let mut out = self.chain(project_id);
        let on_origin = |b: &str| self.branch_info.get(&project_id).is_some_and(|i| i.state.on_origin(b));
        for sub in self.subprojects.iter().filter(|s| s.project_id == project_id) {
            if on_origin(&sub.branch) && !out.contains(&sub.branch) {
                out.push(sub.branch.clone());
            }
        }
        out.retain(|b| on_origin(b));
        out
    }

    pub(crate) fn ci_entry(&self, project_id: i64, branch: &str) -> Option<&CiEntry> {
        self.ci_runs.get(&(project_id, branch.to_string()))
    }

    /// Reads branches' latest runs in the background.
    pub(crate) fn read_ci_runs(&mut self, project_id: i64, branches: Vec<String>, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(project_id).filter(|r| r.provider.is_some()) else { return };
        let branches: Vec<String> = branches.into_iter().filter(|b| !self.ci_entry(project_id, b).is_some_and(|e| e.loading)).collect();
        if branches.is_empty() {
            return;
        }
        for b in &branches {
            self.ci_runs.entry((project_id, b.clone())).or_default().loading = true;
        }
        cx.spawn(async move |view, cx| {
            let results = cx.background_spawn(async move { branches.into_iter().map(|b| { let run = ci::latest_run(&remote, &b); (b, run) }).collect::<Vec<_>>() }).await;
            view.update(cx, |this, cx| {
                for (branch, result) in results {
                    let key = (project_id, branch.clone());
                    let old = this.ci_runs.get(&key).and_then(|e| e.run.clone());
                    let entry = this.ci_runs.entry(key).or_default();
                    entry.loading = false;
                    entry.at = Some(Instant::now());
                    match result {
                        Ok(run) => {
                            entry.error = None;
                            entry.run = run.clone();
                            this.ci_changed(project_id, &branch, old, run, cx);
                        }
                        Err(err) => entry.error = Some(format!("{err:#}")),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// With the branches: CI that's older than a few minutes (or never read) is read.
    pub(crate) fn refresh_ci_stale(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let stale: Vec<String> = self
            .ci_branches(project_id)
            .into_iter()
            .filter(|b| self.ci_entry(project_id, b).is_none_or(|e| e.at.is_none_or(|at| at.elapsed() > STALE)))
            .collect();
        self.read_ci_runs(project_id, stale, cx);
    }

    /// Monaged pushed `branch`: follow its new run until it finishes.
    pub(crate) fn watch_ci(&mut self, project_id: i64, branch: &str, cx: &mut Context<Self>) {
        if !self.remote(project_id).is_some_and(|r| r.provider.is_some()) {
            return;
        }
        let entry = self.ci_runs.entry((project_id, branch.to_string())).or_default();
        entry.watch_until = Some(Instant::now() + WATCH);
        // The host needs a moment to start the run.
        let branch = branch.to_string();
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(6)).await;
            view.update(cx, |this, cx| this.read_ci_runs(project_id, vec![branch], cx)).ok();
        })
        .detach();
    }

    /// Starts the loop that keeps runs that are going (and the project on screen) fresh.
    pub(crate) fn start_ci_polling(&mut self, cx: &mut Context<Self>) {
        if self.ci_polling {
            return;
        }
        self.ci_polling = true;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if view.update(cx, |this, cx| this.ci_tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn ci_tick(&mut self, cx: &mut Context<Self>) {
        // The project on screen: Monaged reopens your last view without reading its
        // branches, and CI comes with them, so read them once, then keep its CI fresh.
        if let Some(project_id) = self.sel_project {
            if self.branch_info.contains_key(&project_id) {
                self.refresh_ci_stale(project_id, cx);
            } else {
                self.refresh_branches(project_id, cx);
            }
        }
        let now = Instant::now();
        let mut due: HashMap<i64, Vec<String>> = HashMap::new();
        for ((project_id, branch), e) in &self.ci_runs {
            if e.loading {
                continue;
            }
            let going = e.run.as_ref().is_some_and(|r| r.status.active());
            let watched = e.watch_until.is_some_and(|t| t > now);
            if going || watched {
                due.entry(*project_id).or_default().push(branch.clone());
            }
        }
        for (project_id, branches) in due {
            self.read_ci_runs(project_id, branches, cx);
        }
    }

    /// A run was read: when one you were following finished, say so.
    fn ci_changed(&mut self, project_id: i64, branch: &str, old: Option<Run>, new: Option<Run>, cx: &mut Context<Self>) {
        let Some(run) = new else { return };
        let key = (project_id, branch.to_string());
        let Some(entry) = self.ci_runs.get(&key) else { return };
        let followed = entry.watch_until.is_some_and(|t| t > Instant::now()) || old.as_ref().is_some_and(|o| o.id == run.id && o.status.active());
        if run.status.active() || !followed || entry.announced == Some(run.id) {
            return;
        }
        if let Some(e) = self.ci_runs.get_mut(&key) {
            e.announced = Some(run.id);
            e.watch_until = None;
        }
        let project = self.projects.iter().find(|p| p.id == project_id).map(|p| p.name.clone()).unwrap_or_default();
        let text = format!("CI on {branch} ({project}): {} (#{}, {}).", run.summary(), run.id, run.sha);
        let (tone, cue) = match run.status {
            Status::Passed => (Tone::Info, crate::sound::Cue::Success),
            Status::Failed => (Tone::Warn, crate::sound::Cue::Error),
            _ => (Tone::Info, crate::sound::Cue::Done),
        };
        self.chime(cue);
        self.toast_action(tone, text.clone(), "Open", ToastAction::OpenUrl(run.url.clone()), cx);
        // The subprojects this concerns: the branch's own, and those just merged into it.
        let subs: Vec<i64> = self
            .subprojects
            .iter()
            .filter(|s| s.project_id == project_id)
            .filter(|s| s.branch == branch || self.ships.get(&s.id).is_some_and(|ship| ship.target == branch && ship.step == crate::ship::Step::Done))
            .map(|s| s.id)
            .collect();
        for sub_id in subs {
            let kind = if run.status == Status::Failed { "failed" } else { "ci" };
            let _ = self.store.log_event(sub_id, None, kind, &text);
            let next = if run.status == Status::Failed { " The dashboard's CI card can send the failure to an agent; tell the user in a sentence." } else { "" };
            self.tell_lead(sub_id, format!("{text}{next}"));
        }
    }

    /// Retry, cancel or start a run on a branch.
    pub(crate) fn ci_action(&mut self, project_id: i64, branch: String, action: CiAction, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(project_id).filter(|r| r.provider.is_some()) else { return };
        let run = self.ci_entry(project_id, &branch).and_then(|e| e.run.clone());
        let word = match action {
            CiAction::Retry => "Retrying the failed jobs",
            CiAction::Cancel => "Canceling the run",
            CiAction::Start => "Starting a new run",
        };
        self.progress("ci", format!("{word} on {branch}…"), cx);
        cx.spawn(async move |view, cx| {
            let b = branch.clone();
            let result = cx
                .background_spawn(async move {
                    match (action, run) {
                        (CiAction::Retry, Some(run)) => ci::retry(&remote, &run),
                        (CiAction::Cancel, Some(run)) => ci::cancel(&remote, &run),
                        (CiAction::Start, _) => ci::start(&remote, &b),
                        (_, None) => anyhow::bail!("there's no run to act on"),
                    }
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.settle("ci", Tone::Info, format!("{word} on {branch}: done. Following it."), cx);
                        this.watch_ci(project_id, &branch, cx);
                    }
                    Err(err) => this.settle("ci", Tone::Error, format!("{word} on {branch} didn't work: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The agent that fixes a subproject's CI: the one on its branch, else its newest working agent.
    pub(crate) fn ci_fix_agent(&self, sub_id: i64) -> Option<i64> {
        self.feature_agent(sub_id).or_else(|| {
            self.agents
                .iter()
                .rev()
                .find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() != crate::pipeline::Role::Lead)
                .map(|a| a.record.id)
        })
    }

    /// Sends a failed run's job logs to an agent to fix.
    pub(crate) fn ci_send_failure(&mut self, project_id: i64, branch: String, agent_id: i64, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(project_id).filter(|r| r.provider.is_some()) else { return };
        let Some(run) = self.ci_entry(project_id, &branch).and_then(|e| e.run.clone()).filter(|r| r.status == Status::Failed) else { return };
        let name = self.agent_name(agent_id);
        self.progress("ci", format!("Reading the failed jobs' logs for {name}…"), cx);
        cx.spawn(async move |view, cx| {
            let (r, failed) = (remote.clone(), run.failed_jobs().into_iter().take(3).cloned().collect::<Vec<_>>());
            let provider = run.provider;
            let logs = cx
                .background_spawn(async move {
                    failed
                        .into_iter()
                        .map(|j| {
                            let tail = ci::job_log_tail(&r, provider, j.id, 60).unwrap_or_else(|e| format!("(couldn't read the log: {e:#})"));
                            (j.name, j.stage, crate::export::mask_all(&tail))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            view.update(cx, |this, cx| {
                let text = failure_message(&remote.host, &run, &logs);
                // You clicked it: it goes out like a message you typed.
                this.bypass_budget = true;
                this.send_prompt(agent_id, text);
                this.bypass_budget = false;
                this.settle("ci", Tone::Info, format!("Sent the CI failure on {branch} to {name}."), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// For the sidebar: a chain branch whose CI failed (first) or is running, and what.
    pub(crate) fn project_ci_state(&self, project_id: i64) -> Option<(ui::State, String)> {
        let runs: Vec<&Run> = self.chain(project_id).iter().filter_map(|b| self.ci_entry(project_id, b).and_then(|e| e.run.as_ref())).collect();
        if let Some(run) = runs.iter().find(|r| r.status == Status::Failed) {
            return Some((ui::State::NeedsYou, format!("CI on {} {}", run.branch, run.summary())));
        }
        runs.iter().find(|r| r.status.active()).map(|run| (ui::State::Working, format!("CI on {} is {}", run.branch, run.summary())))
    }

    /// One line per branch for the lead's `team_status`.
    pub(crate) fn ci_summary(&self, sub_id: i64) -> Option<String> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let mut branches = vec![sub.branch.clone()];
        branches.extend(self.chain(sub.project_id).into_iter().take(1));
        let lines: Vec<String> = branches
            .iter()
            .filter_map(|b| self.ci_entry(sub.project_id, b).and_then(|e| e.run.as_ref()).map(|r| format!("{b}: {} (#{}, {})", r.summary(), r.id, age(&r.updated))))
            .collect();
        (!lines.is_empty()).then(|| lines.join("; "))
    }

    // ---- Views -------------------------------------------------------------------------

    /// A branch's CI badge (click: the run's jobs); None when it has no run.
    pub(crate) fn ci_badge(&self, project_id: i64, branch: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.ci_entry(project_id, branch)?;
        let (state, text) = match &entry.run {
            Some(run) => (state_of(run.status), badge_text(run)),
            None if entry.loading => (ui::State::Idle, "CI …".to_string()),
            None if entry.error.is_some() => (ui::State::Idle, "CI unknown".to_string()),
            None => return None,
        };
        let open = self.ci_open.as_ref().is_some_and(|(p, b)| *p == project_id && b == branch);
        let b = branch.to_string();
        let id = SharedString::from(format!("ci-badge-{project_id}-{branch}"));
        Some(
            div()
                .id(id.clone())
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(5.))
                .cursor_pointer()
                .when(open, |d| d.bg(rgb(theme::selected())))
                .hover(|d| d.bg(rgb(theme::hover())))
                .child(ui::status_dot(id, state))
                .child(div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(if state == ui::State::NeedsYou { theme::red_text() } else { theme::text_2() })).child(text))
                .tooltip(crate::shell::tip_text(entry.error.clone().map(|e| format!("Couldn't read CI: {e}")).unwrap_or_else(|| "The latest CI run on this branch: click for its jobs".into())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = (project_id, b.clone());
                    this.ci_open = if this.ci_open.as_ref() == Some(&key) { None } else { Some(key) };
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// The run behind a badge: what started it, its jobs by stage, and what you can do.
    pub(crate) fn render_ci_detail(&self, project_id: i64, branch: &str, fix_agent: Option<i64>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.ci_entry(project_id, branch)?;
        let run = entry.run.as_ref()?;
        let gitlab = run.provider == crate::hosting::Provider::GitLab;
        let b = branch.to_string();
        let button = |id: &str, kind: Kind, glyph: Icon, label: &str| ui::button(SharedString::from(format!("{id}-{project_id}-{branch}")), kind, Some(glyph), label.to_string());
        let mut actions = div().flex().flex_wrap().gap(px(6.));
        let url = run.url.clone();
        actions = actions.child(button("ci-open", Kind::Secondary, Icon::External, "Open").on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url))));
        if matches!(run.status, Status::Failed | Status::Canceled) {
            let b2 = b.clone();
            actions = actions.child(button("ci-retry", Kind::Secondary, Icon::Refresh, "Retry failed").on_click(cx.listener(move |this, _, _, cx| this.ci_action(project_id, b2.clone(), CiAction::Retry, cx))));
        }
        if run.status.active() {
            let b2 = b.clone();
            actions = actions.child(button("ci-cancel", Kind::Secondary, Icon::Stop, "Cancel").on_click(cx.listener(move |this, _, _, cx| this.ci_action(project_id, b2.clone(), CiAction::Cancel, cx))));
        } else if gitlab {
            let b2 = b.clone();
            actions = actions.child(button("ci-start", Kind::Secondary, Icon::Play, "Run pipeline").on_click(cx.listener(move |this, _, _, cx| this.ci_action(project_id, b2.clone(), CiAction::Start, cx))));
        }
        if run.status == Status::Failed
            && let Some(agent) = fix_agent
        {
            let (b2, name) = (b.clone(), self.agent_name(agent));
            actions = actions.child(
                button("ci-send", Kind::Primary, Icon::Send, &format!("Send to {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.ci_send_failure(project_id, b2.clone(), agent, cx))),
            );
        }
        let b2 = b.clone();
        actions = actions.child(
            ui::icon_button(SharedString::from(format!("ci-refresh-{project_id}-{branch}")), Icon::Refresh)
                .tooltip(crate::shell::tip("Read it again"))
                .on_click(cx.listener(move |this, _, _, cx| this.read_ci_runs(project_id, vec![b2.clone()], cx))),
        );
        let what = if gitlab { "pipeline" } else { "run" };
        let header = format!("CI {what} #{} on {branch}", run.id);
        let meta = format!("{} · {}{} · {} ago", run.sha, run.source, if entry.loading { " · reading…" } else { "" }, age(&run.created));
        let stages = run.stages().into_iter().map(|(stage, jobs)| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .when(!stage.is_empty(), |d| d.child(ui::mono(stage.clone())))
                .children(jobs.into_iter().map(|job| {
                    let url = job.url.clone();
                    div()
                        .id(SharedString::from(format!("ci-job-{}", job.id)))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(4.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(theme::hover())))
                        .child(ui::status_dot(SharedString::from(format!("ci-job-dot-{}", job.id)), state_of(job.status)))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.5)).child(job.name.clone()))
                        .child(ui::mono(match (job.status, job.duration) {
                            (Status::Failed, _) if job.allow_failure => "failed (allowed)".to_string(),
                            (s, Some(d)) if !s.active() => format!("{} · {}", s.word(), duration(d)),
                            (s, _) => s.word().to_string(),
                        }))
                        .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url)))
                }))
        });
        Some(
            ui::card()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::status_dot(SharedString::from(format!("ci-run-{project_id}-{branch}")), state_of(run.status)))
                        .child(ui::label(header))
                        .child(div().flex_1())
                        .child(ui::mono(run.summary())),
                )
                .child(ui::mono(meta))
                .when_some(entry.error.clone(), |d, e| d.child(div().text_size(px(12.)).text_color(rgb(theme::red_text())).child(format!("Couldn't read it again: {e}"))))
                .children(stages)
                .child(actions)
                .into_any_element(),
        )
    }

    /// A subproject's CI on its dashboard: its own branch, and develop once it merged there.
    pub(crate) fn render_sub_ci_card(&self, sub_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let project_id = sub.project_id;
        let mut branches = Vec::new();
        if self.ci_entry(project_id, &sub.branch).is_some_and(|e| e.run.is_some()) {
            branches.push(sub.branch.clone());
        }
        let target = self.merge_target(sub_id)?;
        let merged_here = self.ships.get(&sub_id).is_some_and(|s| s.step == crate::ship::Step::Done)
            || self.merges.get(&sub_id).is_some_and(|m| m.merged.as_ref().is_some_and(|(t, _)| *t == target));
        if merged_here && self.ci_entry(project_id, &target).is_some_and(|e| e.run.is_some()) {
            branches.push(target);
        }
        if branches.is_empty() {
            return None;
        }
        let fix = self.ci_fix_agent(sub_id);
        let open = self.ci_open.clone().filter(|(p, b)| *p == project_id && branches.contains(b));
        Some(
            ui::card()
                .child(div().flex().items_center().gap(px(8.)).child(crate::icons::icon(Icon::Shield).size(px(14.))).child(ui::label("CI")))
                .children(branches.iter().map(|b| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().w(px(150.)).truncate().font_family(theme::MONO).text_size(px(12.)).child(b.clone()))
                        .children(self.ci_badge(project_id, b, cx))
                }))
                .children(open.and_then(|(p, b)| self.render_ci_detail(p, &b, fix, cx)))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::Job;

    fn run(status: Status, jobs: Vec<(&str, Status)>) -> Run {
        Run {
            id: 9,
            provider: crate::hosting::Provider::GitLab,
            status,
            branch: "develop".into(),
            sha: "53f0ccfb".into(),
            url: "u".into(),
            source: "push".into(),
            created: "2026-11-17T20:00:00Z".into(),
            updated: "2026-11-17T20:05:00Z".into(),
            jobs: jobs
                .into_iter()
                .enumerate()
                .map(|(i, (n, s))| Job { id: i as u64, name: n.into(), stage: "test".into(), status: s, duration: None, url: String::new(), allow_failure: false })
                .collect(),
        }
    }

    #[test]
    fn badges_say_what_matters_first() {
        assert_eq!(badge_text(&run(Status::Running, vec![("lint", Status::Passed), ("test", Status::Running), ("build", Status::Waiting)])), "CI running 1/3");
        assert_eq!(badge_text(&run(Status::Failed, vec![("test", Status::Failed), ("build", Status::Failed)])), "CI failed · test +1");
        assert!(badge_text(&run(Status::Passed, vec![])).starts_with("CI passed · "));
    }

    #[test]
    fn a_failure_reaches_the_agent_with_the_logs() {
        let r = run(Status::Failed, vec![("test", Status::Failed)]);
        let text = failure_message("gitlab.com", &r, &[("test".into(), "test".into(), "FAIL a.test.ts\nexit code 1".into())]);
        assert!(text.starts_with("The CI pipeline on `develop` failed (gitlab.com, run #9, commit 53f0ccfb): failed in test."));
        assert!(text.contains("### test (stage test): the end of its log\n```\nFAIL a.test.ts\nexit code 1\n```"));
        assert!(text.contains("Don't push"));
    }
}
