//! Phase 3: running the pipeline (what happens when a role agent finishes a
//! turn), the gates, question cards, and the pipeline panel.

use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::agent::{Item, Tone};
use crate::pipeline::{self, Directive, Gate, Pipeline, Question, Role, Segment, Stage};
use crate::icons::{Icon, icon};
use crate::review_ui::render_markdown;
use crate::{AgentSpec, Monaged, button, dots, git, theme, tone_color, ui};

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

    fn toggle_auto_mode(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.auto_mode = !p.auto_mode;
        }
        self.save_pipeline(sub_id);
        cx.notify();
    }

    pub(crate) fn role_spec(name: &str, role: Role, brief: String, select: bool) -> AgentSpec {
        let on_feature_branch = role == Role::Integrator;
        AgentSpec { name: name.into(), role, on_feature_branch, brief: Some(brief), select }
    }

    // ---- Stage transitions ---------------------------------------------------

    pub(crate) fn start_pipeline(&mut self, sub_id: i64, request: String, auto_mode: bool, cx: &mut Context<Self>) {
        let mut p = Pipeline::new(sub_id, request.clone(), auto_mode);
        p.verifications = pipeline::empty_verifications();
        p.note = Some("The researcher is reading the code.".into());
        self.pipelines.insert(sub_id, p);
        self.save_pipeline(sub_id);
        self.mark_pipeline_started(sub_id);
        self.spawn_agent(sub_id, Self::role_spec("research", Role::Researcher, pipeline::researcher_brief(&request), true), cx);
        cx.notify();
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
        self.spawn_agent(sub_id, Self::role_spec("planner", Role::Planner, brief, true), cx);
        cx.notify();
    }

    /// Checks that builders can commit, then starts one builder per workstream.
    pub(crate) fn start_build(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(project) = self.project_of(sub_id).cloned() else { return };
        self.set_note(sub_id, "Checking that builders can commit…");
        cx.notify();
        cx.spawn(async move |view, cx| {
            let ok = cx.background_spawn(async move { git::identity_ok(&project.distro, &project.path) }).await.unwrap_or(false);
            view.update(cx, |this, cx| {
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
        let Some(plan) = p.plan.clone() else { return };
        let mut streams = pipeline::workstreams(&plan);
        if streams.is_empty() {
            streams.push(pipeline::Workstream { name: "all".into(), value: serde_json::json!({ "name": "all", "goal": "the whole plan" }) });
        }
        p.stage = Stage::Build;
        p.done.clear();
        p.note = Some(format!("{} builder{} working in parallel.", streams.len(), if streams.len() == 1 { " is" } else { "s are" }));
        let request = p.request.clone();
        self.save_pipeline(sub_id);
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let sub_slug = git::slug(&sub.name);
        let mut used: Vec<String> = Vec::new();
        for (i, stream) in streams.iter().enumerate() {
            let mut name = self.unique_agent_name(sub_id, &format!("build-{}", stream.name));
            while used.contains(&name) {
                name = format!("{name}-x");
            }
            used.push(name.clone());
            let branch = format!("agent/{sub_slug}/{name}");
            let brief = pipeline::builder_brief(&request, &plan, stream, &branch);
            self.spawn_agent(sub_id, Self::role_spec(&name, Role::Builder(stream.name.clone()), brief, i == 0), cx);
        }
    }

    pub(crate) fn start_integration(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let branches: Vec<String> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && matches!(a.role(), Role::Builder(_)))
            .map(|a| a.record.branch.clone())
            .collect();
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        let brief = pipeline::integrator_brief(&p.request, &p.plan.clone().unwrap_or_default(), &branches, &sub.branch);
        p.stage = Stage::Integrate;
        p.note = Some(format!("The integrator is merging {} builder branch(es) into {}.", branches.len(), sub.branch));
        self.save_pipeline(sub_id);
        self.spawn_agent(sub_id, Self::role_spec("integrator", Role::Integrator, brief, true), cx);
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
        if role == Role::Free {
            return;
        }
        let (sub_id, name) = (agent.record.subproject_id, agent.record.name.clone());
        let found = pipeline::directives(&agent.last_reply());
        let Some(p) = self.pipelines.get(&sub_id) else { return };
        if matches!(p.stage, Stage::Stopped | Stage::Done) {
            return;
        }
        let auto = p.auto_mode;

        let questions: Vec<Question> = found.iter().filter_map(|d| if let Directive::Question(q) = d { Some(q.clone()) } else { None }).collect();
        if !questions.is_empty() {
            if auto {
                let answers: Vec<(Question, String)> = questions
                    .iter()
                    .map(|q| (q.clone(), q.options[q.recommended.unwrap_or(0)].clone()))
                    .collect();
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.notice(Tone::Info, format!("Auto mode answered {} question(s) with the recommended option.", answers.len()));
                }
                self.send_prompt(agent_id, pipeline::answers_message(&answers, true));
            } else {
                let n = questions.len();
                self.set_note(sub_id, format!("{name} has {n} question{} for you.", if n == 1 { "" } else { "s" }));
            }
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
                    match pipeline::decide_gate(&p.verifications, auto) {
                        Gate::Auto(reason) => {
                            p.note = Some(format!("{reason}. Writing the plan."));
                            self.save_pipeline(sub_id);
                            self.start_planning(sub_id, cx);
                        }
                        Gate::Ask(reason) => {
                            p.note = Some(format!("The research is verified. {reason}. Do we build this?"));
                            self.save_pipeline(sub_id);
                        }
                    }
                } else {
                    p.note = Some(format!("{name} is done; waiting for the other verifier."));
                    self.save_pipeline(sub_id);
                }
            }
            (Role::Planner, Some(Directive::Plan(value))) => {
                let allows = self.project_of(sub_id).map(|pr| pr.id).is_some_and(|id| self.project_auto_start(id));
                let gate = self.budget_gate(&value, pipeline::plan_gate(&value, allows, auto));
                let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
                let title = pipeline::plan_title(&value);
                p.plan = Some(value);
                p.stage = Stage::Approve;
                match gate {
                    Gate::Auto(reason) => {
                        p.note = Some(format!("\"{title}\": {reason}, so building started on its own."));
                        self.save_pipeline(sub_id);
                        self.start_build(sub_id, cx);
                    }
                    Gate::Ask(reason) => {
                        p.note = Some(format!("\"{title}\" is ready for your approval. {reason}."));
                        self.save_pipeline(sub_id);
                    }
                }
            }
            (Role::Builder(_), Some(Directive::Done(_))) => {
                let builders: Vec<String> = self
                    .agents
                    .iter()
                    .filter(|a| a.record.subproject_id == sub_id && matches!(a.role(), Role::Builder(_)))
                    .map(|a| a.record.name.clone())
                    .collect();
                let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
                if !p.done.contains(&name) {
                    p.done.push(name.clone());
                }
                let finished = builders.iter().filter(|b| p.done.contains(b)).count();
                if finished >= builders.len() {
                    p.note = Some("All builders are done. Integrating.".into());
                    self.save_pipeline(sub_id);
                    self.start_integration(sub_id, cx);
                } else {
                    p.note = Some(format!("{finished} of {} builders done.", builders.len()));
                    self.save_pipeline(sub_id);
                }
            }
            (Role::Integrator, Some(Directive::Done(value))) => {
                let branch = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.branch.clone()).unwrap_or_default();
                if let Some(p) = self.pipelines.get_mut(&sub_id) {
                    p.stage = Stage::Done;
                    let summary = value["summary"].as_str().unwrap_or("Integrated.");
                    p.note = Some(format!("Done: {summary} Everything is on {branch}."));
                }
                self.save_pipeline(sub_id);
                self.record_pipeline_run(sub_id);
                if let Some(project_id) = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id) {
                    self.refresh_branches(project_id, cx);
                }
                self.maybe_auto_merge(sub_id, cx);
            }
            (role, _) => self.nudge(agent_id, &role, sub_id),
        }
        cx.notify();
    }

    fn nudge(&mut self, agent_id: i64, role: &Role, sub_id: i64) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let name = agent.record.name.clone();
        if agent.nudged {
            self.set_note(sub_id, format!("{name} finished without delivering its result. Reply to it, or stop the pipeline."));
            return;
        }
        agent.nudged = true;
        self.set_note(sub_id, format!("Reminded {name} to deliver its result."));
        self.send_prompt(agent_id, pipeline::nudge_message(role));
    }

    /// The user picked an option on a question card. Once every question in
    /// that reply has an answer, they're sent together.
    pub(crate) fn answer_question(&mut self, agent_id: i64, item: usize, question: usize, option: usize, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let Some(Item::Agent(text)) = agent.items.get(item) else { return };
        let questions: Vec<Question> =
            pipeline::directives(text).into_iter().filter_map(|d| if let Directive::Question(q) = d { Some(q) } else { None }).collect();
        agent.answers.insert((item, question), option);
        if !(0..questions.len()).all(|q| agent.answers.contains_key(&(item, q))) {
            cx.notify();
            return;
        }
        let answers: Vec<(Question, String)> = questions
            .iter()
            .enumerate()
            .map(|(q, quest)| (quest.clone(), quest.options[agent.answers[&(item, q)]].clone()))
            .collect();
        agent.answers.retain(|(i, _), _| *i != item);
        let sub_id = agent.record.subproject_id;
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.note = None;
        }
        self.save_pipeline(sub_id);
        self.bypass_budget = true;
        self.send_prompt(agent_id, pipeline::answers_message(&answers, false));
        self.bypass_budget = false;
        cx.notify();
    }

    // ---- Rendering --------------------------------------------------------------------

    /// True when the pipeline waits for the user (a gate, or a question).
    pub(crate) fn pipeline_needs_you(&self, sub_id: i64) -> bool {
        self.pipelines.get(&sub_id).is_some_and(|p| {
            matches!(p.stage, Stage::Decide | Stage::Approve) || p.note.as_deref().is_some_and(|n| n.contains("question"))
        })
    }

    /// The inspector's Pipeline tab: where the pipeline is, what each stage
    /// produced, and the decision waiting for you (if any).
    pub(crate) fn render_pipeline_inspector(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(p) = self.pipelines.get(&sub_id) else {
            return ui::panel_section("Pipeline")
                .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(
                    "No pipeline has run here yet. It researches the request, has two agents verify that, writes a plan, then builds it with agents in parallel.",
                ))
                .child(div().flex().child(
                    ui::button("insp-run-pipeline", ui::Kind::Primary, Some(Icon::Play), "Run pipeline")
                        .on_click(cx.listener(move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx))),
                ))
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
                .when(rerunnable, |d| {
                    d.child(
                        div()
                            .id(("stage-rerun", i))
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
        let plan_lines = p.plan.as_ref().map(|plan| {
            let (cost, _, pct) = self.plan_estimate(plan);
            ui::panel_section("Plan")
                .child(div().text_size(px(13.)).text_color(rgb(theme::text())).child(pipeline::plan_title(plan)))
                .child(kv("Estimate", format!("${cost:.0} · ≈{pct:.1}% of week")))
                .child(kv("Spent this week", match cap {
                    Some(c) => format!("${spent:.2} of ${c:.0} cap"),
                    None => format!("${spent:.2}"),
                }))
        });
        let cap_section = ui::panel_section("Spending cap")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().text_size(px(12.5)).text_color(rgb(theme::text_2())).child(match cap {
                        Some(c) => format!("${spent:.2} of ${c:.0} this week"),
                        None => format!("${spent:.2} this week · no cap"),
                    }))
                    .child(ui::button("cap-edit", ui::Kind::Ghost, None, if cap.is_some() { "Change" } else { "Set a cap" }).on_click(cx.listener(move |this, _, window, cx| this.ask_sub_cap(sub_id, window, cx)))),
            )
            .children(cap.map(|c| {
                let ratio = (spent / c).clamp(0.0, 1.0) as f32;
                div().h(px(4.)).rounded_full().bg(rgb(theme::line_strong())).child(div().h_full().rounded_full().w(gpui::relative(ratio)).bg(rgb(if ratio >= 1.0 { theme::red() } else { theme::text_2() })))
            }));

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
            .children(plan_lines)
            .child(cap_section)
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

    fn render_decide_card(&self, p: &Pipeline, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
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
                            button("plan-approve", format!("Approve and build ({builders} builder{})", if builders == 1 { "" } else { "s" }), theme::PRIMARY_BG)
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
                let done = p.done.contains(&a.record.name) || (a.role() == Role::Integrator && p.stage == Stage::Done);
                let (state, word) = if done {
                    (ui::State::Done, "done")
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
        gate_card("Builders").children(rows).into_any_element()
    }

    /// An agent reply containing `monaged-*` blocks: Markdown plus cards.
    pub(crate) fn render_agent_reply(&self, agent_id: i64, item: usize, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(agent) = self.agent(agent_id) else { return render_markdown(text) };
        let answered = agent.items[item + 1..].iter().any(|i| matches!(i, Item::User(_)));
        let mut question_index = 0usize;
        let mut column = div().flex().flex_col().gap_2();
        for segment in pipeline::segments(text) {
            let element: AnyElement = match segment {
                Segment::Text(t) => render_markdown(&t),
                Segment::Directive(Directive::Question(q)) => {
                    let qi = question_index;
                    question_index += 1;
                    let chosen = agent.answers.get(&(item, qi)).copied();
                    let options = q.options.iter().enumerate().map(|(oi, option)| {
                        let selected = chosen == Some(oi);
                        let mut text = option.clone();
                        if q.recommended == Some(oi) {
                            text.push_str("  (recommended)");
                        }
                        div()
                            .id(SharedString::from(format!("q-{agent_id}-{item}-{qi}-{oi}")))
                            .px(px(10.))
                            .py(px(5.))
                            .rounded(px(6.))
                            .border_1()
                            .text_size(px(13.))
                            .border_color(rgb(if selected { theme::text() } else { theme::line_strong() }))
                            .when(selected, |d| d.bg(rgb(theme::selected())))
                            .when(!answered, |d| {
                                d.cursor_pointer().hover(|s| s.bg(rgb(theme::hover()))).on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer_question(agent_id, item, qi, oi, cx)
                                }))
                            })
                            .child(text)
                    });
                    gate_card("Question")
                        .border_color(rgb(if answered { theme::line() } else { theme::red() }))
                        .child(div().font_weight(FontWeight::MEDIUM).child(q.question.clone()))
                        .when(!q.why.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(q.why.clone())))
                        .child(div().flex().flex_wrap().gap_2().children(options))
                        .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(if answered {
                            "Answered."
                        } else {
                            "Pick an option, or type your own answer below."
                        }))
                        .into_any_element()
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
