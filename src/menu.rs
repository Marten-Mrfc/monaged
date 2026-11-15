//! Context menus: right-click a row in the tree, or press the ⋯ button on a
//! row or an agent's header. Every action here is also somewhere visible
//! (or in Ctrl K); the menu just puts it where your pointer already is.

use gpui::{
    AnyElement, ClipboardItem, Context, FocusHandle, IntoElement, MouseButton, Pixels, Point, SharedString, Window, div,
    prelude::*, px, rgb,
};

use crate::agent::{AgentTab, Tone};
use crate::icons::{Icon, icon_sized};
use crate::shell::InspectorTab;
use crate::{Monaged, theme, ui};

const MENU_WIDTH: f32 = 248.;
/// Settings menus list choices with a description beside them.
const WIDE_MENU_WIDTH: f32 = 380.;
const ITEM_HEIGHT: f32 = 28.;
const SEPARATOR_HEIGHT: f32 = 9.;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum MenuTarget {
    Project(i64),
    Sub(i64),
    Agent(i64),
    /// One of an agent's settings: (agent, index into its config options).
    Config(i64, usize),
}

pub(crate) struct MenuState {
    pub target: MenuTarget,
    pub position: Point<Pixels>,
    pub selected: Option<usize>,
    pub focus: FocusHandle,
}

#[derive(Clone)]
pub(crate) enum MenuAction {
    OpenProject(i64),
    Terminal(String),
    CopyConversation(i64),
    Export(i64),
    NewSub(i64),
    ToggleCollapse(i64),
    OpenZed(String),
    Copy(String, &'static str),
    OpenSub(i64),
    NewAgent(i64),
    RunPipeline(i64),
    Inspector(i64, InspectorTab),
    Cleanup(i64),
    OpenAgent(i64),
    OpenBeside(i64),
    ShowChanges(i64),
    Review(i64),
    Stop(i64),
    Restart(i64),
    /// (agent, option id, value)
    SetConfig(i64, String, String),
    Rename(i64),
    Archive(i64),
    Restore(i64),
    SubChanges(i64),
    Replay(i64),
    UndoLastTurn(i64),
    SubCap(i64),
    ProjectSettings(i64),
    SideChat(i64),
}

pub(crate) enum Entry {
    Item { label: String, glyph: Option<Icon>, hint: Option<&'static str>, detail: Option<String>, danger: bool, enabled: bool, action: MenuAction },
    /// A small heading over a group of items.
    Heading(String),
    Separator,
}

fn item(label: impl Into<String>, glyph: Icon, action: MenuAction) -> Entry {
    Entry::Item { label: label.into(), glyph: Some(glyph), hint: None, detail: None, danger: false, enabled: true, action }
}

impl Entry {
    fn hint(mut self, text: &'static str) -> Self {
        if let Entry::Item { hint, .. } = &mut self
            && !text.is_empty()
        {
            *hint = Some(text);
        }
        self
    }

    fn danger(mut self) -> Self {
        if let Entry::Item { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }

    fn enabled(mut self, on: bool) -> Self {
        if let Entry::Item { enabled, .. } = &mut self {
            *enabled = on;
        }
        self
    }

    fn usable(&self) -> bool {
        matches!(self, Entry::Item { enabled: true, .. })
    }
}

impl Monaged {
    pub(crate) fn open_menu(&mut self, target: MenuTarget, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.menu = Some(MenuState { target, position, selected: None, focus });
        cx.notify();
    }

    pub(crate) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            window.focus(&self.focus);
            self.focus_request = self.focus_target();
            cx.notify();
        }
    }

    pub(crate) fn menu_entries(&self, target: MenuTarget) -> Vec<Entry> {
        match target {
            MenuTarget::Project(pid) => {
                let Some(project) = self.projects.iter().find(|p| p.id == pid) else { return Vec::new() };
                let collapsed = self.collapsed.contains(&pid);
                vec![
                    item("Open overview", Icon::Folder, MenuAction::OpenProject(pid)),
                    item("New subproject", Icon::Plus, MenuAction::NewSub(pid)),
                    item("Project settings", Icon::Sliders, MenuAction::ProjectSettings(pid)),
                    item("Export for analysis", Icon::ArrowDown, MenuAction::Export(pid)),
                    Entry::Separator,
                    item("Open in Zed", Icon::External, MenuAction::OpenZed(project.path.clone())),
                    item("Open a terminal here", Icon::Terminal, MenuAction::Terminal(project.path.clone())),
                    item("Copy path", Icon::Copy, MenuAction::Copy(project.path.clone(), "path")),
                    Entry::Separator,
                    item(if collapsed { "Expand" } else { "Collapse" }, if collapsed { Icon::ChevronDown } else { Icon::ChevronRight }, MenuAction::ToggleCollapse(pid)),
                ]
            }
            MenuTarget::Sub(sid) => {
                let Some(sub) = self.subprojects.iter().find(|s| s.id == sid) else { return Vec::new() };
                let path = self.projects.iter().find(|p| p.id == sub.project_id).map(|p| p.path.clone()).unwrap_or_default();
                let running = self.pipelines.get(&sid).is_some_and(|p| !matches!(p.stage, crate::pipeline::Stage::Done | crate::pipeline::Stage::Stopped));
                let idle_agents = self.agents.iter().any(|a| a.record.subproject_id == sid && !a.record.archived && !a.is_working());
                vec![
                    item("Open", Icon::Layers, MenuAction::OpenSub(sid)),
                    item("All changes", Icon::Diff, MenuAction::SubChanges(sid)),
                    item("New agent", Icon::Plus, MenuAction::NewAgent(sid)).hint(crate::keys::label("new-agent")),
                    item(if running { "Pipeline running" } else { "Run the pipeline" }, Icon::Play, MenuAction::RunPipeline(sid)).enabled(!running),
                    Entry::Separator,
                    item("Pipeline", Icon::Flow, MenuAction::Inspector(sid, InspectorTab::Pipeline)),
                    item("Board", Icon::Board, MenuAction::Inspector(sid, InspectorTab::Board)),
                    item("Merge", Icon::Merge, MenuAction::Inspector(sid, InspectorTab::Merge)),
                    Entry::Separator,
                    item("Open the repo in Zed", Icon::External, MenuAction::OpenZed(path.clone())),
                    item("Open a terminal in the repo", Icon::Terminal, MenuAction::Terminal(path)),
                    item("Copy branch name", Icon::Copy, MenuAction::Copy(sub.branch.clone(), "branch name")),
                    item(
                        match self.sub_cap(sid) {
                            Some(cap) => format!("Spending cap: ${cap:.0}/week…"),
                            None => "Set a spending cap…".to_string(),
                        },
                        Icon::Gauge,
                        MenuAction::SubCap(sid),
                    ),
                    Entry::Separator,
                    item("Clean up worktrees…", Icon::Trash, MenuAction::Cleanup(sid)).danger().enabled(idle_agents),
                ]
            }
            MenuTarget::Agent(aid) => {
                let Some(agent) = self.agent(aid) else { return Vec::new() };
                let working = agent.is_working();
                let shown = self.sel_agent == Some(aid) || self.pinned.contains(&aid);
                let failed = matches!(agent.status, crate::agent::Status::Failed(_));
                vec![
                    item("Open", Icon::Agent, MenuAction::OpenAgent(aid)),
                    item("Open beside", Icon::PanelRight, MenuAction::OpenBeside(aid)).hint("Ctrl-click").enabled(!shown),
                    item("Show changes", Icon::Diff, MenuAction::ShowChanges(aid)),
                    item("Replay turn by turn", Icon::Clock, MenuAction::Replay(aid)),
                    item("Side chat with Haiku", Icon::Ask, MenuAction::SideChat(aid)).hint(crate::keys::label("side-chat")),
                    item("Undo the last turn's changes…", Icon::Undo, MenuAction::UndoLastTurn(aid)).enabled(!working && self.checkpoints.contains_key(&(aid, agent.items.iter().filter(|i| matches!(i, crate::agent::Item::User(_))).count() as i64 - 1))),
                    item("Review", Icon::Eye, MenuAction::Review(aid)).enabled(!agent.reviewing),
                    Entry::Separator,
                    item("Open the worktree in Zed", Icon::External, MenuAction::OpenZed(agent.record.worktree.clone())),
                    item("Open a terminal in the worktree", Icon::Terminal, MenuAction::Terminal(agent.record.worktree.clone())),
                    item("Copy the conversation", Icon::Copy, MenuAction::CopyConversation(aid)).enabled(!agent.items.is_empty()),
                    item("Copy worktree path", Icon::Copy, MenuAction::Copy(agent.record.worktree.clone(), "worktree path")),
                    item("Copy branch name", Icon::Branch, MenuAction::Copy(agent.record.branch.clone(), "branch name")),
                    Entry::Separator,
                    item("Stop the turn", Icon::Stop, MenuAction::Stop(aid)).hint("Esc").enabled(working),
                    item(if failed { "Retry the session" } else { "Restart the session" }, Icon::Refresh, MenuAction::Restart(aid)).enabled(!working),
                    Entry::Separator,
                    item("Rename…", Icon::Edit, MenuAction::Rename(aid)),
                    if agent.record.archived {
                        item("Restore", Icon::Archive, MenuAction::Restore(aid))
                    } else {
                        item("Archive", Icon::Archive, MenuAction::Archive(aid)).enabled(!working)
                    },
                ]
            }
            MenuTarget::Config(aid, index) => {
                let Some(option) = self.agent(aid).and_then(|a| a.config.get(index)) else { return Vec::new() };
                let mut out = vec![Entry::Heading(option.name.clone())];
                out.extend(option.choices.iter().map(|choice| Entry::Item {
                    label: choice.name.clone(),
                    glyph: (choice.value == option.current).then_some(Icon::Check),
                    hint: None,
                    detail: (!choice.description.is_empty()).then(|| choice.description.clone()),
                    danger: choice.value == "bypassPermissions",
                    enabled: true,
                    action: MenuAction::SetConfig(aid, option.id.clone(), choice.value.clone()),
                }));
                out
            }
        }
    }

    fn run_menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(window, cx);
        match action {
            MenuAction::OpenProject(pid) => self.select_project(pid, cx),
            MenuAction::NewSub(pid) => self.open_sub_form(pid, window, cx),
            MenuAction::ToggleCollapse(pid) => {
                if !self.collapsed.remove(&pid) {
                    self.collapsed.insert(pid);
                }
            }
            MenuAction::OpenZed(path) => self.open_in_zed(path, cx),
            MenuAction::Terminal(path) => self.open_terminal(&path, cx),
            MenuAction::CopyConversation(aid) => self.copy_conversation(aid, cx),
            MenuAction::Copy(text, what) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.toast(Tone::Info, format!("Copied the {what}"), cx);
            }
            MenuAction::OpenSub(sid) => {
                self.form = None;
                self.select_sub(sid, cx);
            }
            MenuAction::NewAgent(sid) => {
                if self.sel_sub != Some(sid) {
                    self.select_sub(sid, cx);
                }
                self.add_agent(sid, cx);
            }
            MenuAction::RunPipeline(sid) => {
                if self.sel_sub != Some(sid) {
                    self.select_sub(sid, cx);
                }
                self.open_pipeline_form(sid, window, cx);
            }
            MenuAction::Inspector(sid, tab) => self.show_inspector(sid, tab, cx),
            MenuAction::Cleanup(sid) => self.ask_confirm(crate::feedback::ConfirmAction::CleanupWorktrees(sid), window, cx),
            MenuAction::OpenAgent(aid) => self.open_agent(aid, cx),
            MenuAction::OpenBeside(aid) => {
                self.open_agent_sub(aid, cx);
                self.pin_beside(aid, cx);
            }
            MenuAction::ShowChanges(aid) => {
                self.open_agent(aid, cx);
                self.set_tab(aid, AgentTab::Changes, cx);
            }
            MenuAction::Review(aid) => self.review_agent(aid, cx),
            MenuAction::Stop(aid) => {
                self.sel_agent = Some(aid);
                self.focus_request = Some(aid);
                self.stop_agent(aid, cx);
            }
            MenuAction::Restart(aid) => self.retry_session(aid, cx),
            MenuAction::SetConfig(aid, option, value) => self.set_config(aid, &option, &value, cx),
            MenuAction::Rename(aid) => self.start_rename(aid, window, cx),
            MenuAction::Archive(aid) => self.archive_agent(aid, cx),
            MenuAction::Restore(aid) => self.restore_agent(aid, cx),
            MenuAction::SubChanges(sid) => self.open_sub_changes(sid, cx),
            MenuAction::SubCap(sid) => self.ask_sub_cap(sid, window, cx),
            MenuAction::ProjectSettings(pid) => self.open_project_settings(pid, window, cx),
            MenuAction::Export(pid) => self.export(crate::export::Scope::Project(pid), cx),
            MenuAction::SideChat(aid) => {
                self.open_agent(aid, cx);
                self.toggle_side_chat(aid, window, cx);
            }
            MenuAction::Replay(aid) => {
                self.open_agent(aid, cx);
                self.open_replay(aid, window, cx);
            }
            MenuAction::UndoLastTurn(aid) => {
                let last = self.agent(aid).map(|a| a.items.iter().filter(|i| matches!(i, crate::agent::Item::User(_))).count() as i64).unwrap_or(0);
                self.ask_confirm(crate::feedback::ConfirmAction::Rollback(aid, last), window, cx);
            }
        }
        cx.notify();
    }

    /// Shows an agent wherever it lives (switching subproject if needed).
    pub(crate) fn open_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        self.open_agent_sub(agent_id, cx);
        self.select_agent(agent_id, cx);
    }

    fn open_agent_sub(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(sub_id) = self.agent(agent_id).map(|a| a.record.subproject_id) else { return };
        self.form = None;
        if self.sel_sub != Some(sub_id) {
            self.select_sub(sub_id, cx);
        }
    }

    pub(crate) fn menu_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else { return };
        let entries = self.menu_entries(menu.target);
        let usable: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| e.usable()).map(|(i, _)| i).collect();
        if usable.is_empty() {
            return;
        }
        let at = menu.selected.and_then(|s| usable.iter().position(|&i| i == s));
        let next = match at {
            None if delta > 0 => 0,
            None => usable.len() - 1,
            Some(at) => (at as isize + delta).rem_euclid(usable.len() as isize) as usize,
        };
        if let Some(menu) = &mut self.menu {
            menu.selected = Some(usable[next]);
        }
        cx.notify();
    }

    pub(crate) fn run_menu_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &self.menu else { return };
        let Some(selected) = menu.selected else { return };
        let action = match self.menu_entries(menu.target).into_iter().nth(selected) {
            Some(Entry::Item { action, enabled: true, .. }) => action,
            _ => return,
        };
        self.run_menu_action(action, window, cx);
    }

    pub(crate) fn render_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let entries = self.menu_entries(menu.target);
        let width = if matches!(menu.target, MenuTarget::Config(..)) { WIDE_MENU_WIDTH } else { MENU_WIDTH };
        let height: f32 = 8. + entries.iter().map(|e| if matches!(e, Entry::Separator) { SEPARATOR_HEIGHT } else { ITEM_HEIGHT }).sum::<f32>();
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let (x, y) = (f32::from(menu.position.x), f32::from(menu.position.y));
        // Keep the whole menu on screen: flip left or up near the edges.
        let left = if x + width > vw - 8. { (x - width).max(8.) } else { x };
        let top = if y + height > vh - 8. { (y - height).max(8.) } else { y };
        let selected = menu.selected;
        let rows = entries.into_iter().enumerate().map(|(i, entry)| match entry {
            Entry::Separator => div().h(px(SEPARATOR_HEIGHT)).flex().items_center().child(div().w_full().h(px(1.)).bg(rgb(theme::line()))).into_any_element(),
            Entry::Heading(text) => div().h(px(ITEM_HEIGHT)).flex().items_center().px(px(8.)).child(ui::label(text)).into_any_element(),
            Entry::Item { label, glyph, hint, detail, danger, enabled, action } => {
                let on = selected == Some(i);
                let color = match (enabled, danger) {
                    (false, _) => theme::text_4(),
                    (true, true) => theme::red_text(),
                    (true, false) => theme::text(),
                };
                div()
                    .id(("menu-item", i))
                    .h(px(ITEM_HEIGHT))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .px(px(8.))
                    .rounded(px(5.))
                    .text_size(px(13.))
                    .text_color(rgb(color))
                    .when(on && enabled, |d| d.bg(rgb(theme::selected())))
                    .when(enabled, |d| {
                        d.cursor_pointer().hover(|s| s.bg(rgb(theme::hover()))).on_click(cx.listener(move |this, _, window, cx| {
                            this.run_menu_action(action.clone(), window, cx)
                        }))
                    })
                    .child(div().size(px(14.)).flex_none().children(glyph.map(|glyph| {
                        icon_sized(glyph, 14.).text_color(rgb(match (enabled, danger) {
                            (false, _) => theme::text_4(),
                            (true, true) => theme::red_text(),
                            (true, false) => theme::text_3(),
                        }))
                    })))
                    .child(div().flex_none().max_w(px(180.)).truncate().child(label))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(11.5)).text_color(rgb(theme::text_3())).children(detail))
                    .children(hint.map(|h| div().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(theme::text_3())).child(h)))
                    .into_any_element()
            }
        });
        let panel = div()
            .id("menu")
            .track_focus(&menu.focus)
            .key_context("Menu")
            .occlude()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(width))
            .p(px(4.))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::raised()))
            .shadow_lg()
            .children(rows);
        Some(
            div()
                .id("menu-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
                .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
                .child(ui::fade_in(SharedString::from("menu-in"), panel))
                .into_any_element(),
        )
    }

    /// Stops an agent's turn (the Esc key, the stop buttons and the menu).
    pub(crate) fn stop_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        if !agent.is_working() {
            return;
        }
        let session_id = agent.record.session_id.clone();
        // ACP: open permission requests must be answered with "cancelled".
        let open: Vec<usize> = agent
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| matches!(item, crate::agent::Item::Permission { chosen: None, .. }).then_some(i))
            .collect();
        if let Some(session_id) = session_id {
            self.notify_agent("session/cancel", serde_json::json!({ "sessionId": session_id }));
        }
        for index in open {
            self.choose_permission(agent_id, index, None, cx);
        }
        let forms: Vec<usize> = self
            .agent(agent_id)
            .map(|a| {
                a.items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, item)| matches!(item, crate::agent::Item::Elicit(f) if f.state == crate::agent::FormState::Open).then_some(i))
                    .collect()
            })
            .unwrap_or_default();
        for index in forms {
            self.answer_form(agent_id, index, "cancel", cx);
        }
        cx.notify();
    }
}
