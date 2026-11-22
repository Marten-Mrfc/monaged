//! Autopilot and the project chat.
//!
//! In a project's view you type a title and what you want; Monaged makes a
//! feature subproject for it and takes it from there to merged into develop
//! without you: research, two verifiers, a plan, builders, reviews, the
//! integrator, bringing develop in, resolving conflicts and merging. The lead
//! is the conversation: it answers what it can, and passes on only what
//! only you can decide.
//!
//! What it asks you, and nothing else:
//! - questions an agent has that the lead can't settle from the request, the
//!   research or the code;
//! - the plan, once: approving it also says "merge it into develop";
//! - work that stays below the bar after four fix rounds (instead of two);
//! - a merge it can't finish by itself (a refused push, a review that can't
//!   run, findings that survive two fixes).
//!
//! It never merges into the top branch (a project with only main): that one
//! stays yours, as in every other flow.

use gpui::{AnyElement, Context, Entity, Focusable, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::{Item, Tone};
use crate::feedback::ToastAction;
use crate::icons::{Icon, icon, icon_sized};
use crate::pipeline::{self, Stage};
use crate::ship::Step;
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, SubKind, git, review, theme};

/// How often the merge into develop fixes findings or starts over by itself
/// before it stops and asks you.
pub const MERGE_RETRIES: u8 = 2;

/// How many recent autopilot runs the project view lists.
const THREADS_SHOWN: usize = 6;

/// The project chat's box in a project's view.
pub struct ProjectChat {
    pub project_id: i64,
    pub title: Entity<TextInput>,
    pub description: Entity<TextInput>,
    pub busy: bool,
    pub error: Option<String>,
    /// The title was empty.
    pub invalid: bool,
}

/// What the researcher (and everyone after it) starts from.
pub fn request_text(title: &str, description: &str) -> String {
    let (title, description) = (title.trim(), description.trim());
    if description.is_empty() { title.to_string() } else { format!("# {title}\n\n{description}") }
}

/// A branch name for a new feature that no subproject of the project uses yet.
pub fn free_branch(title: &str, taken: &[&str]) -> String {
    let base = format!("feature/{}", git::slug(title));
    if !taken.contains(&base.as_str()) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|b| !taken.contains(&b.as_str())).unwrap_or(base)
}

/// What the lead does differently on autopilot (joins its brief).
pub fn lead_section() -> String {
    "## Autopilot\n\
     The user started this from the project chat on autopilot: Monaged takes it from the request to merged into develop \
     without them. You are the user's one conversation about it, and your job is to keep it moving so they don't have to.\n\
     - When an agent asks something that the request, the research, the plan or the code already settles, answer it \
     yourself with `message_agent` and say in a sentence what you decided. Pass on to the user only what only they can \
     decide (what the product should do, a trade-off with no clear winner, access or secrets), and then ask it as a \
     question block.\n\
     - The user approves the plan once; that approval also covers merging into develop. After it, Monaged builds, \
     reviews, integrates, brings develop in, has conflicts resolved and merges by itself; don't ask the user about any of \
     that, and don't call `merge_to_develop` yourself unless a merge stopped and you've fixed why.\n\
     - When something stops and waits for the user, say in two sentences what happened and the one thing they can do.\n\
     - When the merge into develop is done, tell the user in two or three sentences what landed and how it was checked."
        .to_string()
}

/// The lead's update when an agent asks a question on autopilot.
pub fn question_update(name: &str, asked: &str) -> String {
    format!(
        "{name} asks: {asked}. This run is on autopilot: if the request, the research, the plan or the code settles it, \
         answer {name} yourself with message_agent now, and tell the user in a sentence what you decided. Only if it's \
         really the user's call, leave it to them (it's on the dashboard) and ask them in one sentence."
    )
}

/// Autopilot doesn't merge into the top branch by itself.
pub fn top_branch_note(target: &str) -> String {
    format!(
        "Autopilot stops here: this project has no develop branch, and merging into {target} is always yours. Set up develop \
         and acceptance in the project view, or merge it yourself."
    )
}

impl Monaged {
    pub(crate) fn is_autopilot(&self, sub_id: i64) -> bool {
        self.pipelines.get(&sub_id).is_some_and(|p| p.autopilot)
    }

    /// The project chat's inputs, for the project on screen (made while
    /// rendering, since views can't make entities).
    pub(crate) fn prepare_project_chat(&mut self, project_id: i64, cx: &mut Context<Self>) {
        if self.project_chat.as_ref().is_some_and(|c| c.project_id == project_id) {
            return;
        }
        // Another project gets a fresh box.
        let ctrl_send = self.ctrl_send();
        let title = cx.new(|cx| TextInput::new("Title, e.g. Company car reservation", cx));
        let description = cx.new(|cx| {
            let mut input = TextInput::new("What should it do? Who uses it, what matters, what's out of scope.", cx).multiline().max_lines(8);
            input.set_ctrl_send(ctrl_send, cx);
            input
        });
        self.project_chat = Some(ProjectChat { project_id, title, description, busy: false, error: None, invalid: false });
    }

    /// Settings → Message box: whether Ctrl+Enter sends (and Enter adds a line).
    pub(crate) fn ctrl_send(&self) -> bool {
        self.store.setting("composer:send").as_deref() == Some("ctrl-enter")
    }

    /// Whether Enter came from the project chat's box.
    pub(crate) fn project_chat_focused(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.form.is_none()
            && self.sel_sub.is_none()
            && self.project_chat.as_ref().is_some_and(|c| {
                Some(c.project_id) == self.sel_project && (c.title.focus_handle(cx).is_focused(window) || c.description.focus_handle(cx).is_focused(window))
            })
    }

    /// Enter in the title moves to the description; Enter there starts it.
    pub(crate) fn project_chat_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = &self.project_chat else { return };
        if chat.title.focus_handle(cx).is_focused(window) && chat.description.read(cx).text().trim().is_empty() {
            window.focus(&chat.description.focus_handle(cx));
            return;
        }
        self.submit_project_chat(cx);
    }

    /// Makes the feature subproject and starts it on autopilot.
    pub(crate) fn submit_project_chat(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = &mut self.project_chat else { return };
        if chat.busy {
            return;
        }
        let title = chat.title.read(cx).text().trim().to_string();
        let description = chat.description.read(cx).text().trim().to_string();
        if title.is_empty() {
            chat.error = Some("Give it a title: it names the subproject and its branch.".into());
            chat.invalid = true;
            chat.title.update(cx, |input, cx| input.set_invalid(true, cx));
            cx.notify();
            return;
        }
        let project_id = chat.project_id;
        chat.busy = true;
        chat.error = None;
        chat.invalid = false;
        let taken: Vec<&str> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.branch.as_str()).collect();
        let branch = free_branch(&title, &taken);
        let request = request_text(&title, &description);
        cx.notify();
        self.make_subproject(project_id, SubKind::Feature, title.clone(), branch, cx, move |this, made, cx| {
            let Some(chat) = this.project_chat.as_mut().filter(|c| c.project_id == project_id) else { return };
            chat.busy = false;
            match made {
                Ok(sub_id) => {
                    chat.title.update(cx, |input, cx| input.clear(cx));
                    chat.description.update(cx, |input, cx| input.clear(cx));
                    let _ = this.store.log_event(sub_id, None, "pipeline", "Started from the project chat on autopilot");
                    this.start_pipeline_with(sub_id, request, false, true, cx);
                    this.toast_action(
                        Tone::Info,
                        format!("\"{title}\" is on autopilot. The researcher is reading the code; the lead tells you when it needs you."),
                        "Open",
                        ToastAction::Open { sub_id, agent_id: None },
                        cx,
                    );
                }
                Err(err) => chat.error = Some(format!("Couldn't start it: {err:#}")),
            }
            cx.notify();
        });
    }

    /// Turning autopilot off hands the run back to you (questions, plans and
    /// merges ask as usual); turning it on again lets it finish by itself.
    pub(crate) fn toggle_autopilot(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        p.autopilot = !p.autopilot;
        if p.autopilot {
            p.auto_mode = false;
        }
        let on = p.autopilot;
        self.save_pipeline(sub_id);
        self.tell_lead(
            sub_id,
            if on {
                format!("The user turned autopilot on. {}", lead_section())
            } else {
                "The user turned autopilot off: from now on questions, the plan and the merge into develop wait for them, as usual.".to_string()
            },
        );
        if on {
            // The lead is the conversation on autopilot, even where leads are off.
            self.start_lead(sub_id, cx);
            // What was waiting for you that autopilot decides itself goes on now.
            match self.pipelines.get(&sub_id).map(|p| p.stage) {
                Some(Stage::Decide) => self.start_planning(sub_id, cx),
                Some(Stage::Done) => self.maybe_auto_merge(sub_id, cx),
                _ => {}
            }
        }
        cx.notify();
    }

    /// The merge into develop: the feature branch scored below the bar after
    /// bringing develop in. On autopilot it gets the findings to fix (twice at
    /// most) instead of stopping. Returns whether it took over.
    pub(crate) fn autopilot_fix_before_merge(&mut self, sub_id: i64, agent_id: i64, score: u8, cx: &mut Context<Self>) -> bool {
        if !self.is_autopilot(sub_id) {
            return false;
        }
        let Some(ship) = self.ships.get_mut(&sub_id) else { return false };
        if ship.retries >= MERGE_RETRIES {
            return false;
        }
        let Some(prompt) = self.agent(agent_id).and_then(|a| a.confidence.as_ref()).and_then(review::fix_prompt) else { return false };
        let ship = self.ships.get_mut(&sub_id).expect("checked above");
        ship.retries += 1;
        let (round, target) = (ship.retries, ship.target.clone());
        ship.step = Step::Fixing(agent_id);
        let name = self.agent_name(agent_id);
        let min = self.min_score();
        self.send_prompt(agent_id, format!("{prompt}\n\nMonaged merges this branch into `{target}` once it's fixed and reviewed again. Commit your fixes; don't push."));
        let _ = self.store.log_event(sub_id, Some(agent_id), "merge", &format!("Autopilot: {name} at {score}/5 before merging; sent the findings to fix ({round} of {MERGE_RETRIES})"));
        self.ship_say(
            sub_id,
            format!("{name} scored {score}/5 after bringing {target} in, below the {min}/5 needed. Autopilot sent it the findings to fix ({round} of {MERGE_RETRIES}); the merge goes on when it passes."),
            cx,
        );
        true
    }

    /// The merge into develop: develop kept moving. On autopilot, start over
    /// (twice at most). Returns whether it did.
    pub(crate) fn autopilot_ship_again(&mut self, sub_id: i64, _cx: &mut Context<Self>) -> bool {
        if !self.is_autopilot(sub_id) {
            return false;
        }
        let Some(ship) = self.ships.get_mut(&sub_id).filter(|s| s.retries < MERGE_RETRIES) else { return false };
        ship.retries += 1;
        ship.rounds = 1;
        true
    }

    /// The merge into develop finished on autopilot: the run is over.
    pub(crate) fn autopilot_landed(&mut self, sub_id: i64, merged: bool, cx: &mut Context<Self>) {
        if !self.is_autopilot(sub_id) {
            return;
        }
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let target = self.ships.get(&sub_id).map(|s| s.target.clone()).unwrap_or_else(|| "develop".into());
        let what = if merged { format!("is merged into {target}") } else { format!("has its request open to merge into {target}") };
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.note = Some(format!("Autopilot is done: {} {what}.", sub.name));
        }
        self.save_pipeline(sub_id);
        let _ = self.store.log_event(sub_id, None, "merge", &format!("Autopilot finished: {} {what}", sub.branch));
        self.chime(crate::sound::Cue::Complete);
        self.toast_action(Tone::Info, format!("\"{}\" {what}.", sub.name), "Open", ToastAction::Open { sub_id, agent_id: None }, cx);
    }

    /// One line on where an autopilot run stands, for the project chat.
    fn autopilot_status(&self, sub_id: i64) -> (String, bool) {
        let needs: Vec<String> = self.inbox().into_iter().filter(|i| i.sub_id == sub_id).map(|i| i.title.to_string()).collect();
        if let Some(first) = needs.first() {
            let more = if needs.len() > 1 { format!(" (+{} more)", needs.len() - 1) } else { String::new() };
            return (format!("{first}{more}"), true);
        }
        if let Some(ship) = self.ships.get(&sub_id) {
            return (ship.status.clone(), ship.step == Step::Stuck);
        }
        let note = self.pipelines.get(&sub_id).and_then(|p| p.note.clone()).unwrap_or_default();
        (note, false)
    }

    /// What the lead said last, in a line (empty until its session is open).
    fn lead_line(&self, sub_id: i64) -> Option<String> {
        let lead = self.agent(self.lead_of(sub_id)?)?;
        let text = lead.items.iter().rev().find_map(|i| if let Item::Agent(t) = i { Some(t.clone()) } else { None })?;
        let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("```"))?.trim_start_matches('#').trim().to_string();
        (!line.is_empty()).then(|| review::clip(&line, 220))
    }

    /// The project chat: a box to start a feature on autopilot, and the runs
    /// it started, newest first.
    pub(crate) fn render_project_chat(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(chat) = self.project_chat.as_ref().filter(|c| c.project_id == project_id) else { return div().into_any_element() };
        let target = self.branch_info.get(&project_id).and_then(|i| i.chain.first().cloned());
        let top_only = self.branch_info.get(&project_id).is_some_and(|i| i.chain.len() == 1);
        let lands = match (&target, top_only) {
            (Some(t), false) => format!("merges it into {t}"),
            (Some(t), true) => format!("gets it ready to merge into {t} (yours to merge: there's no develop)"),
            (None, _) => "merges it into develop".to_string(),
        };
        let problem: Option<SharedString> = chat.invalid.then(|| chat.error.clone().unwrap_or_default().into());
        let other_error = chat.error.clone().filter(|_| !chat.invalid);
        let composer = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(ui::field_checked("Title", None, chat.title.clone(), problem, Some(cx)))
            .child(ui::field("Description", None, chat.description.clone()))
            .children(other_error.map(|e| ui::callout(Tone::Error, e)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(if chat.busy {
                        ui::busy_button("chat-start", Kind::Primary, "Starting…")
                    } else {
                        ui::button("chat-start", Kind::Primary, Some(Icon::Play), "Start on autopilot")
                            .on_click(cx.listener(|this, _, _, cx| this.submit_project_chat(cx)))
                    })
                    .child(div().flex_1())
                    .child({
                        let (send, line) = if self.ctrl_send() { ("Ctrl Enter", "Enter") } else { ("Enter", "Shift Enter") };
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.5))
                            .text_color(rgb(theme::text_3()))
                            .child(ui::kbd(send))
                            .child("start")
                            .child(ui::kbd(line))
                            .child("new line")
                    }),
            );

        let mut runs: Vec<i64> = self
            .subprojects
            .iter()
            .filter(|s| s.project_id == project_id && self.is_autopilot(s.id))
            .map(|s| s.id)
            .collect();
        runs.sort_unstable_by(|a, b| b.cmp(a));
        runs.truncate(THREADS_SHOWN);
        let threads = div().flex().flex_col().children(runs.into_iter().enumerate().map(|(i, sid)| {
            let sub = self.subprojects.iter().find(|s| s.id == sid).expect("listed above");
            let stage = self.pipelines.get(&sid).map_or(Stage::Stopped, |p| p.stage);
            let (status, red) = self.autopilot_status(sid);
            let landed = self.ships.get(&sid).is_some_and(|s| s.step == Step::Done);
            let working = self.agents.iter().any(|a| a.record.subproject_id == sid && a.is_working());
            let state = if red {
                ui::State::NeedsYou
            } else if working || self.ship_running(sid) {
                ui::State::Working
            } else if landed || stage == Stage::Done {
                ui::State::Done
            } else {
                ui::State::Idle
            };
            let progress: AnyElement = if landed {
                ui::mono("merged").into_any_element()
            } else {
                match stage {
                    Stage::Done => ui::mono("built").into_any_element(),
                    Stage::Stopped => ui::mono("stopped").into_any_element(),
                    s => ui::stage_dots(format!("chat-stages-{sid}"), s.index(), Some(s.index()), pipeline::STAGES.len()),
                }
            };
            let lead = self.lead_line(sid);
            div()
                .id(("chat-run", sid as usize))
                .flex()
                .gap(px(12.))
                .px(px(14.))
                .py(px(12.))
                .cursor_pointer()
                .when(i > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                .hover(|s| s.bg(rgb(theme::hover())))
                .child(div().pt(px(5.)).child(ui::status_dot(("chat-dot", sid as usize), state)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(sub.name.clone()))
                                .child(div().flex_none().child(progress)),
                        )
                        .when(!status.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(if red { theme::red_text() } else { theme::text_2() }))
                                    .child(review::clip(&status, 260)),
                            )
                        })
                        .children(lead.map(|line| {
                            div()
                                .flex()
                                .gap(px(6.))
                                .text_size(px(12.))
                                .text_color(rgb(theme::text_3()))
                                .child(icon_sized(Icon::Flow, 12.).text_color(rgb(theme::text_4())))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(format!("Lead: {line}")))
                        })),
                )
                .child(div().pt(px(3.)).child(icon_sized(Icon::ChevronRight, 14.).text_color(rgb(theme::text_4()))))
                .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sid, cx)))
        }));
        let has_runs = self.subprojects.iter().any(|s| s.project_id == project_id && self.is_autopilot(s.id));

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().flex().items_center().gap(px(8.)).child(icon(Icon::Flow).size(px(14.))).child(ui::label("Project chat")))
            .child(
                ui::card()
                    .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(format!(
                        "Describe a feature and Monaged takes it from there: it researches, plans, builds, reviews and {lands}, \
                         resolving conflicts itself. It asks you once to approve the plan, and otherwise only what only you can decide."
                    )))
                    .child(composer),
            )
            .when(has_runs, |d| {
                d.child(
                    div()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .overflow_hidden()
                        .child(threads),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Gate, GateStep, QualityGate, autopilot_decide_gate, autopilot_plan_gate};
    use serde_json::json;

    #[test]
    fn the_request_is_the_title_and_the_description() {
        assert_eq!(request_text("  Car reservations ", ""), "Car reservations");
        assert_eq!(request_text("Car reservations", " Employees reserve a car.\n"), "# Car reservations\n\nEmployees reserve a car.");
    }

    #[test]
    fn a_new_feature_gets_a_branch_nobody_uses() {
        assert_eq!(free_branch("Car reservations", &[]), "feature/car-reservations");
        assert_eq!(free_branch("Car reservations", &["feature/car-reservations"]), "feature/car-reservations-2");
        assert_eq!(free_branch("Car reservations", &["feature/car-reservations", "feature/car-reservations-2"]), "feature/car-reservations-3");
        assert_eq!(free_branch("!!!", &[]), "feature/untitled");
    }

    #[test]
    fn autopilot_goes_on_after_verification_even_with_major_issues() {
        let ok = vec![json!({ "verdict": "confirmed" }), json!({ "verdict": "minor_issues" })];
        assert!(matches!(autopilot_decide_gate(&ok), Gate::Auto(_)));
        let major = vec![json!({ "verdict": "major_issues" }), json!({ "verdict": "confirmed" })];
        let Gate::Auto(why) = autopilot_decide_gate(&major) else { panic!("autopilot doesn't stop at decide") };
        assert!(why.contains("1 verifier found major issues") && why.contains("see the plan"));
    }

    #[test]
    fn autopilot_always_asks_once_for_the_plan() {
        let small = json!({ "size": "small", "confidence": 5, "risk_flags": [] });
        let Gate::Ask(why) = autopilot_plan_gate(&small) else { panic!("the plan is the one approval") };
        assert!(why.contains("asks once") && why.contains("develop"));
        let risky = json!({ "size": "large", "confidence": 3, "risk_flags": ["migration", "public_api"] });
        let Gate::Ask(why) = autopilot_plan_gate(&risky) else { panic!("asks") };
        assert!(why.contains("migration, public api"));
    }

    #[test]
    fn autopilot_runs_get_more_fix_rounds_before_asking() {
        let mut p = crate::pipeline::Pipeline::new(1, "x".into(), false);
        assert_eq!(p.fix_rounds(), pipeline::MAX_FIX_ROUNDS);
        p.autopilot = true;
        assert_eq!(p.fix_rounds(), pipeline::AUTOPILOT_FIX_ROUNDS);
        let mut gate = QualityGate::default();
        for round in 1..=pipeline::AUTOPILOT_FIX_ROUNDS {
            assert_eq!(gate.step("build-api", Some(2), "", 4, p.fix_rounds()), GateStep::Fix(round));
            gate.fix("build-api", true);
        }
        assert!(matches!(gate.step("build-api", Some(2), "", 4, p.fix_rounds()), GateStep::Hold(_)));
        // Without autopilot the gate still asks after two rounds.
        assert!(matches!(gate.step("build-api", Some(2), "", 4, pipeline::MAX_FIX_ROUNDS), GateStep::Hold(_)));
    }

    #[test]
    fn the_lead_knows_it_runs_on_autopilot() {
        let brief = crate::lead::brief("Car reservations", "feature/car-reservations", Some("Reserve cars"), true);
        assert!(brief.contains("## Autopilot") && brief.contains("answer it yourself"));
        assert!(!crate::lead::brief("x", "b", Some("y"), false).contains("## Autopilot"));
        assert!(question_update("planner", "\"Which DB?\" (SQLite / Postgres)").contains("answer planner yourself"));
        assert!(top_branch_note("main").contains("always yours"));
    }
}
