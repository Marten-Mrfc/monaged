//! Phase 5: merging with the user's branch rules, promotions along
//! develop → acceptance → main, pushing, GitLab/GitHub requests, and
//! cleaning up worktrees.

use std::collections::HashMap;

use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git::{self, BranchState, Cleanup, MergeOutcome};
use crate::hosting::{self, Remote, Request};
use crate::pipeline::{Role, Stage};
use crate::review_ui::score_color;
use crate::{AgentSpec, Monaged, button, theme, tone_color};

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

    fn short(self) -> &'static str {
        match self {
            Policy::Auto => "auto unless 1/5",
            Policy::Ask => "asks you",
            Policy::Confirm => "you confirm twice",
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

/// The user's rule: develop merges automatically unless confidence is 1;
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

    fn remote(&self, project_id: i64) -> Option<Remote> {
        self.branch_info.get(&project_id)?.state.remote_url.as_deref().and_then(hosting::parse_remote)
    }

    /// Where a subproject's work goes first: the bottom of the chain (develop, or main).
    pub(crate) fn merge_target(&self, sub_id: i64) -> Option<String> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        self.chain(sub.project_id).first().cloned()
    }

    /// The integrator's score, else the lowest builder score, else the lowest of any agent.
    fn sub_confidence(&self, sub_id: i64) -> Option<(u8, String)> {
        let agents: Vec<_> = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).collect();
        if let Some(c) = agents.iter().find(|a| a.role() == Role::Integrator).and_then(|a| a.confidence.as_ref()) {
            return Some((c.score, "integrator".into()));
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
                    let chain = chain_from(&state, &project.base_branch);
                    let mut pairs: Vec<(String, String)> = chain.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();
                    if let Some(first) = chain.first() {
                        pairs.extend(features.iter().filter(|f| state.has(f)).map(|f| (f.clone(), first.clone())));
                    }
                    let counts = git::ahead_counts(&distro, &project.path, &pairs)?;
                    anyhow::Ok((state, chain, pairs.into_iter().zip(counts).collect::<HashMap<_, _>>()))
                })
                .await;
            view.update(cx, |this, cx| {
                let info = this.branch_info.entry(project_id).or_default();
                info.loading = false;
                match result {
                    Ok((state, chain, ahead)) => {
                        info.state = state;
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
                    Err(err) => this.banner = Some((Tone::Error, format!("Couldn't read the branches: {err:#}"))),
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
                        this.banner = Some((Tone::Info, format!("Created develop and acceptance from {top}. New subprojects now branch off develop.")));
                    }
                    Err(err) => this.banner = Some((Tone::Error, format!("Couldn't create the branches: {err:#}"))),
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
    /// own when the target's policy is "auto" and confidence isn't 1.
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
        match self.sub_confidence(sub_id) {
            None if self.backend().is_none() => self.set_merge_message(
                sub_id,
                Tone::Warn,
                "Auto-merge needs a confidence score, and reviews are off. Turn them on in Settings, or merge by hand.",
            ),
            None => self.set_merge_message(sub_id, Tone::Info, "Waiting for the confidence score before merging."),
            Some((1, _)) => self.set_merge_message(sub_id, Tone::Warn, format!("Confidence is 1/5: check the changes before merging into {target}.")),
            Some((score, from)) => {
                self.set_merge_message(sub_id, Tone::Info, format!("Confidence {score}/5 ({from}): merging into {target} automatically."));
                self.merge_subproject(sub_id, cx);
            }
        }
        cx.notify();
    }

    fn promote(&mut self, project_id: i64, from: String, to: String, cx: &mut Context<Self>) {
        if self.policy(project_id, &to) == Policy::Confirm
            && self.promote_confirm.as_ref() != Some(&(project_id, from.clone(), to.clone()))
        {
            self.promote_confirm = Some((project_id, from, to));
            cx.notify();
            return;
        }
        self.promote_confirm = None;
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
        } else {
            self.banner = Some((Tone::Info, format!("Merging {source} into {target}…")));
        }
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
                match sub_id {
                    Some(sub_id) => {
                        if let Some(sha) = &merged_sha {
                            let _ = this.store.set_setting(&format!("sub:{sub_id}:merged"), &format!("{target} {sha}"));
                            if let Some(p) = this.pipelines.get_mut(&sub_id) {
                                p.note = Some(text.clone());
                            }
                            if this.pipelines.contains_key(&sub_id) {
                                let _ = this.store.save_pipeline(&this.pipelines[&sub_id]);
                            }
                        }
                        let state = this.merge_state(sub_id);
                        state.busy = false;
                        state.conflict = conflict;
                        state.message = Some((tone, text));
                        if let Some(sha) = merged_sha {
                            state.merged = Some((target.clone(), sha));
                        }
                    }
                    None => this.banner = Some((tone, text)),
                }
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

    fn push_sub(&mut self, sub_id: i64, then_request: bool, cx: &mut Context<Self>) {
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
                let state = this.merge_state(sub_id);
                state.busy = false;
                state.message = Some(match result {
                    Ok(Some(url)) => (Tone::Info, format!("Opened: {url}")),
                    Ok(None) => (Tone::Info, format!("Pushed {} to origin.", sub.branch)),
                    Err(err) => {
                        let hint = if format!("{err:#}").contains("could not read") || format!("{err:#}").contains("Authentication") {
                            " Turn on \"Use my Windows Git credentials\" in Settings."
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
        let targets: Vec<(i64, String, String)> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && !a.is_working())
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

    pub(crate) fn enable_windows_credentials(&mut self, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let d = distro.clone();
            let result = cx.background_spawn(async move { git::use_windows_credentials(&d) }).await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.creds_ok = Some(true);
                        this.banner = Some((Tone::Info, "Git in WSL now uses your Windows logins (Git Credential Manager) for push and clone.".into()));
                    }
                    Err(err) => this.banner = Some((Tone::Error, format!("{err:#}"))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn check_credentials(&mut self, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let ok = cx.background_spawn(async move { git::uses_windows_credentials(&distro) }).await;
            view.update(cx, |this, cx| {
                this.creds_ok = Some(ok);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Rendering -------------------------------------------------------------------

    pub(crate) fn render_branch_panel(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let chain = self.chain(project_id);
        let info = self.branch_info.get(&project_id);
        let remote = self.remote(project_id);

        let mut flow = div().flex().items_center().flex_wrap().gap_2().text_size(px(13.)).child(div().font_family("Consolas").child("feature/*"));
        for target in &chain {
            let policy = self.policy(project_id, target);
            let t = target.clone();
            flow = flow
                .child(div().text_color(rgb(theme::MUTED)).child("→"))
                .child(div().font_family("Consolas").child(target.clone()))
                .child(
                    div()
                        .id(SharedString::from(format!("policy-{target}")))
                        .px_1p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(rgb(policy_color(policy)))
                        .text_color(rgb(policy_color(policy)))
                        .text_size(px(11.))
                        .cursor_pointer()
                        .child(policy.short())
                        .on_click(cx.listener(move |this, _, _, cx| this.cycle_policy(project_id, t.clone(), cx))),
                );
        }

        let hops = chain.windows(2).map(|pair| {
            let (from, to) = (pair[0].clone(), pair[1].clone());
            let ahead = info.and_then(|i| i.ahead.get(&(from.clone(), to.clone()))).copied().unwrap_or(0);
            let confirming = self.promote_confirm.as_ref() == Some(&(project_id, from.clone(), to.clone()));
            let (f, t) = (from.clone(), to.clone());
            div()
                .flex()
                .items_center()
                .gap_3()
                .text_size(px(13.))
                .child(div().w(px(220.)).font_family("Consolas").child(format!("{from} → {to}")))
                .child(div().w(px(170.)).text_color(rgb(if ahead > 0 { theme::TEXT } else { theme::MUTED })).child(match ahead {
                    0 => "nothing to promote".to_string(),
                    1 => "1 commit to promote".to_string(),
                    n => format!("{n} commits to promote"),
                }))
                .when(ahead > 0 && !confirming, |d| {
                    d.child(button(SharedString::from(format!("promote-{from}-{to}")), format!("Merge into {to}"), theme::INPUT_BG).on_click(
                        cx.listener(move |this, _, _, cx| this.promote(project_id, f.clone(), t.clone(), cx)),
                    ))
                })
                .when(confirming, |d| {
                    let (f, t) = (from.clone(), to.clone());
                    d.child(div().text_color(rgb(theme::WARN)).child(format!("Merge {from} into {to}? This is your top branch.")))
                        .child(button("promote-confirm", format!("Yes, merge into {to}"), theme::DENY_BG).on_click(cx.listener(
                            move |this, _, _, cx| this.promote(project_id, f.clone(), t.clone(), cx),
                        )))
                        .child(button("promote-cancel", "Cancel", theme::INPUT_BG).on_click(cx.listener(|this, _, _, cx| {
                            this.promote_confirm = None;
                            cx.notify();
                        })))
                })
        });

        let remote_line = match (&remote, info.and_then(|i| i.state.remote_url.as_ref())) {
            (Some(r), _) => format!(
                "origin: {}/{}{}",
                r.host,
                r.slug,
                match r.provider {
                    Some(p) => format!(" · {}s via {}", p.request_name(), if p == hosting::Provider::GitHub { "gh" } else { "glab" }),
                    None => " · not GitHub/GitLab: push only".into(),
                }
            ),
            (None, Some(url)) => format!("origin: {url}"),
            (None, None) => "No remote (origin): pushing and merge requests are off for this project.".into(),
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(rgb(theme::PANEL))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child("BRANCHES · CLICK A RULE TO CHANGE IT"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .when(chain.len() == 1, |d| {
                                d.child(button("setup-branches", "Set up develop and acceptance", theme::ALLOW_BG).on_click(cx.listener(
                                    move |this, _, _, cx| this.setup_branches(project_id, cx),
                                )))
                            })
                            .child(
                                button("refresh-branches", if info.is_some_and(|i| i.loading) { "Refreshing…" } else { "Refresh" }, theme::INPUT_BG)
                                    .on_click(cx.listener(move |this, _, _, cx| this.refresh_branches(project_id, cx))),
                            ),
                    ),
            )
            .child(flow)
            .children(hops)
            .when(chain.len() == 1, |d| {
                d.child(div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(format!(
                    "Only {} exists, so features merge straight into it. Set up develop and acceptance to use your develop → acceptance → main flow.",
                    chain.first().cloned().unwrap_or_default()
                )))
            })
            .child(div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(remote_line))
            .into_any_element()
    }

    pub(crate) fn render_merge_card(&self, sub_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let target = self.merge_target(sub_id)?;
        let policy = self.policy(sub.project_id, &target);
        let state = self.merges.get(&sub_id);
        // Before this run touched it, the last merge comes from the database.
        let merged = state.and_then(|s| s.merged.clone()).or_else(|| {
            self.store
                .setting(&format!("sub:{sub_id}:merged"))
                .and_then(|v| v.split_once(' ').map(|(t, s)| (t.to_string(), s.to_string())))
        });
        let merged = merged.as_ref();
        let busy = state.is_some_and(|s| s.busy);
        let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        let remote = self.remote(sub.project_id);
        let provider = remote.as_ref().and_then(|r| r.provider);
        // Any origin can be pushed to; only GitHub/GitLab get merge/pull requests.
        let has_origin = self.branch_info.get(&sub.project_id).is_some_and(|i| i.state.remote_url.is_some());
        let confidence = self.sub_confidence(sub_id);
        let conflict = state.map(|s| s.conflict.clone()).unwrap_or_default();

        let status = match (merged, ahead) {
            (Some((t, sha)), Some(0) | None) if !sha.is_empty() => format!("Merged into {t} ({sha})."),
            (Some((t, _)), Some(0) | None) => format!("Already in {t}."),
            (_, Some(0)) => "Nothing to merge yet.".to_string(),
            (_, Some(n)) => format!("{n} commit{} ready to merge.", if n == 1 { "" } else { "s" }),
            (_, None) => "Checking…".to_string(),
        };

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(theme::BORDER))
                .text_size(px(13.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child("MERGE"))
                        .child(div().font_family("Consolas").child(format!("{} → {target}", sub.branch)))
                        .child(
                            div()
                                .px_1p5()
                                .rounded_sm()
                                .border_1()
                                .border_color(rgb(policy_color(policy)))
                                .text_color(rgb(policy_color(policy)))
                                .text_size(px(11.))
                                .child(policy.short()),
                        )
                        .children(confidence.map(|(score, from)| {
                            div().text_color(rgb(score_color(score))).child(format!("confidence {score}/5 ({from})"))
                        }))
                        .child(div().text_color(rgb(theme::MUTED)).child(status)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(!busy && ahead.unwrap_or(0) > 0, |d| {
                            d.child(button("merge-sub", format!("Merge into {target}"), theme::ALLOW_BG).on_click(cx.listener(
                                move |this, _, _, cx| this.merge_subproject(sub_id, cx),
                            )))
                        })
                        .when(!busy && !conflict.is_empty(), |d| {
                            d.child(button("resolve", "Resolve conflicts with an agent", theme::INPUT_BG).on_click(cx.listener(
                                move |this, _, _, cx| this.resolve_conflicts(sub_id, cx),
                            )))
                        })
                        .when(!busy && has_origin, |d| {
                            d.child(button("push-sub", "Push branch", theme::INPUT_BG).on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, false, cx))))
                        })
                        .when(!busy && provider.is_some() && state.and_then(|s| s.request.as_ref()).is_none(), |d| {
                            let name = provider.map(|p| p.request_name()).unwrap_or("request");
                            d.child(button("open-request", format!("Open {name}"), theme::INPUT_BG).on_click(cx.listener(
                                move |this, _, _, cx| this.push_sub(sub_id, true, cx),
                            )))
                        })
                        .when(!busy, |d| {
                            d.child(button("cleanup", "Clean up worktrees", theme::INPUT_BG).on_click(cx.listener(
                                move |this, _, _, cx| this.cleanup_worktrees(sub_id, cx),
                            )))
                        }),
                )
                .when(!conflict.is_empty(), |d| {
                    d.child(div().text_color(rgb(theme::WARN)).child(format!("Conflicts in: {}", conflict.join(", "))))
                })
                .children(state.and_then(|s| s.request.as_ref()).map(|r| {
                    let url = r.url.clone();
                    div()
                        .id("request-link")
                        .text_color(rgb(theme::ACCENT))
                        .cursor_pointer()
                        .child(format!("#{} · {} · {}", r.number, r.state, r.url))
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                }))
                .children(state.and_then(|s| s.message.as_ref()).map(|(tone, text)| {
                    div().text_color(rgb(tone_color(*tone))).child(text.clone())
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn render_credentials_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let status = match self.creds_ok {
            Some(true) => "On: git in WSL uses your Windows logins (Git Credential Manager).",
            Some(false) => "Off: git in WSL has no credentials, so pushing to GitLab/GitHub and cloning private repos will fail.",
            None => "Checking…",
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Git credentials in WSL"))
            .child(div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(status))
            .when(self.creds_ok == Some(false), |d| {
                d.child(div().flex().child(
                    button("use-gcm", "Use my Windows Git credentials in WSL", theme::INPUT_BG)
                        .on_click(cx.listener(|this, _, _, cx| this.enable_windows_credentials(cx))),
                ))
            })
            .into_any_element()
    }
}

fn policy_color(policy: Policy) -> u32 {
    match policy {
        Policy::Auto => theme::GOOD,
        Policy::Ask => theme::WARN,
        Policy::Confirm => theme::BAD,
    }
}
