//! Phase 5: merging with the user's branch rules, promotions along
//! develop → acceptance → main, pushing, GitLab/GitHub requests, and
//! cleaning up worktrees.

use std::collections::HashMap;

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git::{self, BranchState, Cleanup, MergeOutcome};
use crate::hosting::{self, Remote, Request};
use crate::pipeline::{Role, Stage};
use crate::icons::{Icon, icon, icon_sized};
use crate::ui::{self, Kind};
use crate::{AgentSpec, Monaged, theme, tone_color};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Policy {
    Auto,
    Ask,
    Confirm,
}

impl Policy {
    fn as_str(self) -> &'static str {
        match self {
            Policy::Auto => "auto",
            Policy::Ask => "ask",
            Policy::Confirm => "confirm",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Policy::Auto),
            "ask" => Some(Policy::Ask),
            "confirm" => Some(Policy::Confirm),
            _ => None,
        }
    }

    /// The rule as a chip says it; `min` is the quality bar (Settings → Reviews).
    fn short(self, min: u8) -> String {
        match self {
            Policy::Auto => format!("auto from {min}/5"),
            Policy::Ask => "asks you".into(),
            Policy::Confirm => "you confirm twice".into(),
        }
    }

    fn next(self) -> Self {
        match self {
            Policy::Auto => Policy::Ask,
            Policy::Ask => Policy::Confirm,
            Policy::Confirm => Policy::Auto,
        }
    }
}

/// The user's rule: develop merges automatically when confidence is at the bar (4/5 by default);
/// acceptance asks; the top branch (main) needs an explicit confirmation.
pub fn default_policy(chain: &[String], target: &str) -> Policy {
    match target {
        "develop" => Policy::Auto,
        "acceptance" => Policy::Ask,
        t if chain.len() > 1 && chain.last().is_some_and(|last| last == t) => Policy::Confirm,
        _ => Policy::Ask,
    }
}

/// develop → acceptance → main (whichever exist), main/master last.
pub fn chain_from(state: &BranchState, base: &str) -> Vec<String> {
    let mut chain: Vec<String> = ["develop", "acceptance"].iter().filter(|b| state.has(b)).map(|b| b.to_string()).collect();
    let top = ["main", "master"].iter().find(|b| state.has(b)).map(|b| b.to_string()).unwrap_or_else(|| base.to_string());
    if !chain.contains(&top) {
        chain.push(top);
    }
    chain
}

#[derive(Default)]
pub struct MergeState {
    pub busy: bool,
    /// (target, short sha) once merged.
    pub merged: Option<(String, String)>,
    pub conflict: Vec<String>,
    pub message: Option<(Tone, String)>,
    pub request: Option<Request>,
}

#[derive(Default)]
pub struct BranchInfo {
    pub state: BranchState,
    pub chain: Vec<String>,
    /// (source, target) -> commits in source not yet in target.
    pub ahead: HashMap<(String, String), usize>,
    pub loading: bool,
}

impl Monaged {
    fn merge_state(&mut self, sub_id: i64) -> &mut MergeState {
        self.merges.entry(sub_id).or_insert_with(|| {
            let merged = self
                .store
                .setting(&format!("sub:{sub_id}:merged"))
                .and_then(|v| v.split_once(' ').map(|(t, s)| (t.to_string(), s.to_string())));
            MergeState { merged, ..Default::default() }
        })
    }

    fn set_merge_message(&mut self, sub_id: i64, tone: Tone, text: impl Into<String>) {
        self.merge_state(sub_id).message = Some((tone, text.into()));
    }

    pub(crate) fn chain(&self, project_id: i64) -> Vec<String> {
        match self.branch_info.get(&project_id) {
            Some(info) if !info.chain.is_empty() => info.chain.clone(),
            _ => self.projects.iter().find(|p| p.id == project_id).map(|p| vec![p.base_branch.clone()]).unwrap_or_default(),
        }
    }

    pub(crate) fn policy(&self, project_id: i64, target: &str) -> Policy {
        self.store
            .setting(&format!("project:{project_id}:policy:{target}"))
            .and_then(|v| Policy::parse(&v))
            .unwrap_or_else(|| default_policy(&self.chain(project_id), target))
    }

    fn cycle_policy(&mut self, project_id: i64, target: String, cx: &mut Context<Self>) {
        let next = self.policy(project_id, &target).next();
        let _ = self.store.set_setting(&format!("project:{project_id}:policy:{target}"), next.as_str());
        cx.notify();
    }

    pub(crate) fn remote(&self, project_id: i64) -> Option<Remote> {
        let mut remote = self.branch_info.get(&project_id)?.state.remote_url.as_deref().and_then(hosting::parse_remote)?;
        remote.login = self.git_auth.project_logins.get(&project_id).cloned();
        Some(remote)
    }

    /// Where a subproject's work goes first: the bottom of the chain (develop, or main).
    pub(crate) fn merge_target(&self, sub_id: i64) -> Option<String> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        self.chain(sub.project_id).first().cloned()
    }

    /// The integrator's score, else the lowest builder score, else the lowest of any agent.
    pub(crate) fn sub_confidence(&self, sub_id: i64) -> Option<(u8, String)> {
        let agents: Vec<_> = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).collect();
        // With an integrator, only its score covers what gets merged.
        if let Some(integrator) = agents.iter().rev().find(|a| a.role() == Role::Integrator) {
            return integrator.confidence.as_ref().map(|c| (c.score, "integrator".into()));
        }
        let lowest = |filter: &dyn Fn(&Role) -> bool, label: &str| {
            agents
                .iter()
                .filter(|a| filter(&a.role()))
                .filter_map(|a| a.confidence.as_ref().map(|c| c.score))
                .min()
                .map(|s| (s, label.to_string()))
        };
        lowest(&|r| matches!(r, Role::Builder(_)), "lowest builder").or_else(|| lowest(&|_| true, "lowest agent"))
    }

    // ---- Branch state ----------------------------------------------------------

    pub(crate) fn refresh_branches(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        let features: Vec<String> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.branch.clone()).collect();
        self.branch_info.entry(project_id).or_default().loading = true;
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let state = git::branch_state(&distro, &project.path)?;
                    // Which account the project signs in as (Settings → Git accounts).
                    let login = state
                        .remote_url
                        .as_deref()
                        .filter(|u| u.starts_with("http"))
                        .and_then(|u| crate::git_auth::project_login(&distro, &project.path, &crate::git_auth::normalize_host(u)));
                    let chain = chain_from(&state, &project.base_branch);
                    let mut pairs: Vec<(String, String)> = chain.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
                    if let Some(first) = chain.first() {
                        pairs.extend(features.iter().filter(|f| state.has(f)).map(|f| (f.clone(), first.clone())));
                    }
                    let counts = git::ahead_counts(&distro, &project.path, &pairs)?;
                    anyhow::Ok((state, chain, pairs.into_iter().zip(counts).collect::<HashMap<_, _>>(), login))
                })
                .await;
            view.update(cx, |this, cx| {
                let info = this.branch_info.entry(project_id).or_default();
                info.loading = false;
                match result {
                    Ok((state, chain, ahead, login)) => {
                        info.state = state;
                        match login {
                            Some(login) => drop(this.git_auth.project_logins.insert(project_id, login)),
                            None => drop(this.git_auth.project_logins.remove(&project_id)),
                        }
                        let info = this.branch_info.entry(project_id).or_default();
                        info.chain = chain;
                        info.ahead = ahead;
                        // Finished pipelines may now be allowed to merge on their own.
                        let done: Vec<i64> = this
                            .subprojects
                            .iter()
                            .filter(|s| s.project_id == project_id)
                            .map(|s| s.id)
                            .filter(|id| this.pipelines.get(id).is_some_and(|p| p.stage == Stage::Done))
                            .collect();
                        for sub_id in done {
                            this.maybe_auto_merge(sub_id, cx);
                        }
                    }
                    Err(err) => this.toast_error(format!("Couldn't read the branches: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn setup_branches(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        let top = self.chain(project_id).last().cloned().unwrap_or_else(|| project.base_branch.clone());
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let (d, repo, t) = (distro.clone(), project.path.clone(), top.clone());
            let result = cx
                .background_spawn(async move {
                    git::ensure_branch(&d, &repo, "acceptance", &t)?;
                    git::ensure_branch(&d, &repo, "develop", &t)
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        let _ = this.store.set_project_base(project_id, "develop");
                        if let Some(p) = this.projects.iter_mut().find(|p| p.id == project_id) {
                            p.base_branch = "develop".into();
                        }
                        this.toast(Tone::Info, format!("Created develop and acceptance from {top}. New subprojects now branch off develop."), cx);
                    }
                    Err(err) => this.toast_error(format!("Couldn't create the branches: {err:#}"), cx),
                }
                this.refresh_branches(project_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Merging ---------------------------------------------------------------

    pub(crate) fn merge_subproject(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(target) = self.merge_target(sub_id) else { return };
        let message = format!("Merge {} into {target} ({})", sub.branch, sub.name);
        self.run_merge(sub.project_id, sub.branch, target, message, Some(sub_id), cx);
    }

    /// After the pipeline is done and a confidence score is in: merge on its
    /// own when the target's policy is "auto" and confidence is at the bar.
    pub(crate) fn maybe_auto_merge(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if !self.pipelines.get(&sub_id).is_some_and(|p| p.stage == Stage::Done) {
            return;
        }
        let Some((project_id, branch)) = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| (s.project_id, s.branch.clone())) else { return };
        let Some(target) = self.merge_target(sub_id) else { return };
        // Only when there's something to merge (unknown counts wait for the next refresh).
        let ahead = self.branch_info.get(&project_id).and_then(|i| i.ahead.get(&(branch, target.clone()))).copied();
        if !ahead.is_some_and(|n| n > 0) {
            return;
        }
        // Never retry on our own while conflicts are open: someone has to resolve them first.
        let state = self.merge_state(sub_id);
        if state.busy || !state.conflict.is_empty() {
            return;
        }
        if self.policy(project_id, &target) != Policy::Auto {
            self.set_merge_message(sub_id, Tone::Info, format!("Ready to merge into {target}. This project asks you first."));
            return;
        }
        // The score has to be of what's there now.
        let integrator = self.agents.iter().rev().find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() == Role::Integrator);
        if integrator.is_some_and(|a| a.reviewing || a.review_stale || a.is_working()) {
            self.set_merge_message(sub_id, Tone::Info, "Waiting for the integrator's latest work to be reviewed before merging.");
            cx.notify();
            return;
        }
        match self.sub_confidence(sub_id) {
            None if self.backend().is_none() => self.set_merge_message(
                sub_id,
                Tone::Warn,
                "Auto-merge needs a confidence score, and reviews are off. Turn them on in Settings, or merge by hand.",
            ),
            None => self.set_merge_message(sub_id, Tone::Info, "Waiting for the confidence score before merging."),
            Some((score, _)) if score < self.min_score() => self.set_merge_message(
                sub_id,
                Tone::Warn,
                format!("Confidence is {score}/5, below the {}/5 needed to merge into {target} by itself. Fix the findings, or merge it yourself.", self.min_score()),
            ),
            Some((score, from)) => {
                self.set_merge_message(sub_id, Tone::Info, format!("Confidence {score}/5 ({from}): merging into {target} automatically."));
                self.merge_subproject(sub_id, cx);
            }
        }
        cx.notify();
    }

    fn promote(&mut self, project_id: i64, from: String, to: String, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.policy(project_id, &to) == Policy::Confirm {
            self.ask_confirm(crate::feedback::ConfirmAction::Promote(project_id, from, to), window, cx);
            return;
        }
        self.promote_confirmed(project_id, from, to, cx);
    }

    pub(crate) fn promote_confirmed(&mut self, project_id: i64, from: String, to: String, cx: &mut Context<Self>) {
        let message = format!("Promote {from} into {to}");
        self.run_merge(project_id, from, to, message, None, cx);
    }

    fn run_merge(&mut self, project_id: i64, source: String, target: String, message: String, sub_id: Option<i64>, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        if let Some(sub_id) = sub_id {
            let state = self.merge_state(sub_id);
            state.busy = true;
            state.conflict.clear();
            state.message = Some((Tone::Info, format!("Merging into {target}…")));
        }
        self.progress("merge", format!("Merging {source} into {target}…"), cx);
        cx.notify();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let (s, t) = (source.clone(), target.clone());
            let outcome = cx.background_spawn(async move { git::merge_branch(&distro, &project.path, &s, &t, &message) }).await;
            view.update(cx, |this, cx| {
                let (tone, text, merged_sha, conflict) = match outcome {
                    Ok(MergeOutcome::Merged(sha)) => (Tone::Info, format!("Merged {source} into {target} ({sha})."), Some(sha), Vec::new()),
                    Ok(MergeOutcome::AlreadyMerged) => (Tone::Info, format!("{target} already contains everything from {source}."), Some(String::new()), Vec::new()),
                    Ok(MergeOutcome::Conflict(files)) => (
                        Tone::Warn,
                        format!("Merge conflicts with {target} in {} file(s); nothing was changed.", files.len()),
                        None,
                        files,
                    ),
                    Ok(MergeOutcome::Blocked(why)) => (Tone::Error, format!("Didn't merge: {why}"), None, Vec::new()),
                    Err(err) => (Tone::Error, format!("Merge failed: {err:#}"), None, Vec::new()),
                };
                // A merge that did something rises; one that couldn't, falls.
                match (&merged_sha, tone) {
                    (Some(sha), _) if !sha.is_empty() => this.chime(crate::sound::Cue::Success),
                    (None, Tone::Warn | Tone::Error) => this.chime(crate::sound::Cue::Error),
                    _ => {}
                }
                if let Some(sub_id) = sub_id {
                    if let Some(sha) = &merged_sha {
                        let _ = this.store.set_setting(&format!("sub:{sub_id}:merged"), &format!("{target} {sha}"));
                        if let Some(p) = this.pipelines.get_mut(&sub_id) {
                            p.note = Some(text.clone());
                        }
                        if this.pipelines.contains_key(&sub_id) {
                            let _ = this.store.save_pipeline(&this.pipelines[&sub_id]);
                        }
                    }
                    let kind = if merged_sha.is_some() { "merge" } else if !conflict.is_empty() { "conflict" } else { "failed" };
                    let _ = this.store.log_event(sub_id, None, kind, &text);
                    let state = this.merge_state(sub_id);
                    state.busy = false;
                    state.conflict = conflict;
                    state.message = Some((tone, text.clone()));
                    if let Some(sha) = merged_sha {
                        state.merged = Some((target.clone(), sha));
                    }
                }
                this.settle("merge", tone, text, cx);
                this.advance_merge_queue(cx);
                this.refresh_branches(project_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Hands the conflicts to an agent working on the feature branch.
    fn resolve_conflicts(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(target) = self.merge_target(sub_id) else { return };
        let files = self.merge_state(sub_id).conflict.join(", ");
        let brief = format!(
            "Merge `{target}` into this branch (`{}`) and resolve the conflicts. Git reported conflicts in: {files}. \
             Keep the intent of both sides, run the project's tests, and commit the merge. Don't push. \
             When you're done, say so in one sentence.",
            sub.branch
        );
        let on_feature = self
            .agents
            .iter()
            .find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.record.branch == sub.branch)
            .map(|a| a.record.id);
        match on_feature {
            Some(agent_id) => {
                self.send_prompt(agent_id, brief);
                self.select_agent(agent_id, cx);
            }
            None => {
                let spec = AgentSpec { name: "resolver".into(), role: Role::Free, on_feature_branch: true, brief: Some(brief), select: true };
                self.spawn_agent(sub_id, spec, cx);
            }
        }
        self.set_merge_message(sub_id, Tone::Info, format!("An agent is merging {target} into {} to resolve the conflicts. Merge again when it's done.", sub.branch));
        cx.notify();
    }

    // ---- Pushing and requests ------------------------------------------------------

    pub(crate) fn push_sub(&mut self, sub_id: i64, then_request: bool, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let target = self.merge_target(sub_id).unwrap_or_default();
        let remote = self.remote(project.id);
        let title = self
            .pipelines
            .get(&sub_id)
            .and_then(|p| p.plan.as_ref())
            .map(crate::pipeline::plan_title)
            .unwrap_or_else(|| sub.name.clone());
        let body = {
            let summary = self.pipelines.get(&sub_id).and_then(|p| p.plan.as_ref()).and_then(|p| p["summary"].as_str().map(str::to_owned));
            let confidence = self.sub_confidence(sub_id).map(|(s, from)| format!("\n\nConfidence: {s}/5 ({from})")).unwrap_or_default();
            format!("{}{}\n\nOpened by Monaged.", summary.unwrap_or_else(|| sub.name.clone()), confidence)
        };
        let state = self.merge_state(sub_id);
        state.busy = true;
        state.message = Some((Tone::Info, format!("Pushing {}…", sub.branch)));
        cx.notify();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let branch = sub.branch.clone();
            let result = cx
                .background_spawn(async move {
                    git::push_branch(&distro, &project.path, &branch)?;
                    if !then_request {
                        return anyhow::Ok(None);
                    }
                    let remote = remote.ok_or_else(|| anyhow::anyhow!("origin isn't on GitHub or GitLab"))?;
                    Ok(Some(hosting::create_request(&remote, &branch, &target, &title, &body)?))
                })
                .await;
            view.update(cx, |this, cx| {
                match &result {
                    Ok(Some(url)) => drop(this.store.log_event(sub_id, None, "push", &format!("Pushed {} and opened {url}", sub.branch))),
                    Ok(None) => drop(this.store.log_event(sub_id, None, "push", &format!("Pushed {} to origin", sub.branch))),
                    Err(err) => drop(this.store.log_event(sub_id, None, "failed", &format!("Push failed: {err:#}"))),
                }
                this.chime(if result.is_ok() { crate::sound::Cue::Success } else { crate::sound::Cue::Error });
                let state = this.merge_state(sub_id);
                state.busy = false;
                state.message = Some(match result {
                    Ok(Some(url)) => (Tone::Info, format!("Opened: {url}")),
                    Ok(None) => (Tone::Info, format!("Pushed {} to origin.", sub.branch)),
                    Err(err) => {
                        let text = format!("{err:#}");
                        let hint = if crate::git_auth::looks_like_auth_failure(&text) {
                            " Sign in under Settings → Git accounts (or test the account there)."
                        } else {
                            ""
                        };
                        (Tone::Error, format!("{err:#}{hint}"))
                    }
                });
                this.refresh_request(sub_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn refresh_request(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(remote) = self.remote(sub.project_id).filter(|r| r.provider.is_some()) else { return };
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { hosting::request_status(&remote, &sub.branch) }).await;
            view.update(cx, |this, cx| {
                if let Ok(request) = result {
                    this.merge_state(sub_id).request = request;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Cleanup -------------------------------------------------------------------

    /// Removes the subproject's agent worktrees that have no uncommitted work,
    /// and deletes their branches when everything on them is in the feature branch.
    pub(crate) fn cleanup_worktrees(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let working = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.is_working()).count();
        // While a pipeline runs, its agents and the lead stay: their branches
        // still have to be integrated, and the lead is how you talk to it.
        let running = self.pipelines.get(&sub_id).filter(|p| !matches!(p.stage, crate::pipeline::Stage::Done | crate::pipeline::Stage::Stopped));
        let keep = |a: &crate::agent::AgentView| {
            a.role() == Role::Lead || running.is_some_and(|p| p.members.contains(&a.record.id) || (p.members.is_empty() && a.role() != Role::Free))
        };
        let targets: Vec<(i64, String, String)> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && !a.is_working() && !keep(a))
            .map(|a| (a.record.id, a.record.worktree.clone(), a.record.branch.clone()))
            .collect();
        self.set_merge_message(sub_id, Tone::Info, "Cleaning up worktrees…");
        cx.notify();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let feature = sub.branch.clone();
            let results = cx
                .background_spawn(async move {
                    targets
                        .into_iter()
                        .map(|(id, wt, br)| (id, git::remove_worktree(&distro, &project.path, &wt, &br, &feature)))
                        .collect::<Vec<_>>()
                })
                .await;
            view.update(cx, |this, cx| {
                let (mut removed, mut branches, mut dirty, mut failed) = (0, 0, 0, Vec::new());
                for (agent_id, result) in results {
                    match result {
                        Ok(Cleanup::Removed(deleted_branch)) => {
                            removed += 1;
                            branches += deleted_branch as usize;
                            let _ = this.store.archive_agent(agent_id);
                            if let Some(agent) = this.agent_mut(agent_id) {
                                agent.record.archived = true;
                                agent.live = false;
                            }
                        }
                        Ok(Cleanup::Dirty) => dirty += 1,
                        Ok(Cleanup::Failed(why)) => failed.push(why),
                        Err(err) => failed.push(format!("{err:#}")),
                    }
                }
                if this.sel_agent.is_some_and(|id| this.agent(id).is_some_and(|a| a.record.archived)) {
                    this.sel_agent = this.agents.iter().find(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| a.record.id);
                }
                let mut text = format!("Removed {removed} worktree(s) and {branches} merged branch(es).");
                if dirty > 0 {
                    text.push_str(&format!(" Kept {dirty} with uncommitted changes."));
                }
                if working > 0 {
                    text.push_str(&format!(" Skipped {working} agent(s) still working."));
                }
                if !failed.is_empty() {
                    text.push_str(&format!(" Problems: {}", failed.join("; ")));
                }
                this.set_merge_message(sub_id, if failed.is_empty() { Tone::Info } else { Tone::Warn }, text);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Rendering -------------------------------------------------------------------

    /// A branch rule you can click to change: quiet mono text with a hairline.
    fn policy_chip(&self, project_id: i64, target: &str, cx: &mut Context<Self>) -> AnyElement {
        let policy = self.policy(project_id, target);
        let t = target.to_string();
        div()
            .id(SharedString::from(format!("policy-{target}")))
            .flex_none()
            .whitespace_nowrap()
            .px(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .font_family(theme::MONO)
            .text_size(px(11.))
            .text_color(rgb(theme::text_2()))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
            .tooltip(crate::shell::tip("Click to change the rule"))
            .child(policy.short(self.min_score()))
            .on_click(cx.listener(move |this, _, _, cx| this.cycle_policy(project_id, t.clone(), cx)))
            .into_any_element()
    }

    /// The project's branch flow: feature/* → develop → acceptance → main, the
    /// rule on each hop, what's waiting to be promoted, and the remote.
    pub(crate) fn render_branch_panel(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let chain = self.chain(project_id);
        let info = self.branch_info.get(&project_id);
        let remote = self.remote(project_id);
        let feature_count = self.subprojects.iter().filter(|s| s.project_id == project_id).count();

        let node = |name: String, note: String| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().font_family(theme::MONO).text_size(px(12.5)).text_color(rgb(theme::text())).child(name))
                .child(ui::mono(note))
        };
        let mut flow = div().flex().items_center().gap(px(10.)).child(node(
            "feature/*".into(),
            format!("{feature_count} subproject{}", if feature_count == 1 { "" } else { "s" }),
        ));
        let mut previous = "feature/*".to_string();
        for target in &chain {
            let ahead = info.and_then(|i| i.ahead.get(&(previous.clone(), target.clone()))).copied();
            let note = match ahead {
                Some(0) | None if previous == "feature/*" => "base for features".to_string(),
                Some(0) => "up to date".to_string(),
                Some(n) => format!("{n} waiting"),
                None => String::new(),
            };
            flow = flow
                .child(
                    div()
                        .flex_1()
                        .min_w(px(40.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.))
                        .child(self.policy_chip(project_id, target, cx))
                        .child(div().w_full().h(px(1.)).bg(rgb(theme::line_strong()))),
                )
                .child(node(target.clone(), note));
            previous = target.clone();
        }

        let hops = chain.windows(2).filter_map(|pair| {
            let (from, to) = (pair[0].clone(), pair[1].clone());
            let ahead = info.and_then(|i| i.ahead.get(&(from.clone(), to.clone()))).copied().unwrap_or(0);
            (ahead > 0).then(|| {
                let (f, t) = (from.clone(), to.clone());
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(rgb(theme::line()))
                    .child(icon(Icon::Merge).text_color(rgb(theme::text_3())))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .child(format!("{ahead} commit{} from {from} ready for {to}", if ahead == 1 { "" } else { "s" })),
                    )
                    .child(ui::button(SharedString::from(format!("promote-{from}-{to}")), Kind::Secondary, None, format!("Promote to {to}")).on_click(
                        cx.listener(move |this, _, window, cx| this.promote(project_id, f.clone(), t.clone(), window, cx)),
                    ))
            })
        });

        let remote_line = match (&remote, info.and_then(|i| i.state.remote_url.as_ref())) {
            (Some(r), _) => format!(
                "origin {}/{}{}",
                r.host,
                r.slug,
                match r.provider {
                    Some(p) => match self
                        .git_auth
                        .accounts
                        .iter()
                        .find(|a| a.host == crate::git_auth::normalize_host(&r.host) && r.login.as_ref().map_or(a.default, |l| *l == a.login))
                    {
                        Some(a) => format!(" · {}s as {}", p.request_name(), a.login),
                        None => format!(" · {}s via {}", p.request_name(), if p == hosting::Provider::GitHub { "gh" } else { "glab" }),
                    },
                    None => " · push only".into(),
                }
            ),
            (None, Some(url)) => format!("origin {url}"),
            (None, None) => "No remote: pushing and merge requests are off".into(),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(16.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(theme::line()))
            .bg(rgb(theme::panel()))
            .child(flow)
            .children(hops)
            .when(chain.len() == 1, |d| {
                d.child(div().text_size(px(12.5)).text_color(rgb(theme::text_3())).child(format!(
                    "Only {} exists, so features merge straight into it. Set up develop and acceptance for a develop → acceptance → main flow.",
                    chain.first().cloned().unwrap_or_default()
                )))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(2.))
                    .child(icon_sized(Icon::Branch, 13.).text_color(rgb(theme::text_3())))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(ui::mono(remote_line)))
                    .when(chain.len() == 1, |d| {
                        d.child(ui::button("setup-branches", Kind::Secondary, None, "Set up develop and acceptance").on_click(cx.listener(
                            move |this, _, _, cx| this.setup_branches(project_id, cx),
                        )))
                    })
                    .child(
                        ui::icon_button("refresh-branches", Icon::Refresh)
                            .tooltip(crate::shell::tip("Read the branches again"))
                            .on_click(cx.listener(move |this, _, _, cx| this.refresh_branches(project_id, cx))),
                    ),
            )
            .into_any_element()
    }

    /// The inspector's Merge tab for a subproject.
    pub(crate) fn render_merge_inspector(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return div().into_any_element() };
        let Some(target) = self.merge_target(sub_id) else {
            return ui::panel_section("Merge")
                .child(ui::skeleton("merge-sk-1", px(180.).into(), 10.))
                .child(ui::skeleton("merge-sk-2", px(240.).into(), 10.))
                .child(ui::skeleton("merge-sk-3", px(140.).into(), 10.))
                .into_any_element();
        };
        let project_id = sub.project_id;
        let state = self.merges.get(&sub_id);
        // Before this run touched it, the last merge comes from the database.
        let merged = state.and_then(|s| s.merged.clone()).or_else(|| {
            self.store
                .setting(&format!("sub:{sub_id}:merged"))
                .and_then(|v| v.split_once(' ').map(|(t, s)| (t.to_string(), s.to_string())))
        });
        let busy = state.is_some_and(|s| s.busy);
        let ahead = self.branch_info.get(&project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        let remote = self.remote(project_id);
        let provider = remote.as_ref().and_then(|r| r.provider);
        let has_origin = self.branch_info.get(&project_id).is_some_and(|i| i.state.remote_url.is_some());
        let confidence = self.sub_confidence(sub_id);
        let conflict = state.map(|s| s.conflict.clone()).unwrap_or_default();

        let status = match (&merged, ahead) {
            (Some((t, sha)), Some(0) | None) if !sha.is_empty() => format!("Merged into {t} at {sha}."),
            (Some((t, _)), Some(0) | None) => format!("Already in {t}."),
            (_, Some(0)) => "Nothing to merge yet.".to_string(),
            (_, Some(n)) => format!("{n} commit{} ready to merge.", if n == 1 { "" } else { "s" }),
            (_, None) => "Checking…".to_string(),
        };
        let kv = |k: &str, v: AnyElement| {
            div().flex().items_center().justify_between().gap(px(10.)).text_size(px(12.5)).child(div().text_color(rgb(theme::text_3())).child(k.to_string())).child(v)
        };

        let mut col = div().flex().flex_col();
        col = col.child(
            ui::panel_section("Merge")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .font_family(theme::MONO)
                        .text_size(px(12.))
                        .child(div().min_w(px(0.)).truncate().child(sub.branch.clone()))
                        .child(icon_sized(Icon::ChevronRight, 12.).text_color(rgb(theme::text_3())))
                        .child(target.clone()),
                )
                .child(kv("Rule", self.policy_chip(project_id, &target, cx)))
                .child(kv(
                    "Confidence",
                    match confidence {
                        Some((score, from)) => div().flex().items_center().gap(px(8.)).child(ui::mono(from)).child(ui::score_meter(score)).into_any_element(),
                        None => ui::mono("not reviewed").into_any_element(),
                    },
                ))
                .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(status)),
        );
        let mut actions = div().flex().flex_col().gap(px(8.));
        // Nothing to merge: keep the button where it'll be, greyed, with the reason.
        if !busy && ahead == Some(0) {
            actions = actions.child(
                div()
                    .id("merge-sub-idle")
                    .tooltip(crate::shell::tip("Nothing to merge yet: the branch has no new commits"))
                    .child(ui::disabled_button("merge-sub-off", Kind::Primary, Some(Icon::Merge), format!("Merge into {target}"))),
            );
        }
        if !busy && ahead.unwrap_or(0) > 0 {
            actions = actions.child(
                ui::button("merge-sub", Kind::Primary, Some(Icon::Merge), format!("Merge into {target}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.merge_subproject(sub_id, cx))),
            );
        }
        if !busy && !conflict.is_empty() {
            actions = actions.child(
                ui::button("resolve", Kind::Secondary, Some(Icon::Agent), "Resolve conflicts with an agent")
                    .on_click(cx.listener(move |this, _, _, cx| this.resolve_conflicts(sub_id, cx))),
            );
        }
        if !busy && has_origin {
            actions = actions.child(
                ui::button("push-sub", Kind::Secondary, Some(Icon::Push), "Push branch").on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, false, cx))),
            );
        }
        if !busy && provider.is_some() && state.and_then(|s| s.request.as_ref()).is_none() {
            let name = provider.map(|p| p.request_name()).unwrap_or("request");
            actions = actions.child(
                ui::button("open-request", Kind::Secondary, Some(Icon::External), format!("Open {name}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, true, cx))),
            );
        }
        if !busy {
            actions = actions.child(
                ui::button("cleanup", Kind::Ghost, Some(Icon::Archive), "Clean up worktrees")
                    .on_click(cx.listener(move |this, _, window, cx| this.ask_confirm(crate::feedback::ConfirmAction::CleanupWorktrees(sub_id), window, cx))),
            );
        }
        if busy {
            let doing = state
                .and_then(|s| s.message.as_ref())
                .filter(|(tone, _)| matches!(tone, Tone::Info))
                .map(|(_, text)| text.clone())
                .unwrap_or_else(|| "Working…".into());
            actions = actions.child(ui::busy_button("merge-busy", Kind::Secondary, doing));
        }
        col = col.child(div().px(px(16.)).py(px(14.)).border_b_1().border_color(rgb(theme::line())).child(actions));
        if !conflict.is_empty() {
            col = col.child(
                ui::panel_section("Conflicts")
                    .child(div().flex().flex_col().gap(px(4.)).children(conflict.iter().map(|f| {
                        div().font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::red_text())).child(f.clone())
                    }))),
            );
        }
        if let Some(r) = state.and_then(|s| s.request.as_ref()) {
            let url = r.url.clone();
            col = col.child(
                ui::panel_section(provider.map(|p| p.request_name()).unwrap_or("request")).child(
                    div()
                        .id("request-link")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .hover(|s| s.text_color(rgb(theme::text())))
                        .child(format!("#{} · {}", r.number, r.state))
                        .child(icon_sized(Icon::External, 13.))
                        .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url))),
                ),
            );
        }
        if let Some((tone, text)) = state.and_then(|s| s.message.as_ref()) {
            col = col.child(div().px(px(16.)).py(px(12.)).text_size(px(12.5)).text_color(rgb(tone_color(*tone))).child(text.clone()));
        }
        col.into_any_element()
    }
}
