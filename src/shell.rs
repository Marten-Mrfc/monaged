//! The app shell (the redesign): title bar, sidebar tree with the "Needs you"
//! inbox, agent columns, the inspector, the status bar, the overview, forms.
//!
//! Layout follows the window width. A narrow window shows one agent with a
//! Conversation/Changes switch. Wider windows (21:9) show several agents side
//! by side, or an agent's conversation next to its changes.

use std::time::Instant;

use gpui::{
    AnyElement, AnyView, ClipboardItem, Context, Focusable, FontWeight, IntoElement, MouseButton, MouseDownEvent, Render,
    ScrollHandle, SharedString, Window, WindowControlArea, div, prelude::*, px, relative, rgb,
};

use crate::agent::{AgentTab, AgentView, Item, Status, Tone};
use crate::icons::{Icon, icon, icon_sized};
use crate::menu::MenuTarget;
use crate::pipeline::{self, Role, Stage};
use crate::ui::{self, Kind, State};
use crate::{Conn, Form, Monaged, SubKind, coord, coord_ui, dots, pipeline_ui, review_ui, theme};

pub(crate) const COLUMN_MIN: f32 = 600.;
const READING_WIDTH: f32 = 680.;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum InspectorTab {
    /// The pipeline, and the board its agents coordinate on.
    Pipeline,
    Merge,
    Git,
    Run,
    /// What happened, newest first.
    Timeline,
}

impl InspectorTab {
    /// The dock's tabs in order, with their shortcut ids (Alt 1-5 by default).
    pub(crate) const ALL: [(InspectorTab, &'static str); 5] = [
        (InspectorTab::Pipeline, "dock-pipeline"),
        (InspectorTab::Merge, "dock-merge"),
        (InspectorTab::Git, "dock-git"),
        (InspectorTab::Run, "dock-run"),
        (InspectorTab::Timeline, "dock-activity"),
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            InspectorTab::Pipeline => "Pipeline",
            InspectorTab::Merge => "Merge",
            InspectorTab::Git => "Git",
            InspectorTab::Run => "Run",
            InspectorTab::Timeline => "Activity",
        }
    }

    pub(crate) fn icon(self) -> Icon {
        match self {
            InspectorTab::Pipeline => Icon::Flow,
            InspectorTab::Merge => Icon::Merge,
            InspectorTab::Git => Icon::Branch,
            InspectorTab::Run => Icon::Play,
            InspectorTab::Timeline => Icon::Clock,
        }
    }
}

/// A dock tab's badge.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DockBadge {
    NeedsYou,
    Working,
    Ready,
    Count(usize),
}

/// One pane in the agent area.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Column {
    Agent(i64),
    Changes(i64),
}

/// Something that waits for you.
pub(crate) struct Inbox {
    pub title: String,
    pub detail: String,
    pub sub_id: i64,
    pub agent_id: Option<i64>,
}

/// A tooltip bubble, with the shortcut that does the same when there is one.
pub(crate) struct Tip(SharedString, Option<&'static str>);

impl Tip {
    pub(crate) fn new(text: SharedString) -> Self {
        Tip(text, None)
    }
}

impl Render for Tip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(5.))
            .bg(rgb(theme::raised()))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .shadow_md()
            .text_size(px(12.))
            .text_color(rgb(theme::text()))
            .child(self.0.clone())
            .children(self.1.map(ui::kbd))
    }
}

pub(crate) fn tip(text: &'static str) -> impl Fn(&mut Window, &mut gpui::App) -> AnyView {
    move |_, cx| cx.new(|_| Tip(text.into(), None)).into()
}

/// A tooltip with text worked out at runtime.
pub(crate) fn tip_text(text: String) -> impl Fn(&mut Window, &mut gpui::App) -> AnyView {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| Tip(text.clone(), None)).into()
}

pub(crate) fn tip_key(text: &'static str, key: &'static str) -> impl Fn(&mut Window, &mut gpui::App) -> AnyView {
    move |_, cx| cx.new(|_| Tip(text.into(), (!key.is_empty()).then_some(key))).into()
}

/// The keyboard cursor in the sidebar: a bar on the row's left edge.
fn tree_bar() -> gpui::Div {
    div().absolute().left(px(1.)).top(px(6.)).bottom(px(6.)).w(px(2.)).rounded_full().bg(rgb(theme::text_2()))
}

/// A ⋯ button that shows while you point at its row (group "tree-row") and
/// opens the row's menu. It floats over the row's right edge.
fn row_menu_button(id: impl Into<gpui::ElementId>, target: MenuTarget, selected: bool, cx: &mut Context<Monaged>) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .right(px(4.))
        .top(px(4.))
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(rgb(if selected { theme::selected() } else { theme::hover() }))
        .invisible()
        .group_hover("tree-row", |s| s.visible())
        .hover(|s| s.bg(rgb(theme::line_strong())))
        .child(icon_sized(Icon::More, 14.).text_color(rgb(theme::text_3())))
        .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
            cx.stop_propagation();
            this.open_menu(target, event.position(), window, cx);
        }))
        .into_any_element()
}

/// A status dot with a count beside it when there's more than one.
fn dot_count(dot: AnyElement, n: usize, red: bool) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(dot)
        .when(n > 1, |d| d.child(div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(if red { theme::red_text() } else { theme::text_2() })).child(n.to_string())))
        .into_any_element()
}

/// Something new since you last looked (bold name and this dot).
fn new_dot() -> AnyElement {
    div().flex_none().size(px(6.)).rounded_full().bg(rgb(theme::text())).into_any_element()
}

/// Right-click opens the row's menu.
fn on_right_click(target: MenuTarget, cx: &mut Context<Monaged>) -> impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
        cx.stop_propagation();
        this.open_menu(target, event.position, window, cx);
    })
}

pub(crate) fn agent_state(agent: &AgentView) -> State {
    if agent.needs_attention() || agent.open_form() || agent.paused || open_question(agent) || matches!(agent.status, Status::Failed(_)) {
        State::NeedsYou
    } else if agent.is_working() || matches!(agent.status, Status::Starting | Status::Loading) {
        State::Working
    } else if agent.live || agent.record.session_id.is_some() {
        State::Done
    } else {
        State::Idle
    }
}

/// The agent's current turn answers something you asked (not a brief, a
/// review round, a board digest or a lead update from Monaged).
pub(crate) fn prompted_by_you(agent: &AgentView) -> bool {
    let Some(Item::User(text)) = agent.items.iter().rev().find(|i| matches!(i, Item::User(_))) else { return false };
    !from_monaged(text)
}

/// A message Monaged wrote to an agent (a brief, another agent's question, a
/// lead update, a board digest, a review round), not one you typed.
pub(crate) fn from_monaged(text: &str) -> bool {
    text.starts_with(pipeline::BRIEF_HEADER)
        || text.starts_with("[monaged-ask #")
        || text.starts_with(crate::lead::UPDATE_PREFIX)
        || text.contains(coord::DIGEST_HEADER)
        || text.starts_with("Monaged's review")
}

/// The agent's last reply asks the developer something that isn't answered yet.
pub(crate) fn open_question(agent: &AgentView) -> bool {
    let Some(last) = agent.items.iter().rposition(|i| matches!(i, Item::Agent(_) | Item::User(_))) else { return false };
    matches!(&agent.items[last], Item::Agent(t) if t.contains("```monaged-question"))
}

fn role_icon(role: &Role) -> Icon {
    match role {
        Role::Researcher => Icon::Search,
        Role::Verifier(_) => Icon::Shield,
        Role::Planner => Icon::Flow,
        Role::Builder(_) => Icon::Edit,
        Role::Integrator => Icon::Merge,
        Role::Lead => Icon::Flag,
        Role::Free => Icon::Agent,
    }
}

pub(crate) fn role_label(role: &Role) -> String {
    match role {
        Role::Researcher => "Researcher".into(),
        Role::Verifier(n) => format!("Verifier {n}"),
        Role::Planner => "Planner".into(),
        Role::Builder(ws) => format!("Builder for {ws}"),
        Role::Integrator => "Integrator".into(),
        Role::Lead => "Lead".into(),
        Role::Free => "Agent".into(),
    }
}

fn short_role(role: &Role) -> String {
    match role {
        Role::Researcher => "research".into(),
        Role::Verifier(n) => format!("verify {n}"),
        Role::Planner => "plan".into(),
        // Builders are named after their workstream already (build-<workstream>).
        Role::Builder(_) => "build".into(),
        Role::Integrator => "integrate".into(),
        Role::Lead => "lead".into(),
        Role::Free => String::new(),
    }
}

impl Monaged {
    // ---- Layout ----------------------------------------------------------------------

    pub(crate) fn sidebar_width(width: f32) -> f32 {
        if width >= 2400. { 288. } else { 264. }
    }

    pub(crate) fn inspector_width(width: f32) -> f32 {
        if width >= 2400. { 380. } else { 340. }
    }

    fn inspector_visible(&self) -> bool {
        self.inspector_open && self.sel_sub.is_some() && self.form.is_none()
    }

    /// The panes for the selected subproject, given the room there is.
    /// Whose message box gets the cursor back: the open agent's, or on the
    /// dashboard the lead's (when its chat is on screen).
    pub(crate) fn focus_target(&self) -> Option<i64> {
        self.sel_agent.or_else(|| (self.on_dashboard() && self.dashboard_chat).then(|| self.sel_sub.and_then(|s| self.lead_of(s))).flatten())
    }

    /// A subproject is open without an agent in focus: its dashboard shows.
    pub(crate) fn on_dashboard(&self) -> bool {
        self.form.is_none() && self.sel_agent.is_none() && self.sel_sub.is_some_and(|s| self.sub_view != Some(s))
    }

    pub(crate) fn columns(&self, main_width: f32) -> Vec<Column> {
        let Some(sub_id) = self.sel_sub else { return Vec::new() };
        let max = ((main_width / COLUMN_MIN).floor() as usize).clamp(1, 4);
        let in_sub = |id: i64| {
            self.agent(id).is_some_and(|a| a.record.subproject_id == sub_id && (!a.record.archived || self.sel_agent == Some(id)))
        };
        let mut cols: Vec<Column> = Vec::new();
        let focused = self.sel_agent.filter(|&id| in_sub(id));
        if let Some(id) = focused {
            cols.push(Column::Agent(id));
        }
        // One agent at a time: the focused one (or, with none, the first that
        // needs you or is working).
        if self.one_pane {
            if cols.is_empty()
                && let Some(agent) = self
                    .agents
                    .iter()
                    .filter(|a| in_sub(a.record.id) && matches!(agent_state(a), State::Working | State::NeedsYou))
                    .min_by_key(|a| (agent_state(a) != State::NeedsYou, std::cmp::Reverse(a.record.id)))
            {
                cols.push(Column::Agent(agent.record.id));
            }
            return cols;
        }
        for &id in &self.pinned {
            if in_sub(id) && !cols.contains(&Column::Agent(id)) {
                cols.push(Column::Agent(id));
            }
        }
        // Agents that are busy or waiting on you earn a pane by themselves.
        let mut others: Vec<&AgentView> = self
            .agents
            .iter()
            .filter(|a| in_sub(a.record.id) && !cols.contains(&Column::Agent(a.record.id)) && !self.dismissed.contains(&a.record.id))
            .filter(|a| matches!(agent_state(a), State::Working | State::NeedsYou))
            .collect();
        others.sort_by_key(|a| (agent_state(a) != State::NeedsYou, std::cmp::Reverse(a.record.id)));
        for a in others {
            cols.push(Column::Agent(a.record.id));
        }
        cols.truncate(max);
        // Spare room: show the focused agent's changes beside its conversation.
        if let Some(id) = focused
            && self.changes_beside
            && cols.len() < max
        {
            cols.insert(1, Column::Changes(id));
        }
        cols
    }

    /// Text boxes and scroll positions for the panes on screen.
    pub(crate) fn prepare_columns(&mut self, columns: &[Column], window: &mut Window, cx: &mut Context<Self>) {
        for column in columns {
            let Column::Agent(id) = *column else { continue };
            if !self.inputs.contains_key(&id) {
                let input = self.make_composer(id, cx);
                self.inputs.insert(id, input);
            }
            self.prepare_form_inputs(id, cx);
            self.scrolls.entry(id).or_insert_with(|| {
                let handle = ScrollHandle::new();
                handle.scroll_to_bottom();
                handle
            });
        }
        // The replay and the commit view cover the columns and own the keyboard;
        // a message box gets the cursor once they close.
        if self.replay.is_none()
            && self.commit_view.is_none()
            && let Some(id) = self.focus_request.take()
            && let Some(input) = self.inputs.get(&id)
        {
            window.focus(&input.focus_handle(cx));
        }
    }

    /// Keeps a pane pinned to the newest message, unless you scrolled up to read.
    pub(crate) fn follow(&self, agent_id: i64) {
        if let Some(handle) = self.scrolls.get(&agent_id) {
            let max = f32::from(handle.max_offset().height);
            let at = -f32::from(handle.offset().y);
            if max - at < 80. {
                handle.scroll_to_bottom();
            }
        }
    }

    pub(crate) fn follow_all(&self) {
        let ids: Vec<i64> = self.scrolls.keys().copied().collect();
        for id in ids {
            self.follow(id);
        }
    }

    /// The lead whose dashboard is open: you're reading it already.
    pub(crate) fn lead_in_view(&self, agent: &AgentView) -> bool {
        agent.role() == Role::Lead && self.window_active && self.form.is_none() && self.sel_sub == Some(agent.record.subproject_id) && self.sel_agent.is_none()
    }

    pub(crate) fn pin_beside(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        self.unseen.remove(&agent_id);
        self.dismissed.remove(&agent_id);
        if !self.pinned.contains(&agent_id) && self.sel_agent != Some(agent_id) {
            self.pinned.push(agent_id);
        }
        // Asking for a second pane means side by side.
        if self.one_pane {
            self.set_one_pane(false, cx);
        }
        self.ensure_session(agent_id);
        cx.notify();
    }

    /// Closes an agent's pane, and keeps it closed: an agent that's working
    /// or needs you would otherwise get its pane right back.
    pub(crate) fn close_column(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        self.pinned.retain(|&id| id != agent_id);
        self.dismissed.insert(agent_id);
        if self.sel_agent == Some(agent_id) {
            self.sel_agent = self.pinned.first().copied();
        }
        cx.notify();
    }

    /// One agent at a time, or several side by side (remembered).
    pub(crate) fn set_one_pane(&mut self, on: bool, cx: &mut Context<Self>) {
        self.one_pane = on;
        let _ = self.store.set_setting("ui:panes", if on { "one" } else { "side" });
        cx.notify();
    }

    /// The focused agent's changes in a pane beside it, or only in its tab (remembered).
    pub(crate) fn set_changes_beside(&mut self, on: bool, cx: &mut Context<Self>) {
        self.changes_beside = on;
        let _ = self.store.set_setting("ui:changes_beside", if on { "1" } else { "0" });
        cx.notify();
    }

    // ---- What needs you ---------------------------------------------------------------

    pub(crate) fn inbox(&self) -> Vec<Inbox> {
        let mut out = Vec::new();
        for sub in &self.subprojects {
            if let Some(p) = self.pipelines.get(&sub.id) {
                match p.stage {
                    Stage::Decide => out.push(Inbox {
                        title: "Decide: build this?".into(),
                        detail: sub.name.clone(),
                        sub_id: sub.id,
                        agent_id: None,
                    }),
                    Stage::Approve if p.plan.is_some() => out.push(Inbox {
                        title: if self.identity_missing { "Git identity needed".into() } else { "Plan ready for approval".into() },
                        detail: sub.name.clone(),
                        sub_id: sub.id,
                        agent_id: None,
                    }),
                    _ => {}
                }
                for (name, _) in &p.gate.held {
                    out.push(Inbox { title: "Below the bar".into(), detail: format!("{} · {name}", sub.name), sub_id: sub.id, agent_id: None });
                }
                // On autopilot nobody else picks up a merge that stopped, or
                // one into the top branch, which stays yours.
                if p.autopilot && self.ships.get(&sub.id).is_some_and(|s| s.step == crate::ship::Step::Stuck) {
                    out.push(Inbox { title: "Autopilot stopped at the merge".into(), detail: sub.name.clone(), sub_id: sub.id, agent_id: None });
                } else if p.autopilot && p.stage == Stage::Done && !self.ships.contains_key(&sub.id) {
                    let target = self.merge_target(sub.id);
                    let ahead = target.as_ref().and_then(|t| self.branch_info.get(&sub.project_id)?.ahead.get(&(sub.branch.clone(), t.clone())).copied());
                    if let Some(t) = target.filter(|t| ahead.is_some_and(|n| n > 0) && self.policy(sub.project_id, t) == crate::merge_ui::Policy::Confirm) {
                        out.push(Inbox { title: format!("Ready for you to merge into {t}"), detail: sub.name.clone(), sub_id: sub.id, agent_id: None });
                    }
                }
            }
            for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub.id && !a.record.archived) {
                let title = if self.vault.requests.iter().any(|r| r.agent_id == agent.record.id) {
                    Some("Wants to use secrets".to_string())
                } else if agent.needs_attention() {
                    Some("Asks for permission".to_string())
                } else if agent.open_form() || open_question(agent) {
                    Some("Has a question".to_string())
                } else if agent.paused {
                    Some("Paused by the budget".to_string())
                } else if let Status::Failed(_) = agent.status {
                    Some("Session failed".to_string())
                } else {
                    None
                };
                if let Some(title) = title {
                    out.push(Inbox {
                        title,
                        detail: format!("{} · {}", sub.name, agent.record.name),
                        sub_id: sub.id,
                        agent_id: Some(agent.record.id),
                    });
                }
            }
        }
        out
    }

    /// Everything that waits for you can be answered on the subproject's dashboard.
    fn open_inbox(&mut self, sub_id: i64, _agent_id: Option<i64>, cx: &mut Context<Self>) {
        self.form = None;
        self.select_sub(sub_id, cx);
        cx.notify();
    }

    // ---- The shell -----------------------------------------------------------------------

    pub(crate) fn render_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.left_setup(cx);
        self.sync_awake();
        let width = f32::from(window.viewport_size().width);
        // Keys only reach Monaged's actions through something focused: when a
        // closed palette, menu or form leaves nothing focused, take it back.
        if window.focused(cx).is_none() && self.focus_request.is_none() {
            window.focus(&self.focus);
        }
        let zen = self.zen && self.sel_agent.is_some() && self.form.is_none();
        // The setup guide has the window to itself.
        let setup = matches!(self.form, Some(Form::Setup));
        let sidebar_w = if self.sidebar_open && !zen && !setup { self.sidebar_px(width) } else { 0. };
        let inspector_w = if self.inspector_visible() && !zen && !setup { self.inspector_px(width) } else { 0. };
        let main_w = width - sidebar_w - inspector_w;
        let columns = match (self.form.is_none(), zen) {
            (false, _) => Vec::new(),
            (true, true) => self.sel_agent.map(Column::Agent).into_iter().collect(),
            // The dashboard: only the lead's chat needs a message box, and only
            // when there's room to show it.
            (true, false) if self.on_dashboard() => {
                self.dashboard_chat = main_w >= crate::dashboard::CHAT_MIN_MAIN;
                let lead = self.sel_sub.and_then(|s| self.lead_of(s));
                if self.dashboard_chat {
                    lead.map(Column::Agent).into_iter().collect()
                } else {
                    // A box that isn't drawn mustn't keep the keyboard.
                    if let Some(lead) = lead {
                        if self.focus_request == Some(lead) {
                            self.focus_request = None;
                        }
                        if self.inputs.get(&lead).is_some_and(|i| i.focus_handle(cx).is_focused(window)) {
                            window.focus(&self.focus);
                        }
                    }
                    Vec::new()
                }
            }
            (true, false) => self.columns(main_w),
        };
        // The project view's chat box needs its inputs before it's drawn.
        if self.form.is_none()
            && self.sel_sub.is_none()
            && let Some(project_id) = self.sel_project
        {
            self.prepare_project_chat(project_id, cx);
        }
        if let Some(sub_id) = self.sel_sub {
            self.load_pane_weights(sub_id, columns.len());
            if self.inspector_tab == InspectorTab::Git && self.inspector_open {
                self.prepare_git_panel(sub_id, cx);
            }
            if self.inspector_tab == InspectorTab::Run && self.inspector_open {
                self.prepare_run(sub_id, cx);
            }
        }
        self.prepare_columns(&columns, window, cx);
        // A Changes pane shows the diff without being asked.
        for column in &columns {
            if let Column::Changes(id) = *column
                && self.agent(id).is_some_and(|a| a.changes.is_none() && !a.loading_changes)
            {
                self.refresh_changes(id, false, cx);
            }
        }

        // Focus is tracked below the title bar: a focus-tracking parent would swallow
        // the mouse-down that Windows needs for its minimize/maximize/close buttons.
        let body = div()
            .track_focus(&self.focus)
            .flex_1()
            .min_h(px(0.))
            .flex()
            .when(sidebar_w > 0., |d| d.child(self.render_sidebar(sidebar_w, self.tree_focus.is_focused(window), cx)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(rgb(theme::bg()))
                    .when(!setup, |d| d.children(self.render_connection_bar(cx)).children(self.render_login_banner(cx)).children(self.render_paused_bar(cx)))
                    .child(self.render_content(&columns, main_w, f32::from(window.viewport_size().height), cx)),
            )
            .when(inspector_w > 0., |d| d.child(self.render_inspector(inspector_w, cx)));

        div()
            .key_context("Monaged")
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_toggle_palette))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_toggle_inspector))
            .on_action(cx.listener(Self::on_dock_pipeline))
            .on_action(cx.listener(Self::on_dock_merge))
            .on_action(cx.listener(Self::on_dock_git))
            .on_action(cx.listener(Self::on_dock_run))
            .on_action(cx.listener(Self::on_dock_activity))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_palette_up))
            .on_action(cx.listener(Self::on_palette_down))
            .on_action(cx.listener(Self::on_show_shortcuts))
            .on_action(cx.listener(Self::on_new_agent_here))
            .on_action(cx.listener(Self::on_prev_agent))
            .on_action(cx.listener(Self::on_next_agent))
            .on_action(cx.listener(Self::on_next_needs_you))
            .on_action(cx.listener(Self::on_toggle_zen))
            .on_action(cx.listener(Self::on_find))
            .on_action(cx.listener(Self::on_search_agents))
            .on_action(cx.listener(Self::on_focus_tree))
            .on_action(cx.listener(Self::on_toggle_side_chat))
            // Dragging an edge (sidebar, inspector, pane divider).
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| this.drag_move(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_drag(cx)))
            .when(self.drag.is_some(), |d| d.cursor(gpui::CursorStyle::ResizeLeftRight))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme::bg()))
            .text_color(rgb(theme::text()))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .child(self.render_titlebar(window, sidebar_w, cx))
            .child(body)
            .when(!zen && !setup, |d| d.child(self.render_statusbar(cx)))
            // Floating layers, lowest first.
            .children(self.render_away(cx))
            .children(self.render_toasts(cx))
            .children(self.render_palette(cx))
            .children(self.render_menu(window, cx))
            .children(self.render_confirm(cx))
            .children(self.render_text_prompt(cx))
            .children(self.render_shortcuts(cx))
            .children(self.render_lightbox(cx))
            .into_any_element()
    }

    // ---- Title bar --------------------------------------------------------------------------

    fn render_titlebar(&self, window: &Window, sidebar_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let mut crumbs: Vec<String> = Vec::new();
        if let Some(project) = self.sel_project.and_then(|id| self.projects.iter().find(|p| p.id == id)) {
            crumbs.push(project.name.clone());
        }
        if let Some(sub) = self.sel_sub.and_then(|id| self.subprojects.iter().find(|s| s.id == id)) {
            crumbs.push(sub.name.clone());
        }
        if matches!(self.form, Some(Form::Settings { .. })) {
            crumbs = vec!["Settings".into()];
        }
        if matches!(self.form, Some(Form::Health)) {
            crumbs = vec!["Health check".into()];
        }
        if matches!(self.form, Some(Form::Guide)) {
            crumbs = vec!["Playground guide".into()];
        }
        if matches!(self.form, Some(Form::Setup)) {
            crumbs = vec!["Setup".into()];
        }
        if self.zen && self.form.is_none() {
            crumbs.push(format!("Zen · {} to leave", crate::keys::label("zen")));
        }
                // The search box is centred over the title bar: the path stops short
        // of it (the last part shortens with "…") instead of running under it.
        let window_w = f32::from(window.viewport_size().width);
        let search_w = if window_w < 1200. { 300. } else { 420. };
        let crumbs_max = (window_w / 2. - search_w / 2. - 16. - sidebar_w.max(200.) - 14.).max(0.);
        // Too long for the room: only the most specific part (it says where you are).
        let estimate: f32 = crumbs.iter().map(|c| c.chars().count() as f32 * 7.).sum::<f32>() + crumbs.len().saturating_sub(1) as f32 * 18.;
        if estimate > crumbs_max && crumbs.len() > 1 {
            crumbs.drain(..crumbs.len() - 1);
        }
        let last = crumbs.len().saturating_sub(1);
        let crumb_row = div().flex().items_center().gap(px(6.)).min_w(px(0.)).max_w(px(crumbs_max)).overflow_hidden().children(
            crumbs.into_iter().enumerate().flat_map(|(i, c)| {
                let sep = (i > 0).then(|| icon_sized(Icon::ChevronRight, 12.).text_color(rgb(theme::text_4())).into_any_element());
                let text = div()
                    .when(i == last, |d| d.flex_shrink().min_w(px(0.)).truncate())
                    .when(i != last, |d| d.flex_none())
                    .text_size(px(12.5))
                    .text_color(rgb(if i == last { theme::text() } else { theme::text_3() }))
                    .child(c)
                    .into_any_element();
                sep.into_iter().chain(std::iter::once(text))
            }),
        );
        let maximized = window.is_maximized();
        let control = |id: &'static str, glyph: Icon, area: WindowControlArea, close: bool| {
            div()
                .id(id)
                .w(px(46.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .group("window-control")
                .window_control_area(area)
                .when(close, |d| d.hover(|s| s.bg(rgb(0xc42b1c))))
                .when(!close, |d| d.hover(|s| s.bg(rgb(theme::hover()))))
                .child(icon(glyph).group_hover("window-control", move |s| s.text_color(rgb(if close { 0xffffff } else { theme::text() }))))
        };
        div()
            .h(px(38.))
            .flex_none()
            .relative()
            .flex()
            .items_center()
            .bg(rgb(theme::panel()))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .child(
                div()
                    .id("titlebar-brand")
                    .w(px(sidebar_w.max(200.)))
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(14.))
                    .window_control_area(WindowControlArea::Drag)
                    .child(div().size(px(9.)).rounded_full().bg(rgb(theme::red())))
                    .child(div().font_family(theme::DISPLAY).text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Monaged")),
            )
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .items_center()
                    .px(px(14.))
                    .window_control_area(WindowControlArea::Drag)
                    .child(crumb_row),
            )
            .child(control("win-min", Icon::Minimize, WindowControlArea::Min, false))
            .child(control("win-max", if maximized { Icon::Restore } else { Icon::Maximize }, WindowControlArea::Max, false))
            .child(control("win-close", Icon::X, WindowControlArea::Close, true))
            .child(
                // Centered on the window, above the drag area (occlude keeps clicks here).
                div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().child(
                    div()
                        .id("titlebar-search")
                        .occlude()
                        .w(px(search_w))
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(9.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .bg(rgb(theme::bg()))
                        .text_size(px(12.5))
                        .text_color(rgb(theme::text_3()))
                        .cursor_pointer()
                        .hover(|s| s.border_color(rgb(theme::line_strong())))
                        .child(icon(Icon::Search).size(px(14.)).text_color(rgb(theme::text_3())))
                        .child(div().flex_1().child("Search or run a command"))
                        .children(crate::keys::hint("palette").map(ui::kbd))
                        .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
                ),
            )
            .into_any_element()
    }

    // ---- Sidebar ---------------------------------------------------------------------------

    fn render_sidebar(&self, width: f32, tree_focused: bool, cx: &mut Context<Self>) -> AnyElement {
        use crate::keys::TreeNode;
        let inbox = self.inbox();
        let mut list = div().flex().flex_col().gap(px(1.)).px(px(6.)).pb(px(12.));
        // The keyboard cursor (Ctrl 0) and which child row it is, to scroll to it.
        let cursor = if tree_focused { self.tree_current() } else { None };
        let mut ix = 0usize;
        let mut cursor_ix = None;

        for project in &self.projects {
            let pid = project.id;
            let open = !self.collapsed.contains(&pid);
            let selected = self.sel_project == Some(pid) && self.sel_sub.is_none() && self.form.is_none();
            let on = cursor == Some(TreeNode::Project(pid));
            if on {
                cursor_ix = Some(ix);
            }
            ix += 1;
            // Folded, the project still says what's inside: needs you, working, new.
            let rollup = (!open).then(|| {
                let subs: Vec<i64> = self.subprojects.iter().filter(|s| s.project_id == pid).map(|s| s.id).collect();
                let needs = inbox.iter().filter(|i| subs.contains(&i.sub_id)).count();
                let working = self.agents.iter().filter(|a| subs.contains(&a.record.subproject_id) && a.is_working()).count();
                let fresh = self.unseen.iter().any(|id| self.agent(*id).is_some_and(|a| subs.contains(&a.record.subproject_id)));
                (needs, working, fresh)
            });
            let fresh_inside = rollup.is_some_and(|(_, _, f)| f);
            list = list.child(
                ui::row(("project", pid as usize), selected)
                    .when(on && !selected, |d| d.bg(rgb(theme::hover())))
                    .when(on, |d| d.child(tree_bar()))
                    .relative()
                    .group("tree-row")
                    .on_mouse_down(MouseButton::Right, on_right_click(MenuTarget::Project(pid), cx))
                    .child(
                        div()
                            .id(("project-toggle", pid as usize))
                            .flex_none()
                            .child(ui::disclosure(open))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if !this.collapsed.remove(&pid) {
                                    this.collapsed.insert(pid);
                                }
                                cx.notify();
                            })),
                    )
                    .child(icon(Icon::Folder).text_color(rgb(theme::text_2())))
                    .child(ui::one_line(div().flex_1()).text_color(rgb(theme::text())).when(fresh_inside, |d| d.font_weight(FontWeight::SEMIBOLD)).child(project.name.clone()))
                    .children(rollup.map(|(needs, working, fresh)| {
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .group_hover("tree-row", |s| s.invisible())
                            .when(needs > 0, |d| d.child(dot_count(ui::status_dot(("proj-needs", pid as usize), State::NeedsYou), needs, true)))
                            .when(working > 0, |d| d.child(dot_count(ui::status_dot(("proj-working", pid as usize), State::Working), working, false)))
                            .when(fresh, |d| d.child(new_dot()))
                    }))
                    // CI on develop, acceptance or main failed (red) or is running.
                    .children(self.project_ci_state(pid).map(|(state, why)| {
                        div()
                            .id(("project-ci", pid as usize))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .group_hover("tree-row", |s| s.invisible())
                            .tooltip(crate::shell::tip_text(why))
                            .child(ui::status_dot(("proj-ci-dot", pid as usize), state))
                            .child(div().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(if state == State::NeedsYou { theme::red_text() } else { theme::text_3() })).child("CI"))
                    }))
                    .child(
                        div()
                            .id(("project-new-sub", pid as usize))
                            .absolute()
                            .right(px(28.))
                            .top(px(4.))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .invisible()
                            .group_hover("tree-row", |s| s.visible())
                            .hover(|s| s.bg(rgb(theme::line_strong())))
                            .tooltip(tip("New subproject"))
                            .child(icon_sized(Icon::Plus, 14.).text_color(rgb(theme::text_3())))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_sub_form(pid, window, cx);
                            })),
                    )
                    .child(row_menu_button(("project-more", pid as usize), MenuTarget::Project(pid), selected, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_project(pid, cx))),
            );
            if !open {
                continue;
            }
            for sub in self.subprojects.iter().filter(|s| s.project_id == pid) {
                let sid = sub.id;
                let selected_sub = self.sel_sub == Some(sid) && self.form.is_none();
                let needs = inbox.iter().filter(|i| i.sub_id == sid).count();
                let working = self.agents.iter().filter(|a| a.record.subproject_id == sid && a.is_working()).count();
                let fresh = self.unseen.iter().any(|id| self.agent(*id).is_some_and(|a| a.record.subproject_id == sid));
                let accessory: AnyElement = if needs > 0 {
                    dot_count(ui::status_dot(("sub-dot", sid as usize), State::NeedsYou), needs, true)
                } else if let Some(p) = self.pipelines.get(&sid).filter(|p| !matches!(p.stage, Stage::Done | Stage::Stopped)) {
                    let current = p.stage.index();
                    ui::stage_dots(format!("sub-stages-{sid}"), current, Some(current), pipeline::STAGES.len())
                } else if working > 0 {
                    dot_count(ui::status_dot(("sub-dot", sid as usize), State::Working), working, false)
                } else if self.store.setting(&format!("sub:{sid}:merged")).is_some() {
                    ui::mono("merged").into_any_element()
                } else {
                    div().into_any_element()
                };
                let sub_row_selected = selected_sub && self.sel_agent.is_none();
                let on = cursor == Some(TreeNode::Sub(sid));
                if on {
                    cursor_ix = Some(ix);
                }
                ix += 1;
                list = list.child(
                    ui::row(("sub", sid as usize), sub_row_selected)
                        .when(on && !sub_row_selected, |d| d.bg(rgb(theme::hover())))
                        .when(on, |d| d.child(tree_bar()))
                        .relative()
                        .group("tree-row")
                        .on_mouse_down(MouseButton::Right, on_right_click(MenuTarget::Sub(sid), cx))
                        .pl(px(26.))
                        .child(icon(if sub.kind == "standing" { Icon::Shield } else { Icon::Layers }).text_color(rgb(if selected_sub {
                            theme::text()
                        } else {
                            theme::text_3()
                        })))
                        .child(
                            ui::one_line(div().flex_1())
                                .text_color(rgb(if selected_sub || fresh { theme::text() } else { theme::text_2() }))
                                .when(fresh, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(sub.name.clone()),
                        )
                        .child(div().flex_none().group_hover("tree-row", |s| s.invisible()).child(accessory))
                        .child(row_menu_button(("sub-more", sid as usize), MenuTarget::Sub(sid), sub_row_selected, cx))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sid, cx))),
                );
                if !selected_sub {
                    continue;
                }
                // Pipeline agents whose part is done fold into one row.
                let (visible, folded) = self.sidebar_agents(sid);
                let helpers_open = self.show_helpers.contains(&sid);
                let mut rows: Vec<&AgentView> = visible;
                if helpers_open {
                    rows.extend(folded.iter().copied());
                }
                for agent in rows {
                    let aid = agent.record.id;
                    let role = agent.role();
                    let shown = self.sel_agent == Some(aid);
                    let state = agent_state(agent);
                    let fresh = self.unseen.contains(&aid);
                    let accessory: AnyElement = match (state, agent.turn.as_ref()) {
                        (State::NeedsYou, _) => div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::red_text())).child("needs you").into_any_element(),
                        (State::Working, Some(turn)) => {
                            let secs = turn.started.elapsed().as_secs();
                            ui::mono(if secs >= 60 { format!("{}:{:02}", secs / 60, secs % 60) } else { format!("{secs}s") }).into_any_element()
                        }
                        _ => match &agent.confidence {
                            Some(c) => ui::score_meter(c.score).into_any_element(),
                            None => ui::mono(short_role(&role)).into_any_element(),
                        },
                    };
                    let story = {
                        let doing = match state {
                            State::NeedsYou => "needs you",
                            State::Working => "working",
                            State::Done => "idle",
                            State::Idle => "not started",
                        };
                        let role_text = short_role(&role);
                        let mut parts = vec![agent.record.name.clone()];
                        if !role_text.is_empty() && role_text != agent.record.name {
                            parts.push(role_text);
                        }
                        parts.push(doing.to_string());
                        if let Some(c) = &agent.confidence {
                            parts.push(format!("reviewed {}/5", c.score));
                        }
                        if fresh {
                            parts.push("new reply".into());
                        }
                        parts.join(" · ")
                    };
                    let on = cursor == Some(TreeNode::Agent(aid));
                    if on {
                        cursor_ix = Some(ix);
                    }
                    ix += 1;
                    list = list.child(
                        ui::row(("agent", aid as usize), shown)
                            .when(on && !shown, |d| d.bg(rgb(theme::hover())))
                            .when(on, |d| d.child(tree_bar()))
                            .relative()
                            .group("tree-row")
                            .on_mouse_down(MouseButton::Right, on_right_click(MenuTarget::Agent(aid), cx))
                            .pl(px(46.))
                            .tooltip(crate::shell::tip_text(story))
                            .child(ui::status_dot(("agent-dot", aid as usize), state))
                            .child(
                                ui::one_line(div().flex_shrink())
                                    .when(fresh, |d| d.text_color(rgb(theme::text())).font_weight(FontWeight::SEMIBOLD))
                                    .child(agent.record.name.clone()),
                            )
                            .when(fresh, |d| d.child(new_dot()))
                            .child(div().flex_1())
                            .child(div().flex_none().group_hover("tree-row", |s| s.invisible()).child(accessory))
                            .child(row_menu_button(("agent-more", aid as usize), MenuTarget::Agent(aid), shown, cx))
                            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                                if event.modifiers().control {
                                    this.pin_beside(aid, cx);
                                } else {
                                    this.select_agent(aid, cx);
                                }
                            })),
                    );
                }
                if !folded.is_empty() {
                    ix += 1;
                    let names: Vec<String> = folded.iter().map(|a| a.record.name.clone()).collect();
                    list = list.child(
                        ui::row(("show-helpers", sid as usize), false)
                            .pl(px(46.))
                            .text_color(rgb(theme::text_3()))
                            .child(ui::disclosure(helpers_open))
                            .child(ui::one_line(div().flex_1()).child(if helpers_open {
                                "Hide the agents whose part is done".to_string()
                            } else {
                                format!("{} done: {}", folded.len(), names.join(", "))
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.show_helpers.remove(&sid) {
                                    this.show_helpers.insert(sid);
                                }
                                cx.notify();
                            })),
                    );
                }
                ix += 1;
                list = list.child(if self.creating_agent == Some(sid) {
                    div()
                        .id(("new-agent", sid as usize))
                        .h(px(28.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(46.))
                        .pr(px(10.))
                        .text_size(px(13.))
                        .text_color(rgb(theme::text_2()))
                        .child(ui::spinner(("new-agent-spin", sid as usize), 13., theme::text_2()))
                        .child("Making a worktree…")
                } else {
                    ui::row(("new-agent", sid as usize), false)
                        .pl(px(46.))
                        .text_color(rgb(theme::text_3()))
                        .child(icon_sized(Icon::Plus, 13.).text_color(rgb(theme::text_3())))
                        .child(div().flex_1().child("New agent"))
                        .child(div().invisible().group_hover("new-agent-row", |s| s.visible()).children(crate::keys::hint("new-agent").map(ui::kbd)))
                        .group("new-agent-row")
                        .on_click(cx.listener(move |this, _, _, cx| this.add_agent(sid, cx)))
                });
                let archived: Vec<&AgentView> = self.agents.iter().filter(|a| a.record.subproject_id == sid && a.record.archived).collect();
                if !archived.is_empty() {
                    let open = self.show_archived.contains(&sid);
                    ix += 1;
                    list = list.child(
                        ui::row(("show-archived", sid as usize), false)
                            .pl(px(46.))
                            .text_color(rgb(theme::text_3()))
                            .child(ui::disclosure(open))
                            .child(div().flex_1().child(format!("{} archived", archived.len())))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.show_archived.remove(&sid) {
                                    this.show_archived.insert(sid);
                                }
                                cx.notify();
                            })),
                    );
                    if open {
                        for agent in archived {
                            let aid = agent.record.id;
                            let on = cursor == Some(TreeNode::Agent(aid));
                            if on {
                                cursor_ix = Some(ix);
                            }
                            ix += 1;
                            list = list.child(
                                ui::row(("archived-agent", aid as usize), self.sel_agent == Some(aid))
                                    .when(on, |d| d.child(tree_bar()))
                                    .relative()
                                    .group("tree-row")
                                    .on_mouse_down(MouseButton::Right, on_right_click(MenuTarget::Agent(aid), cx))
                                    .pl(px(62.))
                                    .text_color(rgb(theme::text_3()))
                                    .child(icon_sized(Icon::Archive, 12.).text_color(rgb(theme::text_4())))
                                    .child(div().flex_1().min_w(px(0.)).truncate().child(agent.record.name.clone()))
                                    .child(row_menu_button(("archived-more", aid as usize), MenuTarget::Agent(aid), false, cx))
                                    .on_click(cx.listener(move |this, _, _, cx| this.select_agent(aid, cx))),
                            );
                        }
                    }
                }
            }
        }

        if let Some(row) = cursor_ix
            && self.tree_scroll_pending.replace(false)
        {
            self.tree_scroll.scroll_to_item(row);
        }
        let _ = ix;

        let inbox_el = (!inbox.is_empty()).then(|| {
            div()
                .mx(px(8.))
                .mb(px(4.))
                .flex()
                .flex_col()
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(theme::line()))
                .bg(rgb(theme::raised()))
                .children(inbox.iter().take(6).enumerate().map(|(i, item)| {
                    let (sub_id, agent_id) = (item.sub_id, item.agent_id);
                    div()
                        .id(("inbox", i))
                        .flex()
                        .gap(px(9.))
                        .px(px(10.))
                        .py(px(8.))
                        .cursor_pointer()
                        .when(i > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                        .hover(|s| s.bg(rgb(theme::hover())))
                        .child(div().pt(px(5.)).child(div().size(px(7.)).rounded_full().bg(rgb(theme::red()))))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(div().text_size(px(12.5)).text_color(rgb(theme::text())).child(item.title.clone()))
                                .child(ui::one_line(div()).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(item.detail.clone())),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.open_inbox(sub_id, agent_id, cx)))
                }))
                .when(inbox.len() > 6, |d| {
                    d.child(
                        div()
                            .px(px(10.))
                            .py(px(6.))
                            .border_t_1()
                            .border_color(rgb(theme::line()))
                            .text_size(px(11.5))
                            .text_color(rgb(theme::text_3()))
                            .child(format!("{} more · F8 goes through them", inbox.len() - 6)),
                    )
                })
        });

        let section_label = |text: &'static str, count: Option<usize>| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .px(px(14.))
                .pt(px(14.))
                .pb(px(6.))
                .child(ui::label(text))
                .children(count.map(|n| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(crate::keys::hint("next-needs-you").map(ui::kbd))
                        .child(div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::red_text())).child(n.to_string()))
                }))
        };

        let usage_meta = format!(
            "{:.1}% of {}",
            self.meter.live_share() * 100.0,
            if self.budget.share_pct >= 100 { "no cap".to_string() } else { format!("{}%", self.budget.share_pct) }
        );
        div()
            .relative()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::panel()))
            .border_r_1()
            .border_color(rgb(theme::line()))
            .child(self.sidebar_handle(width, cx))
            .when(!inbox.is_empty(), |d| d.child(section_label("Needs you", Some(inbox.len()))).children(inbox_el))
            .child(
                section_label("Projects", None).child(
                    div()
                        .id("add-project")
                        .text_color(rgb(theme::text_3()))
                        .hover(|s| s.text_color(rgb(theme::text())))
                        .cursor_pointer()
                        .tooltip(tip("Add a project"))
                        .child(icon_sized(Icon::Plus, 14.))
                        .on_click(cx.listener(|this, _, window, cx| this.open_project_form(window, cx))),
                ),
            )
            .child(
                list.id("sidebar-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.tree_scroll)
                    .track_focus(&self.tree_focus)
                    .key_context("Tree")
                    // A click puts the keyboard cursor back on what you open.
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, _| this.tree_cursor = None))
                    .on_action(cx.listener(|this, _: &crate::TreeUp, _, cx| this.tree_step(-1, cx)))
                    .on_action(cx.listener(|this, _: &crate::TreeDown, _, cx| this.tree_step(1, cx)))
                    .on_action(cx.listener(|this, _: &crate::TreeLeft, _, cx| this.tree_left(cx)))
                    .on_action(cx.listener(|this, _: &crate::TreeRight, _, cx| this.tree_right(cx)))
                    .on_action(cx.listener(|this, _: &crate::TreeOpen, window, cx| this.tree_open(window, cx))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .p(px(6.))
                    .border_t_1()
                    .border_color(rgb(theme::line()))
                    .child(
                        ui::row("nav-usage", false)
                            .child(icon(Icon::Gauge))
                            .child(div().flex_1().child("Usage"))
                            .child(ui::mono(usage_meta))
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
                    )
                    .child(
                        ui::row("nav-settings", matches!(self.form, Some(Form::Settings { .. })))
                            .child(icon(Icon::Sliders))
                            .child(div().flex_1().child("Settings"))
                            .children(crate::keys::hint("settings").map(ui::kbd))
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
                    ),
            )
            .into_any_element()
    }

    // ---- Status bar ----------------------------------------------------------------------------

    fn render_statusbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let (state, label): (State, SharedString) = match &self.conn {
            Conn::Starting => (State::Working, "Starting WSL…".into()),
            Conn::Handshaking => (State::Working, "Connecting to Claude…".into()),
            Conn::Ready if self.logged_in == Some(false) => (State::NeedsYou, "Not logged in".into()),
            Conn::Ready => (State::Done, "Claude connected".into()),
            Conn::Failed(_) => match self.reconnect.at {
                Some(at) => (State::NeedsYou, format!("Reconnecting in {}s", at.saturating_duration_since(Instant::now()).as_secs() + 1).into()),
                None => (State::NeedsYou, "Disconnected · click to reconnect".into()),
            },
        };
        let working = self.agents.iter().filter(|a| a.is_working()).count();
        let branch = self.sel_sub.and_then(|id| self.subprojects.iter().find(|s| s.id == id)).map(|s| s.branch.clone());
        let item = |id: &'static str| {
            div()
                .id(id)
                .h_full()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text_2())))
        };
        let week = self.plan_usage.week.as_ref().map(|w| w.utilization * 100.0);
        div()
            .h(px(26.))
            .flex_none()
            .flex()
            .items_center()
            .px(px(4.))
            .bg(rgb(theme::panel()))
            .border_t_1()
            .border_color(rgb(theme::line()))
            .font_family(theme::MONO)
            .text_size(px(11.))
            .text_color(rgb(theme::text_3()))
            .child(
                item("status-sidebar")
                    .cursor_pointer()
                    .tooltip(tip_key("Sidebar", crate::keys::label("sidebar")))
                    .child(icon_sized(Icon::Sidebar, 14.))
                    .on_click(cx.listener(|this, _, window, cx| this.on_toggle_sidebar(&crate::ToggleSidebar, window, cx))),
            )
            .child(
                item("status-conn")
                    .when(matches!(self.conn, Conn::Failed(_)), |d| d.cursor_pointer())
                    .child(ui::status_dot("conn-dot", state))
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if matches!(this.conn, Conn::Failed(_)) || this.logged_in == Some(false) {
                            this.reconnect_now(cx);
                        }
                    })),
            )
            .child(item("status-distro").child(format!("{} · WSL", self.config.distro)))
            .when(working > 0, |d| {
                // Click: the next working agent (round the ones that are).
                d.child(
                    item("status-working")
                        .cursor_pointer()
                        .tooltip(tip("Go to the next working agent"))
                        .child(ui::status_dot("working-dot", State::Working))
                        .child(format!("{working} working"))
                        .on_click(cx.listener(|this, _, _, cx| this.next_working_agent(cx))),
                )
            })
            .child(div().flex_1())
            .children(branch.map(|b| item("status-branch").child(icon_sized(Icon::Branch, 13.)).child(b)))
            .children(week.map(|w| {
                item("status-week")
                    .cursor_pointer()
                    .child(format!("Week {w:.0}%"))
                    .child(
                        div().w(px(44.)).h(px(4.)).rounded_full().bg(rgb(theme::line_strong())).child(
                            div().h_full().rounded_full().bg(rgb(theme::text_2())).w(relative((w as f32 / 100.).clamp(0., 1.))),
                        ),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx)))
            }))
            .child(
                item("status-monaged")
                    .cursor_pointer()
                    .when(self.budget_block().is_some(), |d| d.text_color(rgb(theme::red_text())))
                    .child(format!(
                        "Monaged {:.1}% / {}",
                        self.meter.live_share() * 100.0,
                        if self.budget.share_pct >= 100 { "no cap".to_string() } else { format!("{}%", self.budget.share_pct) }
                    ))
                    .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
            )
            .child(
                item("status-keys")
                    .cursor_pointer()
                    .tooltip(tip_key("Keyboard shortcuts", crate::keys::label("shortcuts")))
                    .child(icon_sized(Icon::Keyboard, 14.))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx))),
            )
            .child(
                item("status-inspector")
                    .cursor_pointer()
                    .tooltip(tip_key("Dock", crate::keys::label("inspector")))
                    .child(icon_sized(Icon::PanelRight, 14.))
                    .on_click(cx.listener(|this, _, window, cx| this.on_toggle_inspector(&crate::ToggleInspector, window, cx))),
            )
            .into_any_element()
    }

    // ---- Banners ---------------------------------------------------------------------------------

    pub(crate) fn notice_bar(&self, text: AnyElement, attention: bool) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(16.))
            .py(px(8.))
            .bg(rgb(if attention { theme::red_soft() } else { theme::panel() }))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .text_size(px(12.5))
            .child(icon(if attention { Icon::Alert } else { Icon::Clock }).text_color(rgb(if attention {
                theme::red_text()
            } else {
                theme::text_3()
            })))
            .child(div().flex_1().min_w(px(0.)).child(text))
    }

    fn render_login_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.logged_in != Some(false) {
            return None;
        }
        let text = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child("Claude isn't signed in inside WSL.")
            .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child("Sign in once: Claude's page opens in your browser and you paste the code it shows. Monaged reconnects by itself."))
            .into_any_element();
        Some(
            self.notice_bar(text, true)
                .child(ui::button("login-open", Kind::Primary, Some(Icon::Key), "Sign in").on_click(cx.listener(|this, _, _, cx| this.sign_in_window(cx))))
                .child(ui::button("login-reconnect", Kind::Ghost, Some(Icon::Refresh), "Reconnect").on_click(cx.listener(|this, _, _, cx| this.reconnect_now(cx))))
                .into_any_element(),
        )
    }

    // ---- Content -------------------------------------------------------------------------------------

    fn render_content(&self, columns: &[Column], main_w: f32, window_h: f32, cx: &mut Context<Self>) -> AnyElement {
        if let Some(Form::Settings { error }) = &self.form {
            return self.render_settings(error.as_ref(), main_w, window_h, cx);
        }
        if let Some(Form::Health) = &self.form {
            return self.render_health(cx);
        }
        if let Some(Form::Guide) = &self.form {
            return self.render_guide(cx);
        }
        if let Some(Form::Setup) = &self.form {
            return self.render_setup(cx);
        }
        if let Some(Form::Project(page)) = &self.form {
            return self.render_project_page(page, cx);
        }
        if let Some(view) = self.render_commit_view(cx) {
            return view;
        }
        if let Some(view) = self.render_replay(cx) {
            return view;
        }
        if let Some(form) = &self.form {
            return self.render_form(form, main_w, cx);
        }
        if let Some(sub_id) = self.sel_sub {
            if self.sub_view == Some(sub_id) {
                return self.render_sub_changes(sub_id, cx);
            }
            if self.on_dashboard() {
                return self.render_dashboard(sub_id, main_w, cx);
            }
            return self.render_columns(sub_id, columns, main_w, cx);
        }
        if let Some(project_id) = self.sel_project {
            return self.render_overview(project_id, main_w, cx);
        }
        self.render_welcome(cx)
    }

    fn empty_state(&self, glyph: Icon, title: &'static str, body: &'static str, action: AnyElement) -> AnyElement {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .max_w(px(420.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(div().text_color(rgb(theme::text_3())).child(icon(glyph).size(px(24.))))
                    .child(ui::heading(title))
                    .child(div().text_size(px(13.)).text_color(rgb(theme::text_3())).text_center().child(body))
                    .child(div().pt(px(6.)).child(action)),
            )
            .into_any_element()
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        self.empty_state(
            Icon::Folder,
            "Add your first project",
            "A project is a git repo inside WSL. Split it into subprojects and let Claude agents work on them side by side, each in its own worktree.",
            div()
                .flex()
                .gap(px(8.))
                .child(ui::button("welcome-add", Kind::Primary, Some(Icon::Plus), "Add project").on_click(cx.listener(|this, _, window, cx| this.open_project_form(window, cx))))
                .child(if self.playground_busy {
                    ui::busy_button("welcome-playground", Kind::Secondary, "Making the playground…")
                } else {
                    ui::button("welcome-playground", Kind::Secondary, Some(Icon::Flow), "Try the playground")
                        .tooltip(tip("A small app with a bug, a flaky test and a merge conflict, to try every feature on"))
                        .on_click(cx.listener(|this, _, _, cx| this.create_playground(cx)))
                })
                .into_any_element(),
        )
    }

    // ---- Agent columns ------------------------------------------------------------------------------

    fn render_columns(&self, sub_id: i64, columns: &[Column], main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        if columns.is_empty() {
            return self.empty_state(
                Icon::Agent,
                "No agents yet",
                "Start an agent in its own worktree, or run the pipeline: research, verification, a plan, then builders.",
                div()
                    .flex()
                    .gap(px(8.))
                    .child(if self.creating_agent == Some(sub_id) {
                        ui::busy_button("empty-new-agent", Kind::Secondary, "Making a worktree…")
                    } else {
                        ui::button("empty-new-agent", Kind::Secondary, Some(Icon::Plus), "New agent").on_click(cx.listener(move |this, _, _, cx| this.add_agent(sub_id, cx)))
                    })
                    .child(
                        ui::button("empty-run-pipeline", Kind::Primary, Some(Icon::Play), "Run pipeline")
                            .on_click(cx.listener(move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx))),
                    )
                    .into_any_element(),
            );
        }
        let multi = columns.iter().filter(|c| matches!(c, Column::Agent(_))).count() > 1;
        let has_room = !self.zen && columns.len() < ((main_w / COLUMN_MIN).floor() as usize).clamp(1, 4);
        let widths = self.pane_widths(sub_id, columns.len(), main_w);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .children(columns.iter().enumerate().map(|(i, column)| {
                let pane_w = widths.get(i).copied().unwrap_or(main_w);
                let pane = match *column {
                    Column::Agent(id) => match self.agent(id) {
                        Some(agent) => self.render_agent_column(agent, multi, has_room, columns, pane_w, cx),
                        None => div().into_any_element(),
                    },
                    Column::Changes(id) => match self.agent(id) {
                        Some(agent) => self.render_changes_column(agent, cx),
                        None => div().into_any_element(),
                    },
                };
                let last = i + 1 == columns.len();
                div()
                    .relative()
                    .when(!last, |d| d.w(px(pane_w)).flex_none())
                    .when(last, |d| d.flex_1().min_w(px(0.)))
                    .h_full()
                    .flex()
                    .flex_col()
                    .when(i > 0, |d| d.border_l_1().border_color(rgb(theme::line())))
                    .child(pane)
                    .when(i > 0, |d| d.child(self.pane_handle(sub_id, i, widths.clone(), cx)))
            }))
            .into_any_element()
    }

    fn render_agent_column(
        &self,
        agent: &AgentView,
        multi: bool,
        has_room: bool,
        columns: &[Column],
        pane_w: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // An exact width: GPUI measures wrapped text against the width it's given,
        // and a max-width column made long replies taller than their layout box.
        let side_open = self.side_chats.get(&agent.record.id).is_some_and(|c| c.open);
        let pane_w = if side_open { pane_w - crate::side_chat::SIDE_WIDTH } else { pane_w };
        let reading = (pane_w - 48.).clamp(240., READING_WIDTH);
        let id = agent.record.id;
        let focused = self.sel_agent == Some(id);
        let changes_beside = columns.contains(&Column::Changes(id));
        let body = if agent.tab == AgentTab::Changes && !changes_beside {
            self.render_changes(agent, cx)
        } else {
            self.render_transcript(agent, reading, cx)
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(self.render_agent_header(agent, focused, multi, has_room, changes_beside, cx))
            .child(match self.render_side_chat(id, cx) {
                Some(side) => div().flex_1().min_h(px(0.)).flex().child(div().flex_1().min_w(px(0.)).flex().flex_col().child(body)).child(side).into_any_element(),
                None => body,
            })
            .children(self.render_env_requests(id, cx).map(|cards| div().px(px(16.)).pb(px(8.)).child(cards)))
            .when((agent.tab == AgentTab::Conversation || changes_beside) && !agent.record.archived, |d| d.child(self.render_message_box(agent, reading, cx)))
            .when(agent.record.archived, |d| {
                d.child(
                    self.notice_bar(div().text_color(rgb(theme::text_2())).child("Archived: read-only. Restore it to keep working with it.").into_any_element(), false)
                        .child(ui::button(("restore-agent", id as usize), Kind::Secondary, Some(Icon::Archive), "Restore").on_click(cx.listener(move |this, _, _, cx| this.restore_agent(id, cx)))),
                )
            })
            .into_any_element()
    }

    fn render_agent_header(
        &self,
        agent: &AgentView,
        focused: bool,
        multi: bool,
        has_room: bool,
        changes_beside: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = agent.record.id;
        let role = agent.role();
        let state = agent_state(agent);
        let status = match (&agent.status, &agent.turn) {
            (_, Some(turn)) => {
                let secs = Instant::now().duration_since(turn.started).as_secs();
                if secs >= 60 { format!("working {}m", secs / 60) } else { "working".into() }
            }
            (Status::Starting, _) => "starting".into(),
            (Status::Loading, _) => "reopening".into(),
            (Status::Failed(_), _) => "failed".into(),
            (Status::Idle, _) => "not connected".into(),
            (Status::Ready, None) if agent.paused => "paused".into(),
            (Status::Ready, None) => "idle".into(),
        };
        let mut meta: Vec<String> = vec![role_label(&role)];
        if let Some(title) = &agent.title {
            meta.push(title.clone());
        }
        if let Some(label) = self.agent_usage_label(agent) {
            meta.push(label);
        }
        let worktree = agent.record.worktree.clone();
        let changes = agent.changes.as_ref().map(Vec::len);
        let segmented = (!changes_beside).then(|| {
            let seg = |tab_id: &'static str, label: String, on: bool| {
                div()
                    .id(tab_id)
                    .px(px(10.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .text_color(rgb(if on { theme::text() } else { theme::text_3() }))
                    .when(on, |d| d.bg(rgb(theme::selected())))
                    .when(!on, |d| d.hover(|s| s.text_color(rgb(theme::text_2()))))
                    .child(label)
            };
            div()
                .flex()
                .p(px(2.))
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(theme::line()))
                .bg(rgb(theme::panel()))
                .child(seg("seg-conversation", "Conversation".into(), agent.tab == AgentTab::Conversation).on_click(
                    cx.listener(move |this, _, _, cx| this.set_tab(id, AgentTab::Conversation, cx)),
                ))
                .child(
                    seg("seg-changes", match changes { Some(n) => format!("Changes {n}"), None => "Changes".into() }, agent.tab == AgentTab::Changes)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(id, AgentTab::Changes, cx))),
                )
        });
        div()
            .id(("agent-header", id as usize))
            .h(px(54.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .when(multi && focused, |d| d.border_t_2().border_color(rgb(theme::text_3())))
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.sel_agent != Some(id) {
                    this.select_agent(id, cx);
                }
            }))
            .child(icon(role_icon(&role)).text_color(rgb(theme::text_2())))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(ui::heading(agent.record.name.clone()))
                            .children(agent.confidence.as_ref().map(|c| ui::score_meter(c.score))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(rgb(theme::text_3()))
                            .min_w(px(0.))
                            .child(div().truncate().child(meta.join(" · ")))
                            .child("·")
                            .child(ui::status_dot(("header-dot", id as usize), state))
                            .child(div().flex_none().text_color(rgb(if state == State::NeedsYou { theme::red_text() } else { theme::text_3() })).child(status)),
                    ),
            )
            .children(segmented)
            .child(if agent.reviewing {
                div()
                    .id(("review", id as usize))
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(tip("Reviewing: running the tests and a skeptical read"))
                    .child(ui::spinner(("review-spin", id as usize), 14., theme::text_2()))
            } else {
                ui::icon_button(("review", id as usize), Icon::Eye)
                    .tooltip(tip("Review: run the tests and a skeptical read"))
                    .on_click(cx.listener(move |this, _, _, cx| this.review_agent(id, cx)))
            })
            .when(focused, |d| {
                let sub_id = agent.record.subproject_id;
                d.child(
                    ui::icon_button(("to-dashboard", id as usize), Icon::Board)
                        .tooltip(tip("The subproject's dashboard: everything that waits for you, the agents and the lead"))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sub_id, cx))),
                )
            })
            .child(
                ui::icon_button(("side-chat", id as usize), Icon::Ask)
                    .tooltip(tip_key("Side chat: ask Haiku about this agent's work (the agent doesn't see it)", crate::keys::label("side-chat")))
                    .on_click(cx.listener(move |this, _, window, cx| this.toggle_side_chat(id, window, cx))),
            )
            .child(
                ui::icon_button(("zed", id as usize), Icon::External)
                    .tooltip(tip("Open the worktree in Zed"))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(worktree.clone(), cx))),
            )
            .when(agent.is_working(), |d| {
                d.child(
                    ui::icon_button(("stop", id as usize), Icon::Stop)
                        .tooltip(tip_key("Stop this turn", "Esc"))
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(id, cx))),
                )
            })
            .when(focused && (multi || changes_beside) && !self.one_pane, |d| {
                d.child(
                    ui::icon_button(("one-pane", id as usize), Icon::Maximize)
                        .tooltip(tip("Show only this agent (Ctrl K → Show agents side by side to go back)"))
                        .on_click(cx.listener(|this, _, _, cx| this.set_one_pane(true, cx))),
                )
            })
            .when(focused && self.one_pane, |d| {
                d.child(
                    ui::icon_button(("side-panes", id as usize), Icon::Restore)
                        .tooltip(tip("Show agents side by side"))
                        .on_click(cx.listener(|this, _, _, cx| this.set_one_pane(false, cx))),
                )
            })
            .when(has_room && focused && !self.one_pane, |d| {
                d.child(
                    ui::icon_button(("beside", id as usize), Icon::PanelRight)
                        .tooltip(tip("Ctrl-click an agent in the tree to open it beside this one"))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(id, AgentTab::Changes, cx))),
                )
            })
            .child(
                ui::icon_button(("agent-menu", id as usize), Icon::More)
                    .tooltip(tip("More actions"))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.open_menu(MenuTarget::Agent(id), event.position(), window, cx);
                    })),
            )
            .when(multi && !focused, |d| {
                d.child(
                    ui::icon_button(("close-col", id as usize), Icon::X)
                        .tooltip(tip("Close this pane"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_column(id, cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_changes_column(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let count = agent.changes.as_ref().map(Vec::len).unwrap_or(0);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(54.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(rgb(theme::line()))
                    .child(icon(Icon::Diff).text_color(rgb(theme::text_2())))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(ui::heading("Changes"))
                            .child(ui::meta(format!("{} · {count} file{}", agent.record.name, if count == 1 { "" } else { "s" }))),
                    )
                    .child(if agent.loading_changes {
                        div()
                            .id(("refresh-changes", id as usize))
                            .size(px(28.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(ui::spinner(("refresh-spin", id as usize), 14., theme::text_2()))
                    } else {
                        ui::icon_button(("refresh-changes", id as usize), Icon::Refresh)
                            .tooltip(tip("Read the diff again"))
                            .on_click(cx.listener(move |this, _, _, cx| this.refresh_changes(id, false, cx)))
                    })
                    .child(
                        ui::icon_button(("close-changes", id as usize), Icon::X)
                            .tooltip(tip("Close this pane (the agent's Changes tab still has them; Ctrl K → Show changes beside the agent brings it back)"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_changes_beside(false, cx);
                            })),
                    ),
            )
            .child(self.render_changes(agent, cx))
            .into_any_element()
    }

    pub(crate) fn render_transcript(&self, agent: &AgentView, reading: f32, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let matches = self.find_matches(cx);
        let items: Vec<AnyElement> = self.transcript_rows(agent, reading, &matches, cx);
        let scroll = self.scrolls.get(&id).cloned().unwrap_or_default();
        // Scrolled up to read while new text arrives: offer the way back down.
        let max = f32::from(scroll.max_offset().height);
        let at = -f32::from(scroll.offset().y);
        let scrolled_up = max - at > 240.;
        let jump = scrolled_up.then(|| {
            div().absolute().bottom(px(12.)).left_0().right_0().flex().justify_center().child(
                div()
                    .id(("jump-latest", id as usize))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(28.))
                    .px(px(12.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(theme::line_strong()))
                    .bg(rgb(theme::raised()))
                    .shadow_md()
                    .text_size(px(12.))
                    .text_color(rgb(theme::text()))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .child(icon_sized(Icon::ArrowDown, 13.).text_color(rgb(theme::text())))
                    .child(if agent.is_working() { "Latest · still working" } else { "Latest" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(handle) = this.scrolls.get(&id) {
                            handle.scroll_to_bottom();
                        }
                        cx.notify();
                    })),
            )
        });
        // A flex column so the scroll area gets a definite height (and scrolls)
        // instead of growing under the message box.
        let tail = [self.render_session_state(agent, cx), self.render_working_line(agent, cx)]
            .into_iter()
            .flatten()
            .map(|el| div().w(px(reading)).flex_none().child(el).into_any_element());
        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .flex()
            .flex_col()
            .children(self.render_find_bar(id, cx))
            .child(
                div()
                    .id(("transcript", id as usize))
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(ui::dense(14., 10.)))
                    .px(px(24.))
                    .py(px(ui::dense(24., 16.)))
                    .children(items)
                    .children(tail),
            )
            .children(jump)
            .into_any_element()
    }

    /// What the session is doing when there's no conversation to show yet:
    /// waiting for Claude, starting, reopening (as a skeleton), or failed.
    fn render_session_state(&self, agent: &AgentView, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = agent.record.id;
        let line = |spin: bool, text: String| {
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .text_size(px(12.5))
                .text_color(rgb(theme::text_3()))
                .when(spin, |d| d.child(ui::spinner(("session-spin", id as usize), 13., theme::text_3())))
                .child(text)
                .into_any_element()
        };
        match &agent.status {
            Status::Ready => None,
            Status::Failed(reason) => Some(
                ui::callout(Tone::Error, format!("Couldn't open the session: {reason}"))
                    .child(
                        ui::button(("retry-session", id as usize), Kind::Secondary, Some(Icon::Refresh), "Try again")
                            .on_click(cx.listener(move |this, _, _, cx| this.retry_session(id, cx))),
                    )
                    .into_any_element(),
            ),
            Status::Starting => Some(line(true, format!("Starting a Claude session in {}", agent.record.worktree))),
            Status::Loading => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(22.))
                    .child(line(true, "Reopening the saved conversation".into()))
                    .child(div().flex().justify_end().child(div().w(relative(0.5)).child(ui::skeleton(("sk-user", id as usize), relative(1.).into(), 34.))))
                    .child(ui::skeleton_text(&format!("sk-a-{id}"), 4))
                    .child(div().flex().justify_end().child(div().w(relative(0.35)).child(ui::skeleton(("sk-user2", id as usize), relative(1.).into(), 34.))))
                    .child(ui::skeleton_text(&format!("sk-b-{id}"), 3))
                    .into_any_element(),
            ),
            Status::Idle => Some(match &self.conn {
                Conn::Starting | Conn::Handshaking => line(true, "Waiting for Claude to connect".into()),
                Conn::Failed(_) => ui::callout(Tone::Warn, "Claude isn't connected, so this agent can't start.")
                    .child(ui::button(("idle-reconnect", id as usize), Kind::Secondary, Some(Icon::Refresh), "Reconnect").on_click(cx.listener(|this, _, _, cx| this.reconnect_now(cx))))
                    .into_any_element(),
                Conn::Ready => div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(line(false, "This agent's session isn't open.".into()))
                    .child(ui::button(("open-session", id as usize), Kind::Secondary, Some(Icon::Play), "Open").on_click(cx.listener(move |this, _, _, cx| this.retry_session(id, cx))))
                    .into_any_element(),
            }),
        }
    }

    pub(crate) fn render_item(&self, agent_id: i64, index: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        match item {
            Item::User(text) if text.starts_with(pipeline::BRIEF_HEADER) => pipeline_ui::render_brief(text),
            Item::User(text) if text.starts_with("[monaged-ask #") || text.contains(coord::DIGEST_HEADER) => coord_ui::render_user_message(text),
            // The answers to a question card: the card above shows each one.
            Item::User(text) if pipeline::parse_answers(text).is_some() => {
                let n = pipeline::parse_answers(text).map_or(0, |a| a.len());
                let auto = text.starts_with("Answers (chosen automatically");
                div()
                    .flex()
                    .justify_end()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.5))
                    .text_color(rgb(theme::text_3()))
                    .child(icon_sized(Icon::Check, 12.).text_color(rgb(theme::text_3())))
                    .child(match (auto, n) {
                        (true, 1) => "Auto mode answered the question".to_string(),
                        (true, n) => format!("Auto mode answered {n} questions"),
                        (false, 1) => "You answered the question".to_string(),
                        (false, n) => format!("You answered {n} questions"),
                    })
                    .into_any_element()
            }
            Item::User(text) if text.starts_with(crate::lead::UPDATE_PREFIX) => div()
                .flex()
                .items_start()
                .gap(px(8.))
                .text_size(px(12.5))
                .text_color(rgb(theme::text_3()))
                .child(div().flex_none().pt(px(2.)).child(icon(Icon::Flow).size(px(13.))))
                .child(div().flex_1().min_w(px(0.)).child(text.split("\n\n").map(|part| part.trim().trim_start_matches(crate::lead::UPDATE_PREFIX).trim()).collect::<Vec<_>>().join(" · ")))
                .into_any_element(),
            Item::User(text) => div()
                .flex()
                .justify_end()
                .items_center()
                .gap(px(6.))
                .group("user-msg")
                .children(self.rollback_point(agent_id, index).map(|ordinal| {
                    div()
                        .id(("rollback", agent_id as usize * 100_000 + index))
                        .invisible()
                        .group_hover("user-msg", |s| s.visible())
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(theme::hover())))
                        .tooltip(tip("Restore the files to before this message"))
                        .child(icon_sized(Icon::Undo, 14.).text_color(rgb(theme::text_3())))
                        .on_click(cx.listener(move |this, _, window, cx| this.ask_confirm(crate::feedback::ConfirmAction::Rollback(agent_id, ordinal), window, cx)))
                }))
                .child(
                    div()
                        .max_w(relative(0.85))
                        .px(px(13.))
                        .py(px(9.))
                        .rounded(px(10.))
                        .bg(rgb(theme::raised()))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .text_size(px(14.))
                        .line_height(px(21.))
                        .child(if crate::markdown::looks_like_markdown(text) { review_ui::render_markdown(text) } else { text.clone().into_any_element() }),
                )
                .into_any_element(),
            Item::Agent(text) if pipeline::contains_directive(text) => self.render_agent_reply(agent_id, index, text, cx),
            Item::Agent(text) => review_ui::render_markdown(text),
            Item::Thought(text) => div()
                .pl(px(12.))
                .border_l_1()
                .border_color(rgb(theme::line_strong()))
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(rgb(theme::text_3()))
                .italic()
                .child(text.clone())
                .into_any_element(),
            Item::Tool { title, kind, status, detail, .. } => self.render_tool_item(agent_id, index, title, kind, status, detail, cx),
            Item::Elicit(form) => self.render_elicitation(agent_id, index, form, cx),
            Item::AgentImage(image) => div().flex().child(self.thumbnail(format!("agent-img-{agent_id}-{index}"), image.clone(), 240., cx)).into_any_element(),
            Item::Attachments(names) => div()
                .flex()
                .justify_end()
                .flex_wrap()
                .gap(px(6.))
                .mt(px(-8.))
                .children(names.iter().enumerate().map(|(n, (name, image))| {
                    if let Some(image) = image {
                        return self.thumbnail(format!("att-{agent_id}-{index}-{n}"), image.clone(), 120., cx);
                    }
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(7.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .text_size(px(11.5))
                        .text_color(rgb(theme::text_3()))
                        .child(icon_sized(Icon::Paperclip, 11.).text_color(rgb(theme::text_3())))
                        .child(name.clone())
                        .into_any_element()
                }))
                .into_any_element(),
            Item::Plan(entries) => ui::card()
                .gap(px(6.))
                .child(ui::label("Plan"))
                .children(entries.iter().map(|entry| {
                    let (glyph, color) = match entry.status.as_str() {
                        "completed" => (Some(Icon::Check), theme::text_3()),
                        "in_progress" => (None, theme::text()),
                        _ => (None, theme::text_2()),
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(13.))
                        .text_color(rgb(color))
                        .child(match glyph {
                            Some(g) => icon(g).size(px(14.)).into_any_element(),
                            None => div()
                                .size(px(14.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().size(px(9.)).rounded_full().border_1().border_color(rgb(color)))
                                .into_any_element(),
                        })
                        .child(entry.content.clone())
                }))
                .into_any_element(),
            Item::Permission { title, options, chosen, .. } => {
                let open = chosen.is_none();
                let card = ui::card()
                    .when(open, |d| d.border_color(rgb(theme::red())))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon(Icon::Key).text_color(rgb(if open { theme::red_text() } else { theme::text_3() })))
                            .child(div().flex_1().text_size(px(13.)).child(format!("Wants to: {title}"))),
                    );
                match chosen {
                    Some(answer) => card.child(ui::meta(format!("You chose: {answer}"))).into_any_element(),
                    None => card
                        .child(div().flex().flex_wrap().gap(px(8.)).children(options.iter().enumerate().map(|(j, option)| {
                            let kind = if option.kind.starts_with("allow") { if j == 0 { Kind::Primary } else { Kind::Secondary } } else { Kind::Danger };
                            let option_id = option.id.clone();
                            ui::button(("perm", (agent_id as usize) * 4096 + index * 16 + j), kind, None, option.name.clone()).on_click(cx.listener(
                                move |this, _, _, cx| this.choose_permission(agent_id, index, Some(option_id.clone()), cx),
                            ))
                        })))
                        .into_any_element(),
                }
            }
            Item::Notice(tone, text) => {
                let color = match tone {
                    Tone::Info => theme::text_3(),
                    Tone::Warn | Tone::Error => theme::red_text(),
                };
                div().text_size(px(12.)).text_color(rgb(color)).child(text.clone()).into_any_element()
            }
        }
    }

    // ---- Inspector ----------------------------------------------------------------------------------

    /// What a dock tab has to say before you open it: something needs you,
    /// something runs, something is ready, or how many.
    fn dock_badge(&self, sub_id: i64, tab: InspectorTab) -> Option<DockBadge> {
        match tab {
            InspectorTab::Pipeline => {
                let needs = self.pipeline_needs_you(sub_id) || self.asks.iter().any(|a| self.sub_of(a.to) == Some(sub_id) && a.waiting());
                let running = self.pipelines.get(&sub_id).is_some_and(|p| !matches!(p.stage, Stage::Done | Stage::Stopped));
                let board = self.board_count(sub_id);
                if needs {
                    Some(DockBadge::NeedsYou)
                } else if running {
                    Some(DockBadge::Working)
                } else {
                    (board > 0).then_some(DockBadge::Count(board))
                }
            }
            InspectorTab::Merge => {
                let state = self.merges.get(&sub_id);
                if state.is_some_and(|m| m.busy) {
                    return Some(DockBadge::Working);
                }
                if state.is_some_and(|m| !m.conflict.is_empty()) {
                    return Some(DockBadge::NeedsYou);
                }
                let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
                let target = self.merge_target(sub_id)?;
                let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target))).copied().unwrap_or(0);
                let at_bar = self.sub_confidence(sub_id).is_some_and(|(score, _)| score >= self.min_score());
                (ahead > 0 && at_bar).then_some(DockBadge::Ready)
            }
            InspectorTab::Git => {
                let panel = self.git_panels.get(&sub_id)?;
                if panel.busy.is_some() {
                    return Some(DockBadge::Working);
                }
                let files = panel.status.as_ref().and_then(|s| s.as_ref().ok()).map(|s| s.files.len()).unwrap_or(0);
                (files > 0).then_some(DockBadge::Count(files))
            }
            InspectorTab::Run => {
                let dirs: Vec<String> = self
                    .agents
                    .iter()
                    .filter(|a| a.record.subproject_id == sub_id && !a.record.archived)
                    .map(|a| a.record.worktree.clone())
                    .chain(self.run_dir(sub_id))
                    .collect();
                let states: Vec<&crate::run_ui::RunState> = dirs.iter().filter_map(|d| self.run_states.get(d)).collect();
                if states.iter().any(|r| r.tests.as_ref().is_some_and(|t| !t.passed)) {
                    Some(DockBadge::NeedsYou)
                } else if states.iter().any(|r| r.testing || r.dev.as_ref().is_some_and(|d| d.running)) {
                    Some(DockBadge::Working)
                } else {
                    None
                }
            }
            InspectorTab::Timeline => None,
        }
    }

    fn render_inspector(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(sub_id) = self.sel_sub else { return div().into_any_element() };
        let body: AnyElement = match self.inspector_tab {
            InspectorTab::Pipeline => self.render_pipeline_inspector(sub_id, cx),
            InspectorTab::Merge => self.render_merge_inspector(sub_id, cx),
            InspectorTab::Git => self.render_git_inspector(sub_id, cx),
            InspectorTab::Timeline => self.render_timeline(sub_id, cx),
            InspectorTab::Run => self.render_run_inspector(sub_id, cx),
        };
        let handle = self.inspector_handle(width, cx);
        // Icons beside the labels only when there's room for both.
        let show_icons = width >= 420.;
        let tabs = InspectorTab::ALL.iter().enumerate().map(|(i, (which, key))| {
            let which = *which;
            let on = self.inspector_tab == which;
            let badge: Option<AnyElement> = self.dock_badge(sub_id, which).map(|b| match b {
                DockBadge::NeedsYou => div().size(px(6.)).rounded_full().bg(rgb(theme::red())).into_any_element(),
                DockBadge::Ready => div().size(px(6.)).rounded_full().bg(rgb(theme::text())).into_any_element(),
                DockBadge::Working => ui::status_dot(("dock-badge", i), ui::State::Working),
                DockBadge::Count(n) => div()
                    .px(px(4.))
                    .min_w(px(16.))
                    .h(px(15.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(if on { theme::line_strong() } else { theme::line() }))
                    .font_family(theme::MONO)
                    .text_size(px(10.))
                    .text_color(rgb(if on { theme::text() } else { theme::text_2() }))
                    .child(if n > 99 { "99+".to_string() } else { n.to_string() })
                    .into_any_element(),
            });
            div()
                .id(("dock-tab", i))
                .flex_none()
                .h(px(28.))
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .rounded(px(6.))
                .text_size(px(12.5))
                .cursor_pointer()
                .text_color(rgb(if on { theme::text() } else { theme::text_3() }))
                .when(on, |d| d.bg(rgb(theme::selected())))
                .when(!on, |d| d.hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text_2()))))
                .tooltip(tip_key(which.label(), crate::keys::label(key)))
                .when(show_icons, |d| d.child(icon(which.icon()).size(px(13.))))
                .child(which.label())
                .children(badge)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.inspector_tab = which;
                    cx.notify();
                }))
        });
        div()
            .relative()
            .w(px(width))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::panel()))
            .border_l_1()
            .border_color(rgb(theme::line()))
            .child(handle)
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(8.))
                    .pr(px(6.))
                    .border_b_1()
                    .border_color(rgb(theme::line()))
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        ui::icon_button("dock-hide", Icon::PanelRight)
                            .size(px(26.))
                            .tooltip(tip_key("Hide the dock", crate::keys::label("inspector")))
                            .on_click(cx.listener(|this, _, window, cx| this.on_toggle_inspector(&crate::ToggleInspector, window, cx))),
                    ),
            )
            .child(div().id("inspector-body").flex_1().min_h(px(0.)).overflow_y_scroll().child(body))
            .into_any_element()
    }

    // ---- Project overview ------------------------------------------------------------------------------

    fn render_overview(&self, project_id: i64, main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id) else { return div().into_any_element() };
        let subs: Vec<_> = self.subprojects.iter().filter(|s| s.project_id == project_id).collect();
        let sub_ids: Vec<i64> = subs.iter().map(|s| s.id).collect();
        let working = self.agents.iter().filter(|a| sub_ids.contains(&a.record.subproject_id) && a.is_working()).count();
        let needs = self.inbox().into_iter().filter(|i| sub_ids.contains(&i.sub_id)).count();
        let week_cost: f64 = sub_ids.iter().filter_map(|id| self.sub_costs.get(id)).sum::<f64>().max(0.0);
        let week_share = week_cost * self.meter.share_per_usd() * 100.0;
        let ahead = self.branch_info.get(&project_id).and_then(|info| { let c = &info.chain; (c.len() >= 2).then(|| info.ahead.get(&(c[0].clone(), c[1].clone())).copied()).flatten() }).unwrap_or(0);
        let path = project.path.clone();

        let widget = |label: &'static str, number: String, foot: String, red: bool| {
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .p(px(16.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(if red { theme::red() } else { theme::line() }))
                .bg(rgb(theme::panel()))
                .child(ui::label(label).when(red, |d| d.text_color(rgb(theme::red_text()))))
                .child(dots::dot_text(&number, 5., rgb(if red { theme::red() } else { theme::text() })))
                .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(foot))
        };
        let widgets = div()
            .flex()
            .gap(px(12.))
            .child(widget("Working", working.to_string(), format!("agent{} running now", if working == 1 { "" } else { "s" }), false))
            .child(widget("Needs you", needs.to_string(), if needs == 0 { "nothing waiting".into() } else { "see the sidebar".into() }, needs > 0))
            .child(widget("This week", format!("${week_cost:.2}"), format!("≈{week_share:.1}% of your week"), false))
            .child(widget("Ready to promote", ahead.to_string(), "commits on the next hop".into(), false));

        let list = div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(theme::line()))
            .overflow_hidden()
            .when(subs.is_empty(), |d| {
                d.child(div().p(px(16.)).text_color(rgb(theme::text_3())).child(
                    "No subprojects yet. A subproject is a feature (its own branch) or a standing concern such as security.",
                ))
            })
            .children(subs.iter().enumerate().map(|(i, sub)| {
                let sid = sub.id;
                let count = self.agents.iter().filter(|a| a.record.subproject_id == sid && !a.record.archived).count();
                let needs_sub = self.inbox().iter().any(|n| n.sub_id == sid);
                let progress: AnyElement = match self.pipelines.get(&sid) {
                    Some(p) if p.stage == Stage::Done => ui::mono("done").into_any_element(),
                    Some(p) if p.stage == Stage::Stopped => ui::mono("stopped").into_any_element(),
                    Some(p) => ui::stage_dots(format!("ov-stages-{sid}"), p.stage.index(), Some(p.stage.index()), pipeline::STAGES.len()),
                    None => ui::mono("no pipeline").into_any_element(),
                };
                let cost = self.sub_costs.get(&sid).copied().unwrap_or(0.0);
                div()
                    .id(("ov-sub", sid as usize))
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .px(px(14.))
                    .cursor_pointer()
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .child(icon(if sub.kind == "standing" { Icon::Shield } else { Icon::Layers }).text_color(rgb(theme::text_3())))
                    .child(div().flex_1().min_w(px(0.)).truncate().child(sub.name.clone()))
                    .child(div().w(px(120.)).child(progress))
                    .child(div().w(px(80.)).child(ui::mono(format!("{count} agent{}", if count == 1 { "" } else { "s" }))))
                    .child(div().w(px(90.)).child(if needs_sub {
                        div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::red_text())).child("needs you")
                    } else {
                        ui::mono(if cost > 0.0 { format!("${cost:.2}") } else { String::new() })
                    }))
                    .child(icon_sized(Icon::ChevronRight, 14.).text_color(rgb(theme::text_4())))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sid, cx)))
            }));

        let branches = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.render_merge_queue(project_id, cx))
            .child(div().h(px(14.)))
            .child(ui::label("Branches"))
            .child(self.render_branch_panel(project_id, cx))
            .child(
                ui::switch(
                    "project-auto-start",
                    "Auto-start small, low-risk plans with confidence 4–5 (in subprojects with auto mode on). Large features, migrations, auth and data changes always wait for you.",
                    self.project_auto_start(project_id),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_project_auto_start(project_id, cx))),
            );
        let subprojects = div().flex().flex_col().gap(px(10.)).child(ui::label("Subprojects")).child(list);
        let wide = main_w >= 1700.;

        div()
            .id("overview")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div()
                    .max_w(px(if wide { 2200. } else { 1100. }))
                    .mx_auto()
                    .px(px(32.))
                    .py(px(28.))
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(px(16.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(ui::title(project.name.clone()))
                                    .child(ui::mono(format!("{} · {} · base {}", project.path, project.distro, project.base_branch))),
                            )
                            .child(
                                ui::button("project-settings", Kind::Secondary, Some(Icon::Sliders), "Project settings")
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_project_settings(project_id, window, cx))),
                            )
                            .child(
                                ui::button("project-zed", Kind::Secondary, Some(Icon::External), "Open in Zed")
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(path.clone(), cx))),
                            )
                            .child(
                                ui::button("project-new-sub", Kind::Primary, Some(Icon::Plus), "New subproject")
                                    .on_click(cx.listener(move |this, _, window, cx| this.open_sub_form(project_id, window, cx))),
                            ),
                    )
                    .child(self.render_project_chat(project_id, cx))
                    .child(widgets)
                    .child(if wide {
                        div().flex().gap(px(28.)).items_start().child(div().flex_1().min_w(px(0.)).child(subprojects)).child(div().flex_1().min_w(px(0.)).child(branches))
                    } else {
                        div().flex().flex_col().gap(px(28.)).child(subprojects).child(branches)
                    }),
            )
            .into_any_element()
    }

    // ---- Forms ---------------------------------------------------------------------------------------

    fn render_form(&self, form: &Form, main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        if let Form::NewProject(page) = form {
            return self.render_add_project(page, main_w, cx);
        }
        // A problem with one field shows under that field; anything else above the buttons.
        let (invalid, form_error) = match form {
            Form::NewSubproject { invalid, error, .. }
            | Form::StartPipeline { invalid, error, .. }
            | Form::EditPlan { invalid, error, .. } => (*invalid, error.clone()),
            Form::Settings { .. } | Form::Health | Form::Project(_) | Form::Guide | Form::Setup | Form::NewProject(_) => (None, None),
        };
        let problem = |label: &'static str| -> Option<SharedString> {
            (invalid == Some(label)).then(|| form_error.clone().unwrap_or_default().into())
        };
        let busy_steps: Option<Vec<ui::StepView>> = match form {
            Form::NewSubproject { project_id, kind, name, run_pipeline, busy: true, .. } => {
                let base = self.projects.iter().find(|p| p.id == *project_id).map(|p| p.base_branch.clone()).unwrap_or_default();
                let branch = format!("{}/{}", kind.as_str(), crate::git::slug(name.read(cx).text()));
                let step = |label: String, state| ui::StepView { label: label.into(), detail: None, state, progress: None };
                Some(vec![
                    step(format!("Make the branch {branch} off {base}"), ui::StepState::Running),
                    step(if *run_pipeline { "Start the research".into() } else { "Make a worktree for its first agent".into() }, ui::StepState::Waiting),
                ])
            }
            _ => None,
        };
        let (title, subtitle, body, busy, error, submit_label): (&str, &str, AnyElement, bool, Option<String>, &str) = match form {
            Form::NewProject(_) => return div().into_any_element(),
            Form::NewSubproject { kind, name, request, run_pipeline, auto_mode, busy, error, .. } => {
                let (kind, run, auto) = (*kind, *run_pipeline, *auto_mode);
                let choice = |id: &'static str, label: &'static str, value: SubKind, cx: &mut Context<Self>| {
                    ui::choice(id, label, kind == value).on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(Form::NewSubproject { kind, name, .. }) = &mut this.form {
                            *kind = value;
                            window.focus(&name.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                };
                let hint = match kind {
                    SubKind::Feature => "A goal like \"company car reservation\". It gets the branch feature/<name>, and its agents branch off it.",
                    SubKind::Standing => "A long-running concern like \"security\". It gets the branch standing/<name>.",
                };
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(div().flex().gap(px(8.)).child(choice("kind-feature", "Feature", SubKind::Feature, cx)).child(choice("kind-standing", "Standing", SubKind::Standing, cx)))
                    .child(ui::field_checked("Name", Some(hint), name.clone(), problem("Name"), Some(cx)))
                    .child(form_switch("toggle-run-pipeline", "Run the pipeline: research, verify, plan, build", run, cx, |form| {
                        if let Form::NewSubproject { run_pipeline, .. } = form {
                            *run_pipeline = !*run_pipeline;
                        }
                    }))
                    .when(run, |d| {
                        d.child(ui::field("What should be built?", Some("The researcher starts from this. Leave empty to use the name."), request.clone()))
                            .child(form_switch(
                                "toggle-auto-mode",
                                "Auto mode: questions get the recommended answer, and confident small plans start by themselves",
                                auto,
                                cx,
                                |form| {
                                    if let Form::NewSubproject { auto_mode, .. } = form {
                                        *auto_mode = !*auto_mode;
                                    }
                                },
                            ))
                    })
                    .into_any_element();
                let label = match (*busy, run) {
                    (true, _) => "Creating…",
                    (false, true) => "Create and start the research",
                    (false, false) => "Create and start an agent",
                };
                ("New subproject", "A feature or a standing concern, with its own branch.", body, *busy, error.clone(), label)
            }
            Form::StartPipeline { request, auto_mode, error, .. } => {
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(18.))
                    .child(ui::field_checked(
                        "What should be built?",
                        Some("A researcher starts from this, then two verifiers, a planner and the builders."),
                        request.clone(),
                        problem("What should be built?"),
                        Some(cx),
                    ))
                    .child(form_switch(
                        "toggle-auto-mode",
                        "Auto mode: questions get the recommended answer, and confident small plans start by themselves",
                        *auto_mode,
                        cx,
                        |form| {
                            if let Form::StartPipeline { auto_mode, .. } = form {
                                *auto_mode = !*auto_mode;
                            }
                        },
                    ))
                    .into_any_element();
                ("Run the pipeline", "Research, two verifiers, a plan, then builders in parallel.", body, false, error.clone(), "Start the research")
            }
            Form::EditPlan { title, summary, rows, error, .. } => {
                let body = self.render_plan_editor(title, summary, rows, problem("Workstreams").map(|s| s.to_string()), cx);
                ("Edit the plan", "Change what the builders do before you approve it.", body, false, error.clone(), "Save plan")
            }
            Form::Settings { .. } | Form::Health | Form::Project(_) | Form::Guide | Form::Setup => return div().into_any_element(),
        };

        div()
            .id("form")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div().flex().justify_center().px(px(32.)).py(px(48.)).child(
                    div()
                        .w_full()
                        .max_w(px(600.))
                        .flex()
                        .flex_col()
                        .gap(px(24.))
                        .child(div().flex().flex_col().gap(px(4.)).child(ui::title(title)).child(div().text_color(rgb(theme::text_3())).child(subtitle)))
                        .child(body)
                        .children(busy_steps.map(|steps| {
                            div()
                                .p(px(16.))
                                .rounded(px(10.))
                                .border_1()
                                .border_color(rgb(theme::line()))
                                .bg(rgb(theme::panel()))
                                .child(ui::steps("form-steps", &steps))
                        }))
                        .children(error.filter(|_| invalid.is_none()).map(|e| {
                            let copy = e.clone();
                            ui::callout(Tone::Error, e).child(
                                ui::button("form-copy-error", Kind::Ghost, Some(Icon::Copy), "Copy").on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                    this.toast(Tone::Info, "Copied to the clipboard", cx);
                                })),
                            )
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .pt(px(4.))
                                .child(if busy {
                                    ui::busy_button("form-submit", Kind::Primary, submit_label)
                                } else {
                                    ui::button("form-submit", Kind::Primary, None, submit_label).on_click(cx.listener(|this, _, _, cx| this.submit_form(cx)))
                                })
                                .child(ui::button("form-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, window, cx| this.on_cancel(&crate::Cancel, window, cx))))
                                .child(div().flex_1())
                                .child(div().flex().items_center().gap(px(6.)).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(ui::kbd("Enter")).child("submit").child(ui::kbd("Esc")).child("cancel")),
                        ),
                ),
            )
            .into_any_element()
    }
}

/// A switch that flips a field of the open form.
fn form_switch(id: &'static str, label: &'static str, on: bool, cx: &mut Context<Monaged>, flip: impl Fn(&mut Form) + 'static) -> gpui::Stateful<gpui::Div> {
    ui::switch(id, label, on).on_click(cx.listener(move |this, _, _, cx| {
        if let Some(form) = &mut this.form {
            flip(form);
        }
        cx.notify();
    }))
}

