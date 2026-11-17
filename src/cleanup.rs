//! Cleaning up after finished work, so the host keeps only develop, acceptance
//! and main instead of every feature and agent branch ever pushed:
//!
//! - **Finish and clean up** (a subproject): removes its worktrees, deletes its
//!   branches here and on origin, archives its agents and archives the
//!   subproject itself (it can be restored from the project view).
//! - **Clean up branches** (a project): deletes every branch here and on origin
//!   whose work is already in develop, except the chain (develop, acceptance,
//!   main), origin's default branch and, here, branches still in use.
//!
//! Both work out a plan first, show it, and ask. Only branches whose work is
//! already in develop are deleted (origin's develop for branches on origin),
//! and that is checked again right before each deletion. The rest are kept
//! and listed with why: commits develop doesn't have, or uncommitted changes.

use gpui::{AppContext as _, Context, Window};

use crate::agent::Tone;
use crate::feedback::ConfirmAction;
use crate::wsl::{quote, run};
use crate::Monaged;

/// What a cleanup would do, worked out before asking.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// The branch work has to be in before its branch is deleted (develop).
    pub target: String,
    /// Agent worktrees without uncommitted changes, removed first.
    pub worktrees: Vec<String>,
    /// Branches here whose work is in the target.
    pub local: Vec<String>,
    /// Branches on origin whose work is in origin's target.
    pub remote: Vec<String>,
    /// Left alone, and why.
    pub kept: Vec<(String, String)>,
}

impl Plan {
    pub fn deletes_anything(&self) -> bool {
        !(self.worktrees.is_empty() && self.local.is_empty() && self.remote.is_empty())
    }

    /// The dialog's text: what goes, then what stays.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.remote.is_empty() {
            parts.push(format!("On origin, deletes {}: {}.", count(self.remote.len(), "branch"), self.remote.join(", ")));
        }
        if !self.local.is_empty() {
            parts.push(format!("Here, deletes {}: {}.", count(self.local.len(), "branch"), self.local.join(", ")));
        }
        if !self.worktrees.is_empty() {
            parts.push(format!("Removes {} (none has uncommitted changes).", count(self.worktrees.len(), "worktree")));
        }
        if parts.is_empty() {
            parts.push("No branch or worktree to delete.".into());
        } else {
            parts.push(format!("All of their work is already in {}.", self.target));
        }
        // Branches still in use are one line (Finish their subproject to clean them up); the rest by name.
        let (in_use, other): (Vec<_>, Vec<_>) = self.kept.iter().partition(|(_, why)| why == "in use" || why == "checked out");
        if !other.is_empty() {
            let kept: Vec<String> = other.iter().map(|(b, why)| format!("{b} ({why})")).collect();
            parts.push(format!("Kept: {}.", kept.join(", ")));
        }
        if !in_use.is_empty() {
            parts.push(format!(
                "Also kept: {} still in use by subprojects or agents here (Finish a subproject to clean up its own).",
                count(in_use.len(), "branch")
            ));
        }
        parts.join(" ")
    }
}

fn count(n: usize, word: &str) -> String {
    let plural = if word.ends_with('h') { format!("{word}es") } else { format!("{word}s") };
    format!("{n} {}", if n == 1 { word.to_string() } else { plural })
}

/// Shell for both plans: fetch (pruning what's gone), the target here and on origin.
fn prelude(repo: &str, target: &str) -> String {
    format!(
        "cd {} || exit 1\nt={}\ngit fetch -q --prune origin >/dev/null 2>&1\n\
         rt=; git rev-parse -q --verify \"refs/remotes/origin/$t\" >/dev/null && rt=\"origin/$t\"\n\
         behind() {{ git rev-list --count \"$1..$2\" 2>/dev/null || echo '?'; }}\n",
        quote(repo),
        quote(target)
    )
}

/// The plan for one subproject: each (branch, worktree) it used.
pub fn sub_plan_script(repo: &str, target: &str, items: &[(String, String)]) -> String {
    let mut s = prelude(repo, target);
    s.push_str(
        "check() {\n\
         br=$1; wt=$2\n\
         if [ -n \"$wt\" ] && [ -d \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain 2>/dev/null)\" ]; then echo \"MONAGED_DIRTY $br\"; return; fi\n\
           echo \"MONAGED_WT $wt\"\n\
         fi\n\
         if git show-ref -q --verify \"refs/heads/$br\"; then\n\
           if git merge-base --is-ancestor \"$br\" \"$t\"; then echo \"MONAGED_LOCAL merged $br\"; else echo \"MONAGED_LOCAL $(behind \"$t\" \"$br\") $br\"; fi\n\
         fi\n\
         if git show-ref -q --verify \"refs/remotes/origin/$br\"; then\n\
           if [ -n \"$rt\" ] && git merge-base --is-ancestor \"origin/$br\" \"$rt\"; then echo \"MONAGED_REMOTE merged $br\"; else echo \"MONAGED_REMOTE $(behind \"${rt:-$t}\" \"origin/$br\") $br\"; fi\n\
         fi\n\
         }\n",
    );
    for (branch, worktree) in items {
        s.push_str(&format!("check {} {}\n", quote(branch), quote(worktree)));
    }
    s
}

/// The plan for a whole project: every branch except `never` (the chain and
/// origin's default); `in_use` (subprojects and agents still around) are
/// kept here but may go on origin when merged.
pub fn project_plan_script(repo: &str, target: &str, never: &[String], in_use: &[String]) -> String {
    let mut s = prelude(repo, target);
    s.push_str(&format!(
        "never=\" {} \"\nin_use=\" {} \"\n\
         head=$(git symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null); head=${{head#origin/}}\n\
         [ -n \"$head\" ] && never=\"$never$head \"\n\
         co=\" $(git worktree list --porcelain | awk '/^branch /{{sub(\"refs/heads/\", \"\", $2); print $2}}' | tr '\\n' ' ') \"\n\
         for br in $(git for-each-ref --format='%(refname:short)' refs/heads); do\n\
           case \"$never\" in *\" $br \"*) continue ;; esac\n\
           case \"$in_use\" in *\" $br \"*) echo \"MONAGED_KEEP $br in use\"; continue ;; esac\n\
           case \"$co\" in *\" $br \"*) echo \"MONAGED_KEEP $br checked out\"; continue ;; esac\n\
           if git merge-base --is-ancestor \"$br\" \"$t\"; then echo \"MONAGED_LOCAL merged $br\"; else echo \"MONAGED_LOCAL $(behind \"$t\" \"$br\") $br\"; fi\n\
         done\n\
         for br in $(git for-each-ref --format='%(refname:lstrip=3)' refs/remotes/origin); do\n\
           [ \"$br\" = HEAD ] && continue\n\
           case \"$never\" in *\" $br \"*) continue ;; esac\n\
           if [ -n \"$rt\" ] && git merge-base --is-ancestor \"origin/$br\" \"$rt\"; then echo \"MONAGED_REMOTE merged $br\"; else echo \"MONAGED_REMOTE $(behind \"${{rt:-$t}}\" \"origin/$br\") $br\"; fi\n\
         done\n",
        never.join(" "),
        in_use.join(" ")
    ));
    s
}

/// Reads a plan script's output.
pub fn parse_plan(out: &str, target: &str) -> Plan {
    let mut plan = Plan { target: target.to_string(), ..Default::default() };
    let commits = |n: &str| match n {
        "1" => format!("1 commit {target} doesn't have"),
        n => format!("{n} commits {target} doesn't have"),
    };
    for line in out.lines() {
        let mut words = line.splitn(3, ' ');
        match (words.next(), words.next(), words.next()) {
            (Some("MONAGED_WT"), Some(wt), rest) => plan.worktrees.push(match rest {
                Some(rest) => format!("{wt} {rest}"),
                None => wt.to_string(),
            }),
            (Some("MONAGED_DIRTY"), Some(br), _) => plan.kept.push((br.to_string(), "uncommitted changes in its worktree".into())),
            (Some("MONAGED_KEEP"), Some(br), Some(why)) => plan.kept.push((br.to_string(), why.to_string())),
            (Some("MONAGED_LOCAL"), Some("merged"), Some(br)) => plan.local.push(br.to_string()),
            (Some("MONAGED_LOCAL"), Some(n), Some(br)) => plan.kept.push((br.to_string(), commits(n))),
            (Some("MONAGED_REMOTE"), Some("merged"), Some(br)) => plan.remote.push(br.to_string()),
            (Some("MONAGED_REMOTE"), Some(n), Some(br)) => plan.kept.push((format!("origin/{br}"), commits(n))),
            _ => {}
        }
    }
    plan.kept.dedup();
    plan
}

/// Carries a plan out, checking each branch is in the target again first.
pub fn run_script(repo: &str, plan: &Plan) -> String {
    let list = |items: &[String]| items.iter().map(|i| quote(i)).collect::<Vec<_>>().join(" ");
    let mut s = prelude(repo, &plan.target);
    s.push_str(&format!(
        "for wt in {wts}; do\n\
           if git worktree remove \"$wt\" >/dev/null 2>&1; then echo \"MONAGED_WT_OK $wt\"; else echo \"MONAGED_WT_FAIL $wt\"; fi\n\
         done\n\
         git worktree prune\n\
         for br in {local}; do\n\
           if git merge-base --is-ancestor \"$br\" \"$t\" && git branch -D \"$br\" >/dev/null 2>&1; then echo \"MONAGED_LOCAL_OK $br\"; else echo \"MONAGED_LOCAL_FAIL $br\"; fi\n\
         done\n\
         del=\n\
         for br in {remote}; do\n\
           if [ -n \"$rt\" ] && git merge-base --is-ancestor \"origin/$br\" \"$rt\"; then del=\"$del $br\"; else echo \"MONAGED_REMOTE_FAIL $br no longer in $t on origin\"; fi\n\
         done\n\
         if [ -n \"$del\" ]; then\n\
           out=$(timeout 180 git push origin --delete $del 2>&1)\n\
           git fetch -q --prune origin >/dev/null 2>&1\n\
           for br in $del; do\n\
             if git show-ref -q --verify \"refs/remotes/origin/$br\"; then echo \"MONAGED_REMOTE_FAIL $br\"; else echo \"MONAGED_REMOTE_OK $br\"; fi\n\
           done\n\
           printf '%s\\n' \"$out\" | grep -iE 'error|denied|protected|fatal' | tail -n 3 | sed 's/^/MONAGED_ERR /'\n\
         fi\n",
        wts = list(&plan.worktrees),
        local = list(&plan.local),
        remote = list(&plan.remote),
    ));
    s
}

/// What happened, from `run_script`'s output.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    pub worktrees: usize,
    pub local: usize,
    pub remote: usize,
    pub failed: Vec<String>,
    pub errors: Vec<String>,
}

pub fn parse_outcome(out: &str) -> Outcome {
    let mut o = Outcome::default();
    for line in out.lines() {
        if line.starts_with("MONAGED_WT_OK ") {
            o.worktrees += 1;
        } else if line.starts_with("MONAGED_LOCAL_OK ") {
            o.local += 1;
        } else if line.starts_with("MONAGED_REMOTE_OK ") {
            o.remote += 1;
        } else if let Some(rest) = line.strip_prefix("MONAGED_WT_FAIL ") {
            o.failed.push(format!("worktree {rest}"));
        } else if let Some(rest) = line.strip_prefix("MONAGED_LOCAL_FAIL ") {
            o.failed.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("MONAGED_REMOTE_FAIL ") {
            o.failed.push(format!("origin/{rest}"));
        } else if let Some(rest) = line.strip_prefix("MONAGED_ERR ") {
            o.errors.push(rest.trim().to_string());
        }
    }
    o
}

impl Outcome {
    pub fn describe(&self) -> String {
        let mut text = format!(
            "Deleted {} on origin and {} here, and removed {}.",
            count(self.remote, "branch"),
            count(self.local, "branch"),
            count(self.worktrees, "worktree")
        );
        if !self.failed.is_empty() {
            text.push_str(&format!(" Couldn't delete: {}.", self.failed.join(", ")));
        }
        if !self.errors.is_empty() {
            let hint = if self.errors.iter().any(|e| crate::git_auth::looks_like_auth_failure(e)) {
                " Sign in under Settings → Git accounts and run it again."
            } else if self.errors.iter().any(|e| e.contains("protected")) {
                " Protected branches can only be deleted on the host."
            } else {
                ""
            };
            text.push_str(&format!(" Origin said: {}.{hint}", self.errors.join(" · ")));
        }
        text
    }
}

impl Monaged {
    /// The branch everything has to reach before it's cleaned up (develop).
    fn cleanup_target(&self, project_id: i64) -> Option<String> {
        self.chain(project_id).first().cloned()
    }

    /// Finish and clean up a subproject: works out what goes, then asks.
    pub(crate) fn finish_sub(&mut self, sub_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let Some(target) = self.cleanup_target(project.id) else { return };
        let working: Vec<String> = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && a.is_working()).map(|a| a.record.name.clone()).collect();
        if !working.is_empty() {
            self.toast(Tone::Warn, format!("{} still working. Finish {} when that's done.", working.join(", ") + if working.len() == 1 { " is" } else { " are" }, sub.name), cx);
            return;
        }
        if self.ship_running(sub_id) {
            self.toast(Tone::Warn, format!("{} is being merged into {target}. Finish it when that's done.", sub.name), cx);
            return;
        }
        // Its own branch (with the worktree that has it), then every agent's.
        let mut items: Vec<(String, String)> = Vec::new();
        let on_feature = self.agents.iter().find(|a| a.record.subproject_id == sub_id && a.record.branch == sub.branch).map(|a| a.record.worktree.clone());
        items.push((sub.branch.clone(), on_feature.unwrap_or_default()));
        for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && a.record.branch != sub.branch) {
            if !items.iter().any(|(b, _)| *b == agent.record.branch) {
                items.push((agent.record.branch.clone(), agent.record.worktree.clone()));
            }
        }
        self.progress("cleanup", format!("Working out what finishing {} cleans up…", sub.name), cx);
        let distro = self.config.distro.clone();
        cx.spawn_in(window, async move |view, cx| {
            let t = target.clone();
            let out = cx.background_executor().spawn(async move { run(&distro, &sub_plan_script(&project.path, &t, &items)) }).await;
            view.update_in(cx, |this, window, cx| match out {
                Ok(out) => {
                    this.dismiss_progress("cleanup", cx);
                    this.ask_confirm(ConfirmAction::FinishSub(sub_id, parse_plan(&out, &target)), window, cx);
                }
                Err(err) => this.settle("cleanup", Tone::Error, format!("Couldn't work out the cleanup: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// You confirmed: delete what the plan says, archive the agents and the subproject.
    pub(crate) fn finish_sub_confirmed(&mut self, sub_id: i64, plan: Plan, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        self.progress("cleanup", format!("Finishing {}: deleting its branches and worktrees…", sub.name), cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let repo = project.path.clone();
            let out = cx.background_spawn(async move { run(&distro, &run_script(&repo, &plan)) }).await;
            view.update(cx, |this, cx| {
                let outcome = match out {
                    Ok(out) => parse_outcome(&out),
                    Err(err) => Outcome { errors: vec![format!("{err:#}")], ..Default::default() },
                };
                // The agents and the subproject go to the archive, whatever git managed.
                let agents: Vec<i64> = this.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| a.record.id).collect();
                for id in &agents {
                    let _ = this.store.archive_agent(*id);
                    if let Some(agent) = this.agent_mut(*id) {
                        agent.record.archived = true;
                        agent.live = false;
                    }
                }
                let _ = this.store.set_subproject_archived(sub_id, true);
                let text = format!("Finished {}: {} Archived it and its {}.", sub.name, outcome.describe(), count(agents.len(), "agent"));
                let _ = this.store.log_event(sub_id, None, "archive", &text);
                this.subprojects.retain(|s| s.id != sub_id);
                this.ships.remove(&sub_id);
                if this.sel_sub == Some(sub_id) {
                    this.select_project(sub.project_id, cx);
                }
                let tone = if outcome.failed.is_empty() && outcome.errors.is_empty() { Tone::Info } else { Tone::Warn };
                this.chime(crate::sound::Cue::Complete);
                this.settle("cleanup", tone, text, cx);
                this.refresh_branches(sub.project_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Clean up a project's branches: works out what goes, then asks.
    pub(crate) fn cleanup_branches(&mut self, project_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        let Some(target) = self.cleanup_target(project_id) else { return };
        let never = self.chain(project_id);
        // Branches of subprojects and agents still around stay here (they may go on origin once merged).
        let subs: Vec<i64> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.id).collect();
        let mut in_use: Vec<String> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.branch.clone()).collect();
        in_use.extend(self.agents.iter().filter(|a| subs.contains(&a.record.subproject_id) && !a.record.archived).map(|a| a.record.branch.clone()));
        self.progress("cleanup", format!("Looking for branches in {} that are already in {target}…", project.name), cx);
        let distro = self.config.distro.clone();
        cx.spawn_in(window, async move |view, cx| {
            let t = target.clone();
            let out = cx.background_executor().spawn(async move { run(&distro, &project_plan_script(&project.path, &t, &never, &in_use)) }).await;
            view.update_in(cx, |this, window, cx| match out {
                Ok(out) => {
                    let plan = parse_plan(&out, &target);
                    if plan.deletes_anything() {
                        this.dismiss_progress("cleanup", cx);
                        this.ask_confirm(ConfirmAction::CleanupBranches(project_id, plan), window, cx);
                    } else {
                        this.settle("cleanup", Tone::Info, format!("Nothing to clean up: no branch here or on origin has only work that's already in {target}. {}", plan.describe()), cx);
                    }
                }
                Err(err) => this.settle("cleanup", Tone::Error, format!("Couldn't look at the branches: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn cleanup_branches_confirmed(&mut self, project_id: i64, plan: Plan, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() else { return };
        self.progress("cleanup", "Deleting merged branches…", cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let repo = project.path.clone();
            let out = cx.background_spawn(async move { run(&distro, &run_script(&repo, &plan)) }).await;
            view.update(cx, |this, cx| {
                let outcome = match out {
                    Ok(out) => parse_outcome(&out),
                    Err(err) => Outcome { errors: vec![format!("{err:#}")], ..Default::default() },
                };
                let tone = if outcome.failed.is_empty() && outcome.errors.is_empty() { Tone::Info } else { Tone::Warn };
                this.settle("cleanup", tone, outcome.describe(), cx);
                this.refresh_branches(project_id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Brings an archived subproject back (its branch is made again from the target if it was deleted).
    pub(crate) fn restore_sub(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.store.archived_subprojects().unwrap_or_default().into_iter().find(|s| s.id == sub_id) else { return };
        let _ = self.store.set_subproject_archived(sub_id, false);
        let target = self.cleanup_target(sub.project_id).unwrap_or_default();
        if let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id) {
            let _ = crate::git::ensure_branch(&self.config.distro, &project.path, &sub.branch, &target);
        }
        let _ = self.store.log_event(sub_id, None, "restore", &format!("{} was restored", sub.name));
        let name = sub.name.clone();
        self.subprojects.push(sub);
        self.toast(Tone::Info, format!("Restored {name}. Its agents stay archived; restore them from its list if you need them."), cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_reads_what_goes_and_what_stays() {
        let out = "MONAGED_WT /home/u/code/.monaged/p/mt/integrator\n\
                   MONAGED_LOCAL merged feature/mobietrain-rework\n\
                   MONAGED_REMOTE merged feature/mobietrain-rework\n\
                   MONAGED_LOCAL merged agent/mobietrain-rework/build-db-core\n\
                   MONAGED_DIRTY agent/mobietrain-rework/lead\n\
                   MONAGED_REMOTE 3 feature/personeelbeheer-improved-filter\n\
                   MONAGED_KEEP agent/bun-audit-fix/agent-1 in use\n\
                   noise";
        let plan = parse_plan(out, "develop");
        assert_eq!(plan.worktrees, ["/home/u/code/.monaged/p/mt/integrator"]);
        assert_eq!(plan.local, ["feature/mobietrain-rework", "agent/mobietrain-rework/build-db-core"]);
        assert_eq!(plan.remote, ["feature/mobietrain-rework"]);
        assert_eq!(
            plan.kept,
            [
                ("agent/mobietrain-rework/lead".to_string(), "uncommitted changes in its worktree".to_string()),
                ("origin/feature/personeelbeheer-improved-filter".to_string(), "3 commits develop doesn't have".to_string()),
                ("agent/bun-audit-fix/agent-1".to_string(), "in use".to_string()),
            ]
        );
        let text = plan.describe();
        assert!(text.starts_with("On origin, deletes 1 branch: feature/mobietrain-rework. Here, deletes 2 branches:"));
        assert!(text.contains("Removes 1 worktree") && text.contains("already in develop") && text.contains("Kept: agent/mobietrain-rework/lead (uncommitted"));
        // Branches in use are counted, not listed.
        assert!(text.contains("Also kept: 1 branch still in use") && !text.contains("agent/bun-audit-fix/agent-1 (in use)"));
        assert!(plan.deletes_anything() && !Plan::default().deletes_anything());
    }

    #[test]
    fn an_outcome_says_what_happened_and_why_not() {
        let out = "MONAGED_WT_OK /a\nMONAGED_LOCAL_OK x\nMONAGED_LOCAL_OK y\nMONAGED_REMOTE_OK x\nMONAGED_REMOTE_FAIL z\n\
                   MONAGED_ERR remote: GitLab: You can only delete protected branches using the web interface.";
        let o = parse_outcome(out);
        assert_eq!((o.worktrees, o.local, o.remote), (1, 2, 1));
        assert_eq!(o.failed, ["origin/z"]);
        let text = o.describe();
        assert!(text.starts_with("Deleted 1 branch on origin and 2 branches here, and removed 1 worktree."));
        assert!(text.contains("Couldn't delete: origin/z") && text.contains("Protected branches can only be deleted on the host"));
    }

    #[test]
    fn the_scripts_never_touch_the_chain_and_check_again_before_deleting() {
        let never = vec!["develop".to_string(), "acceptance".to_string(), "main".to_string()];
        let s = project_plan_script("/r", "develop", &never, &["feature/x".to_string()]);
        assert!(s.contains("never=\" develop acceptance main \"") && s.contains("in_use=\" feature/x \""));
        let plan = Plan { target: "develop".into(), local: vec!["a".into()], remote: vec!["b".into()], ..Default::default() };
        let run = run_script("/r", &plan);
        // Each branch is checked against develop again right before it goes.
        assert!(run.contains("git merge-base --is-ancestor \"$br\" \"$t\" && git branch -D"));
        assert!(run.contains("git merge-base --is-ancestor \"origin/$br\" \"$rt\"; then del="));
        assert!(run.contains("git push origin --delete $del"));
    }
}

#[cfg(test)]
mod real_repo {
    use super::*;

    /// Prints what Clean up branches and Finish would do in a real repo,
    /// deleting nothing (`MONAGED_TEST_REPO`; needs WSL):
    /// `cargo test -- --ignored plans_on_a_real_repo --nocapture`.
    #[test]
    #[ignore]
    fn plans_on_a_real_repo() {
        let Ok(repo) = std::env::var("MONAGED_TEST_REPO") else { return };
        let distro = std::env::var("MONAGED_TEST_DISTRO").unwrap_or_else(|_| "Ubuntu".into());
        let never = vec!["develop".to_string(), "acceptance".to_string(), "main".to_string()];
        let in_use = std::env::var("MONAGED_TEST_IN_USE").unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(String::from).collect::<Vec<_>>();
        let out = run(&distro, &project_plan_script(&repo, "develop", &never, &in_use)).unwrap();
        println!("PROJECT: {}", parse_plan(&out, "develop").describe());
        if let Ok(items) = std::env::var("MONAGED_TEST_SUB") {
            let items: Vec<(String, String)> = items.split(',').map(|b| (b.to_string(), String::new())).collect();
            let out = run(&distro, &sub_plan_script(&repo, "develop", &items)).unwrap();
            println!("SUB: {}", parse_plan(&out, "develop").describe());
        }
    }
}
