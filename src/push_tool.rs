//! Agents push through Monaged, when you ask them to. Claude Code's own
//! `git push` is denied for agents (`env::DENY_RULES`), because Monaged checks
//! the new commits for secrets before anything leaves the machine. But there
//! was no way through, so "push and merge!" to an agent ended in "permission
//! denied". The `push` tool is that way through:
//! - it only works in a turn that came from a message you typed (not from the
//!   lead, a brief or Monaged), so an agent never decides to push by itself;
//! - only the agent's own branch, its subproject's branch, or develop (the
//!   first branch of the project's chain). Acceptance and main stay yours;
//! - Monaged's secret check runs first, as for every push.

use std::sync::mpsc::Sender;

use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use crate::coord::ToolReply;
use crate::{Monaged, git};

pub fn tool_def() -> Value {
    json!({
        "name": "push",
        "description": "Push a branch to origin through Monaged: use it when the user's message asks you to push (or to push and merge). Running `git push` yourself is blocked; this is the way. Monaged checks the new commits for secrets first. You can push your own branch (the default), your subproject's branch, or develop (the project's first branch, after you merged into it); acceptance and main stay the user's. It only works in a turn that started with the user's own message.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "branch": { "type": "string", "description": "The branch to push. Leave it out for your own branch." }
            }
        }
    })
}

/// The branch a `push` call means: your own when none is given, without an `origin/` in front.
pub fn pick_branch(given: Option<&str>, own: &str) -> String {
    let given = given.map(str::trim).filter(|b| !b.is_empty()).unwrap_or(own);
    given.strip_prefix("origin/").unwrap_or(given).to_string()
}

/// What an agent may push: its own branch, its subproject's, and the first of the chain.
pub fn allowed_branches(own: &str, sub_branch: &str, chain: &[String]) -> Vec<String> {
    let mut out = vec![own.to_string(), sub_branch.to_string()];
    if let Some(first) = chain.first() {
        out.push(first.clone());
    }
    out.dedup();
    out
}

impl Monaged {
    pub(crate) fn tool_push(&mut self, agent_id: i64, arguments: &Value, reply: Sender<ToolReply>, cx: &mut Context<Self>) {
        let fail = |reply: Sender<ToolReply>, text: String| {
            let _ = reply.send(ToolReply::error(text));
        };
        let Some(agent) = self.agent(agent_id) else { return fail(reply, "Monaged doesn't know this agent any more.".into()) };
        if !agent.user_turn {
            return fail(
                reply,
                "Not pushed: you can push only in a turn that started with the user's own message asking for it. This turn came from Monaged or the lead. Tell the user it's ready to push.".into(),
            );
        }
        let (own, sub_id) = (agent.record.branch.clone(), agent.record.subproject_id);
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return fail(reply, "This subproject is gone.".into()) };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return fail(reply, "This project is gone.".into()) };
        let branch = pick_branch(arguments["branch"].as_str(), &own);
        let allowed = allowed_branches(&own, &sub.branch, &self.chain(project.id));
        if !allowed.contains(&branch) {
            return fail(
                reply,
                format!(
                    "Not pushed: agents can push {}. {branch} goes on through Monaged's branch rules (the user promotes it in the project view).",
                    allowed.iter().map(|b| format!("`{b}`")).collect::<Vec<_>>().join(", ")
                ),
            );
        }
        let name = agent.record.name.clone();
        let distro = self.config.distro.clone();
        self.progress("push", format!("{name} is pushing {branch}…"), cx);
        cx.spawn(async move |view, cx| {
            let (repo, b) = (project.path.clone(), branch.clone());
            let result = cx.background_spawn(async move { git::push_branch(&distro, &repo, &b) }).await;
            view.update(cx, |this, cx| {
                let answer = match result {
                    Ok(out) => {
                        let _ = this.store.log_event(sub_id, Some(agent_id), "push", &format!("{name} pushed {branch} to origin (you asked)"));
                        this.chime(crate::sound::Cue::Success);
                        this.settle("push", crate::agent::Tone::Info, format!("{name} pushed {branch} to origin."), cx);
                        this.watch_ci(project.id, &branch, cx);
                        let tail: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
                        ToolReply::ok(format!("Pushed {branch} to origin.\n{}", tail.join("\n")))
                    }
                    Err(err) => {
                        let text = format!("{err:#}");
                        let hint = if crate::git_auth::looks_like_auth_failure(&text) {
                            " Git couldn't sign in to the host: ask the user to sign in under Settings → Git accounts, then push again."
                        } else {
                            ""
                        };
                        let _ = this.store.log_event(sub_id, Some(agent_id), "failed", &format!("{name} couldn't push {branch}: {text}"));
                        this.settle("push", crate::agent::Tone::Error, format!("{name} couldn't push {branch}: {text}{hint}"), cx);
                        ToolReply::error(format!("Not pushed: {text}{hint}"))
                    }
                };
                let _ = reply.send(answer);
                this.refresh_branches(project.id, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_pushes_its_own_branch_its_subprojects_or_develop() {
        let chain = vec!["develop".to_string(), "acceptance".to_string(), "main".to_string()];
        let allowed = allowed_branches("agent/bun-audit-fix/agent-1", "standing/bun-audit-fix", &chain);
        assert_eq!(allowed, ["agent/bun-audit-fix/agent-1", "standing/bun-audit-fix", "develop"]);
        assert!(!allowed.contains(&"main".to_string()) && !allowed.contains(&"acceptance".to_string()));
        assert_eq!(pick_branch(None, "agent/x/a"), "agent/x/a");
        assert_eq!(pick_branch(Some("  "), "agent/x/a"), "agent/x/a");
        assert_eq!(pick_branch(Some("origin/develop"), "agent/x/a"), "develop");
    }
}
