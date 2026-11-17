//! "Merge into develop" in one go. One button on the dashboard (or the lead,
//! when you ask it) runs every step itself and says where it is in plain words,
//! on the card, in the timeline and to the lead:
//!
//! 1. Check: the score is at the bar and nobody is working on the feature branch.
//! 2. Bring develop in (after it caught up with origin), so the merge itself
//!    can't conflict. Conflicts go to the agent on the feature branch (the
//!    integrator), and what it did is reviewed again before anything merges.
//! 3. Merge into develop the project's way: here, here then push, or a request.
//!
//! Before this, a merge that hit conflicts stopped with "nothing was changed",
//! the next steps were buttons in the Merge tab, the lead said it couldn't
//! merge, and an automatic merge never tried again.

use gpui::{AnyElement, Context, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git::{self, MergeOutcome, MergeReport};
use crate::icons::{Icon, icon};
use crate::merge_ui::Method;
use crate::pipeline::Stage;
use crate::ui::{self, Kind};
use crate::{AgentSpec, Monaged, pipeline::Role, theme};

/// Where a merge into develop is.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Bringing the target's newer commits into the feature branch.
    Updating,
    /// The agent on the feature branch resolves the conflicts that brought up.
    Resolving,
    /// The feature branch changed: its review has to reach the bar again.
    Reviewing(i64),
    /// Merging into the target, the project's way.
    Merging,
    Done,
    Stuck,
}

#[derive(Clone, Debug)]
pub struct Ship {
    pub step: Step,
    pub target: String,
    /// Where it stands, in a sentence (the card and the lead get this).
    pub status: String,
    /// Times the target was brought in: it can move while we work.
    pub rounds: u8,
}

impl Ship {
    pub fn running(&self) -> bool {
        !matches!(self.step, Step::Done | Step::Stuck)
    }

    /// 1-3 for the card's steps.
    pub fn number(&self) -> usize {
        match self.step {
            Step::Updating | Step::Resolving | Step::Reviewing(_) => 2,
            Step::Merging | Step::Done => 3,
            Step::Stuck => 0,
        }
    }
}

/// How often develop may be brought in before giving up (it kept moving).
const MAX_ROUNDS: u8 = 3;

/// The message the agent on the feature branch gets for conflicts.
pub fn resolve_brief(target: &str, branch: &str, files: &[String]) -> String {
    format!(
        "Monaged is merging this branch into `{target}`, and `{target}` has newer work that conflicts with it. Merge `{target}` \
         into this branch (`{branch}`) and resolve the conflicts in: {}.\n\n\
         - Keep the intent of both sides. Where both sides raised the same counter (a schema or migration version, a \
         numbered list), keep theirs and take the next free number for ours.\n\
         - Install the dependencies if needed, run the project's typecheck and tests, and commit the merge. Don't push.\n\
         - End with one or two sentences on what you resolved and how you checked it. Monaged reviews it, then merges.",
        files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")
    )
}

/// "4 files: a, b, c and d".
fn files_list(files: &[String]) -> String {
    let names: Vec<&str> = files.iter().map(|f| f.rsplit('/').next().unwrap_or(f)).collect();
    let list = match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        n if n <= 5 => format!("{} and {}", names[..n - 1].join(", "), names[n - 1]),
        n => format!("{} and {} more", names[..4].join(", "), n - 4),
    };
    format!("{} file{} ({list})", files.len(), if files.len() == 1 { "" } else { "s" })
}

impl Monaged {
    pub(crate) fn ship_running(&self, sub_id: i64) -> bool {
        self.ships.get(&sub_id).is_some_and(Ship::running)
    }

    /// The agent that works on the feature branch itself (the integrator, or a resolver).
    fn feature_agent(&self, sub_id: i64) -> Option<i64> {
        let branch = &self.subprojects.iter().find(|s| s.id == sub_id)?.branch;
        self.agents.iter().rev().find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.record.branch == *branch).map(|a| a.record.id)
    }

    /// Starts merging the subproject into develop (the first branch of its
    /// chain). `by` is who asked, for the timeline. Returns where it stands.
    pub(crate) fn ship(&mut self, sub_id: i64, by: &str, cx: &mut Context<Self>) -> String {
        if let Some(s) = self.ships.get(&sub_id).filter(|s| s.running()) {
            return format!("Already merging: {}", s.status);
        }
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return "This subproject is gone.".into() };
        let Some(target) = self.merge_target(sub_id) else { return "This project has no branch to merge into.".into() };
        self.ships.insert(sub_id, Ship { step: Step::Updating, target: target.clone(), status: String::new(), rounds: 0 });
        let _ = self.store.log_event(sub_id, None, "merge", &format!("Merge into {target} started by {by}"));
        // Nobody may be changing the branch while it merges.
        if let Some(name) = self.feature_agent(sub_id).and_then(|id| self.agent(id)).filter(|a| a.is_working()).map(|a| a.record.name.clone()) {
            return self.ship_stuck(sub_id, format!("{name} is still working on {}. Merge again when its turn is done.", sub.branch), cx);
        }
        let min = self.min_score();
        if let Some((score, from)) = self.sub_confidence(sub_id)
            && score < min
        {
            return self.ship_stuck(
                sub_id,
                format!("The confidence is {score}/5 ({from}), below the {min}/5 a merge into {target} needs. Fix the findings or review again first."),
                cx,
            );
        }
        // A score from before the agent's last turn doesn't cover what's there now.
        if let Some(agent) = self.feature_agent(sub_id).filter(|&id| self.agent(id).is_some_and(|a| a.review_stale || a.reviewing))
            && self.backend().is_some()
        {
            let name = self.agent_name(agent);
            self.set_ship_step(sub_id, Step::Reviewing(agent));
            self.ship_say(sub_id, format!("{name} changed the branch since its last review. Reviewing it before merging into {target}."), cx);
            self.review_fresh(agent, cx);
            return self.ships[&sub_id].status.clone();
        }
        self.ship_update(sub_id, cx);
        self.ships.get(&sub_id).map(|s| s.status.clone()).unwrap_or_default()
    }

    fn set_ship_step(&mut self, sub_id: i64, step: Step) {
        if let Some(s) = self.ships.get_mut(&sub_id) {
            s.step = step;
        }
    }

    /// Step 2: bring the target in, so the merge itself can't conflict.
    fn ship_update(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let Some(ship) = self.ships.get_mut(&sub_id) else { return };
        ship.step = Step::Updating;
        ship.rounds += 1;
        let (target, rounds) = (ship.target.clone(), ship.rounds);
        if rounds > MAX_ROUNDS {
            self.ship_stuck(sub_id, format!("{target} kept changing while merging ({MAX_ROUNDS} rounds). Try again in a moment."), cx);
            return;
        }
        self.ship_say(sub_id, format!("Bringing the latest {target} into {} first, so the merge can't conflict.", sub.branch), cx);
        let state = self.merge_state(sub_id);
        state.busy = true;
        state.conflict.clear();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let (repo, feature, t) = (project.path.clone(), sub.branch.clone(), target.clone());
            let result = cx
                .background_spawn(async move {
                    let _ = git::catch_up(&distro, &repo, &t);
                    git::merge_branch(&distro, &repo, &t, &feature, &format!("Merge {t} into {feature}"), false)
                })
                .await;
            view.update(cx, |this, cx| {
                this.merge_state(sub_id).busy = false;
                if !this.ship_running(sub_id) {
                    return;
                }
                match result {
                    Ok(MergeReport { outcome: MergeOutcome::AlreadyMerged, .. }) => this.ship_merge(sub_id, cx),
                    Ok(MergeReport { outcome: MergeOutcome::Merged(sha), .. }) => {
                        // New code on the branch: it needs a score again before it merges.
                        match this.feature_agent(sub_id).filter(|_| this.backend().is_some()) {
                            Some(agent) => {
                                let name = this.agent_name(agent);
                                this.set_ship_step(sub_id, Step::Reviewing(agent));
                                this.ship_say(sub_id, format!("{target} had newer work; it's in {} now (at {sha}). Reviewing {name}'s branch again before merging.", sub.branch), cx);
                                this.review_fresh(agent, cx);
                            }
                            None => this.ship_merge(sub_id, cx),
                        }
                    }
                    Ok(MergeReport { outcome: MergeOutcome::Conflict(files), .. }) => this.ship_resolve(sub_id, files, cx),
                    Ok(MergeReport { outcome: MergeOutcome::Blocked(why), .. }) => {
                        this.ship_stuck(sub_id, format!("Couldn't bring {target} in: {why}"), cx);
                    }
                    Err(err) => {
                        this.ship_stuck(sub_id, format!("Couldn't bring {target} in: {err:#}"), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Conflicts with the target go to the agent on the feature branch.
    fn ship_resolve(&mut self, sub_id: i64, files: Vec<String>, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let target = self.ships.get(&sub_id).map(|s| s.target.clone()).unwrap_or_default();
        self.merge_state(sub_id).conflict = files.clone();
        self.set_ship_step(sub_id, Step::Resolving);
        let brief = resolve_brief(&target, &sub.branch, &files);
        let name = match self.feature_agent(sub_id) {
            Some(agent) => {
                self.send_prompt(agent, brief);
                self.agent_name(agent)
            }
            None => {
                let spec = AgentSpec { name: "resolver".into(), role: Role::Free, on_feature_branch: true, brief: Some(brief), select: false, merge_from: Vec::new() };
                self.spawn_agent(sub_id, spec, cx);
                "A new agent (resolver)".into()
            }
        };
        let _ = self.store.log_event(sub_id, None, "conflict", &format!("{target} conflicts with {} in {}", sub.branch, files_list(&files)));
        self.ship_say(sub_id, format!("{target} conflicts with {} in {}. {name} is resolving them; the merge goes on when it's done.", sub.branch, files_list(&files)), cx);
    }

    /// Step 3: merge into the target, the project's way.
    fn ship_merge(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let target = self.ships.get(&sub_id).map(|s| s.target.clone()).unwrap_or_default();
        self.set_ship_step(sub_id, Step::Merging);
        let how = match self.method(sub.project_id) {
            Method::Push => " and pushing it to origin",
            Method::Request => " through a merge request",
            Method::Local => "",
        };
        self.ship_say(sub_id, format!("{} has everything from {target} and its score is at the bar. Merging it into {target}{how}.", sub.branch), cx);
        self.merge_subproject(sub_id, cx);
    }

    /// An agent's turn ended: if it was resolving our conflicts, review what it did.
    pub(crate) fn ship_after_turn(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        let (sub_id, name, branch) = (agent.record.subproject_id, agent.record.name.clone(), agent.record.branch.clone());
        if !self.ships.get(&sub_id).is_some_and(|s| s.step == Step::Resolving) {
            return;
        }
        if self.subprojects.iter().find(|s| s.id == sub_id).is_none_or(|s| s.branch != branch) {
            return;
        }
        if self.backend().is_none() {
            // No reviews: bring the target in again (a clean merge means it's resolved).
            self.ship_say(sub_id, format!("{name} is done with the conflicts. Checking that the branch merges now."), cx);
            self.ship_update(sub_id, cx);
            return;
        }
        self.set_ship_step(sub_id, Step::Reviewing(agent_id));
        self.ship_say(sub_id, format!("{name} is done with the conflicts. Reviewing its work before merging."), cx);
        self.review_fresh(agent_id, cx);
    }

    /// A review finished: if the merge waited for it, go on or stop.
    pub(crate) fn ship_after_review(&mut self, agent_id: i64, score: Option<u8>, cx: &mut Context<Self>) {
        let Some(sub_id) = self.agent(agent_id).map(|a| a.record.subproject_id) else { return };
        if !self.ships.get(&sub_id).is_some_and(|s| s.step == Step::Reviewing(agent_id)) {
            return;
        }
        let name = self.agent_name(agent_id);
        let min = self.min_score();
        match score {
            Some(score) if score >= min => self.ship_update(sub_id, cx),
            Some(score) => {
                let findings: Vec<String> = self
                    .agent(agent_id)
                    .and_then(|a| a.confidence.as_ref())
                    .map(|c| c.issues.iter().filter(|i| i.severity != "low").take(3).map(|i| format!("{}: {}", i.file, crate::review::clip(&i.problem, 160))).collect())
                    .unwrap_or_default();
                let what = if findings.is_empty() { String::new() } else { format!(" Main findings: {}.", findings.join("; ")) };
                self.ship_stuck(sub_id, format!("{name} scored {score}/5 after the update, below the {min}/5 needed.{what} Fix the findings, then merge again."), cx);
            }
            None => {
                let why = self.agent(agent_id).and_then(|a| a.review_error.clone()).unwrap_or_else(|| "no score".into());
                self.ship_stuck(sub_id, format!("{name} couldn't be reviewed ({why}). Review it, then merge again."), cx);
            }
        }
    }

    /// A merge finished (from `finish_merge`): done, try again, or stop.
    pub(crate) fn ship_after_merge(&mut self, sub_id: i64, merged: bool, conflict: &[String], tone: Tone, text: &str, cx: &mut Context<Self>) {
        if !self.ships.get(&sub_id).is_some_and(|s| s.step == Step::Merging) {
            return;
        }
        if merged || (matches!(tone, Tone::Info) && conflict.is_empty()) {
            // Merged, or a request is open and merges once the host allows it.
            self.set_ship_step(sub_id, Step::Done);
            self.ship_say(sub_id, text.to_string(), cx);
        } else if !conflict.is_empty() {
            // The target moved in the meantime: bring it in again.
            self.ship_update(sub_id, cx);
        } else {
            self.ship_stuck(sub_id, text.to_string(), cx);
        }
    }

    /// Says where it stands: on the card, in the Merge tab and to the lead.
    fn ship_say(&mut self, sub_id: i64, text: String, cx: &mut Context<Self>) {
        if let Some(s) = self.ships.get_mut(&sub_id) {
            s.status = text.clone();
        }
        self.set_merge_message(sub_id, Tone::Info, text.clone());
        self.tell_lead(sub_id, format!("Merge into develop: {text}"));
        cx.notify();
    }

    fn ship_stuck(&mut self, sub_id: i64, why: String, cx: &mut Context<Self>) -> String {
        if let Some(s) = self.ships.get_mut(&sub_id) {
            s.step = Step::Stuck;
            s.status = why.clone();
        }
        self.merge_state(sub_id).busy = false;
        self.set_merge_message(sub_id, Tone::Warn, why.clone());
        let _ = self.store.log_event(sub_id, None, "failed", &format!("Merge stopped: {why}"));
        self.tell_lead(sub_id, format!("Merge into develop stopped: {why} Tell the user in a sentence what they can do."));
        self.chime(crate::sound::Cue::NeedsYou);
        cx.notify();
        why
    }

    /// One line for the lead's `team_status`.
    pub(crate) fn ship_summary(&self, sub_id: i64) -> Option<String> {
        if let Some(s) = self.ships.get(&sub_id) {
            let state = match s.step {
                Step::Done => "done",
                Step::Stuck => "stopped",
                _ => "running",
            };
            return Some(format!("Merge into {} ({state}): {}", s.target, s.status));
        }
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let target = self.merge_target(sub_id)?;
        let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied()?;
        if ahead == 0 {
            return Some(format!("{target} already has everything from {}.", sub.branch));
        }
        let score = self.sub_confidence(sub_id).map(|(s, from)| format!(", confidence {s}/5 ({from})")).unwrap_or_default();
        Some(format!(
            "{} has {ahead} commit{} {target} doesn't{score}. When the user wants it merged, call merge_to_develop.",
            sub.branch,
            if ahead == 1 { "" } else { "s" }
        ))
    }

    /// The dashboard card: ready, under way (three steps), done or stopped.
    pub(crate) fn render_ship_card(&self, sub_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let target = self.merge_target(sub_id)?;
        let ship = self.ships.get(&sub_id);
        let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        let done_pipeline = self.pipelines.get(&sub_id).is_some_and(|p| p.stage == Stage::Done);
        // Show it when there's something to merge after a finished pipeline, or a merge to report.
        if ship.is_none() && !(done_pipeline && ahead.is_some_and(|n| n > 0)) {
            return None;
        }
        let title = match ship.map(|s| &s.step) {
            Some(Step::Done) => format!("Merged into {target}"),
            Some(Step::Stuck) => format!("Merging into {target} stopped"),
            Some(_) => format!("Merging into {target}"),
            None => format!("Ready to merge into {target}"),
        };
        let text = match ship {
            Some(s) => s.status.clone(),
            None => {
                let commits = ahead.map(|n| format!("{n} commit{} on {} that {target} doesn't have.", if n == 1 { "" } else { "s" }, sub.branch)).unwrap_or_default();
                let score = self.sub_confidence(sub_id).map(|(s, from)| format!(" Confidence {s}/5 ({from}).")).unwrap_or_default();
                format!(
                    "{commits}{score} One click brings {target} in, has any conflicts resolved and reviewed, then merges{}.",
                    match self.method(sub.project_id) {
                        Method::Push => " and pushes",
                        Method::Request => " through a merge request",
                        Method::Local => "",
                    }
                )
            }
        };
        let stuck = ship.is_some_and(|s| s.step == Step::Stuck);
        let running = ship.is_some_and(Ship::running);
        let step = ship.map_or(0, Ship::number);
        let steps = ["Check", &format!("Bring {target} in"), "Merge"];
        let step_row = div().flex().flex_wrap().gap(px(14.)).children(steps.iter().enumerate().map(|(i, label)| {
            let n = i + 1;
            let (state, color) = match ship.map(|s| &s.step) {
                Some(Step::Done) => (ui::State::Done, theme::text_2()),
                _ if n < step => (ui::State::Done, theme::text_2()),
                _ if n == step && running => (ui::State::Working, theme::text()),
                _ => (ui::State::Idle, theme::text_3()),
            };
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(rgb(color))
                .child(ui::status_dot(("ship-step", sub_id as usize * 10 + n), state))
                .child(label.to_string())
        }));
        let button = if running {
            ui::busy_button("dash-ship", Kind::Primary, "Merging…").into_any_element()
        } else if ship.is_some_and(|s| s.step == Step::Done) {
            div().into_any_element()
        } else {
            ui::button("dash-ship", Kind::Primary, Some(Icon::Merge), if stuck { "Try again".to_string() } else { format!("Merge into {target}") })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.ship(sub_id, "you", cx);
                }))
                .into_any_element()
        };
        Some(
            ui::card()
                .child(div().flex().items_center().gap(px(8.)).child(icon(Icon::Merge).size(px(14.))).child(ui::label(title)))
                .when(ship.is_some(), |d| d.child(step_row))
                .child(div().text_size(px(13.)).text_color(rgb(if stuck { theme::red_text() } else { theme::text_2() })).child(text))
                .child(div().flex().gap(px(6.)).child(button))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_numbered_for_the_card() {
        let ship = |step| Ship { step, target: "develop".into(), status: String::new(), rounds: 0 };
        assert_eq!(ship(Step::Updating).number(), 2);
        assert_eq!(ship(Step::Resolving).number(), 2);
        assert_eq!(ship(Step::Reviewing(3)).number(), 2);
        assert_eq!(ship(Step::Merging).number(), 3);
        assert!(ship(Step::Merging).running() && !ship(Step::Done).running() && !ship(Step::Stuck).running());
    }

    #[test]
    fn the_resolver_is_told_what_conflicts_and_how_to_number() {
        let files = vec!["packages/db/src/schema-version.ts".to_string(), "packages/db/package.json".to_string()];
        let brief = resolve_brief("develop", "feature/mobietrain-rework", &files);
        assert!(brief.contains("`packages/db/src/schema-version.ts`, `packages/db/package.json`"));
        assert!(brief.contains("next free number") && brief.contains("Don't push"));
        assert_eq!(files_list(&files), "2 files (schema-version.ts and package.json)");
        let many: Vec<String> = (1..=7).map(|n| format!("f{n}.ts")).collect();
        assert_eq!(files_list(&many), "7 files (f1.ts, f2.ts, f3.ts, f4.ts and 3 more)");
    }
}
