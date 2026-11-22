//! The plan page: a plan as a document you read, comment on and approve.
//!
//! - **Header**: the title, its version (step back through earlier ones),
//!   where it stands, and Approve / Send comments / Edit / Stop.
//! - **Outline** on the left: its sections; click one to go there.
//! - **The document**: Markdown with tables and drawn diagrams. Point at any
//!   part (a heading, a paragraph, a table, a diagram) for **Comment**; your
//!   comments sit under what they're about.
//! - **At a glance** on the right: size, confidence, risks, cost, the build
//!   order as a diagram, every comment with where it stands, and a comment on
//!   the whole plan.
//!
//! **Send to planner** sends all new comments at once; the planner answers
//! with the next version, which says what changed and waits for you again.

use gpui::{AnyElement, Context, Entity, Focusable, ScrollHandle, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon, icon_sized};
use crate::markdown::Block;
use crate::pipeline::{self, Role, Stage};
use crate::plan::{self, PlanComment};
use crate::review_ui::render_block;
use crate::shell::tip;
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, review, theme};

/// Below this the outline and the side panel fold into the one column.
const WIDE: f32 = 1180.;
const OUTLINE_W: f32 = 210.;
const SIDE_W: f32 = 320.;
const DOC_MAX: f32 = 820.;

pub struct PlanView {
    pub sub_id: i64,
    /// The version on screen; None: the latest.
    pub shown: Option<u32>,
    /// A comment being written: on a block, or (None) on the whole plan.
    pub composing: Option<(Option<usize>, Entity<TextInput>)>,
    /// The box for a comment on the whole plan.
    pub general: Entity<TextInput>,
    pub scroll: ScrollHandle,
}

impl Monaged {
    // ---- Plans as data ----------------------------------------------------------

    /// A plan from the planner: keeps it as the next version. Returns its number.
    pub(crate) fn add_plan_version(&mut self, sub_id: i64, reply: &str, value: &serde_json::Value) -> u32 {
        let document = plan::usable_document(reply, value);
        let book = self.plans.entry(sub_id).or_default();
        let version = book.next_version();
        let _ = self.store.add_plan_version(sub_id, version, &document, value);
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        book.versions.push(plan::PlanVersion { version, document, plan: value.clone(), at });
        version
    }

    /// A builder's brief, with the plan document the developer approved.
    pub(crate) fn with_plan_document(&self, sub_id: i64, brief: String) -> String {
        match self.plans.get(&sub_id).and_then(|b| b.latest()) {
            Some(v) => format!("{brief}\n\n## The plan document (as the developer approved it)\n\n{}", review::clip(&v.document, 16_000)),
            None => brief,
        }
    }

    // ---- Opening and closing ---------------------------------------------------

    pub(crate) fn open_plan(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if !self.plans.get(&sub_id).is_some_and(|b| !b.versions.is_empty()) {
            self.toast(Tone::Info, "There's no plan yet: the planner writes it after the research.", cx);
            return;
        }
        self.form = None;
        self.select_sub(sub_id, cx);
        // The lead's box isn't on this page: it mustn't take the keys.
        self.focus_request = None;
        let general = cx.new(|cx| TextInput::new("A comment on the whole plan…", cx).multiline().max_lines(6));
        self.plan_view = Some(PlanView { sub_id, shown: None, composing: None, general, scroll: ScrollHandle::new() });
        cx.notify();
    }

    pub(crate) fn close_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.plan_view = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// Whether the plan page is what's on screen.
    pub(crate) fn plan_on_screen(&self) -> Option<i64> {
        let view = self.plan_view.as_ref()?;
        (self.form.is_none() && self.sel_agent.is_none() && self.sel_sub == Some(view.sub_id) && self.sub_view.is_none()).then_some(view.sub_id)
    }

    fn show_version(&mut self, version: u32, cx: &mut Context<Self>) {
        if let Some(view) = &mut self.plan_view {
            let latest = self.plans.get(&view.sub_id).and_then(|b| b.latest()).map(|v| v.version);
            view.shown = (Some(version) != latest).then_some(version);
            view.composing = None;
            view.scroll.scroll_to_item(0);
        }
        cx.notify();
    }

    // ---- Comments ----------------------------------------------------------------

    fn start_plan_comment(&mut self, block: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = &mut self.plan_view else { return };
        let input = cx.new(|cx| TextInput::new("What should change, or what's unclear?", cx).multiline().max_lines(8));
        window.focus(&input.focus_handle(cx));
        view.composing = Some((block, input));
        cx.notify();
    }

    fn cancel_plan_comment(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(view) = &mut self.plan_view else { return false };
        let had = view.composing.take().is_some();
        cx.notify();
        had
    }

    /// Esc on the plan page: a comment being written first, then the page.
    pub(crate) fn plan_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.plan_on_screen().is_none() {
            return false;
        }
        if !self.cancel_plan_comment(cx) {
            self.close_plan(window, cx);
        }
        true
    }

    /// Enter in a comment box on the plan page saves it.
    pub(crate) fn plan_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(view) = &self.plan_view else { return false };
        if view.composing.as_ref().is_some_and(|(_, input)| input.focus_handle(cx).is_focused(window)) {
            self.save_plan_comment(cx);
            return true;
        }
        if view.general.focus_handle(cx).is_focused(window) {
            self.save_general_comment(cx);
            return true;
        }
        false
    }

    fn save_plan_comment(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &mut self.plan_view else { return };
        let Some((block, input)) = view.composing.take() else { return };
        let body = input.read(cx).text().trim().to_string();
        if body.is_empty() {
            cx.notify();
            return;
        }
        let sub_id = view.sub_id;
        let shown = view.shown;
        self.add_plan_comment(sub_id, shown, block, body, cx);
    }

    fn save_general_comment(&mut self, cx: &mut Context<Self>) {
        let Some(view) = &self.plan_view else { return };
        let body = view.general.read(cx).text().trim().to_string();
        if body.is_empty() {
            return;
        }
        let (sub_id, shown, general) = (view.sub_id, view.shown, view.general.clone());
        general.update(cx, |input, cx| input.clear(cx));
        self.add_plan_comment(sub_id, shown, None, body, cx);
    }

    fn add_plan_comment(&mut self, sub_id: i64, shown: Option<u32>, block: Option<usize>, body: String, cx: &mut Context<Self>) {
        let Some(book) = self.plans.get(&sub_id) else { return };
        let Some(version) = shown.and_then(|n| book.version(n)).or(book.latest()) else { return };
        let quote = block.and_then(|i| plan::parse(&version.document).get(i).map(plan::block_text)).unwrap_or_default();
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut comment = PlanComment { id: 0, sub_id, version: version.version, block, quote: review::clip(&quote, 600), body, sent_in: None, at };
        match self.store.add_plan_comment(&comment) {
            Ok(id) => {
                comment.id = id;
                self.plans.entry(sub_id).or_default().comments.push(comment);
            }
            Err(err) => self.toast_error(format!("Couldn't save the comment: {err:#}"), cx),
        }
        cx.notify();
    }

    fn remove_plan_comment(&mut self, sub_id: i64, id: i64, cx: &mut Context<Self>) {
        let _ = self.store.remove_plan_comment(id);
        if let Some(book) = self.plans.get_mut(&sub_id) {
            book.comments.retain(|c| c.id != id);
        }
        cx.notify();
    }

    /// The planner of this subproject's latest run.
    fn planner_of(&self, sub_id: i64) -> Option<i64> {
        self.agents.iter().rev().find(|a| a.record.subproject_id == sub_id && !a.record.archived && a.role() == Role::Planner).map(|a| a.record.id)
    }

    /// Sends every new comment to the planner in one message.
    pub(crate) fn send_plan_comments(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(book) = self.plans.get(&sub_id) else { return };
        let Some(latest) = book.latest().map(|v| v.version) else { return };
        let unsent: Vec<PlanComment> = book.unsent().into_iter().cloned().collect();
        if unsent.is_empty() {
            return;
        }
        let Some(planner) = self.planner_of(sub_id) else {
            self.toast(Tone::Warn, "The planner isn't there any more. Edit the plan yourself, or run the pipeline again.", cx);
            return;
        };
        let items: Vec<(Option<String>, String, String)> = unsent
            .iter()
            .map(|c| {
                let blocks = book.version(c.version).map(|v| plan::parse(&v.document)).unwrap_or_default();
                let section = c.block.and_then(|i| (i < blocks.len()).then(|| plan::section_of(&blocks, i)).flatten());
                (section, c.quote.clone(), c.body.clone())
            })
            .collect();
        let message = plan::comments_message(latest, &items);
        let ids: Vec<i64> = unsent.iter().map(|c| c.id).collect();
        let _ = self.store.mark_plan_comments_sent(&ids, latest);
        if let Some(book) = self.plans.get_mut(&sub_id) {
            for c in book.comments.iter_mut().filter(|c| ids.contains(&c.id)) {
                c.sent_in = Some(latest);
            }
        }
        // Your words: they go out whatever the budget says.
        self.bypass_budget = true;
        self.send_prompt(planner, message);
        self.bypass_budget = false;
        let n = ids.len();
        if let Some(p) = self.pipelines.get_mut(&sub_id) {
            p.stage = Stage::Plan;
            p.note = Some(format!("The planner is revising the plan after your {n} comment{}.", if n == 1 { "" } else { "s" }));
        }
        self.save_pipeline(sub_id);
        let _ = self.store.log_event(sub_id, Some(planner), "stage", &format!("{n} plan comment(s) sent to the planner (on version {latest})"));
        self.tell_lead(sub_id, format!("The user sent {n} comment(s) on the plan (version {latest}) to the planner, who's revising it. Nothing to do unless they ask."));
        self.toast(Tone::Info, format!("Sent {n} comment{} to the planner. The revised plan opens here when it's ready.", if n == 1 { "" } else { "s" }), cx);
        cx.notify();
    }

    // ---- The page ----------------------------------------------------------------

    pub(crate) fn render_plan_view(&self, sub_id: i64, main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let (Some(view), Some(book)) = (self.plan_view.as_ref(), self.plans.get(&sub_id)) else { return div().into_any_element() };
        let Some(latest) = book.latest() else { return div().into_any_element() };
        let version = view.shown.and_then(|n| book.version(n)).unwrap_or(latest);
        let is_latest = version.version == latest.version;
        let blocks = plan::parse(&version.document);
        let wide = main_w >= WIDE;
        let pipeline = self.pipelines.get(&sub_id);
        let stage = pipeline.map_or(Stage::Stopped, |p| p.stage);

        let header = self.plan_header(sub_id, version, latest.version, stage, cx);
        let document = self.plan_document(sub_id, view, &blocks, version.version, is_latest, wide, cx);
        let body = if wide {
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .child(self.plan_outline(view, &blocks))
                .child(document)
                .child(
                    div()
                        .id("plan-side")
                        .w(px(SIDE_W))
                        .flex_none()
                        .h_full()
                        .overflow_y_scroll()
                        .border_l_1()
                        .border_color(rgb(theme::line()))
                        .child(self.plan_side(sub_id, view, version, latest.version, cx)),
                )
        } else {
            div().flex_1().min_h(px(0.)).flex().child(document)
        };
        div().flex_1().min_h(px(0.)).flex().flex_col().child(header).child(body).into_any_element()
    }

    fn plan_header(&self, sub_id: i64, version: &plan::PlanVersion, latest: u32, stage: Stage, cx: &mut Context<Self>) -> AnyElement {
        let value = &version.plan;
        let title = pipeline::plan_title(value);
        let autopilot = self.is_autopilot(sub_id);
        let book = self.plans.get(&sub_id);
        let unsent = book.map_or(0, |b| b.unsent().len());
        let waiting = book.is_some_and(plan::PlanBook::waiting);
        let n = version.version;

        // Where it stands, in a line.
        let (status, attention) = match stage {
            Stage::Approve if version.version == latest => (if autopilot { "Waiting for your approval: then it builds and merges into develop by itself" } else { "Waiting for your approval" }.to_string(), true),
            Stage::Plan if waiting => ("The planner is revising it after your comments…".to_string(), false),
            Stage::Plan => ("The planner is writing it…".to_string(), false),
            Stage::Build => ("Approved · building".to_string(), false),
            Stage::Integrate => ("Approved · integrating".to_string(), false),
            Stage::Done => ("Approved · built".to_string(), false),
            Stage::Stopped => ("Stopped".to_string(), false),
            _ => (String::new(), false),
        };

        let nav = div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                ui::icon_button("plan-prev", Icon::ChevronLeft)
                    .tooltip(tip("The version before"))
                    .when(n > 1, |d| d.on_click(cx.listener(move |this, _, _, cx| this.show_version(n - 1, cx))))
                    .when(n <= 1, |d| d.opacity(0.35)),
            )
            .child(ui::mono(if version.at > 0 { format!("v{n} of {latest} · {}", crate::dashboard::ago(version.at)) } else { format!("v{n} of {latest}") }))
            .child(
                ui::icon_button("plan-next", Icon::ChevronRight)
                    .tooltip(tip("The version after"))
                    .when(n < latest, |d| d.on_click(cx.listener(move |this, _, _, cx| this.show_version(n + 1, cx))))
                    .when(n >= latest, |d| d.opacity(0.35)),
            );

        let strings = |key: &str| -> Vec<String> { value[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect() };
        let confidence = value["confidence"].as_u64().unwrap_or(0) as u8;
        let mut meta = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(crate::pipeline_ui::pill(value["size"].as_str().unwrap_or("?"), false))
            .child(div().flex().items_center().gap(px(6.)).child(ui::score_meter(confidence)).child(ui::mono(format!("{confidence}/5"))));
        for risk in strings("risk_flags") {
            meta = meta.child(crate::pipeline_ui::pill(&risk.replace('_', " "), true));
        }
        meta = meta.child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(self.plan_estimate_line(value)));

        let actionable = stage == Stage::Approve && version.version == latest;
        let mut actions = div().flex().flex_wrap().items_center().gap(px(6.));
        if unsent > 0 && matches!(stage, Stage::Approve | Stage::Plan) {
            actions = actions.child(
                ui::button("plan-send", Kind::Secondary, Some(Icon::Send), format!("Send {unsent} comment{} to the planner", if unsent == 1 { "" } else { "s" }))
                    .tooltip(tip("The planner answers with a new version, which waits for you again"))
                    .on_click(cx.listener(move |this, _, _, cx| this.send_plan_comments(sub_id, cx))),
            );
        }
        if actionable {
            let target = self.merge_target(sub_id).unwrap_or_else(|| "develop".into());
            let builders = pipeline::workstreams(value).len().max(1);
            let label = if autopilot {
                format!("Approve: build and merge into {target}")
            } else {
                format!("Approve and build ({builders} builder{})", if builders == 1 { "" } else { "s" })
            };
            actions = actions
                .child(
                    ui::button("plan-edit", Kind::Ghost, Some(Icon::Edit), "Edit")
                        .tooltip(tip("Change the workstreams yourself"))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_plan_editor(sub_id, window, cx))),
                )
                .child(ui::button("plan-stop", Kind::Ghost, Some(Icon::Stop), "Stop").on_click(cx.listener(move |this, _, _, cx| this.stop_pipeline(sub_id, cx))))
                .child(if unsent > 0 {
                    ui::disabled_button("plan-approve", Kind::Primary, Some(Icon::Check), label).tooltip(tip("Send or remove your comments first"))
                } else {
                    ui::button("plan-approve", Kind::Primary, Some(Icon::Check), label).on_click(cx.listener(move |this, _, _, cx| {
                        this.plan_view = None;
                        this.start_build(sub_id, cx);
                    }))
                });
        }

        div()
            .flex_none()
            .px(px(24.))
            .pt(px(16.))
            .pb(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        ui::button("plan-back", Kind::Ghost, Some(Icon::ChevronLeft), "Dashboard")
                            .tooltip(crate::shell::tip_key("Back to the dashboard", "Esc"))
                            .on_click(cx.listener(|this, _, window, cx| this.close_plan(window, cx))),
                    )
                    .child(div().flex_1().min_w(px(0.)).flex().items_center().gap(px(10.)).child(icon(Icon::Flow)).child(div().min_w(px(0.)).truncate().child(ui::heading(title))))
                    .child(nav),
            )
            .child(meta)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(200.))
                            .text_size(px(13.))
                            .text_color(rgb(if attention { theme::text() } else { theme::text_2() }))
                            .when(stage == Stage::Plan, |d| d.child(div().flex().items_center().gap(px(8.)).child(ui::spinner("plan-wait", 12., theme::text_3())).child(status.clone())))
                            .when(stage != Stage::Plan, |d| d.child(status.clone())),
                    )
                    .child(actions),
            )
            .into_any_element()
    }

    fn plan_outline(&self, view: &PlanView, blocks: &[Block]) -> AnyElement {
        let items = plan::outline(blocks);
        div()
            .id("plan-outline")
            .w(px(OUTLINE_W))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(rgb(theme::line()))
            .px(px(12.))
            .py(px(16.))
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(div().pb(px(6.)).child(ui::label("Contents")))
            .children(items.into_iter().map(|(index, level, text)| {
                let scroll = view.scroll.clone();
                div()
                    .id(("plan-toc", index))
                    .pl(px(((level.max(1) - 1) as f32) * 12.))
                    .py(px(3.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_size(px(if level <= 1 { 13. } else { 12.5 }))
                    .text_color(rgb(if level <= 2 { theme::text_2() } else { theme::text_3() }))
                    .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
                    .truncate()
                    .child(text)
                    .on_click(move |_, _, _| scroll.scroll_to_top_of_item(index))
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_document(&self, sub_id: i64, view: &PlanView, blocks: &[Block], version: u32, is_latest: bool, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let comments: Vec<&PlanComment> = self.plans.get(&sub_id).map(|b| b.comments.iter().filter(|c| c.version == version).collect()).unwrap_or_default();
        let latest = self.plans.get(&sub_id).and_then(|b| b.latest()).map_or(version, |v| v.version);
        let can_comment = matches!(self.pipelines.get(&sub_id).map(|p| p.stage), Some(Stage::Approve | Stage::Plan));
        let mut column = div()
            .id("plan-doc")
            .track_scroll(&view.scroll)
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_y_scroll()
            .px(px(28.))
            .py(px(20.));
        // One child per block, so the outline can scroll to a block by its index.
        for (i, block) in blocks.iter().enumerate() {
            let group: SharedString = format!("plan-block-{i}").into();
            let here: Vec<&PlanComment> = comments.iter().copied().filter(|c| c.block == Some(i)).collect();
            let composing = view.composing.as_ref().filter(|(b, _)| *b == Some(i)).map(|(_, input)| input.clone());
            let commented = !here.is_empty();
            column = column.child(
                div().w_full().flex().justify_center().child(
                    div()
                        .id(("plan-block", i))
                        .group(group.clone())
                        .w_full()
                        .max_w(px(DOC_MAX))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .py(px(5.))
                        .child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .pl(px(10.))
                                        .border_l_2()
                                        .border_color(rgb(if commented { theme::line_strong() } else { theme::bg() }))
                                        .text_size(px(14.))
                                        .text_color(rgb(theme::text_body()))
                                        .child(render_block(block.clone())),
                                )
                                .when(can_comment, |d| {
                                    d.child(
                                        div().flex_none().w(px(28.)).child(
                                            ui::icon_button(("plan-comment", i), Icon::Ask)
                                                .invisible()
                                                .group_hover(group.clone(), |s| s.visible())
                                                .tooltip(tip("Comment on this"))
                                                .on_click(cx.listener(move |this, _, window, cx| this.start_plan_comment(Some(i), window, cx))),
                                        ),
                                    )
                                }),
                        )
                        .children(here.into_iter().map(|c| self.comment_card(c, latest, cx)))
                        .children(composing.map(|input| self.comment_composer(input, cx))),
                ),
            );
        }
        if !is_latest {
            column = column.child(
                div().w_full().flex().justify_center().pt(px(12.)).child(
                    div().max_w(px(DOC_MAX)).w_full().child(
                        ui::callout(Tone::Info, format!("You're reading version {version}; version {latest} is the latest.")).child(
                            ui::button("plan-latest", Kind::Secondary, None, "Show the latest").on_click(cx.listener(move |this, _, _, cx| this.show_version(latest, cx))),
                        ),
                    ),
                ),
            );
        }
        // Narrow: what the side panel holds comes after the document.
        if !wide && let Some(book) = self.plans.get(&sub_id) {
            let version = book.version(version).or(book.latest());
            if let Some(version) = version {
                column = column.child(
                    div().w_full().flex().justify_center().pt(px(20.)).child(
                        div().max_w(px(DOC_MAX)).w_full().border_t_1().border_color(rgb(theme::line())).child(self.plan_side(sub_id, view, version, latest, cx)),
                    ),
                );
            }
        }
        column.child(div().h(px(48.))).into_any_element()
    }

    fn comment_card(&self, c: &PlanComment, latest: u32, cx: &mut Context<Self>) -> AnyElement {
        let (sub_id, id) = (c.sub_id, c.id);
        let state = plan::comment_state(c, latest);
        div()
            .ml(px(12.))
            .p(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(theme::line()))
            .bg(rgb(theme::panel()))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().text_size(px(13.)).line_height(px(19.)).text_color(rgb(theme::text())).child(c.body.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon_sized(if c.sent_in.is_some() { Icon::Send } else { Icon::Ask }, 12.).text_color(rgb(theme::text_3())))
                    .child(ui::mono(state))
                    .child(div().flex_1())
                    .when(c.sent_in.is_none(), |d| {
                        d.child(
                            div()
                                .id(("plan-c-remove", id as usize))
                                .text_size(px(12.))
                                .text_color(rgb(theme::text_3()))
                                .cursor_pointer()
                                .hover(|s| s.text_color(rgb(theme::text())))
                                .child("Remove")
                                .on_click(cx.listener(move |this, _, _, cx| this.remove_plan_comment(sub_id, id, cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn comment_composer(&self, input: Entity<TextInput>, cx: &mut Context<Self>) -> AnyElement {
        let send = if self.ctrl_send() { "Ctrl Enter" } else { "Enter" };
        div()
            .ml(px(12.))
            .p(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::panel()))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(input)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::button("plan-c-save", Kind::Primary, None, "Comment").on_click(cx.listener(|this, _, _, cx| this.save_plan_comment(cx))))
                    .child(ui::button("plan-c-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_plan_comment(cx);
                    })))
                    .child(div().flex_1())
                    .child(div().flex().items_center().gap(px(6.)).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(ui::kbd(send)).child("save").child(ui::kbd("Esc")).child("cancel")),
            )
            .into_any_element()
    }

    /// At a glance, the build order, and every comment.
    fn plan_side(&self, sub_id: i64, view: &PlanView, version: &plan::PlanVersion, latest: u32, cx: &mut Context<Self>) -> AnyElement {
        let value = &version.plan;
        let streams = pipeline::workstreams(value);
        let book = self.plans.get(&sub_id);
        let comments: Vec<PlanComment> = book.map(|b| b.comments.clone()).unwrap_or_default();
        let unsent = comments.iter().filter(|c| c.sent_in.is_none()).count();
        let can_comment = matches!(self.pipelines.get(&sub_id).map(|p| p.stage), Some(Stage::Approve | Stage::Plan));
        let fact = |label: &'static str, value: String| {
            div()
                .flex()
                .gap(px(10.))
                .text_size(px(12.5))
                .child(div().w(px(84.)).flex_none().text_color(rgb(theme::text_3())).child(label))
                .child(div().flex_1().min_w(px(0.)).text_color(rgb(theme::text_2())).child(value))
        };
        let risks: Vec<String> = value["risk_flags"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).map(|r| r.replace('_', " ")).collect();
        let mut glance = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(ui::label("At a glance"))
            .child(fact("Size", value["size"].as_str().unwrap_or("?").to_string()))
            .child(fact("Confidence", format!("{}/5{}", value["confidence"].as_u64().unwrap_or(0), value["confidence_reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default())))
            .child(fact("Risks", if risks.is_empty() { "none flagged".into() } else { risks.join(", ") }))
            .child(fact("Builders", format!("{} workstream{}", streams.len(), if streams.len() == 1 { "" } else { "s" })))
            .child(fact("Cost", self.plan_estimate_line(value)));
        if streams.len() > 1
            && let Some(code) = crate::diagram::build_order(&streams)
            && let Some(diagram) = crate::diagram::render(&code)
        {
            glance = glance.child(div().pt(px(6.)).child(ui::label("Build order"))).child(diagram);
        }

        let scroll = view.scroll.clone();
        let list = div().flex().flex_col().gap(px(8.)).children(comments.iter().rev().map(|c| {
            let jump = (c.version == version.version).then_some(c.block).flatten();
            let scroll = scroll.clone();
            let place = match (c.block, c.quote.trim()) {
                (None, _) | (_, "") => "The whole plan".to_string(),
                (_, q) => review::clip(q, 120).replace('\n', " "),
            };
            div()
                .id(("plan-side-c", c.id as usize))
                .p(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(theme::line()))
                .flex()
                .flex_col()
                .gap(px(4.))
                .when(jump.is_some(), |d| d.cursor_pointer().hover(|s| s.bg(rgb(theme::hover()))))
                .child(div().text_size(px(11.5)).text_color(rgb(theme::text_3())).truncate().child(format!("v{} · {} · {place}", c.version, crate::dashboard::ago(c.at))))
                .child(div().text_size(px(12.5)).text_color(rgb(theme::text())).child(review::clip(&c.body, 400)))
                .child(ui::mono(plan::comment_state(c, latest)))
                .on_click(move |_, _, _| {
                    if let Some(index) = jump {
                        scroll.scroll_to_top_of_item(index);
                    }
                })
        }));

        let general = view.general.clone();
        div()
            .px(px(16.))
            .py(px(16.))
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(glance)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().flex().items_center().gap(px(8.)).child(ui::label("Comments")).children((!comments.is_empty()).then(|| ui::mono(comments.len().to_string()))))
                    .when(comments.is_empty(), |d| {
                        d.child(div().text_size(px(12.5)).text_color(rgb(theme::text_3())).child(if can_comment {
                            "Point at any part of the plan and click the comment button beside it. Comments go to the planner together."
                        } else {
                            "No comments on this plan."
                        }))
                    })
                    .child(list)
                    .when(can_comment, |d| {
                        d.child(div().p(px(8.)).rounded(px(6.)).border_1().border_color(rgb(theme::line_strong())).child(general)).child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .child(ui::button("plan-general", Kind::Secondary, None, "Comment on the whole plan").on_click(cx.listener(|this, _, _, cx| this.save_general_comment(cx))))
                                .when(unsent > 0, |d| {
                                    d.child(
                                        ui::button("plan-send-side", Kind::Primary, Some(Icon::Send), format!("Send {unsent} to the planner"))
                                            .on_click(cx.listener(move |this, _, _, cx| this.send_plan_comments(sub_id, cx))),
                                    )
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}
