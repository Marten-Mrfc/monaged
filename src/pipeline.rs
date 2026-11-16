//! Phase 3: the feature pipeline.
//!
//! Research → 2 verifiers → decide → plan → approve → build (1-10 builders)
//! → integrate → done.
//!
//! Every role agent gets a brief as its first message. Agents report back with
//! fenced JSON blocks in their replies, which Monaged turns into cards and
//! uses to advance the pipeline:
//!
//! ```text
//! ```monaged-question      a question for the developer (stop and wait)
//! ```monaged-research      the researcher's report
//! ```monaged-verification  a verifier's verdict
//! ```monaged-plan          the planner's plan (with size, risk flags, confidence)
//! ```monaged-done          a builder's or the integrator's result
//! ```monaged-answer        an answer to another agent's question (via Monaged's `ask` tool)
//! ```

use serde_json::{Value, json};

pub const MAX_BUILDERS: usize = 10;
pub const BRIEF_HEADER: &str = "# Monaged brief:";

// ---- Roles and stages ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    Free,
    Researcher,
    Verifier(u8),
    Planner,
    Builder(String),
    Integrator,
    /// Oversees the subproject's agents; you talk to it on the dashboard.
    Lead,
}

impl Role {
    pub fn to_db(&self) -> String {
        match self {
            Role::Free => String::new(),
            Role::Researcher => "researcher".into(),
            Role::Verifier(n) => format!("verifier-{n}"),
            Role::Planner => "planner".into(),
            Role::Builder(workstream) => format!("builder:{workstream}"),
            Role::Integrator => "integrator".into(),
            Role::Lead => "lead".into(),
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "researcher" => Role::Researcher,
            "planner" => Role::Planner,
            "integrator" => Role::Integrator,
            "lead" => Role::Lead,
            v if v.starts_with("verifier-") => Role::Verifier(v[9..].parse().unwrap_or(1)),
            v if v.starts_with("builder:") => Role::Builder(v[8..].to_string()),
            _ => Role::Free,
        }
    }

    /// The fenced block this role must finish with.
    pub fn result_block(&self) -> Option<&'static str> {
        match self {
            Role::Researcher => Some("monaged-research"),
            Role::Verifier(_) => Some("monaged-verification"),
            Role::Planner => Some("monaged-plan"),
            Role::Builder(_) | Role::Integrator => Some("monaged-done"),
            Role::Free | Role::Lead => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stage {
    Research,
    Verify,
    Decide,
    Plan,
    Approve,
    Build,
    Integrate,
    Done,
    Stopped,
}

pub const STAGES: [Stage; 8] =
    [Stage::Research, Stage::Verify, Stage::Decide, Stage::Plan, Stage::Approve, Stage::Build, Stage::Integrate, Stage::Done];

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Research => "research",
            Stage::Verify => "verify",
            Stage::Decide => "decide",
            Stage::Plan => "plan",
            Stage::Approve => "approve",
            Stage::Build => "build",
            Stage::Integrate => "integrate",
            Stage::Done => "done",
            Stage::Stopped => "stopped",
        }
    }

    pub fn parse(value: &str) -> Self {
        STAGES.iter().copied().find(|s| s.as_str() == value).unwrap_or(Stage::Stopped)
    }

    pub fn label(self) -> &'static str {
        match self {
            Stage::Research => "Research",
            Stage::Verify => "Verify",
            Stage::Decide => "Decide",
            Stage::Plan => "Plan",
            Stage::Approve => "Approve",
            Stage::Build => "Build",
            Stage::Integrate => "Integrate",
            Stage::Done => "Done",
            Stage::Stopped => "Stopped",
        }
    }

    pub fn index(self) -> usize {
        STAGES.iter().position(|s| *s == self).unwrap_or(0)
    }
}

#[derive(Clone)]
pub struct Pipeline {
    pub subproject_id: i64,
    pub stage: Stage,
    pub request: String,
    pub auto_mode: bool,
    pub research: Option<Value>,
    /// Indexed by verifier number - 1.
    pub verifications: Vec<Value>,
    pub plan: Option<Value>,
    /// Names of builder agents that delivered their result and passed the quality gate.
    pub done: Vec<String>,
    /// What the pipeline is waiting for, shown to the user.
    pub note: Option<String>,
    /// Work that says it's done but hasn't passed review yet.
    pub gate: QualityGate,
    /// The agents this run started (ids). Only they move it on; an agent
    /// from an earlier run, or one asked something out of turn, doesn't.
    /// Empty for runs from before this was kept: then anyone counts.
    pub members: Vec<i64>,
}

impl Pipeline {
    /// Whether this agent's result belongs to the stage the run is in.
    pub fn expects(&self, agent_id: i64, role: &Role) -> bool {
        let member = self.members.is_empty() || self.members.contains(&agent_id);
        let stage = match role {
            Role::Researcher => self.stage == Stage::Research,
            Role::Verifier(_) => self.stage == Stage::Verify,
            // Replying to the planner during Approve gives a new plan.
            Role::Planner => matches!(self.stage, Stage::Plan | Stage::Approve),
            Role::Builder(_) => self.stage == Stage::Build,
            Role::Integrator => self.stage == Stage::Integrate,
            Role::Free | Role::Lead => false,
        };
        member && stage
    }

    /// An agent was renamed: its name changes everywhere the run keeps it.
    pub fn rename(&mut self, old: &str, new: &str) {
        let swap = |n: &mut String| {
            if n == old {
                *n = new.to_string();
            }
        };
        self.done.iter_mut().for_each(swap);
        self.gate.pending.iter_mut().for_each(swap);
        self.gate.fixing.iter_mut().for_each(swap);
        self.gate.held.iter_mut().for_each(|(n, _)| swap(n));
        self.gate.rounds.iter_mut().for_each(|(n, _)| swap(n));
    }
}

/// The lowest confidence score that integrates or merges without asking you.
pub const DEFAULT_MIN_SCORE: u8 = 4;
/// How many times Monaged sends the findings back before it asks you.
pub const MAX_FIX_ROUNDS: u8 = 2;

/// Builders and the integrator only count as done once a review scores their
/// work at the bar (4/5 by default). Below it, Monaged sends the findings
/// back to fix; after a couple of rounds it stops and asks you.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QualityGate {
    /// Reported done; waiting for a fresh review.
    pub pending: Vec<String>,
    /// Sent the findings to fix; waiting for it to report done again.
    pub fixing: Vec<String>,
    /// Waiting for you: (agent, why).
    pub held: Vec<(String, String)>,
    /// Automatic fix rounds used, per agent.
    pub rounds: Vec<(String, u8)>,
}

/// What to do with a review of work that says it's done.
#[derive(Clone, Debug, PartialEq)]
pub enum GateStep {
    Accept,
    /// Send the findings back (this is fix round n).
    Fix(u8),
    /// Stop and ask you, with why.
    Hold(String),
}

impl QualityGate {
    pub fn rounds_for(&self, name: &str) -> u8 {
        self.rounds.iter().find(|(n, _)| n == name).map_or(0, |(_, r)| *r)
    }

    pub fn held_reason(&self, name: &str) -> Option<&str> {
        self.held.iter().find(|(n, _)| n == name).map(|(_, why)| why.as_str())
    }

    /// Takes the agent out of every list (it was accepted, or starts over).
    pub fn clear(&mut self, name: &str) {
        self.pending.retain(|n| n != name);
        self.fixing.retain(|n| n != name);
        self.held.retain(|(n, _)| n != name);
    }

    /// The agent says it's done: review it.
    pub fn submit(&mut self, name: &str) {
        self.clear(name);
        self.pending.push(name.to_string());
    }

    pub fn fix(&mut self, name: &str, automatic: bool) -> u8 {
        self.clear(name);
        self.fixing.push(name.to_string());
        let round = self.rounds_for(name) + u8::from(automatic);
        self.rounds.retain(|(n, _)| n != name);
        self.rounds.push((name.to_string(), round));
        round
    }

    pub fn hold(&mut self, name: &str, why: String) {
        self.clear(name);
        self.held.push((name.to_string(), why));
    }

    /// The decision for a finished review: `score` is None when the work
    /// couldn't be scored (`why` says why).
    pub fn step(&self, name: &str, score: Option<u8>, why: &str, min: u8) -> GateStep {
        match score {
            Some(score) if score >= min => GateStep::Accept,
            Some(score) => {
                let used = self.rounds_for(name);
                if used < MAX_FIX_ROUNDS {
                    GateStep::Fix(used + 1)
                } else {
                    GateStep::Hold(format!(
                        "{name} is at {score}/5 after {used} fix round{}; it needs {min}/5 to go on by itself.",
                        if used == 1 { "" } else { "s" }
                    ))
                }
            }
            None => GateStep::Hold(why.to_string()),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "pending": self.pending,
            "fixing": self.fixing,
            "held": self.held.iter().map(|(n, why)| json!({ "name": n, "why": why })).collect::<Vec<_>>(),
            "rounds": self.rounds.iter().map(|(n, r)| json!({ "name": n, "rounds": r })).collect::<Vec<_>>(),
        })
    }

    pub fn from_json(text: &str) -> Self {
        let Ok(v) = serde_json::from_str::<Value>(text) else { return Self::default() };
        let names = |key: &str| -> Vec<String> { v[key].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)).collect() };
        Self {
            pending: names("pending"),
            fixing: names("fixing"),
            held: v["held"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| Some((h["name"].as_str()?.to_string(), h["why"].as_str().unwrap_or_default().to_string())))
                .collect(),
            rounds: v["rounds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some((r["name"].as_str()?.to_string(), r["rounds"].as_u64().unwrap_or(0).min(255) as u8)))
                .collect(),
        }
    }
}

impl Pipeline {
    pub fn new(subproject_id: i64, request: String, auto_mode: bool) -> Self {
        Self {
            subproject_id,
            stage: Stage::Research,
            request,
            auto_mode,
            research: None,
            verifications: Vec::new(),
            plan: None,
            done: Vec::new(),
            note: None,
            gate: QualityGate::default(),
            members: Vec::new(),
        }
    }

    pub fn verification_count(&self) -> usize {
        self.verifications.iter().filter(|v| !v.is_null()).count()
    }
}

// ---- Directive blocks ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    /// A short topic ("Storage"), when the agent gives one.
    pub header: String,
    pub question: String,
    pub why: String,
    pub options: Vec<QOption>,
    pub recommended: Option<usize>,
    /// More than one option can be picked.
    pub multi: bool,
}

/// One answer an agent offers: a label, and what choosing it means.
#[derive(Clone, Debug, PartialEq)]
pub struct QOption {
    pub label: String,
    pub description: String,
}

impl Question {
    /// The options' labels (for messages to the lead and auto mode).
    pub fn labels(&self) -> Vec<String> {
        self.options.iter().map(|o| o.label.clone()).collect()
    }

    /// The option auto mode and "Use the recommended answers" pick.
    pub fn recommended_label(&self) -> String {
        self.options.get(self.recommended.unwrap_or(0)).map(|o| o.label.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub enum Directive {
    Question(Question),
    Research(Value),
    Verification(Value),
    Plan(Value),
    Done(Value),
    /// An agent's answer to another agent's question (Phase 4).
    Answer(Value),
    Invalid { kind: String, error: String },
}

pub enum Segment {
    Text(String),
    Directive(Directive),
}

/// Splits an agent reply into Markdown text and `monaged-*` blocks.
/// An unclosed block (still streaming) stays text.
pub fn segments(text: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    let mut lines = text.split('\n');
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end_matches('\r').trim_start();
        let Some(kind) = trimmed.strip_prefix("```monaged-") else {
            buffer.push_str(line);
            buffer.push('\n');
            continue;
        };
        let kind = kind.trim().to_string();
        let mut body = String::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if inner.trim() == "```" {
                closed = true;
                break;
            }
            body.push_str(inner);
            body.push('\n');
        }
        if !closed {
            buffer.push_str(line);
            buffer.push('\n');
            buffer.push_str(&body);
            continue;
        }
        if !buffer.trim().is_empty() {
            out.push(Segment::Text(std::mem::take(&mut buffer)));
        }
        buffer.clear();
        out.push(Segment::Directive(parse_directive(&kind, &body)));
    }
    if !buffer.trim().is_empty() {
        out.push(Segment::Text(buffer));
    }
    out
}

pub fn directives(text: &str) -> Vec<Directive> {
    segments(text)
        .into_iter()
        .filter_map(|s| if let Segment::Directive(d) = s { Some(d) } else { None })
        .collect()
}

pub fn contains_directive(text: &str) -> bool {
    text.contains("```monaged-")
}

fn parse_directive(kind: &str, body: &str) -> Directive {
    let value: Value = match serde_json::from_str(body.trim()) {
        Ok(value) => value,
        Err(err) => return Directive::Invalid { kind: kind.to_string(), error: format!("not valid JSON: {err}") },
    };
    match kind {
        "question" => match parse_question(&value) {
            Some(q) => Directive::Question(q),
            None => Directive::Invalid { kind: kind.into(), error: "a question needs \"question\" and at least one option".into() },
        },
        "research" => Directive::Research(value),
        "verification" => Directive::Verification(value),
        "plan" => Directive::Plan(value),
        "done" => Directive::Done(value),
        "answer" => Directive::Answer(value),
        other => Directive::Invalid { kind: other.into(), error: "unknown block type".into() },
    }
}

fn parse_question(value: &Value) -> Option<Question> {
    let question = value["question"].as_str()?.trim().to_string();
    // Options are plain strings, or {"label", "description"} objects.
    let options: Vec<QOption> = value["options"]
        .as_array()?
        .iter()
        .filter_map(|o| match o {
            Value::String(s) => Some(QOption { label: s.trim().to_string(), description: String::new() }),
            Value::Object(_) => {
                let label = o["label"].as_str().or(o["title"].as_str())?.trim().to_string();
                Some(QOption { label, description: o["description"].as_str().unwrap_or_default().trim().to_string() })
            }
            _ => None,
        })
        .filter(|o| !o.label.is_empty())
        .collect();
    if question.is_empty() || options.is_empty() {
        return None;
    }
    let recommended = value["recommended"].as_u64().map(|i| i as usize).filter(|i| *i < options.len());
    Some(Question {
        header: value["header"].as_str().unwrap_or_default().trim().chars().take(24).collect(),
        question,
        why: value["why"].as_str().unwrap_or_default().to_string(),
        options,
        recommended,
        multi: value["multi"].as_bool().or(value["multiSelect"].as_bool()).unwrap_or(false),
    })
}

/// The answers in a message `answers_message` wrote ("1. question → answer"),
/// in order; None when the message isn't one.
pub fn parse_answers(message: &str) -> Option<Vec<String>> {
    if !(message.starts_with("Answers from the developer:") || message.starts_with("Answers (chosen automatically")) {
        return None;
    }
    let answers: Vec<String> = message
        .lines()
        .filter(|l| l.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())))
        .filter_map(|l| l.rsplit_once(" → ").map(|(_, a)| a.trim().to_string()))
        .collect();
    (!answers.is_empty()).then_some(answers)
}

/// The message that answers a set of questions, e.g. after the user (or auto mode) chose.
pub fn answers_message(answers: &[(Question, String)], auto: bool) -> String {
    let mut message = String::from(if auto {
        "Answers (chosen automatically by Monaged's auto mode, using your recommended options):\n"
    } else {
        "Answers from the developer:\n"
    });
    for (i, (question, answer)) in answers.iter().enumerate() {
        message.push_str(&format!("{}. {} → {}\n", i + 1, question.question, answer));
    }
    message.push_str("\nContinue with these answers.");
    message
}

pub fn nudge_message(role: &Role) -> String {
    format!(
        "You ended your turn without the required ```{}``` block. Finish now: end your reply with that block exactly as \
         described in your brief (valid JSON inside the fence). If you're blocked, ask with a ```monaged-question``` block instead.",
        role.result_block().unwrap_or("monaged-done")
    )
}

// ---- Gates --------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum Gate {
    Auto(String),
    Ask(String),
}

/// After verification: continue to planning automatically only in auto mode
/// and when no verifier found major issues.
pub fn decide_gate(verifications: &[Value], auto_mode: bool) -> Gate {
    let major = verifications.iter().filter(|v| v["verdict"] == "major_issues").count();
    if major > 0 {
        return Gate::Ask(format!("{major} verifier{} found major issues in the research", if major == 1 { "" } else { "s" }));
    }
    if !auto_mode {
        return Gate::Ask("Auto mode is off, so you decide whether to build this".into());
    }
    Gate::Auto("Both verifiers confirmed the research".into())
}

/// Small, low-risk, confident plans may start on their own (when the project
/// allows it and auto mode is on). Everything else waits for the developer.
pub fn plan_gate(plan: &Value, project_allows_auto: bool, auto_mode: bool) -> Gate {
    let size = plan["size"].as_str().unwrap_or("large");
    let confidence = plan["confidence"].as_u64().unwrap_or(0);
    let risks: Vec<&str> = plan["risk_flags"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !risks.is_empty() {
        return Gate::Ask(format!("Risky change ({}): read the plan first", risks.join(", ")));
    }
    if size != "small" {
        return Gate::Ask(format!("A {size} feature: read the plan first"));
    }
    if confidence < 4 {
        return Gate::Ask(format!("The planner's confidence is {confidence}/5"));
    }
    if !auto_mode {
        return Gate::Ask("Auto mode is off, so plans wait for your approval".into());
    }
    if !project_allows_auto {
        return Gate::Ask("This project always asks before building".into());
    }
    Gate::Auto(format!("Small, no risk flags, confidence {confidence}/5"))
}

#[derive(Clone, Debug)]
pub struct Workstream {
    pub name: String,
    pub value: Value,
}

pub fn workstreams(plan: &Value) -> Vec<Workstream> {
    plan["workstreams"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_BUILDERS)
        .enumerate()
        .map(|(i, w)| Workstream {
            name: w["name"].as_str().map(crate::git::slug).unwrap_or_else(|| format!("part-{}", i + 1)),
            value: w.clone(),
        })
        .collect()
}

// ---- Briefs -------------------------------------------------------------------

const QUESTION_FORMAT: &str = "To ask the developer something you can't find out from the code, put each question in its \
own block, then stop and wait for the answers (at most 4 per turn; they're shown together, and the developer answers \
them all at once):\n\n\
```monaged-question\n\
{\"header\": \"Storage\", \"question\": \"...?\", \"why\": \"how the answer changes the work\", \"options\": [{\"label\": \"...\", \"description\": \"what choosing it means\"}, {\"label\": \"...\", \"description\": \"...\"}], \"recommended\": 0}\n\
```\n\n\
`header` is a topic of a word or two; give 2 to 4 options, each with a short label and a one-line description of its \
consequence; `recommended` is the index of the option you'd pick; add `\"multi\": true` when several can apply. The \
developer can always answer in their own words instead. Good questions raise everyone's confidence; don't ask what the \
code already tells you.";

/// How builders work together through Monaged's MCP tools (Phase 4).
const COORDINATION: &str = "## Working with the other builders\n\
You have Monaged's coordination tools (`board_read`, `board_post`, `ask`, `claim_files`, from the `monaged` MCP server). \
Use them sparingly; every call costs tokens:\n\
- Call `board_read` once before you start.\n\
- As soon as you settle something another workstream depends on (an endpoint's shape, a type, a function signature, a \
file name), post it with `board_post` (kind \"contract\", with `paths`). Post decisions others should know as kind \"decision\".\n\
- Before you edit a file your workstream doesn't own, call `claim_files`. If it warns you, don't edit it: `ask` the owner \
or list the change under `needs_from_others`.\n\
- `ask` another builder only when you're truly blocked; an answer can take minutes. Usually it's better to make a \
sensible assumption, post it as a decision, and carry on.\n\
- New contracts and decisions from the other builders arrive with your next message from Monaged. In a long \
turn, call `board_read` again before you finish, in case someone posted something you depend on.";

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

pub fn researcher_brief(request: &str) -> String {
    format!(
        "{BRIEF_HEADER} Researcher\n\n\
         You are the RESEARCHER for a new feature in this repository, working in your own git worktree. Monaged (the app \
         coordinating several agents) shows your output to the developer and to the agents after you.\n\n\
         ## Feature request\n{request}\n\n\
         ## Your job\nFind out what this feature needs: read the code, the structure, existing patterns, and what it \
         touches. Do not change any files and do not commit.\n\n{QUESTION_FORMAT}\n\n\
         ## When you're done\nEnd your reply with exactly one block:\n\n\
         ```monaged-research\n\
         {{\"summary\": \"3-5 sentences\", \"findings\": [\"concrete facts, with file paths\"], \"affected_areas\": [\"paths or modules\"], \
         \"risks\": [\"...\"], \"open_questions\": [\"...\"]}}\n\
         ```"
    )
}

pub fn verifier_brief(number: u8, request: &str, research: &Value) -> String {
    format!(
        "{BRIEF_HEADER} Verifier {number}\n\n\
         You are VERIFIER {number} of 2 for a new feature, working in your own git worktree. Another agent researched the \
         feature; check its report against the actual code. Be skeptical: look for wrong claims, missed files, and risks \
         it underplays. Do not change any files.\n\n\
         ## Feature request\n{request}\n\n## Research to verify\n```json\n{}\n```\n\n\
         ## When you're done\nEnd your reply with exactly one block:\n\n\
         ```monaged-verification\n\
         {{\"verdict\": \"confirmed | minor_issues | major_issues\", \"corrections\": [\"finding X is wrong because ...\"], \
         \"missed\": [\"important things the research missed\"], \"confidence\": 1-5}}\n\
         ```",
        pretty(research)
    )
}

pub fn planner_brief(request: &str, research: &Value, verifications: &[Value]) -> String {
    format!(
        "{BRIEF_HEADER} Planner\n\n\
         You are the PLANNER for a new feature, working in your own git worktree. Using the research and its verification \
         below, write the implementation plan. The developer may approve it before builders start. Do not change any files.\n\n\
         ## Feature request\n{request}\n\n## Research\n```json\n{}\n```\n\n## Verification\n```json\n{}\n```\n\n\
         {QUESTION_FORMAT}\n\n\
         ## How to plan\n\
         - Split the work into workstreams that can run **in parallel**, each done by one builder agent (max {MAX_BUILDERS}; \
         use 1 for small features). Each workstream owns specific paths; two workstreams must never edit the same files.\n\
         - Define the contracts between workstreams (API shapes, data models, function signatures) so builders can work \
         without talking to each other.\n\
         - size: small (one workstream, roughly an hour), medium, or large.\n\
         - risk_flags: any of \"db_migration\", \"auth\", \"data_loss\", \"public_api\", \"infra\", \"security\".\n\
         - confidence: 1-5, honest. Questions that would raise it are worth asking first.\n\n\
         ## When you're done\nEnd your reply with exactly one block:\n\n\
         ```monaged-plan\n\
         {{\"title\": \"...\", \"summary\": \"...\", \"size\": \"small\", \"risk_flags\": [], \"confidence\": 4, \
         \"confidence_reason\": \"...\", \"contracts\": [\"...\"], \"workstreams\": [{{\"name\": \"backend-api\", \"goal\": \"...\", \
         \"owns\": [\"src/api/**\"], \"steps\": [\"...\"], \"depends_on\": []}}], \"tests\": [\"how we'll know it works\"]}}\n\
         ```",
        pretty(research),
        pretty(&Value::Array(verifications.to_vec())),
    )
}

pub fn builder_brief(request: &str, plan: &Value, workstream: &Workstream, branch: &str) -> String {
    format!(
        "{BRIEF_HEADER} Builder ({name})\n\n\
         You are a BUILDER for workstream \"{name}\" of the plan below, working in your own git worktree on branch \
         `{branch}`. Other builders handle the other workstreams at the same time.\n\n\
         ## Rules\n\
         - Only edit the files your workstream owns (see `owns`). If you need a change elsewhere, don't make it: list it \
         under `needs_from_others` in your result.\n\
         - Follow the plan's contracts exactly; other builders rely on them.\n\
         - Run the tests you can. Commit your work on your branch with clear messages. Don't push.\n\
         - {REVIEWED}\n\n\
         {COORDINATION}\n\n\
         {QUESTION_FORMAT}\n\n\
         ## Feature request\n{request}\n\n## Plan\n```json\n{}\n```\n\n## Your workstream\n```json\n{}\n```\n\n\
         ## When you're done\nEnd your reply with exactly one block:\n\n\
         ```monaged-done\n\
         {{\"summary\": \"...\", \"files\": [\"...\"], \"tests_run\": \"...\", \"needs_from_others\": [], \"confidence\": 1-5}}\n\
         ```",
        pretty(plan),
        pretty(&workstream.value),
        name = workstream.name,
    )
}

/// What builders and the integrator are told about the quality gate.
const REVIEWED: &str = "When you report done, Monaged runs the project's tests and a skeptical review of your diff. \
Your work only counts as done with a high score; otherwise you get the findings back to fix. Check each finding \
against the code: fix the real ones, say why when one isn't.";

pub fn integrator_brief(request: &str, plan: &Value, branches: &[String], feature_branch: &str) -> String {
    let list = branches.iter().map(|b| format!("- `{b}`")).collect::<Vec<_>>().join("\n");
    format!(
        "{BRIEF_HEADER} Integrator\n\n\
         You are the INTEGRATOR. The builders finished their workstreams on these branches:\n{list}\n\n\
         Your worktree is on the feature branch `{feature_branch}`. Merge each builder branch into it, resolve conflicts \
         keeping both sides' intent, make sure the plan's contracts line up across the pieces, run the project's tests, \
         and commit. Don't push and don't merge into any other branch. {REVIEWED}\n\n\
         Start with `board_read` (all=true, a Monaged tool): the builders posted their contracts, decisions and answers \
         there, and anything they agreed on overrides the plan.\n\n\
         ## Feature request\n{request}\n\n## Plan\n```json\n{}\n```\n\n\
         ## When you're done\nEnd your reply with exactly one block:\n\n\
         ```monaged-done\n\
         {{\"summary\": \"...\", \"files\": [\"...\"], \"tests_run\": \"...\", \"needs_from_others\": [], \"confidence\": 1-5}}\n\
         ```",
        pretty(plan),
    )
}

/// The short version of a plan for the pipeline note.
pub fn plan_title(plan: &Value) -> String {
    plan["title"].as_str().unwrap_or("Plan").to_string()
}

pub fn empty_verifications() -> Vec<Value> {
    vec![Value::Null, Value::Null]
}

pub fn to_json(values: &[Value]) -> String {
    json!(values).to_string()
}
