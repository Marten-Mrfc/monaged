//! The lead: one agent per subproject that oversees the others. You talk to
//! it on the subproject's dashboard. Monaged keeps it posted ("updates") and
//! gives it tools to see the team and direct it; it doesn't write code, and it
//! can't accept work below the confidence bar for you.

use std::collections::HashSet;

use gpui::Context;
use serde_json::{Value, json};

use crate::agent::AgentView;
use crate::coord::ToolReply;
use crate::pipeline::{self, Role};
use crate::shell::agent_state;
use crate::ui::State;
use crate::{AgentSpec, Monaged, review};

/// Starts Monaged's messages in the lead's conversation.
pub const UPDATE_PREFIX: &str = "[monaged-update]";

/// The most agents the lead may have running in one subproject.
const MAX_AGENTS: usize = 8;

pub fn brief(sub: &str, branch: &str, request: Option<&str>) -> String {
    let request = match request.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("## What the user asked for\n{r}"),
        None => "## What the user asked for\nNothing yet: the user will tell you on the dashboard.".to_string(),
    };
    format!(
        "{header} Lead\n\n\
         You are the LEAD for the subproject \"{sub}\" (feature branch `{branch}`). Other agents do the work, each in its \
         own git worktree; you oversee them. The user talks to you from the subproject's dashboard, so you're their one \
         place to ask how things stand and to say what should happen next. Don't write or change code yourself.\n\n\
         ## Your tools (the `monaged` MCP server)\n\
         - `team_status`: every agent with its role, state, confidence score, open review findings and what it's doing; \
         the pipeline's stage; and what waits for the user. Call it before you answer anything about progress.\n\
         - `message_agent`: pass an instruction or an answer to one agent. Short and concrete; say what to do, not how you feel.\n\
         - `start_agent`: start a new agent for a piece of work nobody is on (a name and its task). Only when the user asks \
         for work, or when the plan clearly needs it.\n\
         - `fix_findings`: send an agent its review findings to fix. `review_agent`: review an agent's work again.\n\n\
         ## How Monaged works around you\n\
         When a pipeline runs, Monaged moves it on by itself: research, verification, a plan, builders, then an integrator. \
         Builders and the integrator only count as done when a review scores their work at the bar (usually 4/5); below it \
         Monaged sends the findings back, and after two rounds it asks the user. Only the user can accept work below the bar, \
         or approve a plan: never tell an agent its work is accepted.\n\n\
         Monaged sends you short updates that start with `{UPDATE_PREFIX}`. React only when something needs doing (for example \
         answer an agent's question you know the answer to, or point a builder at the real problem), and reply in one or two \
         sentences; the user reads along.\n\n\
         Ask the user only what only they can decide, as a question block (then stop and wait):\n\n\
         ```monaged-question\n\
         {{\"question\": \"...\", \"why\": \"how the answer changes the work\", \"options\": [\"...\", \"...\"], \"recommended\": 0}}\n\
         ```\n\n\
         {request}\n\n\
         Start with `team_status`, then greet the user in two sentences at most: where things stand and what (if anything) \
         waits for them.",
        header = pipeline::BRIEF_HEADER,
    )
}

/// The tools only the lead gets.
pub fn tools() -> Value {
    let agent = json!({ "type": "string", "description": "The agent's name, as team_status lists it." });
    json!([
        {
            "name": "team_status",
            "description": "See the subproject's agents (role, state, confidence score, open review findings, what each is doing), the pipeline's stage and what waits for the user.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "message_agent",
            "description": "Send one agent a message: an instruction, a correction or an answer to its question. It gets it now, or when its current turn ends.",
            "inputSchema": {
                "type": "object",
                "properties": { "to": agent, "message": { "type": "string" } },
                "required": ["to", "message"]
            }
        },
        {
            "name": "start_agent",
            "description": "Start a new agent in its own worktree with a task. Use a short name (it becomes its branch name).",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" }, "task": { "type": "string", "description": "Everything it needs to know: the goal, the files, what done looks like." } },
                "required": ["name", "task"]
            }
        },
        {
            "name": "fix_findings",
            "description": "Send an agent its latest review findings (and failing tests) to fix.",
            "inputSchema": { "type": "object", "properties": { "agent": agent }, "required": ["agent"] }
        },
        {
            "name": "review_agent",
            "description": "Run the tests and a review of an agent's work now. The score comes back as an update.",
            "inputSchema": { "type": "object", "properties": { "agent": agent }, "required": ["agent"] }
        }
    ])
}

pub fn is_tool(name: &str) -> bool {
    matches!(name, "team_status" | "message_agent" | "start_agent" | "fix_findings" | "review_agent")
}

/// One agent in `team_status`.
fn describe_agent(agent: &AgentView) -> String {
    let state = match agent_state(agent) {
        State::NeedsYou if agent.paused => "paused by the budget".to_string(),
        State::NeedsYou => "waiting for the user".to_string(),
        State::Working => match &agent.turn {
            Some(turn) => format!("working ({}m)", turn.started.elapsed().as_secs() / 60),
            None => "starting".to_string(),
        },
        _ => "idle".to_string(),
    };
    let mut line = format!("- {} ({}): {state}", agent.record.name, crate::shell::role_label(&agent.role()));
    if let Some(c) = &agent.confidence {
        line.push_str(&format!(", confidence {}/5", c.score));
    }
    if agent.reviewing {
        line.push_str(", being reviewed");
    }
    if let Some(summary) = &agent.summary {
        line.push_str(&format!("\n  Summary: {}", summary.text.replace('\n', " ")));
    } else {
        let task = agent.task();
        if !task.starts_with(pipeline::BRIEF_HEADER) && !task.trim().is_empty() {
            line.push_str(&format!("\n  Task: {}", review::clip(task.trim(), 300).replace('\n', " ")));
        }
    }
    if let Some(c) = &agent.confidence {
        for issue in c.issues.iter().take(5) {
            line.push_str(&format!("\n  Finding [{}] {}: {}", issue.severity, issue.file, issue.problem.replace('\n', " ")));
        }
        if c.issues.len() > 5 {
            line.push_str(&format!("\n  …and {} more findings", c.issues.len() - 5));
        }
    }
    line
}

impl Monaged {
    /// The subproject's lead, if it has one.
    pub(crate) fn lead_of(&self, sub_id: i64) -> Option<i64> {
        self.agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() == Role::Lead)
            .map(|a| a.record.id)
            .max()
    }

    /// Starts the subproject's lead (once).
    pub(crate) fn start_lead(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if self.lead_of(sub_id).is_some() || self.starting_lead.contains(&sub_id) {
            return;
        }
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let request = self.pipelines.get(&sub_id).map(|p| p.request.clone());
        self.starting_lead.insert(sub_id);
        let spec = AgentSpec {
            name: "lead".into(),
            role: Role::Lead,
            on_feature_branch: false,
            brief: Some(brief(&sub.name, &sub.branch, request.as_deref())),
            select: false,
        };
        self.spawn_agent(sub_id, spec, cx);
    }

    /// Tells the lead what just happened (it reacts only if something needs doing).
    pub(crate) fn tell_lead(&mut self, sub_id: i64, text: impl Into<String>) {
        let Some(lead) = self.lead_of(sub_id) else { return };
        // Updates aren't worth spending past the budget on.
        if self.budget_block().is_some() || self.sub_cap_block(sub_id).is_some() {
            return;
        }
        let text = text.into();
        // While it works, updates wait and go out together after its turn,
        // after anything you typed (so your words stay yours).
        if self.agent(lead).is_some_and(|a| a.is_working()) {
            self.lead_updates.entry(lead).or_default().push(text);
            return;
        }
        self.send_prompt(lead, format!("{UPDATE_PREFIX} {text}"));
    }

    /// The lead's turn ended: its waiting updates join its next message.
    pub(crate) fn take_lead_updates(&mut self, agent_id: i64) {
        let Some(updates) = self.lead_updates.remove(&agent_id).filter(|u| !u.is_empty()) else { return };
        let text = format!("{UPDATE_PREFIX} {}", updates.join("\n\n"));
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.queued_prompt = Some(match agent.queued_prompt.take() {
                Some(typed) => format!("{typed}\n\n{text}"),
                None => text,
            });
        }
    }

    /// What `team_status` returns.
    pub(crate) fn team_status(&self, sub_id: i64) -> String {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return "This subproject is gone.".into() };
        let mut out = format!("Subproject \"{}\" on branch `{}`.", sub.name, sub.branch);
        if let Some(p) = self.pipelines.get(&sub_id) {
            out.push_str(&format!("\nPipeline: {}.", p.stage.label()));
            if let Some(note) = &p.note {
                out.push_str(&format!(" Status: {note}"));
            }
            if let Some(plan) = &p.plan {
                out.push_str(&format!("\nPlan: \"{}\". Workstreams:", pipeline::plan_title(plan)));
                for stream in pipeline::workstreams(plan) {
                    let goal = stream.value["goal"].as_str().unwrap_or_default();
                    out.push_str(&format!("\n- {}: {}", stream.name, review::clip(goal, 200).replace('\n', " ")));
                }
            }
            out.push_str(&format!("\nThe bar: {}/5. Passed: {}.", self.min_score(), if p.done.is_empty() { "none yet".into() } else { p.done.join(", ") }));
            for (name, why) in &p.gate.held {
                out.push_str(&format!("\nWaiting for the user: {name}: {why}"));
            }
            if !p.gate.fixing.is_empty() {
                out.push_str(&format!("\nFixing review findings: {}.", p.gate.fixing.join(", ")));
            }
            if !p.gate.pending.is_empty() {
                out.push_str(&format!("\nIn review: {}.", p.gate.pending.join(", ")));
            }
        } else {
            out.push_str("\nNo pipeline: the agents here work on what they were asked directly.");
        }
        let agents: Vec<&AgentView> =
            self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() != Role::Lead).collect();
        if agents.is_empty() {
            out.push_str("\n\nNo agents yet.");
        } else {
            out.push_str("\n\nAgents:");
            for agent in agents {
                out.push('\n');
                out.push_str(&describe_agent(agent));
            }
        }
        let waiting: Vec<String> = self.inbox().into_iter().filter(|i| i.sub_id == sub_id).map(|i| format!("{} ({})", i.title, i.detail)).collect();
        if !waiting.is_empty() {
            out.push_str(&format!("\n\nWaiting for the user on the dashboard: {}.", waiting.join("; ")));
        }
        out
    }

    /// The lead's own tools; None when `tool` isn't one of them.
    pub(crate) fn lead_tool(&mut self, from: i64, tool: &str, args: &Value, cx: &mut Context<Self>) -> Option<ToolReply> {
        if !is_tool(tool) {
            return None;
        }
        let Some(agent) = self.agent(from) else { return Some(ToolReply::error("Unknown agent.")) };
        if agent.role() != Role::Lead {
            return Some(ToolReply::error("Only the subproject's lead has this tool."));
        }
        let sub_id = agent.record.subproject_id;
        let target = |this: &Self, key: &str| -> Result<(i64, String), ToolReply> {
            let wanted = args[key].as_str().unwrap_or_default().trim().trim_start_matches('@');
            match this.find_target(sub_id, wanted).filter(|&id| id != from) {
                Some(id) => Ok((id, this.agent_name(id))),
                None => Err(ToolReply::error(format!("No agent called \"{wanted}\" here. team_status lists them."))),
            }
        };
        let reply = match tool {
            "team_status" => ToolReply::ok(self.team_status(sub_id)),
            "message_agent" => match target(self, "to") {
                Err(e) => e,
                Ok((id, name)) => {
                    let message = args["message"].as_str().unwrap_or_default().trim().to_string();
                    if message.is_empty() {
                        return Some(ToolReply::error("message_agent needs a `message`."));
                    }
                    let busy = self.agent(id).is_some_and(|a| a.is_working());
                    self.send_prompt(id, format!("From the lead (via Monaged): {message}"));
                    ToolReply::ok(if busy { format!("Queued: {name} is working and gets it when its turn ends.") } else { format!("Sent to {name}.") })
                }
            },
            "start_agent" => {
                let name = crate::git::slug(args["name"].as_str().unwrap_or_default());
                let task = args["task"].as_str().unwrap_or_default().trim().to_string();
                let running = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).count();
                if name.is_empty() || task.is_empty() {
                    ToolReply::error("start_agent needs a `name` and a `task`.")
                } else if running >= MAX_AGENTS {
                    ToolReply::error(format!("This subproject already has {running} agents. Give the work to one of them with message_agent, or ask the user to archive some."))
                } else {
                    let spec = AgentSpec { name: name.clone(), role: Role::Free, on_feature_branch: false, brief: Some(task), select: false };
                    self.spawn_agent(sub_id, spec, cx);
                    let _ = self.store.log_event(sub_id, Some(from), "lead", &format!("The lead started {name}"));
                    ToolReply::ok(format!("Starting {name} in its own worktree; it gets the task as soon as its session is open."))
                }
            }
            "fix_findings" => match target(self, "agent") {
                Err(e) => e,
                Ok((id, name)) => match self.agent(id).and_then(|a| a.confidence.as_ref()).and_then(review::fix_prompt) {
                    Some(prompt) => {
                        self.send_prompt(id, prompt);
                        ToolReply::ok(format!("Sent {name} its findings to fix."))
                    }
                    None => ToolReply::ok(format!("{name}'s last review found nothing to fix.")),
                },
            },
            "review_agent" => match target(self, "agent") {
                Err(e) => e,
                Ok((id, name)) => {
                    self.lead_reviews.insert(id);
                    self.review_fresh(id, cx);
                    ToolReply::ok(format!("Reviewing {name}; the score comes as an update."))
                }
            },
            _ => return None,
        };
        cx.notify();
        Some(reply)
    }

    /// A review the lead asked for finished: tell it the score.
    pub(crate) fn lead_review_done(&mut self, agent_id: i64) {
        if !self.lead_reviews.remove(&agent_id) {
            return;
        }
        let Some(agent) = self.agent(agent_id) else { return };
        let (sub_id, name) = (agent.record.subproject_id, agent.record.name.clone());
        let text = match &agent.confidence {
            Some(c) => format!("{name} was reviewed: {}/5 with {} finding(s).", c.score, c.issues.len()),
            None => format!("{name} couldn't be reviewed{}.", agent.review_error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default()),
        };
        self.tell_lead(sub_id, text);
    }
}

/// Agents the lead asked a review of (their score goes back to it).
pub type LeadReviews = HashSet<i64>;
