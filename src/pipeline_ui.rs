//! Phase 3: running the pipeline (what happens when a role agent finishes a
//! turn), the gates, question cards, and the pipeline panel.

use gpui::{AnyElement, Context, Focusable, FontWeight, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::agent::{Item, Pick, Tone};
use crate::pipeline::{self, Directive, Gate, Pipeline, Question, Role, Segment, Stage};
use crate::icons::{Icon, icon};
use crate::review_ui::render_markdown;
use crate::review;
use crate::ui::Kind;
use crate::{AgentSpec, Monaged, button, dots, git, theme, tone_color, ui};

/// The plan's workstreams, or one for the whole plan when it names none.
pub(crate) fn plan_streams(plan: &Value) -> Vec<pipeline::Workstream> {
    let mut streams = pipeline::workstreams(plan);
    if streams.is_empty() {
        streams.push(pipeline::Workstream { name: "all".into(), value: serde_json::json!({ "name": "all", "goal": "the whole plan" }) });
    }
    streams
}

impl Monaged {
    pub(crate) fn unique_agent_name(&self, sub_id: i64, base: &str) -> String {
        let taken = |name: &str| self.agents.iter().any(|a| a.record.subproject_id == sub_id && a.record.name == name);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}-{n}")).find(|name| !taken(name)).unwrap_or_else(|| base.to_string())
    }

    pub(crate) fn save_pipeline(&mut self, sub_id: i64) {
        if let Some(p) = self.pipelines.get(&sub_id)
            && let Err(err) = self.store.save_pipeline(p)
        {
            eprintln!("[app] could not save pipeline: {err:#}");
        }
        // Each new stage goes into the timeline once.
        if let Some(p) = self.pipelines.get(&sub_id) {
            let stage = p.stage.label().to_string();
            if self.logged_stage.get(&sub_id) != Some(&stage) {
                let _ = self.store.log_event(sub_id, None, "stage", &format!("Pipeline: {stage}"));
                self.logged_stage.insert(sub_id, stage);
            }
        }
    }

    fn set_note(&mut self, sub_id: i64, note: impl Into<String>) {
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.note = Some(note.into());
        }
        self.save_pipeline(sub_id);
    }

    fn project_of(&self, sub_id: i64) -> Option<&crate::store::Project> {
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        self.projects.iter().find(|p| p.id == sub.project_id)
    }

    pub(crate) fn project_auto_start(&self, project_id: i64) -> bool {
        self.store.setting(&format!("project:{project_id}:auto_start")).is_none_or(|v| v == "1")
    }

    pub(crate) fn toggle_project_auto_start(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let value = !self.project_auto_start(project_id);
        let _ = self.store.set_setting(&format!("project:{project_id}:auto_start"), if value { "1" } else { "0" });
        cx.notify();
    }

    pub(crate) fn toggle_auto_mode(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.auto_mode = !p.auto_mode;
        }
        self.save_pipeline(sub_id);
        cx.notify();
    }

    pub(crate) fn role_spec(name: &str, role: Role, brief: String, select: bool) -> AgentSpec {
        let on_feature_branch = role == Role::Integrator;
        AgentSpec { name: name.into(), role, on_feature_branch, brief: Some(brief), select, merge_from: Vec::new() }
    }

    // ---- Stage transitions ---------------------------------------------------

    pub(crate) fn start_pipeline(&mut self, sub_id: i64, request: String, auto_mode: bool, cx: &mut Context<Self>) {
        self.start_pipeline_with(sub_id, request, auto_mode, false, cx);
    }

    /// `autopilot`: the run goes all the way to develop by itself (the project chat).
    pub(crate) fn start_pipeline_with(&mut self, sub_id: i64, request: String, auto_mode: bool, autopilot: bool, cx: &mut Context<Self>) {
        let mut p = Pipeline::new(sub_id, request.clone(), auto_mode && !autopilot);
        p.autopilot = autopilot;
        p.verifications = pipeline::empty_verifications();
        p.note = Some("The researcher is reading the code.".into());
        self.pipelines.insert(sub_id, p);
        self.save_pipeline(sub_id);
        self.mark_pipeline_started(sub_id);
        self.spawn_agent(sub_id, Self::role_spec("research", Role::Researcher, pipeline::researcher_brief(&request), false), cx);
        // The lead oversees the run; you talk to it on the dashboard. On
        // autopilot it's the conversation itself, so it always starts.
        if autopilot || self.lead_enabled() {
            self.start_lead(sub_id, cx);
        }
        cx.notify();
    }

    /// Whether pipelines start a lead (Settings → Agents).
    pub(crate) fn lead_enabled(&self) -> bool {
        self.store.setting("lead:auto").as_deref() != Some("0")
    }

    pub(crate) fn start_verification(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        let (request, research) = (p.request.clone(), p.research.clone().unwrap_or_default());
        p.stage = Stage::Verify;
        p.verifications = pipeline::empty_verifications();
        p.note = Some("Two verifiers are checking the research against the code.".into());
        self.save_pipeline(sub_id);
        for n in 1..=2u8 {
            let brief = pipeline::verifier_brief(n, &request, &research);
            self.spawn_agent(sub_id, Self::role_spec(&format!("verifier-{n}"), Role::Verifier(n), brief, false), cx);
        }
    }

    pub(crate) fn start_planning(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        let brief = pipeline::planner_brief(&p.request, &p.research.clone().unwrap_or_default(), &p.verifications);
        p.stage = Stage::Plan;
        p.note = Some("The planner is writing the plan.".into());
        self.save_pipeline(sub_id);
        self.spawn_agent(sub_id, Self::role_spec("planner", Role::Planner, brief, false), cx);
        cx.notify();
    }

    /// Checks that builders can commit, then starts one builder per workstream.
    pub(crate) fn start_build(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(project) = self.project_of(sub_id).cloned() else { return };
        // A second click while git is being checked doesn't start a second set.
        if !self.starting_build.insert(sub_id) {
            return;
        }
        self.set_note(sub_id, "Checking that builders can commit…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let ok = cx.background_spawn(async move { git::identity_ok(&project.distro, &project.path) }).await.unwrap_or(false);
            view.update(cx, |this, cx| {
                this.starting_build.remove(&sub_id);
                this.identity_missing = !ok;
                if ok {
                    this.launch_builders(sub_id, cx);
                } else {
                    if let Some(p) = this.pipelines.get_mut(&sub_id) {
                        p.stage = Stage::Approve;
                    }
                    this.set_note(
                        sub_id,
                        "Builders commit their work, but git inside WSL has no name/email yet. Copy your Windows git identity below, then approve again.",
                    );
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn launch_builders(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        if p.stage != Stage::Approve || p.plan.is_none() {
            return;
        }
        p.stage = Stage::Build;
        p.done.clear();
        self.save_pipeline(sub_id);
        // This run's builders are the agents made from now on.
        let newest = self.agents.iter().map(|a| a.record.id).max().unwrap_or(0);
        let _ = self.store.set_setting(&format!("pipeline:{sub_id}:build_from"), &newest.to_string());
        let started = self.launch_ready_builders(sub_id, cx);
        let total = self.pipelines.get(&sub_id).and_then(|p| p.plan.as_ref()).map_or(0, |plan| plan_streams(plan).len());
        let waiting = total.saturating_sub(started.len());
        let note = if waiting == 0 {
            format!("{} builder{} working in parallel.", started.len(), if started.len() == 1 { " is" } else { "s are" })
        } else {
            format!(
                "{} builder{} working; {waiting} more start{} once the work they build on passes review.",
                started.len(),
                if started.len() == 1 { " is" } else { "s are" },
                if waiting == 1 { "s" } else { "" }
            )
        };
        self.set_note(sub_id, note);
    }

    /// This build run's builders, as (workstream, agent name, branch). Runs
    /// from before builders waited for each other count every builder.
    fn run_builders(&self, sub_id: i64) -> Vec<(String, String, String)> {
        let from: i64 = self.store.setting(&format!("pipeline:{sub_id}:build_from")).and_then(|v| v.parse().ok()).unwrap_or(0);
        self.current_builders(sub_id)
            .iter()
            .filter(|a| a.record.id > from)
            .filter_map(|a| match a.role() {
                Role::Builder(stream) => Some((stream, a.record.name.clone(), a.record.branch.clone())),
                _ => None,
            })
            .collect()
    }

    /// Starts every workstream whose dependencies passed the gate, with their
    /// work merged into its branch. Returns the workstreams it started.
    pub(crate) fn launch_ready_builders(&mut self, sub_id: i64, cx: &mut Context<Self>) -> Vec<String> {
        // Runs from before builders waited for each other started them all at once.
        if self.store.setting(&format!("pipeline:{sub_id}:build_from")).is_none() {
            return Vec::new();
        }
        let Some(p) = self.pipelines.get(&sub_id) else { return Vec::new() };
        let Some(plan) = p.plan.clone().filter(|_| p.stage == Stage::Build) else { return Vec::new() };
        let (request, done_names) = (p.request.clone(), p.done.clone());
        let streams = plan_streams(&plan);
        let run = self.run_builders(sub_id);
        let mut started: Vec<String> = run.iter().map(|(s, ..)| s.clone()).collect();
        started.extend(self.starting_streams.iter().filter(|(id, _)| *id == sub_id).map(|(_, s)| s.clone()));
        let done: Vec<String> = run.iter().filter(|(_, n, _)| done_names.contains(n)).map(|(s, ..)| s.clone()).collect();
        let ready = pipeline::ready_streams(&streams, &started, &done);
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return Vec::new() };
        let sub_slug = git::slug(&sub.name);
        let mut used: Vec<String> = Vec::new();
        for stream in &ready {
            let mut name = self.unique_agent_name(sub_id, &format!("build-{}", stream.name));
            while used.contains(&name) {
                name = format!("{name}-x");
            }
            used.push(name.clone());
            let branch = format!("agent/{sub_slug}/{name}");
            // The reviewed work it builds on.
            let base: Vec<(String, String)> = stream
                .depends_on()
                .iter()
                .filter_map(|d| run.iter().find(|(s, n, _)| s == d && done_names.contains(n)).map(|(s, _, b)| (s.clone(), b.clone())))
                .collect();
            let merged: Vec<String> = base.iter().map(|(s, _)| s.clone()).collect();
            let brief = pipeline::builder_brief(&request, &plan, stream, &branch, &merged);
            let mut spec = Self::role_spec(&name, Role::Builder(stream.name.clone()), brief, false);
            spec.merge_from = base.into_iter().map(|(_, b)| b).collect();
            self.starting_streams.insert((sub_id, stream.name.clone()));
            self.spawn_agent(sub_id, spec, cx);
        }
        ready.into_iter().map(|w| w.name).collect()
    }

    /// The builder doing each workstream now: the newest one (a rerun replaces the old).
    /// Workstreams still waiting for their dependencies have none yet.
    pub(crate) fn current_builders(&self, sub_id: i64) -> Vec<&crate::agent::AgentView> {
        let members = self.pipelines.get(&sub_id).map(|p| p.members.clone()).unwrap_or_default();
        let mut latest: Vec<&crate::agent::AgentView> = Vec::new();
        for agent in self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && (members.is_empty() || members.contains(&a.record.id)))
        {
            let Role::Builder(stream) = agent.role() else { continue };
            match latest.iter_mut().find(|b| b.role() == Role::Builder(stream.clone())) {
                Some(slot) if slot.record.id < agent.record.id => *slot = agent,
                Some(_) => {}
                None => latest.push(agent),
            }
        }
        latest
    }

    pub(crate) fn start_integration(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let branches: Vec<String> = self.current_builders(sub_id).iter().map(|a| a.record.branch.clone()).collect();
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        // The integrator works on the feature branch itself, which only one
        // worktree can have: an existing integrator gets the new brief.
        let existing = self
            .agents
            .iter()
            .rev()
            .find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() == Role::Integrator)
            .map(|a| (a.record.id, a.record.name.clone()));
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        let brief = pipeline::integrator_brief(&p.request, &p.plan.clone().unwrap_or_default(), &branches, &sub.branch);
        p.stage = Stage::Integrate;
        p.note = Some(format!("The integrator is merging {} builder branch(es) into {}.", branches.len(), sub.branch));
        if let Some((id, name)) = existing {
            if !p.members.contains(&id) {
                p.members.push(id);
            }
            p.gate.clear(&name);
            self.save_pipeline(sub_id);
            self.send_prompt(id, brief);
            return;
        }
        self.save_pipeline(sub_id);
        self.spawn_agent(sub_id, Self::role_spec("integrator", Role::Integrator, brief, false), cx);
    }

    pub(crate) fn stop_pipeline(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.stage = Stage::Stopped;
            p.note = Some("Stopped. The agents and their worktrees are still there; you can keep talking to them.".into());
        }
        // Messages waiting on the budget belonged to the pipeline; drop them.
        for agent in self.agents.iter_mut().filter(|a| a.record.subproject_id == sub_id && a.paused) {
            agent.paused = false;
            agent.queued_prompt = None;
            agent.notice(Tone::Info, "Dropped the message that was waiting for the budget (pipeline stopped).");
        }
        self.save_pipeline(sub_id);
        cx.notify();
    }

    pub(crate) fn copy_git_identity(&mut self, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        self.progress("identity", "Copying your Windows git identity into WSL…", cx);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { git::copy_windows_identity(&distro) }).await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.identity_missing = false;
                        this.settle("identity", Tone::Info, "Done: git in WSL now uses the same name and email as on Windows.", cx);
                        let waiting: Vec<i64> = this
                            .pipelines
                            .values()
                            .filter(|p| p.stage == Stage::Approve && p.note.as_deref().is_some_and(|n| n.contains("git inside WSL")))
                            .map(|p| p.subproject_id)
                            .collect();
                        for sub_id in waiting {
                            this.set_note(sub_id, "Git identity is set. Approve the plan to start building.");
                        }
                    }
                    Err(err) => this.settle("identity", Tone::Error, format!("Couldn't copy the git identity: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Reacting to a finished turn -----------------------------------------------

    /// A role agent finished a turn: answer or surface its questions, or take
    /// its result and move the pipeline on.
    pub(crate) fn pipeline_on_turn(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        let role = agent.role();
        // Your own agents and the lead don't move the pipeline on.
        if matches!(role, Role::Free | Role::Lead) {
            return;
        }
        let (sub_id, name) = (agent.record.subproject_id, agent.record.name.clone());
        let found = pipeline::directives(&agent.last_reply());
        let Some(p) = self.pipelines.get(&sub_id) else { return };
        if matches!(p.stage, Stage::Stopped | Stage::Done) {
            return;
        }
        // Agents from an earlier run don't move this one.
        if !(p.members.is_empty() || p.members.contains(&agent_id)) {
            return;
        }
        // Only the stage's own agents deliver results (or get reminded to);
        // a planner asked "why?" during Build doesn't re-plan.
        let expected = p.expects(agent_id, &role);
        let auto = p.auto_mode;
        let autopilot = p.autopilot;

        let questions: Vec<Question> = found.iter().filter_map(|d| if let Directive::Question(q) = d { Some(q.clone()) } else { None }).collect();
        if !questions.is_empty() {
            if auto {
                let answers: Vec<(Question, String)> = questions
                    .iter()
                    .map(|q| (q.clone(), q.recommended_label()))
                    .collect();
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.notice(Tone::Info, format!("Auto mode answered {} question(s) with the recommended option.", answers.len()));
                }
                self.send_prompt(agent_id, pipeline::answers_message(&answers, true));
            } else {
                let n = questions.len();
                self.set_note(sub_id, format!("{name} has {n} question{} for you.", if n == 1 { "" } else { "s" }));
                let asked = questions.iter().map(|q| format!("\"{}\" ({})", q.question, q.labels().join(" / "))).collect::<Vec<_>>().join("; ");
                self.tell_lead(
                    sub_id,
                    if autopilot {
                        crate::autopilot::question_update(&name, &asked)
                    } else {
                        format!("{name} asks the user: {asked}. If the user already told you the answer, give it to {name} with message_agent; otherwise leave it to the user (it's on the dashboard).")
                    },
                );
            }
            return;
        }
        if !expected {
            return;
        }
        if let Some((kind, error)) = found.iter().find_map(|d| if let Directive::Invalid { kind, error } = d { Some((kind.clone(), error.clone())) } else { None }) {
            self.send_prompt(
                agent_id,
                format!("Your ```monaged-{kind}``` block couldn't be read ({error}). Send it again as valid JSON inside the fence."),
            );
            return;
        }
        let result = found.into_iter().find(|d| !matches!(d, Directive::Question(_) | Directive::Invalid { .. } | Directive::Answer(_)));
        if result.is_some()
            && let Some(agent) = self.agent_mut(agent_id)
        {
            agent.nudged = false;
        }

        match (role.clone(), result) {
            (Role::Researcher, Some(Directive::Research(value))) => {
                if let Some(p) = self.pipelines.get_mut(&sub_id) {
                    p.research = Some(value);
                }
                self.start_verification(sub_id, cx);
            }
            (Role::Verifier(n), Some(Directive::Verification(value))) => {
                let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
                let slot = (n as usize).saturating_sub(1);
                if p.verifications.len() <= slot {
                    p.verifications.resize(slot + 1, Value::Null);
                }
                p.verifications[slot] = value;
                if p.verification_count() >= 2 {
                    p.stage = Stage::Decide;
                    let gate = if autopilot { pipeline::autopilot_decide_gate(&p.verifications) } else { pipeline::decide_gate(&p.verifications, auto) };
                    match gate {
                        Gate::Auto(reason) => {
                            p.note = Some(format!("{reason}. Writing the plan."));
                            self.save_pipeline(sub_id);
                            self.start_planning(sub_id, cx);
                        }
                        Gate::Ask(reason) => {
                            p.note = Some(format!("The research is verified. {reason}. Do we build this?"));
                            self.save_pipeline(sub_id);
                            self.tell_lead(sub_id, format!("Research is verified ({reason}); the user decides on the dashboard whether to build it."));
                        }
                    }
                } else {
                    p.note = Some(format!("{name} is done; waiting for the other verifier."));
                    self.save_pipeline(sub_id);
                }
            }
            (Role::Planner, Some(Directive::Plan(value))) => {
                let allows = self.project_of(sub_id).map(|pr| pr.id).is_some_and(|id| self.project_auto_start(id));
                let gate = if autopilot { pipeline::autopilot_plan_gate(&value) } else { self.budget_gate(&value, pipeline::plan_gate(&value, allows, auto)) };
                let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
                let title = pipeline::plan_title(&value);
                p.plan = Some(value);
                p.stage = Stage::Approve;
                match gate {
                    Gate::Auto(reason) => {
                        p.note = Some(format!("\"{title}\": {reason}, so building started on its own."));
                        self.save_pipeline(sub_id);
                        self.tell_lead(sub_id, format!("The plan \"{title}\" is in and building started on its own ({reason})."));
                        self.start_build(sub_id, cx);
                    }
                    Gate::Ask(reason) => {
                        p.note = Some(format!("\"{title}\" is ready for your approval. {reason}."));
                        self.save_pipeline(sub_id);
                        self.tell_lead(sub_id, format!("The plan \"{title}\" waits for the user's approval on the dashboard ({reason})."));
                        if autopilot {
                            self.chime(crate::sound::Cue::NeedsYou);
                        }
                    }
                }
            }
            // "Done" isn't done until a review says so (the quality gate).
            (Role::Builder(_) | Role::Integrator, Some(Directive::Done(_))) => self.gate_submit(sub_id, agent_id, cx),
            (role, _) => self.nudge(agent_id, &role, sub_id),
        }
        cx.notify();
    }

    // ---- The quality gate --------------------------------------------------------------

    /// The lowest score that goes on without asking you (Settings → Reviews).
    pub(crate) fn min_score(&self) -> u8 {
        self.store.setting("review:min_score").and_then(|v| v.parse::<u8>().ok()).map_or(pipeline::DEFAULT_MIN_SCORE, |v| v.clamp(1, 5))
    }

    /// A builder or the integrator says it's done: review what's there now.
    fn gate_submit(&mut self, sub_id: i64, agent_id: i64, cx: &mut Context<Self>) {
        let Some(name) = self.agent(agent_id).map(|a| a.record.name.clone()) else { return };
        let min = self.min_score();
        let reviews_off = self.backend().is_none();
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        if reviews_off {
            p.gate.hold(
                &name,
                format!("Reviews are off (no API key, and the Max plan fallback is off), so Monaged can't check {name}'s work. Turn reviews on in Settings, or accept it as it is."),
            );
            p.note = Some(format!("{name} says it's done, but can't be reviewed. It waits for you."));
            self.save_pipeline(sub_id);
            self.chime(crate::sound::Cue::NeedsYou);
            cx.notify();
            return;
        }
        // Done again means reviewed again, even if it passed before.
        p.done.retain(|n| *n != name);
        p.gate.submit(&name);
        p.note = Some(format!("{name} says it's done. Reviewing it: it needs {min}/5 to go on."));
        self.save_pipeline(sub_id);
        self.review_fresh(agent_id, cx);
    }

    /// Reviews the gate was waiting for when Monaged closed (they ran in the old run).
    pub(crate) fn resume_gates(&mut self, cx: &mut Context<Self>) {
        let waiting: Vec<i64> = self
            .pipelines
            .iter()
            .filter(|(_, p)| !matches!(p.stage, Stage::Done | Stage::Stopped))
            .flat_map(|(sub_id, p)| {
                p.gate.pending.iter().filter_map(|name| self.agents.iter().find(|a| a.record.subproject_id == *sub_id && a.record.name == *name).map(|a| a.record.id)).collect::<Vec<_>>()
            })
            .collect();
        for agent_id in waiting {
            self.review_fresh(agent_id, cx);
        }
        // Builders whose dependencies passed while Monaged was closing.
        let building: Vec<i64> = self.pipelines.iter().filter(|(_, p)| p.stage == Stage::Build).map(|(id, _)| *id).collect();
        for sub_id in building {
            self.launch_ready_builders(sub_id, cx);
        }
    }

    /// Reviews now, or right after the review that's running (which may be of older work).
    pub(crate) fn review_fresh(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        match self.agent_mut(agent_id) {
            Some(agent) if agent.reviewing => agent.review_again = true,
            Some(_) => self.review_agent(agent_id, cx),
            None => {}
        }
    }

    /// A review finished (`score` None: it couldn't be scored, `why` says why).
    pub(crate) fn gate_on_review(&mut self, agent_id: i64, score: Option<u8>, why: String, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        if !matches!(agent.role(), Role::Builder(_) | Role::Integrator) {
            return;
        }
        let (sub_id, name) = (agent.record.subproject_id, agent.record.name.clone());
        let min = self.min_score();
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        if !p.gate.pending.contains(&name) || matches!(p.stage, Stage::Stopped | Stage::Done) {
            return;
        }
        let rounds = p.fix_rounds();
        match p.gate.step(&name, score, &why, min, rounds) {
            pipeline::GateStep::Accept => {
                p.gate.clear(&name);
                self.gate_accept(sub_id, agent_id, false, cx);
            }
            pipeline::GateStep::Fix(round) => {
                p.gate.fix(&name, true);
                let score = score.unwrap_or(0);
                p.note = Some(format!("{name} scored {score}/5, below the {min}/5 needed. Monaged sent it the findings to fix (round {round} of {rounds})."));
                let _ = self.store.log_event(sub_id, Some(agent_id), "gate", &format!("{name}: {score}/5, sent back to fix (round {round})"));
                self.save_pipeline(sub_id);
                self.send_fix(agent_id);
                self.tell_lead(sub_id, format!("{name} scored {score}/5 (the bar is {min}/5); Monaged sent it the findings to fix, round {round} of {rounds}."));
            }
            pipeline::GateStep::Hold(why) => {
                p.gate.hold(&name, why.clone());
                p.note = Some(why.clone());
                let _ = self.store.log_event(sub_id, Some(agent_id), "gate", &format!("{name}: waits for you"));
                self.save_pipeline(sub_id);
                self.chime(crate::sound::Cue::NeedsYou);
                self.tell_lead(sub_id, format!("{why} It waits for the user on the dashboard (fix again, review again or accept it anyway)."));
            }
        }
        cx.notify();
    }

    /// Sends the review's findings back, asking for the result block again.
    fn send_fix(&mut self, agent_id: i64) {
        let Some(c) = self.agent(agent_id).and_then(|a| a.confidence.clone()) else {
            // No score to point at (reviews off, or it couldn't be reviewed).
            let why = self.agent(agent_id).and_then(|a| a.review_error.clone()).unwrap_or_else(|| "it couldn't be reviewed".into());
            self.send_prompt(
                agent_id,
                format!("Monaged couldn't score your work ({why}). Check it yourself: run the tests, finish what's missing, then end with your ```monaged-done``` block again so it's reviewed again."),
            );
            return;
        };
        let findings = review::fix_prompt(&c).unwrap_or_else(|| {
            format!(
                "Monaged's review scored your work {}/5, which isn't enough to integrate it. What it said:\n{}\n\nFinish what's missing.",
                c.score,
                c.reasons.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
            )
        });
        self.send_prompt(agent_id, format!("{findings}\n\nWhen it's fixed, end with your ```monaged-done``` block again so it's reviewed again."));
    }

    /// Work passed the gate (or you accepted it): builders count as done, the
    /// integrator finishes the pipeline.
    fn gate_accept(&mut self, sub_id: i64, agent_id: i64, anyway: bool, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        let (name, role) = (agent.record.name.clone(), agent.role());
        let score = agent.confidence.as_ref().map(|c| c.score);
        let how = match (anyway, score) {
            (true, Some(s)) => format!("was accepted by you at {s}/5"),
            (true, None) => "was accepted by you without a score".into(),
            (false, Some(s)) => format!("passed review at {s}/5"),
            (false, None) => "passed".into(),
        };
        let _ = self.store.log_event(sub_id, Some(agent_id), "gate", &format!("{name}: {how}"));
        if role == Role::Integrator {
            let summary = self.agent(agent_id).and_then(last_done_summary).unwrap_or_else(|| "Integrated.".into());
            let branch = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.branch.clone()).unwrap_or_default();
            if let Some(p) = self.pipelines.get_mut(&sub_id) {
                p.stage = Stage::Done;
                p.note = Some(format!("Done ({how}): {summary} Everything is on {branch}."));
            }
            self.save_pipeline(sub_id);
            self.record_pipeline_run(sub_id);
            self.chime(crate::sound::Cue::Complete);
            self.tell_lead(sub_id, format!("The integrator {how}: {summary} Everything is on {branch}. Tell the user in a sentence."));
            if let Some(project_id) = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id) {
                self.refresh_branches(project_id, cx);
            }
            self.maybe_auto_merge(sub_id, cx);
            return;
        }
        let builders: Vec<String> = self.current_builders(sub_id).iter().map(|a| a.record.name.clone()).collect();
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        if !p.done.contains(&name) {
            p.done.push(name.clone());
        }
        let finished = builders.iter().filter(|b| p.done.contains(b)).count();
        // Workstreams still waiting for their dependencies have no builder yet, but count.
        let total = builders.len().max(p.plan.as_ref().map_or(0, |plan| plan_streams(plan).len()));
        let building = p.stage == Stage::Build;
        if finished >= total && building {
            p.note = Some(format!("{name} {how}. Every builder is through. Integrating."));
            self.save_pipeline(sub_id);
            self.tell_lead(sub_id, format!("{name} {how}. Every builder is through; the integrator is merging their branches."));
            self.start_integration(sub_id, cx);
        } else {
            self.save_pipeline(sub_id);
            // What waited for this work can start now.
            let started = if building { self.launch_ready_builders(sub_id, cx) } else { Vec::new() };
            let next = if started.is_empty() {
                String::new()
            } else {
                format!(" Started the builder{} for {}, on top of the reviewed work.", if started.len() == 1 { "" } else { "s" }, started.join(", "))
            };
            self.set_note(sub_id, format!("{name} {how}. {finished} of {total} builders done.{next}"));
            self.tell_lead(sub_id, format!("{name} {how} ({finished} of {total} builders done).{next}"));
        }
        cx.notify();
    }

    fn gate_agent(&self, sub_id: i64, name: &str) -> Option<i64> {
        self.agents.iter().find(|a| a.record.subproject_id == sub_id && a.record.name == name).map(|a| a.record.id)
    }

    /// You: send the findings back once more.
    pub(crate) fn gate_fix_again(&mut self, sub_id: i64, name: &str, cx: &mut Context<Self>) {
        let Some(agent_id) = self.gate_agent(sub_id, name) else { return };
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.gate.fix(name, false);
            p.note = Some(format!("{name} is fixing the findings."));
        }
        self.save_pipeline(sub_id);
        self.bypass_budget = true;
        self.send_fix(agent_id);
        self.bypass_budget = false;
        cx.notify();
    }

    /// You: review it again as it is now.
    pub(crate) fn gate_review_again(&mut self, sub_id: i64, name: &str, cx: &mut Context<Self>) {
        let Some(agent_id) = self.gate_agent(sub_id, name) else { return };
        self.gate_submit(sub_id, agent_id, cx);
    }

    /// You: go on with it as it is, below the bar.
    pub(crate) fn gate_accept_anyway(&mut self, sub_id: i64, name: &str, cx: &mut Context<Self>) {
        let Some(agent_id) = self.gate_agent(sub_id, name) else { return };
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.gate.clear(name);
        }
        self.gate_accept(sub_id, agent_id, true, cx);
    }

    fn nudge(&mut self, agent_id: i64, role: &Role, sub_id: i64) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let name = agent.record.name.clone();
        if agent.nudged {
            self.set_note(sub_id, format!("{name} finished without delivering its result. Reply to it, or stop the pipeline."));
            // Not a silent wait: it's in Needs you, and the lead hears of it.
            if self.stall.undelivered.insert(agent_id) {
                self.chime(crate::sound::Cue::NeedsYou);
                self.tell_lead(
                    sub_id,
                    format!("{name} finished twice without its result block, so the pipeline waits on it. Ask it with message_agent what's blocking it and to finish, or tell the user if it needs them."),
                );
            }
            return;
        }
        agent.nudged = true;
        self.set_note(sub_id, format!("Reminded {name} to deliver its result."));
        self.send_prompt(agent_id, pipeline::nudge_message(role));
    }

    /// The user picked an option on a question card. Once every question in
    /// that reply has an answer, they're sent together.
    /// The questions in one agent reply, in order.
    pub(crate) fn questions_at(&self, agent_id: i64, item: usize) -> Vec<Question> {
        let Some(Item::Agent(text)) = self.agent(agent_id).and_then(|a| a.items.get(item)) else { return Vec::new() };
        pipeline::directives(text).into_iter().filter_map(|d| if let Directive::Question(q) = d { Some(q) } else { None }).collect()
    }

    /// Your answer to one question so far: None until it's answered (and
    /// "something else" needs words in its box).
    pub(crate) fn answer_text(&self, agent_id: i64, item: usize, qi: usize, q: &Question, cx: &gpui::App) -> Option<String> {
        match self.agent(agent_id)?.answers.get(&(item, qi))? {
            Pick::Options(list) if !list.is_empty() => Some(list.iter().filter_map(|&i| q.options.get(i).map(|o| o.label.clone())).collect::<Vec<_>>().join(", ")),
            Pick::Options(_) => None,
            Pick::Other => {
                let text = self.answer_inputs.get(&(agent_id, item, qi))?.read(cx).text().trim().to_string();
                (!text.is_empty()).then(|| format!("{text} (in my own words)"))
            }
        }
    }

    /// Picks an option (or, on a question that allows several, toggles it).
    pub(crate) fn pick_option(&mut self, agent_id: i64, item: usize, qi: usize, oi: usize, multi: bool, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let entry = agent.answers.entry((item, qi)).or_insert(Pick::Options(Vec::new()));
        match entry {
            Pick::Options(list) if multi => {
                if let Some(at) = list.iter().position(|&i| i == oi) {
                    list.remove(at);
                } else {
                    list.push(oi);
                    list.sort_unstable();
                }
            }
            _ => *entry = Pick::Options(vec![oi]),
        }
        cx.notify();
    }

    /// "Something else": a box for your own words, ready to type in.
    pub(crate) fn pick_other(&mut self, agent_id: i64, item: usize, qi: usize, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.answers.insert((item, qi), Pick::Other);
        }
        let input = self
            .answer_inputs
            .entry((agent_id, item, qi))
            .or_insert_with(|| cx.new(|cx| crate::text_input::TextInput::new("Your answer, in your own words", cx)))
            .clone();
        window.focus(&input.focus_handle(cx));
        cx.notify();
    }

    /// Fills in every question you haven't answered with the agent's recommendation.
    pub(crate) fn use_recommended(&mut self, agent_id: i64, item: usize, cx: &mut Context<Self>) {
        let questions = self.questions_at(agent_id, item);
        let open: Vec<usize> = (0..questions.len()).filter(|&qi| self.answer_text(agent_id, item, qi, &questions[qi], cx).is_none()).collect();
        let Some(agent) = self.agent_mut(agent_id) else { return };
        for qi in open {
            agent.answers.insert((item, qi), Pick::Options(vec![questions[qi].recommended.unwrap_or(0)]));
        }
        cx.notify();
    }

    /// Sends every answer at once (only when each question has one).
    pub(crate) fn send_answers(&mut self, agent_id: i64, item: usize, cx: &mut Context<Self>) {
        let questions = self.questions_at(agent_id, item);
        if questions.is_empty() {
            return;
        }
        let answers: Option<Vec<(Question, String)>> =
            questions.iter().enumerate().map(|(qi, q)| self.answer_text(agent_id, item, qi, q, cx).map(|a| (q.clone(), a))).collect();
        let Some(answers) = answers else {
            self.toast(Tone::Info, "Answer every question first, or use the recommended answers for the rest.", cx);
            return;
        };
        let Some(agent) = self.agent_mut(agent_id) else { return };
        agent.answers.retain(|(i, _), _| *i != item);
        let sub_id = agent.record.subproject_id;
        self.answer_inputs.retain(|(a, i, _), _| !(*a == agent_id && *i == item));
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.note = None;
        }
        self.save_pipeline(sub_id);
        self.bypass_budget = true;
        self.send_prompt(agent_id, pipeline::answers_message(&answers, false));
        self.bypass_budget = false;
        self.focus_request = Some(agent_id);
        cx.notify();
    }

    /// The "something else" box that has the keyboard: (agent, reply item).
    pub(crate) fn focused_answer_box(&self, window: &gpui::Window, cx: &gpui::App) -> Option<(i64, usize)> {
        self.answer_inputs.iter().find(|(_, input)| input.focus_handle(cx).is_focused(window)).map(|((agent, item, _), _)| (*agent, *item))
    }

    // ---- Rendering --------------------------------------------------------------------

    /// True when the pipeline waits for the user (a gate, or a question).
    pub(crate) fn pipeline_needs_you(&self, sub_id: i64) -> bool {
        self.pipelines.get(&sub_id).is_some_and(|p| {
            matches!(p.stage, Stage::Decide | Stage::Approve)
                || !p.gate.held.is_empty()
                || p.note.as_deref().is_some_and(|n| n.contains("question"))
        })
    }

    /// The inspector's Pipeline tab: where the pipeline is, what each stage
    /// produced, and the decision waiting for you (if any).
    pub(crate) fn render_pipeline_inspector(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(p) = self.pipelines.get(&sub_id) else {
            return div()
                .flex()
                .flex_col()
                .child(
                    ui::panel_section("Pipeline")
                        .child(div().text_size(px(13.)).line_height(px(19.)).text_color(rgb(theme::text_2())).child(
                            "No pipeline has run here yet. It researches the request, has two agents verify that, writes a plan, then builds it with agents in parallel.",
                        ))
                        .child(div().flex().child(
                            ui::button("insp-run-pipeline", ui::Kind::Primary, Some(Icon::Play), "Run pipeline")
                                .on_click(cx.listener(move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx))),
                        )),
                )
                .children(self.render_board_sections(sub_id, false, cx))
                .into_any_element();
        };
        let stopped = p.stage == Stage::Stopped;
        let finished = p.stage == Stage::Done;
        let current = p.stage.index();
        let waiting = matches!(p.stage, Stage::Decide | Stage::Approve) || self.pipeline_needs_you(sub_id);
        let builders = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && matches!(a.role(), Role::Builder(_))).count();

        // Where it is: a big dot-matrix step count, the stage, and the note.
        let step = if finished { pipeline::STAGES.len() } else { (current + 1).min(pipeline::STAGES.len()) };
        let headline = div()
            .flex()
            .items_end()
            .gap(px(12.))
            .child(dots::dot_text(&format!("{step}/{}", pipeline::STAGES.len()), 4., rgb(if waiting && !stopped { theme::red() } else { theme::text() })))
            .child(div().pb(px(1.)).text_size(px(13.)).text_color(rgb(theme::text_2())).child(if stopped {
                "Stopped".to_string()
            } else if finished {
                "Done".to_string()
            } else {
                p.stage.label().to_string()
            }));
        let note = p.note.clone().map(|n| div().text_size(px(12.5)).line_height(px(18.)).text_color(rgb(if waiting { theme::red_text() } else { theme::text_3() })).child(n));

        let meta_for = |stage: Stage| -> String {
            match stage {
                Stage::Verify => format!("{} of 2", p.verification_count()),
                Stage::Plan => p.plan.as_ref().map(|pl| format!("{} · {}/5", pl["size"].as_str().unwrap_or("?"), pl["confidence"].as_u64().unwrap_or(0))).unwrap_or_default(),
                Stage::Decide | Stage::Approve if p.auto_mode => "auto".into(),
                Stage::Decide | Stage::Approve => "you".into(),
                Stage::Build if builders > 0 => format!("{} / {builders}", p.done.len().min(builders)),
                _ => String::new(),
            }
        };
        let steps = div().flex().flex_col().gap(px(10.)).children(pipeline::STAGES.iter().enumerate().map(|(i, stage)| {
            let done = finished || i < current;
            let now = i == current && !finished && !stopped;
            let mark: AnyElement = if done {
                icon(Icon::Check).size(px(14.)).text_color(rgb(theme::text_3())).into_any_element()
            } else if now {
                div()
                    .size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(ui::status_dot(("stage-now", i), if waiting { ui::State::NeedsYou } else { ui::State::Working }))
                    .into_any_element()
            } else {
                div()
                    .size(px(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().size(px(9.)).rounded_full().border_1().border_color(rgb(theme::text_4())))
                    .into_any_element()
            };
            let stage = *stage;
            let rerunnable = done && matches!(stage, Stage::Research | Stage::Verify | Stage::Plan | Stage::Build | Stage::Integrate) && !p.done.is_empty() || (done && matches!(stage, Stage::Research | Stage::Verify | Stage::Plan));
            div()
                .id(("stage-row", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(24.))
                .text_size(px(13.))
                .text_color(rgb(if now { theme::text() } else if done { theme::text_3() } else { theme::text_2() }))
                .group("stage-row")
                .child(mark)
                .child(div().flex_1().child(stage.label()))
                .child(ui::mono(if done || now { meta_for(stage) } else { String::new() }))
                .when(!rerunnable, |d| d.child(div().flex_none().size(px(22.))))
                .when(rerunnable, |d| {
                    d.child(
                        div()
                            .id(("stage-rerun", i))
                            .flex_none()
                            .invisible()
                            .group_hover("stage-row", |s| s.visible())
                            .size(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::hover())))
                            .tooltip(crate::shell::tip("Run this stage again"))
                            .child(icon(Icon::Refresh).size(px(13.)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_confirm(crate::feedback::ConfirmAction::Rerun(sub_id, stage), window, cx)
                            })),
                    )
                })
        }));

        let gate: Option<AnyElement> = match p.stage {
            Stage::Decide => Some(self.render_decide_card(p, sub_id, cx)),
            Stage::Approve => p.plan.as_ref().map(|plan| self.render_plan_card(plan, sub_id, true, cx)),
            Stage::Build | Stage::Integrate => Some(self.render_build_card(p, sub_id, cx)),
            _ => None,
        };
        let spent = self.sub_costs.get(&sub_id).copied().unwrap_or(0.0);
        let cap = self.sub_cap(sub_id);
        // The plan and what it costs, in one place.
        let budget = ui::panel_section(if p.plan.is_some() { "Plan and budget" } else { "Budget" })
            .children(p.plan.as_ref().map(|plan| div().text_size(px(13.)).text_color(rgb(theme::text())).child(pipeline::plan_title(plan))))
            .children(p.plan.as_ref().map(|plan| {
                let (cost, _, pct) = self.plan_estimate(plan);
                kv("Estimate", format!("${cost:.0} · ≈{pct:.1}% of week"))
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .min_h(px(24.))
                    .child(div().text_size(px(12.5)).text_color(rgb(theme::text_3())).child("This week"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::mono(match cap {
                                Some(c) => format!("${spent:.2} of ${c:.0} cap"),
                                None => format!("${spent:.2} · no cap"),
                            }))
                            .child(
                                ui::menu_chip("cap-edit", if cap.is_some() { "change" } else { "set a cap" })
                                    .on_click(cx.listener(move |this, _, window, cx| this.ask_sub_cap(sub_id, window, cx))),
                            ),
                    ),
            )
            .children(cap.map(|c| {
                let ratio = (spent / c).clamp(0.0, 1.0) as f32;
                div().h(px(4.)).rounded_full().bg(rgb(theme::line_strong())).child(div().h_full().rounded_full().w(gpui::relative(ratio)).bg(rgb(if ratio >= 1.0 { theme::red() } else { theme::text_2() })))
            }));
        let board = self.render_board_sections(sub_id, matches!(p.stage, Stage::Build | Stage::Integrate), cx);

        div()
            .flex()
            .flex_col()
            .child(ui::panel_section("Stage").child(headline).children(note))
            .when(self.identity_missing, |d| {
                d.child(div().px(px(16.)).py(px(12.)).border_b_1().border_color(rgb(theme::line())).child(
                    ui::button("copy-identity", ui::Kind::Primary, Some(Icon::User), "Copy my Windows git identity into WSL")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_git_identity(cx))),
                ))
            })
            .children(gate.map(|g| div().px(px(12.)).py(px(12.)).border_b_1().border_color(rgb(theme::line())).child(g)))
            .child(div().px(px(16.)).py(px(14.)).border_b_1().border_color(rgb(theme::line())).child(steps))
            .children(board)
            .child(budget)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(16.))
                    .py(px(14.))
                    .child(ui::switch("auto-mode", "Auto mode: recommended answers, confident small plans start by themselves", p.auto_mode).on_click(cx.listener(move |this, _, _, cx| this.toggle_auto_mode(sub_id, cx))))
                    .child(div().flex().gap(px(8.)).map(|d| {
                        if matches!(p.stage, Stage::Done | Stage::Stopped) {
                            d.child(
                                ui::button("run-again", ui::Kind::Secondary, Some(Icon::Play), "Run again")
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx))),
                            )
                        } else {
                            d.child(ui::button("stop-pipeline", ui::Kind::Danger, Some(Icon::Stop), "Stop pipeline").on_click(cx.listener(move |this, _, window, cx| this.ask_confirm(crate::feedback::ConfirmAction::StopPipeline(sub_id), window, cx))))
                        }
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn render_decide_card(&self, p: &Pipeline, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let research = p.research.as_ref();
        let summary = research.and_then(|r| r["summary"].as_str()).unwrap_or("(no summary)").to_string();
        let verdicts = p.verifications.iter().enumerate().filter(|(_, v)| !v.is_null()).map(|(i, v)| {
            let verdict = v["verdict"].as_str().unwrap_or("?").replace('_', " ");
            let confidence = v["confidence"].as_u64().unwrap_or(0) as u8;
            let corrections: Vec<String> = v["corrections"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(v["missed"].as_array().into_iter().flatten())
                .filter_map(|c| c.as_str().map(str::to_owned))
                .take(4)
                .collect();
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().font_weight(FontWeight::MEDIUM).child(format!("Verifier {}", i + 1)))
                        .child(div().text_color(rgb(if verdict.contains("major") { theme::red_text() } else { theme::text_2() })).child(verdict))
                        .child(ui::score_meter(confidence)),
                )
                .children(corrections.into_iter().map(|c| div().pl_3().text_color(rgb(theme::text_3())).child(format!("• {c}"))))
        });
        gate_card("Research verified: do we build this?")
            .child(div().line_height(px(20.)).child(summary))
            .children(verdicts)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(button("decide-go", "Write the plan", theme::PRIMARY_BG).on_click(cx.listener(move |this, _, _, cx| this.start_planning(sub_id, cx))))
                    .child(button("decide-stop", "Don't build this", theme::red_soft()).on_click(cx.listener(move |this, _, _, cx| this.stop_pipeline(sub_id, cx)))),
            )
            .into_any_element()
    }

    pub(crate) fn render_plan_card(&self, plan: &Value, sub_id: i64, actionable: bool, cx: &mut Context<Self>) -> AnyElement {
        let strings = |key: &str| -> Vec<String> { plan[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect() };
        let confidence = plan["confidence"].as_u64().unwrap_or(0) as u8;
        let size = plan["size"].as_str().unwrap_or("?").to_string();
        let risks = strings("risk_flags");
        let streams = pipeline::workstreams(plan);
        let builders = streams.len().max(1);

        let mut header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(div().font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(pipeline::plan_title(plan)))
            .child(pill(&size, false))
            .child(ui::score_meter(confidence));
        for risk in &risks {
            header = header.child(pill(&risk.replace('_', " "), true));
        }

        gate_card("Plan")
            .child(header)
            .children(plan["summary"].as_str().map(|s| div().line_height(px(20.)).child(s.to_string())))
            .children(plan["confidence_reason"].as_str().map(|s| {
                div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(format!("Confidence: {s}"))
            }))
            .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(self.plan_estimate_line(plan)))
            .child(label("Workstreams (one builder each, in parallel)"))
            .children(streams.iter().map(|w| {
                let goal = w.value["goal"].as_str().unwrap_or_default().to_string();
                let owns = w.value["owns"].as_array().into_iter().flatten().filter_map(|o| o.as_str()).collect::<Vec<_>>().join(", ");
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(div().flex_none().font_family(theme::MONO).text_size(px(12.)).child(w.name.clone()))
                            .child(div().flex_1().min_w(px(120.)).text_color(rgb(theme::text_2())).child(goal)),
                    )
                    .when(!owns.is_empty(), |d| d.child(div().pl_3().text_size(px(12.)).text_color(rgb(theme::text_3())).child(format!("owns {owns}"))))
            }))
            .when(!strings("contracts").is_empty(), |d| {
                d.child(label("Contracts")).children(strings("contracts").into_iter().map(|c| div().pl_3().child(format!("• {c}"))))
            })
            .when(!strings("tests").is_empty(), |d| {
                d.child(label("How we'll know it works")).children(strings("tests").into_iter().map(|t| div().pl_3().child(format!("• {t}"))))
            })
            .when(actionable, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(
                            button(
                                "plan-approve",
                                if self.pipelines.get(&sub_id).is_some_and(|p| p.autopilot) {
                                    let target = self.merge_target(sub_id).unwrap_or_else(|| "develop".into());
                                    format!("Approve: build and merge into {target}")
                                } else {
                                    format!("Approve and build ({builders} builder{})", if builders == 1 { "" } else { "s" })
                                },
                                theme::PRIMARY_BG,
                            )
                                .on_click(cx.listener(move |this, _, _, cx| this.start_build(sub_id, cx))),
                        )
                        .child(
                            ui::button("plan-edit", ui::Kind::Secondary, Some(Icon::Edit), "Edit plan")
                                .on_click(cx.listener(move |this, _, window, cx| this.open_plan_editor(sub_id, window, cx))),
                        )
                        .child(button("plan-stop", "Stop", theme::red_soft()).on_click(cx.listener(move |this, _, _, cx| this.stop_pipeline(sub_id, cx)))),
                )
                .child(
                    div().text_size(px(12.)).text_color(rgb(theme::text_3())).child("Edit it yourself, or reply to the planner: its new plan replaces this one."),
                )
            })
            .into_any_element()
    }

    fn render_build_card(&self, p: &Pipeline, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && matches!(a.role(), Role::Builder(_) | Role::Integrator))
            .map(|a| {
                let name = &a.record.name;
                let done = p.done.contains(name) || (a.role() == Role::Integrator && p.stage == Stage::Done);
                let (state, word) = if done {
                    (ui::State::Done, "passed")
                } else if p.gate.held_reason(name).is_some() {
                    (ui::State::NeedsYou, "needs you")
                } else if p.gate.pending.contains(name) {
                    (ui::State::Working, "in review")
                } else if p.gate.fixing.contains(name) {
                    (ui::State::Working, "fixing")
                } else if a.is_working() {
                    (ui::State::Working, "working")
                } else {
                    (ui::State::Idle, "waiting")
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::status_dot(("build-dot", a.record.id as usize), state))
                    .child(div().flex_1().font_family(theme::MONO).text_size(px(12.)).child(a.record.name.clone()))
                    .child(ui::mono(word))
                    .children(a.confidence.as_ref().map(|c| ui::score_meter(c.score)))
                    .when(!a.is_working() && matches!(a.role(), Role::Builder(_)), |d| {
                        let workstream = match a.role() {
                            Role::Builder(w) => w,
                            _ => String::new(),
                        };
                        d.child(
                            ui::icon_button(("rerun-builder", a.record.id as usize), Icon::Refresh)
                                .tooltip(crate::shell::tip("Start a new builder for this workstream"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.ask_confirm(crate::feedback::ConfirmAction::RerunBuilder(sub_id, workstream.clone()), window, cx)
                                })),
                        )
                    })
            });
        // Workstreams that start once what they build on has passed review.
        let waiting: Vec<(String, Vec<String>)> = match (&p.plan, p.stage) {
            (Some(plan), Stage::Build) if self.store.setting(&format!("pipeline:{sub_id}:build_from")).is_some() => {
                let run = self.run_builders(sub_id);
                plan_streams(plan)
                    .into_iter()
                    .filter(|w| !run.iter().any(|(s, ..)| *s == w.name) && !self.starting_streams.contains(&(sub_id, w.name.clone())))
                    .map(|w| {
                        let open = w.depends_on().into_iter().filter(|d| !run.iter().any(|(s, n, _)| s == d && p.done.contains(n))).collect();
                        (w.name, open)
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let waiting_rows = waiting.into_iter().enumerate().map(|(i, (stream, open))| {
            let after = if open.is_empty() { "starting".to_string() } else { format!("after {}", open.join(", ")) };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ui::status_dot(("build-wait-dot", i), ui::State::Idle))
                .child(div().flex_1().font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::text_3())).child(format!("build-{stream}")))
                .child(ui::mono(after))
        });
        gate_card("Builders")
            .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(format!("Each needs a review of {}/5 or better before it's integrated.", self.min_score())))
            .children(rows)
            .children(waiting_rows)
            .children(p.gate.held.iter().enumerate().map(|(i, (name, why))| self.render_held(sub_id, i, name, why, cx)))
            .into_any_element()
    }

    /// Work below the bar that waits for you, with what you can do about it.
    pub(crate) fn render_held(&self, sub_id: i64, i: usize, name: &str, why: &str, cx: &mut Context<Self>) -> AnyElement {
        let (fix, again, anyway) = (name.to_string(), name.to_string(), name.to_string());
        let agent_id = self.gate_agent(sub_id, name);
        let findings = agent_id.and_then(|id| self.agent(id)).and_then(|a| a.confidence.as_ref()).map_or(0, |c| c.issues.len());
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .child(div().text_size(px(13.)).text_color(rgb(theme::text_body())).child(why.to_string()))
            .when(findings > 0, |d| d.child(ui::mono(format!("{findings} open finding{}", if findings == 1 { "" } else { "s" }))))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(
                        ui::button(("held-fix", i), Kind::Primary, Some(Icon::Edit), "Fix again")
                            .tooltip(crate::shell::tip("Send the findings back to the agent once more"))
                            .on_click(cx.listener(move |this, _, _, cx| this.gate_fix_again(sub_id, &fix, cx))),
                    )
                    .child(
                        ui::button(("held-review", i), Kind::Secondary, Some(Icon::Eye), "Review again")
                            .on_click(cx.listener(move |this, _, _, cx| this.gate_review_again(sub_id, &again, cx))),
                    )
                    .children(agent_id.map(|id| {
                        ui::button(("held-open", i), Kind::Secondary, Some(Icon::Diff), "Changes").on_click(cx.listener(move |this, _, _, cx| {
                            this.select_agent(id, cx);
                            this.set_tab(id, crate::agent::AgentTab::Changes, cx);
                        }))
                    }))
                    .child(
                        ui::button(("held-anyway", i), Kind::Secondary, None, "Accept anyway")
                            .tooltip(crate::shell::tip("Go on with this work as it is, below the bar"))
                            .on_click(cx.listener(move |this, _, _, cx| this.gate_accept_anyway(sub_id, &anyway, cx))),
                    ),
            )
            .into_any_element()
    }

    /// An agent's questions as one card: each with its options (what each
    /// means under it, the recommended one marked) and "something else" with a
    /// box for your own words; how many are answered, the recommended answers
    /// in one click, and Send. Once sent, what you answered.
    pub(crate) fn render_question_set(&self, agent_id: i64, item: usize, questions: &[Question], cx: &mut Context<Self>) -> AnyElement {
        self.question_set(agent_id, item, questions, false, cx)
    }

    /// The same, inside another card (the dashboard's): no frame of its own.
    fn question_set(&self, agent_id: i64, item: usize, questions: &[Question], nested: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(agent) = self.agent(agent_id) else { return div().into_any_element() };
        let name = agent.record.name.clone();
        // Answered once a message of yours (or auto mode's) follows.
        let reply = agent.items.get(item + 1..).unwrap_or_default().iter().find_map(|i| if let Item::User(t) = i { Some(t.clone()) } else { None });
        let n = questions.len();
        let title = if n == 1 { format!("A question from {name}") } else { format!("{n} questions from {name}") };
        let head = |status: String, attention: bool| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(icon(Icon::Ask).size(px(15.)).text_color(rgb(if attention { theme::red_text() } else { theme::text_3() })))
                .child(div().flex_1().min_w(px(0.)).text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(title.clone()))
                .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(if attention { theme::red_text() } else { theme::text_3() })).child(status))
        };
        let number = |qi: usize| {
            div()
                .flex_none()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(theme::line_strong()))
                .font_family(theme::MONO)
                .text_size(px(10.5))
                .text_color(rgb(theme::text_2()))
                .child((qi + 1).to_string())
        };
        let topic = |q: &Question| {
            (!q.header.is_empty()).then(|| div().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(theme::text_3())).child(q.header.to_uppercase()))
        };

        // Sent: each question with what you answered.
        if let Some(reply) = reply {
            let given = pipeline::parse_answers(&reply);
            return ui::card()
                .gap(px(10.))
                .child(head("answered".into(), false))
                .children(questions.iter().enumerate().map(|(qi, q)| {
                    let answer = given.as_ref().and_then(|g| g.get(qi).cloned()).unwrap_or_else(|| "answered in your message".into());
                    div()
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .child(number(qi))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(q.question.clone()))
                                .child(
                                    div()
                                        .flex()
                                        .items_start()
                                        .gap(px(6.))
                                        .text_size(px(13.))
                                        .text_color(rgb(theme::text()))
                                        .child(div().pt(px(3.)).child(icon(Icon::Check).size(px(12.)).text_color(rgb(theme::text_3()))))
                                        .child(div().flex_1().min_w(px(0.)).child(answer)),
                                ),
                        )
                }))
                .into_any_element();
        }

        let picks: Vec<Option<Pick>> = (0..n).map(|qi| agent.answers.get(&(item, qi)).cloned()).collect();
        let done = (0..n).filter(|&qi| self.answer_text(agent_id, item, qi, &questions[qi], cx).is_some()).count();
        let status = if done == n { "ready to send".to_string() } else if n == 1 { "waiting for you".to_string() } else { format!("{done} of {n} answered") };
        let key = |qi: usize, oi: usize| agent_id as usize * 1_000_000 + item * 1_000 + qi * 20 + oi;

        let option_row = |id: usize, selected: bool, multi: bool, label: String, description: String, recommended: bool| {
            let mark = if multi {
                div()
                    .size(px(15.))
                    .rounded(px(3.))
                    .border_1()
                    .border_color(rgb(if selected { theme::text() } else { theme::line_strong() }))
                    .when(selected, |d| d.bg(rgb(theme::text())))
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(selected.then(|| icon(Icon::Check).size(px(11.)).text_color(rgb(theme::bg()))))
            } else {
                div()
                    .size(px(15.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(if selected { theme::text() } else { theme::line_strong() }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(selected.then(|| div().size(px(7.)).rounded_full().bg(rgb(theme::text()))))
            };
            div()
                .id(("q-option", id))
                .flex()
                .items_start()
                .gap(px(10.))
                .px(px(11.))
                .py(px(7.))
                .rounded(px(7.))
                .border_1()
                .border_color(rgb(if selected { theme::text_3() } else { theme::line() }))
                .when(selected, |d| d.bg(rgb(theme::selected())))
                .when(!selected, |d| d.hover(|s| s.bg(rgb(theme::hover())).border_color(rgb(theme::line_strong()))))
                .cursor_pointer()
                .child(div().flex_none().pt(px(2.)).child(mark))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().text_size(px(13.5)).text_color(rgb(theme::text())).child(label))
                                .when(recommended, |d| {
                                    d.child(
                                        div()
                                            .flex_none()
                                            .px(px(5.))
                                            .rounded(px(3.))
                                            .border_1()
                                            .border_color(rgb(theme::line_strong()))
                                            .font_family(theme::MONO)
                                            .text_size(px(10.))
                                            .text_color(rgb(theme::text_2()))
                                            .child("recommended"),
                                    )
                                }),
                        )
                        .when(!description.is_empty(), |d| d.child(div().text_size(px(12.5)).line_height(px(18.)).text_color(rgb(theme::text_3())).child(description))),
                )
        };

        let blocks = questions.iter().enumerate().map(|(qi, q)| {
            let pick = picks[qi].clone();
            let multi = q.multi;
            let options = q.options.iter().enumerate().map(|(oi, o)| {
                let selected = matches!(&pick, Some(Pick::Options(list)) if list.contains(&oi));
                option_row(key(qi, oi), selected, multi, o.label.clone(), o.description.clone(), q.recommended == Some(oi))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_option(agent_id, item, qi, oi, multi, cx)))
                    .into_any_element()
            });
            let other_on = matches!(pick, Some(Pick::Other));
            let other = option_row(key(qi, 19), other_on, multi, "Something else…".into(), String::new(), false)
                .on_click(cx.listener(move |this, _, window, cx| this.pick_other(agent_id, item, qi, window, cx)));
            let input = other_on.then(|| self.answer_inputs.get(&(agent_id, item, qi)).cloned()).flatten();
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .when(qi > 0, |d| d.pt(px(14.)).border_t_1().border_color(rgb(theme::line())))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(px(10.))
                        .child(number(qi))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(3.))
                                .children(topic(q))
                                .child(div().text_size(px(14.)).line_height(px(20.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(q.question.clone()))
                                .when(!q.why.is_empty(), |d| d.child(div().text_size(px(12.5)).line_height(px(18.)).text_color(rgb(theme::text_3())).child(q.why.clone())))
                                .when(multi, |d| d.child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child("Pick any that apply."))),
                        ),
                )
                .child(div().flex().flex_col().gap(px(6.)).pl(px(30.)).children(options).child(other).children(input.map(|i| div().pl(px(25.)).child(i))))
        });

        let all = done == n;
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .pt(px(12.))
            .border_t_1()
            .border_color(rgb(theme::line()))
            .when(!all, |d| {
                d.child(
                    ui::button(("q-recommended", key(0, 0)), Kind::Secondary, None, if done == 0 { "Use the recommended answers" } else { "Recommended for the rest" })
                        .tooltip(crate::shell::tip("Pick the option the agent recommends wherever you haven't answered"))
                        .on_click(cx.listener(move |this, _, _, cx| this.use_recommended(agent_id, item, cx))),
                )
            })
            .child(ui::one_line(div().flex_1()).text_size(px(12.)).text_color(rgb(theme::text_3())).child(if all { "" } else { "Or reply in the message box." }))
            .child(if all {
                ui::button(("q-send", key(0, 0)), Kind::Primary, Some(Icon::Send), if n == 1 { "Send the answer".to_string() } else { format!("Send {n} answers") })
                    .on_click(cx.listener(move |this, _, _, cx| this.send_answers(agent_id, item, cx)))
                    .into_any_element()
            } else {
                div()
                    .id(("q-send-off", key(0, 0)))
                    .tooltip(crate::shell::tip("Answer every question first"))
                    .child(ui::disabled_button(("q-send-disabled", key(0, 0)), Kind::Primary, Some(Icon::Send), if n == 1 { "Send the answer".to_string() } else { format!("Send {n} answers") }))
                    .into_any_element()
            });

        let frame = if nested { div().flex().flex_col().gap(px(14.)).pt(px(4.)) } else { ui::card().gap(px(14.)).border_color(rgb(theme::red_line())) };
        frame.child(head(status, !all)).children(blocks).child(footer).into_any_element()
    }

    /// The questions in an agent's last reply that still wait for an answer.
    pub(crate) fn render_open_questions(&self, agent_id: i64, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(agent) = self.agent(agent_id) else { return Vec::new() };
        if !crate::shell::open_question(agent) {
            return Vec::new();
        }
        let Some(item) = agent.items.iter().rposition(|i| matches!(i, Item::Agent(_))) else { return Vec::new() };
        let questions = self.questions_at(agent_id, item);
        if questions.is_empty() {
            return Vec::new();
        }
        vec![self.question_set(agent_id, item, &questions, true, cx)]
    }

    /// An agent reply containing `monaged-*` blocks: Markdown plus cards.
    pub(crate) fn render_agent_reply(&self, agent_id: i64, item: usize, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(agent) = self.agent(agent_id) else { return render_markdown(text) };
        let questions = self.questions_at(agent_id, item);
        let mut questions_shown = false;
        let mut column = div().flex().flex_col().gap_2();
        for segment in pipeline::segments(text) {
            let element: AnyElement = match segment {
                Segment::Text(t) => render_markdown(&t),
                // All of a reply's questions in one card, where the first was.
                Segment::Directive(Directive::Question(_)) if questions_shown => continue,
                Segment::Directive(Directive::Question(_)) => {
                    questions_shown = true;
                    self.render_question_set(agent_id, item, &questions, cx)
                }
                Segment::Directive(Directive::Research(v)) => result_card("Research report", v["summary"].as_str()),
                Segment::Directive(Directive::Verification(v)) => {
                    let line = format!(
                        "{} · confidence {}/5",
                        v["verdict"].as_str().unwrap_or("?").replace('_', " "),
                        v["confidence"].as_u64().unwrap_or(0)
                    );
                    result_card("Verification", Some(&line))
                }
                Segment::Directive(Directive::Plan(v)) => self.render_plan_card(&v, agent.record.subproject_id, false, cx),
                Segment::Directive(Directive::Done(v)) => result_card("Result", v["summary"].as_str()),
                Segment::Directive(Directive::Answer(v)) => result_card("Answer (sent back through Monaged)", v["answer"].as_str()),
                Segment::Directive(Directive::Invalid { kind, error }) => div()
                    .text_size(px(12.))
                    .text_color(rgb(tone_color(Tone::Error)))
                    .child(format!("Unreadable monaged-{kind} block: {error}"))
                    .into_any_element(),
            };
            column = column.child(element);
        }
        column.into_any_element()
    }
}

/// A role brief in the transcript, collapsed to one quiet line.
pub(crate) fn render_brief(text: &str) -> AnyElement {
    let title = text.lines().next().unwrap_or_default().trim_start_matches('#').trim().replace("Monaged brief:", "Monaged brief ·");
    let lines = text.lines().count();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .py(px(9.))
        .border_t_1()
        .border_b_1()
        .border_color(rgb(theme::line()))
        .text_size(px(12.5))
        .text_color(rgb(theme::text_3()))
        .child(icon(Icon::Board).size(px(14.)))
        .child(div().flex_1().child(title))
        .child(ui::mono(format!("{lines} lines · sent automatically")))
        .into_any_element()
}

/// A card for a decision or a result: hairline, quiet label.
fn gate_card(title: &'static str) -> gpui::Div {
    ui::card().text_size(px(13.)).child(ui::label(title))
}

fn result_card(title: &'static str, body: Option<&str>) -> AnyElement {
    gate_card(title).children(body.map(|b| div().line_height(px(20.)).text_color(rgb(theme::text_body())).child(b.to_string()))).into_any_element()
}

fn label(text: &'static str) -> AnyElement {
    div().pt(px(4.)).child(ui::label(text)).into_any_element()
}

/// Mono metadata with a hairline; `attention` only for risks.
fn pill(text: &str, attention: bool) -> AnyElement {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(if attention { theme::red() } else { theme::line_strong() }))
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(rgb(if attention { theme::red_text() } else { theme::text_2() }))
        .child(text.to_string())
        .into_any_element()
}

fn kv(k: &'static str, v: String) -> gpui::Div {
    div()
        .flex()
        .justify_between()
        .gap(px(10.))
        .text_size(px(12.5))
        .child(div().text_color(rgb(theme::text_3())).child(k))
        .child(div().font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::text())).child(v))
}

/// The summary in an agent's last `monaged-done` block.
fn last_done_summary(agent: &crate::agent::AgentView) -> Option<String> {
    agent.items.iter().rev().find_map(|item| {
        let Item::Agent(text) = item else { return None };
        pipeline::directives(text).into_iter().find_map(|d| match d {
            Directive::Done(value) => Some(value["summary"].as_str().unwrap_or("Integrated.").to_string()),
            _ => None,
        })
    })
}
