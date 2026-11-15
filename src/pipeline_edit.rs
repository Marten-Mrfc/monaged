//! Changing a pipeline that's under way:
//! - edit the plan before you approve it (its title, summary and workstreams:
//!   what each builder does and which files it owns);
//! - run one stage again (research, verification, the plan, the build or the
//!   integration) or just one builder, without starting over.

use gpui::{AnyElement, Context, Entity, Focusable, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::agent::Tone;
use crate::icons::Icon;
use crate::pipeline::{self, Role, Stage};
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Form, Monaged, theme};

/// One workstream in the plan editor.
pub(crate) struct PlanRow {
    pub name: Entity<TextInput>,
    pub goal: Entity<TextInput>,
    pub owns: Entity<TextInput>,
}

fn input(placeholder: &str, text: &str, cx: &mut Context<Monaged>) -> Entity<TextInput> {
    let (placeholder, text) = (placeholder.to_string(), text.to_string());
    cx.new(|cx| {
        let mut input = TextInput::new(placeholder, cx);
        input.set_text(text, cx);
        input
    })
}

impl Monaged {
    // ---- Plan editor -------------------------------------------------------------------------

    pub(crate) fn open_plan_editor(&mut self, sub_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.pipelines.get(&sub_id).and_then(|p| p.plan.clone()) else { return };
        let title = input("A short title", &pipeline::plan_title(&plan), cx);
        let summary = input("What gets built, in a sentence or two", plan["summary"].as_str().unwrap_or_default(), cx);
        let rows = pipeline::workstreams(&plan)
            .into_iter()
            .map(|w| {
                let owns = w.value["owns"].as_array().into_iter().flatten().filter_map(|o| o.as_str()).collect::<Vec<_>>().join(", ");
                PlanRow {
                    name: input("name", &w.name, cx),
                    goal: input("What this builder does", w.value["goal"].as_str().unwrap_or_default(), cx),
                    owns: input("Files it owns, comma separated (src/api/**, tests/api_test.py)", &owns, cx),
                }
            })
            .collect();
        window.focus(&title.focus_handle(cx));
        self.form = Some(Form::EditPlan { sub_id, title, summary, rows, error: None, invalid: None });
        cx.notify();
    }

    pub(crate) fn add_plan_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = match &self.form {
            Some(Form::EditPlan { rows, .. }) => rows.len(),
            _ => return,
        };
        if count >= pipeline::MAX_BUILDERS {
            self.toast(Tone::Warn, format!("A plan has at most {} workstreams.", pipeline::MAX_BUILDERS), cx);
            return;
        }
        let row = PlanRow {
            name: input("name", &format!("part-{}", count + 1), cx),
            goal: input("What this builder does", "", cx),
            owns: input("Files it owns, comma separated", "", cx),
        };
        window.focus(&row.goal.focus_handle(cx));
        if let Some(Form::EditPlan { rows, .. }) = &mut self.form {
            rows.push(row);
        }
        cx.notify();
    }

    pub(crate) fn remove_plan_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(Form::EditPlan { rows, .. }) = &mut self.form
            && index < rows.len()
            && rows.len() > 1
        {
            rows.remove(index);
        }
        cx.notify();
    }

    /// Saves the edited plan into the pipeline (it still waits for your approval).
    pub(crate) fn save_plan_edit(&mut self, cx: &mut Context<Self>) {
        let Some(Form::EditPlan { sub_id, title, summary, rows, error, invalid }) = &mut self.form else { return };
        let sub_id = *sub_id;
        let mut workstreams = Vec::new();
        let mut names = Vec::new();
        for row in rows.iter() {
            let name = crate::git::slug(row.name.read(cx).text());
            let goal = row.goal.read(cx).text().trim().to_string();
            let owns: Vec<String> = row.owns.read(cx).text().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if goal.is_empty() {
                *error = Some(format!("Say what the \"{name}\" builder does."));
                *invalid = Some("Workstreams");
                cx.notify();
                return;
            }
            if names.contains(&name) {
                *error = Some(format!("Two workstreams are called \"{name}\"; builders need different names."));
                *invalid = Some("Workstreams");
                cx.notify();
                return;
            }
            names.push(name.clone());
            workstreams.push(json!({ "name": name, "goal": goal, "owns": owns }));
        }
        let (title, summary) = (title.read(cx).text().trim().to_string(), summary.read(cx).text().trim().to_string());
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        let mut plan = p.plan.clone().unwrap_or_else(|| json!({}));
        if let Value::Object(map) = &mut plan {
            if !title.is_empty() {
                map.insert("title".into(), Value::String(title));
            }
            map.insert("summary".into(), Value::String(summary));
            map.insert("workstreams".into(), Value::Array(workstreams));
            map.insert("edited_by_developer".into(), Value::Bool(true));
        }
        p.plan = Some(plan);
        p.note = Some("You edited the plan. Approve it to start the builders.".into());
        self.save_pipeline(sub_id);
        let _ = self.store.log_event(sub_id, None, "stage", "You edited the plan");
        self.form = None;
        self.show_inspector(sub_id, crate::shell::InspectorTab::Pipeline, cx);
        self.toast(Tone::Info, "Saved your changes to the plan.", cx);
        cx.notify();
    }

    /// The editor's fields, for `render_form`.
    pub(crate) fn render_plan_editor(&self, title: &Entity<TextInput>, summary: &Entity<TextInput>, rows: &[PlanRow], problem: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let row_count = rows.len();
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(ui::field("Title", None, title.clone()))
            .child(ui::field("Summary", Some("Builders read this first."), summary.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().text_size(px(12.5)).text_color(rgb(theme::text_2())).child(format!(
                                "Workstreams: one builder each, in parallel ({row_count} of at most {})",
                                pipeline::MAX_BUILDERS
                            )))
                            .child(ui::button("plan-add-row", Kind::Secondary, Some(Icon::Plus), "Add workstream").on_click(cx.listener(|this, _, window, cx| this.add_plan_row(window, cx)))),
                    )
                    .children(rows.iter().enumerate().map(|(i, row)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(12.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(theme::line()))
                            .bg(rgb(theme::panel()))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(div().w(px(200.)).flex_none().child(row.name.clone()))
                                    .child(div().flex_1().min_w(px(0.)).child(row.goal.clone()))
                                    .when(row_count > 1, |d| {
                                        d.child(ui::icon_button(("plan-row-remove", i), Icon::Trash).tooltip(crate::shell::tip("Remove this workstream")).on_click(cx.listener(
                                            move |this, _, _, cx| this.remove_plan_row(i, cx),
                                        )))
                                    }),
                            )
                            .child(row.owns.clone())
                    }))
                    .children(problem.map(|p| ui::callout(Tone::Error, p))),
            )
            .child(ui::meta("Builders only edit the files their workstream owns, so keep owners from overlapping. Contracts, tests and risks stay as the planner wrote them."))
            .into_any_element()
    }

    // ---- Running a stage again ---------------------------------------------------------------

    /// Starts one stage over (asked to confirm first, since it starts agents).
    pub(crate) fn rerun_stage(&mut self, sub_id: i64, stage: Stage, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get_mut(&sub_id) else { return };
        match stage {
            Stage::Research => {
                let request = p.request.clone();
                p.stage = Stage::Research;
                p.research = None;
                p.verifications = pipeline::empty_verifications();
                p.plan = None;
                p.done.clear();
                p.note = Some("The researcher is reading the code again.".into());
                self.save_pipeline(sub_id);
                self.spawn_agent(sub_id, Self::role_spec("research", Role::Researcher, pipeline::researcher_brief(&request), true), cx);
            }
            Stage::Verify => self.start_verification(sub_id, cx),
            Stage::Plan => self.start_planning(sub_id, cx),
            Stage::Build => self.start_build(sub_id, cx),
            Stage::Integrate => self.start_integration(sub_id, cx),
            _ => return,
        }
        let _ = self.store.log_event(sub_id, None, "stage", &format!("You ran {} again", stage.label().to_lowercase()));
        self.toast(Tone::Info, format!("Running {} again.", stage.label().to_lowercase()), cx);
        cx.notify();
    }

    /// Starts a fresh builder for one workstream (the old one stays, for comparison).
    pub(crate) fn rerun_builder(&mut self, sub_id: i64, workstream: &str, cx: &mut Context<Self>) {
        let Some(p) = self.pipelines.get(&sub_id) else { return };
        let Some(plan) = p.plan.clone() else { return };
        let Some(stream) = pipeline::workstreams(&plan).into_iter().find(|w| w.name == workstream) else { return };
        let request = p.request.clone();
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let name = self.unique_agent_name(sub_id, &format!("build-{}", stream.name));
        let branch = format!("agent/{}/{name}", crate::git::slug(&sub.name));
        let brief = pipeline::builder_brief(&request, &plan, &stream, &branch);
        let replaced: Vec<String> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && a.role() == Role::Builder(stream.name.clone()))
            .map(|a| a.record.name.clone())
            .collect();
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.stage = Stage::Build;
            p.done.retain(|n| !replaced.contains(n));
            for old in &replaced {
                p.gate.clear(old);
            }
            p.note = Some(format!("A new builder is redoing {}.", stream.name));
        }
        self.save_pipeline(sub_id);
        self.spawn_agent(sub_id, Self::role_spec(&name, Role::Builder(stream.name.clone()), brief, true), cx);
        let _ = self.store.log_event(sub_id, None, "stage", &format!("You restarted the {} builder", stream.name));
        self.toast(Tone::Info, format!("Started {name} for {}.", stream.name), cx);
        cx.notify();
    }
}
