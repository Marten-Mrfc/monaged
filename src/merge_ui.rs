//! Phase 5: merging with the user's branch rules, promotions along
//! develop → acceptance → main, pushing, GitLab/GitHub requests, and
//! cleaning up worktrees.

use std::collections::HashMap;

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git::{self, BranchState, Cleanup, MergeOutcome, MergeReport};
use crate::hosting::{self, Mergeable, Remote, Request};
use crate::pipeline::{Role, Stage};
use crate::icons::{Icon, icon, icon_sized};
use crate::ui::{self, Kind};
use crate::{AgentSpec, Monaged, theme};

/// How work reaches a branch: merged here and pushed, merged here only, or
/// merged by GitHub or GitLab through a pull/merge request (protected
/// branches, required reviews and checks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Method {
    Push,
    Local,
    Request,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Push => "push",
            Method::Local => "local",
            Method::Request => "request",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "push" => Some(Method::Push),
            "local" => Some(Method::Local),
            "request" => Some(Method::Request),
            _ => None,
        }
    }

    /// As a chip says it.
    pub fn label(self, request_name: &str) -> String {
        match self {
            Method::Push => "here, then push".into(),
            Method::Local => "here only".into(),
            Method::Request => format!("through {request_name}s"),
        }
    }
}

/// How a branch stands against origin's copy, in words: `None` is "not on
/// origin", else (commits to push, commits to pull).
pub(crate) fn sync_words(counts: Option<(usize, usize)>) -> String {
    match counts {
        None => "not on origin".into(),
        Some((0, 0)) => "in sync".into(),
        Some((a, 0)) => format!("{a} to push"),
        Some((0, b)) => format!("{b} to pull"),
        Some((a, b)) => format!("{a} to push · {b} to pull"),
    }
}

/// "1 commit", "3 commits".
pub(crate) fn commits_word(n: usize) -> String {
    format!("{n} commit{}", if n == 1 { "" } else { "s" })
}

/// What a merge through a request came to.
enum RequestOutcome {
    /// Merged on the host; `caught_up` commits came into the local target.
    Merged { number: u64, caught_up: usize },
    /// Open, but the host won't merge it yet (and why).
    Waiting(Mergeable),
}

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
    /// Why the host won't merge the open request yet (from the last try).
    pub waiting: Option<String>,
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

    pub(crate) fn set_policy(&mut self, project_id: i64, target: &str, policy: Policy, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(&format!("project:{project_id}:policy:{target}"), policy.as_str());
        cx.notify();
    }

    /// How merges reach their branch in this project (see `Method`).
    pub(crate) fn method(&self, project_id: i64) -> Method {
        let has_origin = self.branch_info.get(&project_id).is_some_and(|i| i.state.remote_url.is_some());
        let hosted = self.remote(project_id).is_some_and(|r| r.provider.is_some());
        match self.store.setting(&format!("git:method:{project_id}")).as_deref().and_then(Method::parse) {
            Some(Method::Request) if !hosted => if has_origin { Method::Push } else { Method::Local },
            Some(Method::Push) if !has_origin => Method::Local,
            Some(m) => m,
            None if has_origin => Method::Push,
            None => Method::Local,
        }
    }

    pub(crate) fn set_method(&mut self, project_id: i64, method: Method, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(&format!("git:method:{project_id}"), method.as_str());
        cx.notify();
    }

    /// How a local branch stands against origin: `None` when unknown (no
    /// origin, or not read yet), `Some(None)` when origin doesn't have it.
    pub(crate) fn origin_counts(&self, project_id: i64, branch: &str) -> Option<Option<(usize, usize)>> {
        let info = self.branch_info.get(&project_id)?;
        if info.state.remote_url.is_none() || !info.state.has(branch) {
            return None;
        }
        if !info.state.on_origin(branch) {
            return Some(None);
        }
        let theirs = format!("origin/{branch}");
        let ahead = *info.ahead.get(&(branch.to_string(), theirs.clone()))?;
        let behind = *info.ahead.get(&(theirs, branch.to_string()))?;
        Some(Some((ahead, behind)))
    }

    /// The description a pull/merge request opens with: what was built, how
    /// it was checked, and the score, so a reviewer starts informed.
    pub(crate) fn request_body(&self, sub_id: i64) -> String {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return String::new() };
        let p = self.pipelines.get(&sub_id);
        let mut out = String::new();
        let summary = p.and_then(|p| p.plan.as_ref()).and_then(|plan| plan["summary"].as_str().map(str::to_owned));
        out.push_str(&summary.or_else(|| p.map(|p| p.request.clone())).unwrap_or_else(|| sub.name.clone()));
        out.push('\n');
        let agents: Vec<_> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && matches!(a.role(), Role::Builder(_) | Role::Integrator | Role::Free))
            .filter_map(|a| a.summary.as_ref().map(|s| (a.record.name.clone(), s.text.clone())))
            .collect();
        if !agents.is_empty() {
            out.push_str("\n## What changed\n\n");
            for (name, text) in agents {
                out.push_str(&format!("- **{name}**: {}\n", text.replace('\n', " ")));
            }
        }
        let reviewed = self
            .agents
            .iter()
            .rev()
            .find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() == Role::Integrator)
            .or_else(|| self.agents.iter().find(|a| a.record.subproject_id == sub_id && a.confidence.is_some()))
            .and_then(|a| a.confidence.as_ref());
        if let Some(c) = reviewed {
            out.push_str(&format!("\n## Checks\n\n{}\n", c.checks.lines().next().unwrap_or_default()));
            out.push_str(&format!("\n## Confidence: {}/5\n\n", c.score));
            for reason in c.reasons.iter().take(6) {
                out.push_str(&format!("- {reason}\n"));
            }
            for issue in c.issues.iter().take(8) {
                out.push_str(&format!("- Open finding [{}] {}: {}\n", issue.severity, issue.file, issue.problem));
            }
        }
        out.push_str("\n---\nOpened by Monaged.");
        crate::export::mask_all(&out)
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
                        // And the other way: what the target has that a feature doesn't (it moved on).
                        pairs.extend(features.iter().filter(|f| state.has(f)).map(|f| (first.clone(), f.clone())));
                    }
                    // Each branch against origin's copy (as of the last fetch): to push, to pull.
                    for branch in chain.iter().chain(features.iter()).filter(|b| state.has(b) && state.on_origin(b)) {
                        let theirs = format!("origin/{branch}");
                        pairs.push((branch.clone(), theirs.clone()));
                        pairs.push((theirs, branch.clone()));
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

    /// Brings the target's newer commits into the feature branch (after the
    /// target caught up with origin), so conflicts show up now, not at merge time.
    pub(crate) fn update_from_target(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let Some(target) = self.merge_target(sub_id) else { return };
        let state = self.merge_state(sub_id);
        state.busy = true;
        state.conflict.clear();
        state.message = Some((Tone::Info, format!("Bringing {target} into {}…", sub.branch)));
        cx.notify();
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
                let (tone, text, conflict) = match result {
                    Ok(MergeReport { outcome: MergeOutcome::Merged(sha), .. }) => (Tone::Info, format!("{} has the latest of {target} now ({sha}).", sub.branch), Vec::new()),
                    Ok(MergeReport { outcome: MergeOutcome::AlreadyMerged, .. }) => (Tone::Info, format!("{} already has everything from {target}.", sub.branch), Vec::new()),
                    Ok(MergeReport { outcome: MergeOutcome::Conflict(files), .. }) => (
                        Tone::Warn,
                        format!("{target} conflicts with {} in {}; nothing was changed. An agent can resolve them.", sub.branch, files_word(files.len())),
                        files,
                    ),
                    Ok(MergeReport { outcome: MergeOutcome::Blocked(why), .. }) => (Tone::Error, format!("Didn't update: {why}"), Vec::new()),
                    Err(err) => (Tone::Error, format!("Couldn't update: {err:#}"), Vec::new()),
                };
                let _ = this.store.log_event(sub_id, None, if conflict.is_empty() { "merge" } else { "conflict" }, &text);
                let state = this.merge_state(sub_id);
                state.busy = false;
                state.conflict = conflict;
                state.message = Some((tone, text));
                this.refresh_branches(sub.project_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        let method = self.method(project_id);
        if method == Method::Request
            && let Some(remote) = self.remote(project_id).filter(|r| r.provider.is_some())
        {
            return self.merge_through_request(project_id, remote, source, target, message, sub_id, cx);
        }
        if let Some(sub_id) = sub_id {
            let state = self.merge_state(sub_id);
            state.busy = true;
            state.conflict.clear();
            state.message = Some((Tone::Info, format!("Merging into {target}…")));
        }
        self.progress("merge", format!("Merging {source} into {target}…"), cx);
        cx.notify();
        let distro = self.config.distro.clone();
        let push = method == Method::Push;
        cx.spawn(async move |view, cx| {
            let (s, t) = (source.clone(), target.clone());
            let report = cx.background_spawn(async move { git::merge_branch(&distro, &project.path, &s, &t, &message, push) }).await;
            view.update(cx, |this, cx| {
                let caught = report.as_ref().map(|r| r.caught_up).unwrap_or(0);
                let caught_note = if caught > 0 { format!(" ({target} first caught up with {caught} commit{} from origin.)", if caught == 1 { "" } else { "s" }) } else { String::new() };
                let pushed = report.as_ref().ok().and_then(|r| r.pushed.clone());
                let (mut tone, mut text, merged_sha, conflict) = match report.map(|r| r.outcome) {
                    Ok(MergeOutcome::Merged(sha)) => (Tone::Info, format!("Merged {source} into {target} ({sha})."), Some(sha), Vec::new()),
                    Ok(MergeOutcome::AlreadyMerged) => (Tone::Info, format!("{target} already contains everything from {source}."), Some(String::new()), Vec::new()),
                    Ok(MergeOutcome::Conflict(files)) => (
                        Tone::Warn,
                        format!("Merge conflicts with {target} in {}; nothing was changed.", files_word(files.len())),
                        None,
                        files,
                    ),
                    Ok(MergeOutcome::Blocked(why)) => (Tone::Error, format!("Didn't merge: {why}"), None, Vec::new()),
                    Err(err) => (Tone::Error, format!("Merge failed: {err:#}"), None, Vec::new()),
                };
                text.push_str(&caught_note);
                match &pushed {
                    Some(Ok(())) => text.push_str(&format!(" Pushed {target} to origin.")),
                    Some(Err(why)) => {
                        tone = Tone::Warn;
                        let protected = why.contains("protected") || why.contains("declined") || why.contains("GH006");
                        text.push_str(&format!(
                            " Pushing {target} failed: {why}{}",
                            if protected { " The branch looks protected: switch the project to merging through requests (Project overview → Branches)." } else { " Push it from the Git tab." }
                        ));
                    }
                    None => {}
                }
                this.finish_merge(project_id, sub_id, &target, tone, text, merged_sha, conflict, cx);
            })
            .ok();
        })
        .detach();
    }

    /// The end of any merge: sound, the event, the state, the queue, the branches.
    #[allow(clippy::too_many_arguments)]
    fn finish_merge(&mut self, project_id: i64, sub_id: Option<i64>, target: &str, tone: Tone, text: String, merged_sha: Option<String>, conflict: Vec<String>, cx: &mut Context<Self>) {
        // A merge that did something rises; one that couldn't, falls.
        match (&merged_sha, tone) {
            (Some(sha), _) if !sha.is_empty() => self.chime(crate::sound::Cue::Success),
            (None, Tone::Warn | Tone::Error) => self.chime(crate::sound::Cue::Error),
            _ => {}
        }
        if let Some(sub_id) = sub_id {
            if let Some(sha) = &merged_sha {
                let _ = self.store.set_setting(&format!("sub:{sub_id}:merged"), &format!("{target} {sha}"));
                if let Some(p) = self.pipelines.get_mut(&sub_id) {
                    p.note = Some(text.clone());
                }
                if self.pipelines.contains_key(&sub_id) {
                    let _ = self.store.save_pipeline(&self.pipelines[&sub_id]);
                }
            }
            let kind = if merged_sha.is_some() { "merge" } else if !conflict.is_empty() { "conflict" } else { "failed" };
            let _ = self.store.log_event(sub_id, None, kind, &text);
            let state = self.merge_state(sub_id);
            state.busy = false;
            state.conflict = conflict;
            state.message = Some((tone, text.clone()));
            if let Some(sha) = merged_sha {
                state.merged = Some((target.to_string(), sha));
            }
        }
        self.settle("merge", tone, text, cx);
        self.advance_merge_queue(cx);
        self.refresh_branches(project_id, cx);
        if let Some(sub_id) = sub_id {
            self.refresh_request(sub_id, cx);
        }
        cx.notify();
    }

    /// Merging through GitHub or GitLab: push the branch, open (or reuse) its
    /// request, and merge it there if the host allows it now; then catch the
    /// local target up. Below the quality bar a new request opens as a draft.
    #[allow(clippy::too_many_arguments)]
    fn merge_through_request(&mut self, project_id: i64, remote: Remote, source: String, target: String, message: String, sub_id: Option<i64>, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        let name = remote.provider.map(|p| p.request_name()).unwrap_or("request");
        let below_bar = sub_id.and_then(|s| self.sub_confidence(s)).is_some_and(|(score, _)| score < self.min_score());
        let (title, body) = match sub_id {
            Some(s) => (
                self.pipelines.get(&s).and_then(|p| p.plan.as_ref()).map(crate::pipeline::plan_title).unwrap_or_else(|| self.sub_name(s)),
                self.request_body(s),
            ),
            None => (message.clone(), format!("{message}.\n\n---\nOpened by Monaged.")),
        };
        if let Some(sub_id) = sub_id {
            let state = self.merge_state(sub_id);
            state.busy = true;
            state.conflict.clear();
            state.message = Some((Tone::Info, format!("Pushing {source} and checking its {name}…")));
            state.waiting = None;
        }
        self.progress("merge", format!("Merging {source} into {target} through a {name}…"), cx);
        cx.notify();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let (s, t, repo, r) = (source.clone(), target.clone(), project.path.clone(), remote.clone());
            let result = cx
                .background_spawn(async move {
                    git::push_branch(&distro, &repo, &s)?;
                    let existing = hosting::request_status(&r, &s)?.filter(|q| q.is_open());
                    if existing.is_none() {
                        hosting::create_request(&r, &s, &t, &title, &body, below_bar)?;
                    }
                    let request = hosting::request_status(&r, &s)?.filter(|q| q.is_open()).ok_or_else(|| anyhow::anyhow!("the {name} didn't show up on {}", r.host))?;
                    // At the bar now: a draft from earlier is ready.
                    if request.draft && !below_bar {
                        hosting::mark_ready(&r, &request)?;
                    }
                    // The host works out mergeability in the background; ask a few times.
                    let mut mergeable = Mergeable::Unknown(String::new());
                    for attempt in 0..4 {
                        mergeable = hosting::request_mergeable(&r, request.number)?;
                        if !matches!(mergeable, Mergeable::Unknown(_)) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(2 + attempt));
                    }
                    if mergeable != Mergeable::Ready || below_bar {
                        return anyhow::Ok((request, RequestOutcome::Waiting(if below_bar { Mergeable::Draft } else { mergeable })));
                    }
                    hosting::merge_request(&r, request.number, &message)?;
                    let caught_up = git::catch_up(&distro, &repo, &t).unwrap_or(0);
                    Ok((request.clone(), RequestOutcome::Merged { number: request.number, caught_up }))
                })
                .await;
            view.update(cx, |this, cx| {
                let (tone, text, merged) = match &result {
                    Ok((_, RequestOutcome::Merged { number, caught_up })) => (
                        Tone::Info,
                        format!(
                            "Merged {name} #{number} into {target} on {}{}.",
                            remote.host,
                            if *caught_up > 0 { format!("; {target} here caught up ({caught_up} commit{})", if *caught_up == 1 { "" } else { "s" }) } else { String::new() }
                        ),
                        Some(format!("#{number}")),
                    ),
                    Ok((request, RequestOutcome::Waiting(why))) => {
                        let why = if below_bar && matches!(why, Mergeable::Draft) {
                            format!("is a draft until the score reaches {}/5", this.min_score())
                        } else {
                            why.describe()
                        };
                        (Tone::Info, format!("{name} #{} is open: it {why}. Monaged merges it once it can (or merge it there).", request.number), None)
                    }
                    Err(err) => {
                        let text = format!("{err:#}");
                        let hint = if crate::git_auth::looks_like_auth_failure(&text) { " Sign in under Settings → Git accounts." } else { "" };
                        (Tone::Error, format!("Couldn't merge through a {name}: {text}{hint}"), None)
                    }
                };
                if let (Some(sub_id), Ok((request, outcome))) = (sub_id, &result) {
                    let min = this.min_score();
                    let state = this.merge_state(sub_id);
                    state.request = Some(request.clone());
                    state.waiting = match outcome {
                        RequestOutcome::Waiting(Mergeable::Draft) if below_bar => Some(format!("is a draft until the score reaches {min}/5")),
                        RequestOutcome::Waiting(why) => Some(why.describe()),
                        RequestOutcome::Merged { .. } => None,
                    };
                }
                this.finish_merge(project_id, sub_id, &target, tone, text, merged, Vec::new(), cx);
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
        let body = self.request_body(sub_id);
        // Below the quality bar it opens as a draft: not ready for anyone to merge.
        let draft = self.sub_confidence(sub_id).is_none_or(|(score, _)| score < self.min_score());
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
                    Ok(Some(hosting::create_request(&remote, &branch, &target, &title, &body, draft)?))
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
                    let state = this.merge_state(sub_id);
                    if !request.as_ref().is_some_and(|r| r.is_open()) {
                        state.waiting = None;
                    }
                    state.request = request;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Pushes one of the project's branches (develop after merging here, say).
    pub(crate) fn push_named_branch(&mut self, project_id: i64, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone()) else { return };
        self.progress("push", format!("Pushing {branch}…"), cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let b = branch.clone();
            let result = cx.background_spawn(async move { git::push_branch(&distro, &repo, &b) }).await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(_) => {
                        this.chime(crate::sound::Cue::Success);
                        this.settle("push", Tone::Info, format!("Pushed {branch} to origin."), cx);
                    }
                    Err(err) => {
                        let text = format!("{err:#}");
                        let hint = if crate::git_auth::looks_like_auth_failure(&text) { " Sign in under Settings → Git accounts." } else { "" };
                        this.chime(crate::sound::Cue::Error);
                        this.settle("push", Tone::Error, format!("Couldn't push {branch}: {text}{hint}"), cx);
                    }
                }
                this.refresh_branches(project_id, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Brings one of the project's branches up to origin's copy (fast-forward only).
    pub(crate) fn pull_named_branch(&mut self, project_id: i64, branch: String, cx: &mut Context<Self>) {
        let Some(repo) = self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone()) else { return };
        self.progress("pull", format!("Pulling {branch}…"), cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let b = branch.clone();
            let result = cx.background_spawn(async move { crate::git_ops::pull_ff(&distro, &repo, &b) }).await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(sha) => this.settle("pull", Tone::Info, format!("{branch} is now at {sha}, like origin."), cx),
                    Err(err) => this.settle("pull", Tone::Error, format!("Couldn't pull {branch}: {err:#}"), cx),
                }
                this.refresh_branches(project_id, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Fetches the project from origin, then reads its branches (and open Git
    /// tabs) again. `quiet` keeps a failed background fetch out of your way.
    pub(crate) fn fetch_project(&mut self, project_id: i64, quiet: bool, cx: &mut Context<Self>) {
        let Some(path) = self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone()) else { return };
        self.fetched.insert(project_id, std::time::Instant::now());
        self.branch_info.entry(project_id).or_default().loading = true;
        cx.notify();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { crate::git_ops::fetch(&distro, &path) }).await;
            view.update(cx, |this, cx| {
                if let Err(err) = result
                    && !quiet
                {
                    let text = format!("{err:#}");
                    let hint = if crate::git_auth::looks_like_auth_failure(&text) { " Sign in under Settings → Git accounts." } else { "" };
                    this.toast_error(format!("Couldn't fetch from origin: {text}{hint}"), cx);
                }
                let subs: Vec<i64> = this.git_panels.keys().copied().filter(|id| this.subprojects.iter().any(|s| s.id == *id && s.project_id == project_id)).collect();
                for sub in subs {
                    this.refresh_git(sub, cx);
                }
                this.refresh_branches(project_id, cx);
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

    /// When merges into `target` happen; opens a menu of the choices.
    fn policy_chip(&self, project_id: i64, index: usize, target: &str, cx: &mut Context<Self>) -> AnyElement {
        let policy = self.policy(project_id, target);
        let min = self.min_score();
        ui::menu_chip(SharedString::from(format!("policy-{target}")), policy.short(min))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.open_menu(crate::menu::MenuTarget::MergeRule(project_id, index), event.position(), window, cx)
            }))
            .into_any_element()
    }

    /// How merges reach their branch; opens a menu of the choices.
    fn method_chip(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let name = self.remote(project_id).and_then(|r| r.provider).map(|p| p.request_name()).unwrap_or("request");
        let method = self.method(project_id);
        ui::menu_chip(("method-chip", project_id as usize), method.label(name))
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.open_menu(crate::menu::MenuTarget::MergeMethod(project_id), event.position(), window, cx)
            }))
            .into_any_element()
    }

    /// A branch against origin, small: "in sync", or "3 to push" you can click.
    fn sync_chip(&self, project_id: i64, branch: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let counts = self.origin_counts(project_id, branch)?;
        let text = sync_words(counts);
        let quiet = |text: String| div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_4())).child(text).into_any_element();
        let (glyph, tip, push) = match counts {
            Some((0, 0)) => return Some(quiet(text)),
            Some((a, b)) if a > 0 && b > 0 => {
                return Some(
                    div()
                        .id(SharedString::from(format!("sync-{branch}")))
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .text_color(rgb(theme::text_3()))
                        .tooltip(crate::shell::tip("Both moved: Pull (merge) in a subproject's Git tab brings them together"))
                        .child(text)
                        .into_any_element(),
                );
            }
            None => (Icon::ArrowUp, format!("Push {branch}: origin doesn't have it yet"), true),
            Some((a, _)) if a > 0 => (Icon::ArrowUp, format!("Push {branch}: {} origin doesn't have", commits_word(a)), true),
            Some((_, b)) => (Icon::ArrowDown, format!("Pull {branch}: origin has {} more (a fast-forward)", commits_word(b)), false),
        };
        let b = branch.to_string();
        Some(
            div()
                .id(SharedString::from(format!("sync-{branch}")))
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(5.))
                .mx(px(-5.))
                .h(px(20.))
                .rounded(px(4.))
                .font_family(theme::MONO)
                .text_size(px(11.))
                .text_color(rgb(theme::text_2()))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
                .tooltip(crate::shell::tip_text(tip))
                .child(icon_sized(glyph, 11.).text_color(rgb(theme::text_3())))
                .child(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if push {
                        this.push_named_branch(project_id, b.clone(), cx)
                    } else {
                        this.pull_named_branch(project_id, b.clone(), cx)
                    }
                }))
                .into_any_element(),
        )
    }

    /// The project's branch flow: feature/* → develop → acceptance → main, the
    /// rule on each hop, each branch against origin, what's waiting to be
    /// promoted, and how merges reach origin.
    pub(crate) fn render_branch_panel(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let chain = self.chain(project_id);
        let info = self.branch_info.get(&project_id);
        let remote = self.remote(project_id);
        let has_origin = info.is_some_and(|i| i.state.remote_url.is_some());
        let feature_count = self.subprojects.iter().filter(|s| s.project_id == project_id).count();

        let node = |name: String, note: String, sync: Option<AnyElement>| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().font_family(theme::MONO).text_size(px(12.5)).text_color(rgb(theme::text())).child(name))
                .child(ui::mono(note))
                .children(sync)
        };
        let mut flow = div().flex().items_start().gap(px(10.)).child(node(
            "feature/*".into(),
            format!("{feature_count} subproject{}", if feature_count == 1 { "" } else { "s" }),
            None,
        ));
        let mut previous = "feature/*".to_string();
        for (index, target) in chain.iter().enumerate() {
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
                        .child(self.policy_chip(project_id, index, target, cx))
                        .child(div().w_full().h(px(1.)).bg(rgb(theme::line_strong()))),
                )
                .child(node(target.clone(), note, self.sync_chip(project_id, target, cx)));
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
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(rgb(theme::line()))
                    .child(icon(Icon::Merge).text_color(rgb(theme::text_3())))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(13.)).child(format!("{} from {from} ready for {to}", commits_word(ahead))))
                    .child(ui::button(SharedString::from(format!("promote-{from}-{to}")), Kind::Secondary, None, format!("Promote to {to}")).on_click(
                        cx.listener(move |this, _, window, cx| this.promote(project_id, f.clone(), t.clone(), window, cx)),
                    ))
            })
        });

        let origin_line = match (&remote, info.and_then(|i| i.state.remote_url.as_ref())) {
            (Some(r), _) => format!(
                "{}/{}{}",
                r.host,
                r.slug,
                match r.provider {
                    Some(p) => match self
                        .git_auth
                        .accounts
                        .iter()
                        .find(|a| a.host == crate::git_auth::normalize_host(&r.host) && r.login.as_ref().map_or(a.default, |l| *l == a.login))
                    {
                        Some(a) => format!(" · as {}", a.login),
                        None => format!(" · {}s via {}", p.request_name(), if p == hosting::Provider::GitHub { "gh" } else { "glab" }),
                    },
                    None => String::new(),
                }
            ),
            (None, Some(url)) => url.clone(),
            (None, None) => "No origin remote: merges stay on this PC".into(),
        };
        let fetched = self.fetched.get(&project_id).map(|at| format!("fetched {} ago", crate::git_ui::humanize(at.elapsed())));
        let loading = info.is_some_and(|i| i.loading);

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
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .pt(px(12.))
                        .border_t_1()
                        .border_color(rgb(theme::line()))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(12.5)).text_color(rgb(theme::text_3())).child(format!(
                            "Only {} exists, so features merge straight into it. Add develop and acceptance for a develop → acceptance → main flow.",
                            chain.first().cloned().unwrap_or_default()
                        )))
                        .child(ui::button("setup-branches", Kind::Secondary, None, "Set up develop and acceptance").on_click(cx.listener(
                            move |this, _, _, cx| this.setup_branches(project_id, cx),
                        ))),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(rgb(theme::line()))
                    .child(icon_sized(Icon::Branch, 13.).flex_none().text_color(rgb(theme::text_3())))
                    .child(div().flex_none().text_size(px(12.5)).text_color(rgb(theme::text_2())).child("origin"))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(ui::mono(origin_line)))
                    .child(div().flex_none().text_size(px(12.5)).text_color(rgb(theme::text_3())).child("Merges"))
                    .child(self.method_chip(project_id, cx))
                    .when(has_origin, |d| {
                        d.child(div().flex_none().pl(px(6.)).child(ui::mono(if loading { "reading…".to_string() } else { fetched.unwrap_or_default() })))
                            .child(
                                ui::icon_button("fetch-branches", Icon::Refresh)
                                    .tooltip(crate::shell::tip("Fetch from origin and read the branches again"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.fetch_project(project_id, false, cx))),
                            )
                    })
                    .when(!has_origin, |d| {
                        d.child(
                            ui::icon_button("refresh-branches", Icon::Refresh)
                                .tooltip(crate::shell::tip("Read the branches again"))
                                .on_click(cx.listener(move |this, _, _, cx| this.refresh_branches(project_id, cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The inspector's Merge tab for a subproject: where its work stands, the
    /// one thing to do next, the other things you can do, and the rules.
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
        let chain_index = self.chain(project_id).iter().position(|b| *b == target).unwrap_or(0);
        let state = self.merges.get(&sub_id);
        // Before this run touched it, the last merge comes from the database.
        let merged = state.and_then(|s| s.merged.clone()).or_else(|| {
            self.store
                .setting(&format!("sub:{sub_id}:merged"))
                .and_then(|v| v.split_once(' ').map(|(t, s)| (t.to_string(), s.to_string())))
        });
        let busy = state.is_some_and(|s| s.busy);
        let info = self.branch_info.get(&project_id);
        let ahead = info.and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        // What the target has that this branch doesn't (it moved on).
        let behind = info.and_then(|i| i.ahead.get(&(target.clone(), sub.branch.clone()))).copied().unwrap_or(0);
        let method = self.method(project_id);
        let provider = self.remote(project_id).and_then(|r| r.provider);
        let name = provider.map(|p| p.request_name()).unwrap_or("request");
        let has_origin = info.is_some_and(|i| i.state.remote_url.is_some());
        let sync = self.origin_counts(project_id, &sub.branch);
        let confidence = self.sub_confidence(sub_id);
        let min = self.min_score();
        let below_bar = confidence.as_ref().is_some_and(|(score, _)| *score < min);
        let conflict = state.map(|s| s.conflict.clone()).unwrap_or_default();
        let request = state.and_then(|s| s.request.clone());
        let open_request = request.clone().filter(|r| r.is_open());
        let waiting = state.and_then(|s| s.waiting.clone()).filter(|w| !w.is_empty());
        let ci = self.ci.get(&sub_id).and_then(|(_, c)| c.clone()).filter(|c| c.state != "none");

        // Where it stands, in one line.
        let (headline, attention) = if !conflict.is_empty() {
            (format!("Conflicts with {target} in {}", files_word(conflict.len())), true)
        } else {
            match ahead {
                None => ("Reading the branches…".to_string(), false),
                Some(0) => match &merged {
                    Some((t, sha)) if !sha.is_empty() => (format!("Merged into {t} at {sha}"), false),
                    Some((t, _)) => (format!("Everything is in {t}"), false),
                    None => ("Nothing to merge yet".to_string(), false),
                },
                Some(n) => match (&open_request, &waiting) {
                    (Some(r), Some(why)) => (format!("{} · {name} #{} {why}", commits_word(n), r.number), false),
                    _ if below_bar => (format!("{} · confidence below the {min}/5 bar", commits_word(n)), true),
                    _ => (format!("{} ready to merge", commits_word(n)), false),
                },
            }
        };
        let header = ui::panel_section("Merge")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .font_family(theme::MONO)
                    .text_size(px(12.))
                    .text_color(rgb(theme::text_2()))
                    .child(div().min_w(px(0.)).truncate().child(sub.branch.clone()))
                    .child(icon_sized(Icon::ChevronRight, 12.).flex_none().text_color(rgb(theme::text_3())))
                    .child(div().flex_none().text_color(rgb(theme::text())).child(target.clone())),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .text_color(rgb(if attention { theme::red_text() } else { theme::text() }))
                    .child(headline),
            )
            // What the last action came to (a conflict says so in its own section).
            .children(
                state
                    .and_then(|s| s.message.clone())
                    .filter(|(tone, _)| !busy && !(!conflict.is_empty() && matches!(tone, Tone::Warn)))
                    .map(|(tone, text)| ui::callout(tone, text).text_size(px(12.5)).line_height(px(18.))),
            );

        // The one thing to do next.
        let primary: Option<AnyElement> = if busy {
            let doing = state
                .and_then(|s| s.message.as_ref())
                .filter(|(tone, _)| matches!(tone, Tone::Info))
                .map(|(_, text)| text.clone())
                .unwrap_or_else(|| "Working…".into());
            Some(ui::busy_button("merge-busy", Kind::Primary, doing).justify_center().into_any_element())
        } else if !conflict.is_empty() {
            Some(
                ui::button("resolve", Kind::Primary, Some(Icon::Agent), "Resolve conflicts with an agent")
                    .justify_center()
                    .tooltip(crate::shell::tip_text(format!("An agent merges {target} into {} and settles the conflicts, keeping both sides' intent.", sub.branch)))
                    .on_click(cx.listener(move |this, _, _, cx| this.resolve_conflicts(sub_id, cx)))
                    .into_any_element(),
            )
        } else if ahead.unwrap_or(0) > 0 {
            let label = match (method, &open_request) {
                (Method::Request, Some(r)) => format!("Merge {name} #{}", r.number),
                (Method::Request, None) => format!("Merge through a {name}"),
                _ => format!("Merge into {target}"),
            };
            Some(
                ui::button("merge-sub", Kind::Primary, Some(Icon::Merge), label)
                    .justify_center()
                    .when(below_bar, |b| b.tooltip(crate::shell::tip_text(format!("Confidence is below {min}/5: merging now is your call."))))
                    .on_click(cx.listener(move |this, _, _, cx| this.merge_subproject(sub_id, cx)))
                    .into_any_element(),
            )
        } else {
            None
        };

        // Everything else you can do here, with what it would act on.
        let mut rows: Vec<AnyElement> = Vec::new();
        if !busy {
            if !conflict.is_empty() {
                rows.push(
                    ui::action_row("conflict-sides", Icon::Branch, "Choose sides yourself", Some("Git tab".into()))
                        .on_click(cx.listener(move |this, _, _, cx| this.show_inspector(sub_id, crate::shell::InspectorTab::Git, cx)))
                        .into_any_element(),
                );
                if ahead.unwrap_or(0) > 0 {
                    rows.push(
                        ui::action_row("merge-again", Icon::Merge, "Merge again", Some("once they're resolved".into()))
                            .on_click(cx.listener(move |this, _, _, cx| this.merge_subproject(sub_id, cx)))
                            .into_any_element(),
                    );
                }
            } else if behind > 0 {
                rows.push(
                    ui::action_row("update-from-target", Icon::ArrowDown, format!("Update from {target}"), Some(format!("{behind} new commit{}", if behind == 1 { "" } else { "s" })))
                        .tooltip(crate::shell::tip_text(format!(
                            "Merge {target}'s new commits into this branch (after catching {target} up with origin), so conflicts show up now."
                        )))
                        .on_click(cx.listener(move |this, _, _, cx| this.update_from_target(sub_id, cx)))
                        .into_any_element(),
                );
            }
            if has_origin && matches!(sync, Some(None) | Some(Some((1.., _)))) {
                rows.push(
                    ui::action_row("push-sub", Icon::Push, "Push branch", sync.map(sync_words))
                        .on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, false, cx)))
                        .into_any_element(),
                );
            }
            match &open_request {
                Some(r) => {
                    let url = r.url.clone();
                    rows.push(
                        ui::action_row("view-request", Icon::External, format!("View {name} #{}", r.number), Some(if r.draft { "draft".into() } else { r.state.clone() }))
                            .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url)))
                            .into_any_element(),
                    );
                }
                None if provider.is_some() && ahead.unwrap_or(0) > 0 => {
                    let draft = confidence.as_ref().is_none_or(|(score, _)| *score < min);
                    rows.push(
                        ui::action_row("open-request", Icon::External, format!("Open a {name}"), Some(if draft { "as a draft".into() } else { "ready for review".into() }))
                            .tooltip(crate::shell::tip_text(format!(
                                "Push the branch and open a {name} into {target}, described from the work. Below {min}/5 it opens as a draft."
                            )))
                            .on_click(cx.listener(move |this, _, _, cx| this.push_sub(sub_id, true, cx)))
                            .into_any_element(),
                    );
                }
                None => {}
            }
            rows.push(
                ui::action_row("cleanup", Icon::Archive, "Clean up worktrees", Some("idle agents".into()))
                    .tooltip(crate::shell::tip("Removes idle agents' worktrees that have no uncommitted work, and branches already in the feature branch"))
                    .on_click(cx.listener(move |this, _, window, cx| this.ask_confirm(crate::feedback::ConfirmAction::CleanupWorktrees(sub_id), window, cx)))
                    .into_any_element(),
            );
        }
        let actions = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(16.))
            .py(px(14.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .children(primary)
            .child(div().flex().flex_col().mx(px(-8.)).children(rows));

        let conflicts = (!conflict.is_empty()).then(|| {
            ui::panel_section("Conflicts")
                .child(ui::meta(format!("{target} and {} both changed:", sub.branch)))
                .child(div().flex().flex_col().gap(px(4.)).children(conflict.iter().map(|f| {
                    ui::one_line(div()).font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::red_text())).child(f.clone())
                })))
        });

        let request_section = request.as_ref().map(|r| {
            let url = r.url.clone();
            let mut section = ui::panel_section(name).child(
                div()
                    .id("request-link")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .hover(|s| s.text_color(rgb(theme::text())))
                    .tooltip(crate::shell::tip("Open it on the host"))
                    .child(div().font_family(theme::MONO).child(format!("#{}", r.number)))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_color(rgb(theme::text_2())).child(if r.draft && r.is_open() { format!("{} · draft", r.state) } else { r.state.clone() }))
                    .child(icon_sized(Icon::External, 13.).text_color(rgb(theme::text_3())))
                    .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url))),
            );
            if let Some(c) = &ci {
                let (glyph, color, text) = match c.state.as_str() {
                    "success" => (Icon::Check, theme::text_2(), "CI passed".to_string()),
                    "failure" => (Icon::Alert, theme::red_text(), "CI failing".to_string()),
                    other => (Icon::Clock, theme::text_3(), format!("CI {other}")),
                };
                let url = c.url.clone();
                section = section.child(
                    div()
                        .id("request-ci")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .text_color(rgb(color))
                        .hover(|s| s.text_color(rgb(theme::text())))
                        .child(icon_sized(glyph, 13.).text_color(rgb(color)))
                        .child(div().flex_1().min_w(px(0.)).truncate().child(if c.name.is_empty() { text } else { format!("{text} · {}", c.name) }))
                        .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url))),
                );
            }
            if let (true, Some(why)) = (r.is_open(), &waiting) {
                section = section.child(ui::meta(format!("It {why}; Monaged merges it once it can.")));
            }
            section
        });

        let kv = |k: &str, v: AnyElement| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .min_h(px(24.))
                .text_size(px(12.5))
                .child(div().flex_none().text_color(rgb(theme::text_3())).child(k.to_string()))
                .child(v)
        };
        let rules = ui::panel_section("How it merges")
            .child(kv("When", self.policy_chip(project_id, chain_index, &target, cx)))
            .child(kv("How", self.method_chip(project_id, cx)))
            .child(kv(
                "Confidence",
                match &confidence {
                    Some((score, from)) => div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::mono(from.clone()))
                        .child(ui::score_meter(*score))
                        .child(div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(if *score < min { theme::red_text() } else { theme::text_2() })).child(format!("{score}/5")))
                        .into_any_element(),
                    None => ui::mono("not reviewed yet").into_any_element(),
                },
            ))
            .when_some(sync.filter(|_| has_origin), |d, counts| d.child(kv("Origin", ui::mono(sync_words(counts)).into_any_element())));

        div()
            .flex()
            .flex_col()
            .child(header)
            .child(actions)
            .children(conflicts)
            .children(request_section)
            .child(rules)
            .into_any_element()
    }
}

/// "1 file", "3 files".
fn files_word(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}
