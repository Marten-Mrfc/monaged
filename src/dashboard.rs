//! A subproject's dashboard: what you asked for, everything that waits for
//! you (answered right here), the agents with their state and score, and the
//! lead to talk to. Clicking a subproject opens it; agents open from it.

use gpui::{AnyElement, Context, div, prelude::*, px, rgb};

use crate::agent::{AgentView, Item, Status, Tone};
use crate::icons::{Icon, icon};
use crate::pipeline::{Role, STAGES, Stage};
use crate::review_ui::render_markdown;
use crate::shell::{agent_state, role_label, tip};
use crate::ui::{self, Kind, State};
use crate::{Monaged, review, theme};

/// Below this the lead's chat isn't shown beside the dashboard (it opens as a pane).
pub(crate) const CHAT_MIN_MAIN: f32 = 980.;
const CONTENT_MAX: f32 = 1100.;
const PAD_X: f32 = 24.;
const TILE_MIN: f32 = 290.;
const TILE_GAP: f32 = 10.;

fn section_title(text: &str, count: Option<usize>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(ui::label(text.to_string()))
        .children(count.filter(|&n| n > 0).map(|n| {
            div().px(px(6.)).rounded(px(8.)).bg(rgb(theme::red())).text_size(px(11.)).text_color(rgb(theme::text())).child(n.to_string())
        }))
        .into_any_element()
}

impl Monaged {
    pub(crate) fn render_dashboard(&self, sub_id: i64, main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let chat_w = if main_w >= CHAT_MIN_MAIN { (main_w * 0.4).clamp(380., 560.) } else { 0. };
        // Cards get an exact width: GPUI measures wrapped text against the
        // width it's given, and a flexible card came out far too tall.
        let inner = ((main_w - chat_w).min(CONTENT_MAX) - 2. * PAD_X - 8.).max(240.);
        let per_row = if inner >= 3. * TILE_MIN + 2. * TILE_GAP { 3. } else if inner >= 2. * TILE_MIN + TILE_GAP { 2. } else { 1. };
        let tile_w = ((inner - TILE_GAP * (per_row - 1.)) / per_row).floor();
        // The column gets an exact width too: capped only by a maximum, GPUI
        // measured its text against the wider pane, so on a wide screen the
        // cards came out shorter than their text and the sections overlapped.
        let column_w = inner + 2. * PAD_X;
        let main = self.render_dashboard_main(sub_id, chat_w == 0., (tile_w, per_row as usize), column_w, cx);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .child(div().flex_1().min_w(px(0.)).h_full().flex().flex_col().child(main))
            .when(chat_w > 0., |d| d.child(self.render_lead_chat(sub_id, chat_w, cx)))
            .into_any_element()
    }

    fn render_dashboard_main(&self, sub_id: i64, narrow: bool, (tile_w, per_row): (f32, usize), column_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return div().into_any_element() };
        let pipeline = self.pipelines.get(&sub_id);
        let running = pipeline.is_some_and(|p| !matches!(p.stage, Stage::Done | Stage::Stopped));
        let lead = self.lead_of(sub_id);

        // ---- Header
        let mut actions = div().flex().flex_wrap().items_center().gap(px(6.));
        if narrow && let Some(lead) = lead {
            actions = actions.child(
                ui::button("dash-lead", Kind::Primary, Some(Icon::Flow), "Talk to the lead").on_click(cx.listener(move |this, _, _, cx| this.select_agent(lead, cx))),
            );
        }
        actions = actions
            .child(
                ui::button("dash-changes", Kind::Secondary, Some(Icon::Diff), "All changes")
                    .tooltip(tip("The feature branch and every agent's worktree"))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_sub_changes(sub_id, cx))),
            )
            .child(if self.creating_agent == Some(sub_id) {
                ui::busy_button("dash-new-agent", Kind::Secondary, "Making a worktree…")
            } else {
                ui::button("dash-new-agent", Kind::Secondary, Some(Icon::Plus), "New agent").on_click(cx.listener(move |this, _, _, cx| this.add_agent(sub_id, cx)))
            });
        actions = if running {
            actions.child(ui::button("dash-stop", Kind::Secondary, Some(Icon::Stop), "Stop pipeline").on_click(cx.listener(move |this, _, _, cx| this.stop_pipeline(sub_id, cx))))
        } else {
            actions.child(
                ui::button("dash-run", Kind::Secondary, Some(Icon::Play), if pipeline.is_some() { "Run again" } else { "Run pipeline" })
                    .on_click(cx.listener(move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx))),
            )
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .min_w(px(0.))
                            .child(ui::heading(sub.name.clone()))
                            .child(ui::mono(sub.branch.clone())),
                    )
                    .child(actions),
            )
            .children(pipeline.map(|p| self.render_stage_strip(sub_id, p.stage, p.auto_mode, cx)));

        // ---- The request and where it stands
        let request = pipeline.map(|p| p.request.clone()).or_else(|| {
            self.agents
                .iter()
                .filter(|a| a.record.subproject_id == sub_id && a.role() == Role::Free)
                .map(|a| a.task())
                .find(|t| !t.trim().is_empty())
        });
        let note = pipeline.and_then(|p| p.note.clone());
        let request_card = ui::card()
            .child(ui::label("Request"))
            .child(match &request {
                Some(text) => div().text_size(px(13.5)).child(render_markdown(&review::clip(text.trim(), 4000))).into_any_element(),
                None => div()
                    .text_size(px(13.))
                    .text_color(rgb(theme::text_3()))
                    .child("No request yet. Run the pipeline with one, start an agent, or tell the lead what you want.")
                    .into_any_element(),
            })
            .children(note.map(|note| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .pt(px(4.))
                    .text_size(px(13.))
                    .text_color(rgb(theme::text_2()))
                    .child(div().flex_none().pt(px(2.)).child(icon(Icon::Info).size(px(14.))))
                    .child(div().flex_1().min_w(px(0.)).child(note))
            }));

        // ---- Needs you
        let needs = self.dashboard_needs(sub_id, cx);
        let needs_count = needs.len();
        let needs_section = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Needs you", Some(needs_count)))
            .when(needs.is_empty(), |d| {
                d.child(div().text_size(px(13.)).text_color(rgb(theme::text_3())).child("Nothing waits for you right now. Questions, approvals and work below the bar show up here."))
            })
            .children(needs);

        // ---- Agents
        let mut agents: Vec<&AgentView> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && (narrow || a.role() != Role::Lead))
            .collect();
        // What needs you or works first, then the agents doing the work, then
        // the ones whose part is long done (research, verification, the plan).
        let rank = |a: &AgentView| {
            let state = match agent_state(a) {
                State::NeedsYou => 0,
                State::Working => 1,
                _ => 2,
            };
            let role = match a.role() {
                Role::Lead => 0,
                Role::Integrator => 1,
                Role::Builder(_) => 2,
                Role::Free => 3,
                Role::Planner => 4,
                Role::Verifier(_) => 5,
                Role::Researcher => 6,
            };
            (state, role, std::cmp::Reverse(a.record.id))
        };
        agents.sort_by_key(|a| rank(a));
        // What happened lately, in words (turns are left out: there are many, and the cards say who's working).
        let events: Vec<_> = self.store.timeline(sub_id, 40).unwrap_or_default().into_iter().filter(|e| e.kind != "turn").take(8).collect();
        let name_of = |id: Option<i64>| id.map(|id| self.agent_name(id)).unwrap_or_else(|| "Monaged".into());
        let agents_section = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(section_title("Agents", None))
            .when(agents.is_empty(), |d| d.child(div().text_size(px(13.)).text_color(rgb(theme::text_3())).child("No agents yet.")))
            .children({
                // Rows of cards (not a wrapping row: GPUI sized wrapped cards
                // by their text at a much smaller width, leaving them tall).
                agents
                    .chunks(per_row)
                    .map(|row| div().flex().items_start().gap(px(TILE_GAP)).children(row.iter().map(|a| self.render_agent_tile(a, tile_w, cx))).into_any_element())
                    .collect::<Vec<_>>()
            });

        div()
            .id(("dashboard", sub_id as usize))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(22.))
                    .px(px(PAD_X))
                    .py(px(20.))
                    .w(px(column_w))
                    .child(header)
                    .child(request_card)
                    .child(needs_section)
                    .child(agents_section)
                    .when(!events.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(section_title("Recent", None))
                                .children(events.iter().map(|e| {
                                    let (glyph, text, attention) = crate::timeline::describe(e, &name_of);
                                    div()
                                        .flex()
                                        .gap(px(10.))
                                        .text_size(px(12.5))
                                        .items_center()
                                        .child(div().flex_none().w(px(64.)).font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(ago(e.at)))
                                        .child(icon(glyph).size(px(13.)).flex_none().text_color(rgb(if attention { theme::red_text() } else { theme::text_3() })))
                                        .child(
                                            ui::one_line(div().flex_1())
                                                .text_color(rgb(if attention { theme::red_text() } else { theme::text_2() }))
                                                .child(text.replace('\n', " ")),
                                        )
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_stage_strip(&self, sub_id: i64, stage: Stage, auto_mode: bool, cx: &mut Context<Self>) -> AnyElement {
        let current = STAGES.iter().position(|s| *s == stage);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .children(STAGES.iter().enumerate().map(|(i, s)| {
                let (past, now) = match current {
                    Some(c) => (i < c, i == c),
                    None => (false, false),
                };
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(10.))
                    .text_size(px(12.))
                    .border_1()
                    .border_color(rgb(if now { theme::text_2() } else { theme::line() }))
                    .when(now, |d| d.bg(rgb(theme::selected())))
                    .text_color(rgb(if now {
                        theme::text()
                    } else if past {
                        theme::text_2()
                    } else {
                        theme::text_3()
                    }))
                    .child(s.label().to_string())
            }))
            .when(stage == Stage::Stopped, |d| d.child(ui::mono("stopped")))
            .child(div().flex_1())
            .child(
                ui::switch(("dash-auto", sub_id as usize), "Auto mode", auto_mode)
                    .tooltip(tip("Answer the agents' questions with the recommended option and start small, safe plans by themselves"))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_auto_mode(sub_id, cx))),
            )
            .into_any_element()
    }

    /// Everything in this subproject that waits for you, answerable in place.
    fn dashboard_needs(&self, sub_id: i64, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some(p) = self.pipelines.get(&sub_id) {
            match p.stage {
                Stage::Decide => out.push(self.render_decide_card(p, sub_id, cx)),
                Stage::Approve if p.plan.is_some() => {
                    if self.identity_missing {
                        out.push(
                            ui::card()
                                .child(div().text_size(px(13.)).child("Builders commit their work, but git inside WSL has no name or email yet."))
                                .child(div().flex().child(
                                    ui::button("dash-identity", Kind::Primary, Some(Icon::User), "Copy my Windows git identity into WSL")
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_git_identity(cx))),
                                ))
                                .into_any_element(),
                        );
                    }
                    if let Some(plan) = &p.plan {
                        out.push(self.render_plan_card(plan, sub_id, true, cx));
                    }
                }
                _ => {}
            }
            for (i, (name, why)) in p.gate.held.iter().enumerate() {
                out.push(
                    ui::card()
                        .child(div().flex().items_center().gap(px(8.)).child(icon(Icon::Shield).size(px(14.)).text_color(rgb(theme::red_text()))).child(ui::label(format!("Below the bar: {name}"))))
                        .child(self.render_held(sub_id, i, name, why, cx))
                        .into_any_element(),
                );
            }
        }
        for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived) {
            if let Some(card) = self.agent_needs_card(agent, cx) {
                out.push(card);
            }
        }
        if let Some(merge) = self.dashboard_merge(sub_id, cx) {
            out.push(merge);
        }
        out
    }

    /// One agent's open questions, permissions, forms and requests, with a way to reply.
    fn agent_needs_card(&self, agent: &AgentView, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = agent.record.id;
        let mut body: Vec<AnyElement> = Vec::new();
        for (index, item) in agent.items.iter().enumerate() {
            let open = match item {
                Item::Permission { chosen: None, .. } => true,
                Item::Elicit(form) => form.state == crate::agent::FormState::Open,
                _ => false,
            };
            if open {
                body.push(self.render_item(id, index, item, cx));
            }
        }
        body.extend(self.render_open_questions(id, cx));
        if let Some(requests) = self.render_env_requests(id, cx) {
            body.push(requests);
        }
        if agent.paused {
            body.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child("Paused by the budget: its next message waits."))
                    .child(ui::button(("dash-resume", id as usize), Kind::Secondary, None, "Send anyway").on_click(cx.listener(move |this, _, _, cx| {
                        this.resume_agent(id);
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }
        if let Status::Failed(error) = &agent.status {
            body.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().min_w(px(200.)).text_size(px(13.)).text_color(rgb(theme::red_text())).child(format!("Its session failed: {error}")))
                    .child(ui::button(("dash-retry", id as usize), Kind::Secondary, Some(Icon::Refresh), "Restart").on_click(cx.listener(move |this, _, _, cx| this.retry_session(id, cx))))
                    .into_any_element(),
            );
        }
        if body.is_empty() {
            return None;
        }
        let name = agent.record.name.clone();
        Some(
            ui::card()
                .border_color(rgb(theme::red()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ui::status_dot(("dash-need-dot", id as usize), State::NeedsYou))
                        .child(div().font_family(theme::MONO).text_size(px(12.5)).child(name.clone()))
                        .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(role_label(&agent.role())))
                        .child(div().flex_1())
                        .child(
                            ui::button(("dash-reply", id as usize), Kind::Secondary, Some(Icon::Send), "Reply")
                                .tooltip(tip("Type your own answer or instruction"))
                                .on_click(cx.listener(move |this, _, window, cx| this.ask_reply(id, window, cx))),
                        )
                        .child(ui::button(("dash-open-need", id as usize), Kind::Secondary, None, "Open").on_click(cx.listener(move |this, _, _, cx| this.select_agent(id, cx)))),
                )
                .children(body)
                .into_any_element(),
        )
    }

    /// The feature is done but the merge waits for you.
    fn dashboard_merge(&self, sub_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.pipelines.get(&sub_id).filter(|p| p.stage == Stage::Done)?;
        let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
        let target = self.merge_target(sub_id)?;
        let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        if !ahead.is_some_and(|n| n > 0) {
            return None;
        }
        let state = self.merges.get(&sub_id);
        let busy = state.is_some_and(|s| s.busy);
        let message = state.and_then(|s| s.message.clone()).map(|(_, m)| m).unwrap_or_else(|| p.note.clone().unwrap_or_default());
        let score = self.sub_confidence(sub_id).map(|(s, from)| format!("Confidence {s}/5 ({from})."));
        Some(
            ui::card()
                .child(div().flex().items_center().gap(px(8.)).child(icon(Icon::Merge).size(px(14.))).child(ui::label(format!("Ready to merge into {target}"))))
                .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(message))
                .children(score.map(ui::mono))
                .child(div().flex().gap(px(6.)).child(if busy {
                    ui::busy_button("dash-merge", Kind::Primary, "Merging…")
                } else {
                    ui::button("dash-merge", Kind::Primary, Some(Icon::Merge), format!("Merge into {target}")).on_click(cx.listener(move |this, _, _, cx| this.merge_subproject(sub_id, cx)))
                }))
                .into_any_element(),
        )
    }

    /// One agent on the dashboard: state, score, what it's doing, and controls.
    fn render_agent_tile(&self, agent: &AgentView, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let state = agent_state(agent);
        let word = match state {
            State::NeedsYou => "needs you",
            State::Working if agent.reviewing => "working · in review",
            State::Working => "working",
            _ if agent.reviewing => "in review",
            State::Done => "idle",
            State::Idle => "not started",
        };
        let gate = self
            .pipelines
            .get(&agent.record.subproject_id)
            .and_then(|p| {
                let name = &agent.record.name;
                if p.done.contains(name) {
                    Some("passed review")
                } else if p.gate.held_reason(name).is_some() {
                    Some("below the bar")
                } else if p.gate.fixing.contains(name) {
                    Some("fixing findings")
                } else if p.gate.pending.contains(name) {
                    Some("waiting for review")
                } else {
                    None
                }
            });
        let line = match (&agent.summary, agent.task()) {
            (Some(summary), _) => summary.text.clone(),
            (None, task) if !task.starts_with(crate::pipeline::BRIEF_HEADER) && !task.trim().is_empty() => task.trim().to_string(),
            _ => String::new(),
        };
        let can_fix = !agent.reviewing && agent.confidence.as_ref().is_some_and(|c| review::fix_prompt(c).is_some());
        let findings = agent.confidence.as_ref().map_or(0, |c| c.issues.len());
        // The card's padding and border, both sides.
        let text_w = (width - 2. * 15.).max(120.);
        ui::card()
            .w(px(width))
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::status_dot(("dash-dot", id as usize), state))
                    .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(12.5)).child(agent.record.name.clone()))
                    .children(agent.confidence.as_ref().map(|c| ui::score_meter(c.score))),
            )
            .child({
                // One line (rows that wrap inside a card made GPUI size it far too tall).
                let mut parts = vec![role_label(&agent.role()), word.to_string()];
                parts.extend(gate.map(str::to_string));
                if findings > 0 {
                    parts.push(format!("{findings} finding{}", if findings == 1 { "" } else { "s" }));
                }
                let below = gate == Some("below the bar");
                div()
                    .w(px(text_w))
                    .truncate()
                    .text_size(px(12.))
                    .text_color(rgb(if below { theme::red_text() } else { theme::text_3() }))
                    .child(parts.join(" · "))
            })
            .children(self.render_working_line(agent, cx))
            .when(!line.is_empty(), |d| d.child(div().w(px(text_w)).text_size(px(13.)).text_color(rgb(theme::text_2())).child(review::clip(&line, 280))))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::button(("tile-open", id as usize), Kind::Secondary, None, "Open").on_click(cx.listener(move |this, _, _, cx| this.select_agent(id, cx))))
                    .child(ui::button(("tile-changes", id as usize), Kind::Secondary, Some(Icon::Diff), "Changes").on_click(cx.listener(move |this, _, _, cx| {
                        this.select_agent(id, cx);
                        this.set_tab(id, crate::agent::AgentTab::Changes, cx);
                    })))
                    .when(agent.role() != Role::Lead, |d| {
                        d.child(
                            ui::icon_button(("tile-review", id as usize), Icon::Eye)
                                .tooltip(tip("Review: tests and a skeptical read"))
                                .on_click(cx.listener(move |this, _, _, cx| this.review_agent(id, cx))),
                        )
                    })
                    .when(can_fix, |d| {
                        d.child(
                            ui::icon_button(("tile-fix", id as usize), Icon::Edit)
                                .tooltip(tip("Fix findings: send the review's findings and failing tests to the agent"))
                                .on_click(cx.listener(move |this, _, _, cx| this.fix_findings(id, cx))),
                        )
                    })
                    .when(agent.is_working(), |d| {
                        d.child(
                            ui::icon_button(("tile-stop", id as usize), Icon::Stop)
                                .tooltip(tip("Stop its turn"))
                                .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(id, cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The lead's conversation beside the dashboard (or a way to start it).
    fn render_lead_chat(&self, sub_id: i64, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let column = div().w(px(width)).flex_none().h_full().flex().flex_col().border_l_1().border_color(rgb(theme::line()));
        let lead = self.lead_of(sub_id).and_then(|id| self.agent(id));
        let header = |status: String| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .h(px(44.))
                .flex_none()
                .border_b_1()
                .border_color(rgb(theme::line()))
                .child(icon(Icon::Flow).size(px(14.)).text_color(rgb(theme::text_2())))
                .child(div().text_size(px(13.)).font_weight(gpui::FontWeight::MEDIUM).child("Lead"))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_3())).child(status))
        };
        let Some(lead) = lead else {
            let starting = self.starting_lead.contains(&sub_id);
            return column
                .child(header(String::new()))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .p(px(24.))
                        .child(div().text_color(rgb(theme::text_3())).child(icon(Icon::Flow).size(px(22.))))
                        .child(
                            div().max_w(px(320.)).text_center().text_size(px(13.)).text_color(rgb(theme::text_2())).child(
                                "A lead agent oversees this subproject: ask it how things stand, tell it what should happen next, and it passes it on to the agents. It doesn't write code.",
                            ),
                        )
                        .child(if starting {
                            ui::busy_button("lead-start", Kind::Primary, "Starting the lead…")
                        } else {
                            ui::button("lead-start", Kind::Primary, Some(Icon::Play), "Start the lead").on_click(cx.listener(move |this, _, _, cx| {
                                this.start_lead(sub_id, cx);
                                cx.notify();
                            }))
                        }),
                )
                .into_any_element();
        };
        let id = lead.record.id;
        let status = match agent_state(lead) {
            State::Working => "working".to_string(),
            State::NeedsYou => "waiting for you".to_string(),
            _ if lead.live => "ready".to_string(),
            _ => "starting".to_string(),
        };
        let reading = (width - 40.).max(240.);
        column
            .child(header(status).child(
                ui::icon_button("lead-pane", Icon::Maximize).tooltip(tip("Open the lead as a pane")).on_click(cx.listener(move |this, _, _, cx| this.select_agent(id, cx))),
            ))
            .child(self.render_transcript(lead, reading, cx))
            .children(self.render_env_requests(id, cx).map(|cards| div().px(px(16.)).pb(px(8.)).child(cards)))
            .when(!lead.record.archived, |d| d.child(self.render_message_box(lead, reading, cx)))
            .into_any_element()
    }

    /// Opens a box to type your own reply to an agent.
    pub(crate) fn ask_reply(&mut self, agent_id: i64, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        self.ask_text(format!("Reply to {name}"), "Your answer or instruction".into(), "Send".into(), String::new(), crate::feedback::TextAction::Reply(agent_id), window, cx);
    }

    /// Sends what you typed in the reply box.
    pub(crate) fn send_reply(&mut self, agent_id: i64, text: &str, cx: &mut Context<Self>) {
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.answers.clear();
        }
        self.bypass_budget = true;
        self.send_prompt(agent_id, text.to_string());
        self.bypass_budget = false;
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        self.toast(Tone::Info, format!("Sent to {name}."), cx);
        cx.notify();
    }
}

/// "3m ago", "2h ago", "yesterday".
fn ago(at: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(at);
    match (now - at).max(0) {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 2 * 86_400 => "yesterday".into(),
        s => format!("{}d ago", s / 86_400),
    }
}
