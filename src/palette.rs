//! The command palette (Ctrl K), after Raycast: type to jump to any project,
//! subproject or agent, or to run an action. Arrows move, Enter runs, Esc closes.

use gpui::{AnyElement, Context, Entity, Focusable, FontWeight, Subscription, Window, div, prelude::*, px, relative, rgb, rgba};

use crate::icons::{Icon, icon};
use crate::shell::{InspectorTab, agent_state};
use crate::text_input::TextInput;
use crate::ui::{self, State};
use crate::{Monaged, PaletteDown, PaletteUp, theme};

pub(crate) struct PaletteState {
    pub input: Entity<TextInput>,
    pub selected: usize,
    pub mode: PaletteMode,
    _changed: Subscription,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PaletteMode {
    /// Everything: actions, places, files, conversations.
    All,
    /// Only what was said in conversations (Ctrl Shift F).
    Conversations,
}

/// Groups in the order the palette lists them.
fn group_rank(group: &str) -> u8 {
    match group {
        "Actions" => 0,
        "Go to" => 1,
        "Files" => 2,
        "In conversations" => 3,
        "Layouts" => 4,
        "Theme" => 5,
        _ => 6,
    }
}

#[derive(Clone)]
pub(crate) enum Command {
    Project(i64),
    Sub(i64),
    Agent(i64),
    NewProject,
    NewSub(i64),
    NewAgent(i64),
    RunPipeline(i64),
    OpenInZed(String),
    Push(i64),
    Review(i64),
    Inspector(InspectorTab),
    Theme(&'static str),
    Settings,
    ToggleSidebar,
    Reconnect,
    Shortcuts,
    OpenFile(String),
    /// (agent, conversation item)
    Conversation(i64, usize),
    Layout(i64, String),
    SaveLayout(i64),
    DeleteLayout(i64, String),
    Zen,
    ToggleSounds,
    OnePane(bool),
    ChangesBeside(bool),
    Dashboard(i64),
    Export(crate::export::Scope),
    StartLead(i64),
    Density(bool),
    Health,
    SubChanges(i64),
    CheckUpdates,
    ProjectSettings(i64),
    CreatePlayground,
    Guide,
    Setup,
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub group: &'static str,
    pub glyph: Icon,
    pub title: String,
    pub detail: String,
    pub state: Option<State>,
    pub hint: Option<&'static str>,
    pub command: Command,
}

/// Every query character appears in order; earlier and tighter matches score higher.
fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text = text.to_lowercase();
    if text.starts_with(query) {
        return Some(300);
    }
    if let Some(at) = text.find(query) {
        return Some(200 - at as i32);
    }
    // Letters in order, but close together: "rpip" finds "Run pipeline", "inte" doesn't find "Coordination test".
    let mut chars = text.char_indices();
    let mut first = None;
    let mut last = 0;
    for q in query.chars() {
        let (i, _) = chars.find(|(_, c)| *c == q)?;
        first.get_or_insert(i);
        last = i;
    }
    let span = last - first.unwrap_or(0) + 1;
    (span <= query.len() * 3).then(|| 100 - span as i32)
}

/// The detail line only matches as a plain substring.
fn score_detail(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    text.to_lowercase().find(query).map(|at| 60 - at.min(50) as i32)
}

impl Monaged {
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_with(PaletteMode::All, window, cx);
    }

    pub(crate) fn open_palette_with(&mut self, mode: PaletteMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.as_ref().is_some_and(|p| p.mode == mode) {
            self.close_palette(window, cx);
            return;
        }
        self.refresh_file_index(cx);
        let placeholder = match mode {
            PaletteMode::All => "Search projects, agents, files, conversations and actions",
            PaletteMode::Conversations => "Search what was said in every conversation",
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx).bare());
        let changed = cx.observe(&input, |this, _, cx| {
            if let Some(p) = &mut this.palette {
                p.selected = 0;
            }
            cx.notify();
        });
        window.focus(&input.focus_handle(cx));
        self.palette = Some(PaletteState { input, selected: 0, mode, _changed: changed });
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        if let Some(id) = self.sel_agent {
            self.focus_request = Some(id);
        } else {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let sub = self.sel_sub.and_then(|id| self.subprojects.iter().find(|s| s.id == id));
        // Actions for where you are, first.
        if let Some(sub) = sub {
            let running = self.pipelines.get(&sub.id).is_some_and(|p| !matches!(p.stage, crate::pipeline::Stage::Done | crate::pipeline::Stage::Stopped));
            if !running {
                out.push(Entry { group: "Actions", glyph: Icon::Play, title: "Run pipeline".into(), detail: sub.name.clone(), state: None, hint: None, command: Command::RunPipeline(sub.id) });
            }
            out.push(Entry { group: "Actions", glyph: Icon::Plus, title: "New agent".into(), detail: sub.name.clone(), state: None, hint: None, command: Command::NewAgent(sub.id) });
            out.push(Entry { group: "Actions", glyph: Icon::Diff, title: "All changes".into(), detail: sub.name.clone(), state: None, hint: None, command: Command::SubChanges(sub.id) });
            out.push(Entry { group: "Actions", glyph: Icon::Push, title: "Push branch".into(), detail: sub.branch.clone(), state: None, hint: None, command: Command::Push(sub.id) });
            out.push(Entry { group: "Actions", glyph: Icon::Flow, title: "Show pipeline".into(), detail: "Inspector".into(), state: None, hint: None, command: Command::Inspector(InspectorTab::Pipeline) });
            out.push(Entry { group: "Actions", glyph: Icon::Board, title: "Show board".into(), detail: "Inspector".into(), state: None, hint: None, command: Command::Inspector(InspectorTab::Board) });
            out.push(Entry { group: "Actions", glyph: Icon::Merge, title: "Show merge".into(), detail: "Inspector".into(), state: None, hint: None, command: Command::Inspector(InspectorTab::Merge) });
            out.push(Entry { group: "Actions", glyph: Icon::Play, title: "Run the app or its tests".into(), detail: "Inspector".into(), state: None, hint: None, command: Command::Inspector(InspectorTab::Run) });
            out.push(Entry { group: "Actions", glyph: Icon::Branch, title: "Show git".into(), detail: "Inspector".into(), state: None, hint: None, command: Command::Inspector(InspectorTab::Git) });
        }
        if let Some(agent) = self.sel_agent.and_then(|id| self.agent(id)) {
            out.push(Entry { group: "Actions", glyph: Icon::Eye, title: "Review".into(), detail: agent.record.name.clone(), state: None, hint: None, command: Command::Review(agent.record.id) });
            out.push(Entry { group: "Actions", glyph: Icon::External, title: "Open worktree in Zed".into(), detail: agent.record.name.clone(), state: None, hint: None, command: Command::OpenInZed(agent.record.worktree.clone()) });
        }
        if let Some(project) = self.sel_project.and_then(|id| self.projects.iter().find(|p| p.id == id)) {
            out.push(Entry { group: "Actions", glyph: Icon::Layers, title: "New subproject".into(), detail: project.name.clone(), state: None, hint: None, command: Command::NewSub(project.id) });
            out.push(Entry { group: "Actions", glyph: Icon::Sliders, title: "Project settings".into(), detail: "MCP servers, knowledge, run commands".into(), state: None, hint: None, command: Command::ProjectSettings(project.id) });
        }
        out.push(Entry { group: "Actions", glyph: Icon::Folder, title: "Add a project".into(), detail: String::new(), state: None, hint: None, command: Command::NewProject });
        if matches!(self.conn, crate::Conn::Failed(_)) || self.logged_in == Some(false) {
            out.push(Entry { group: "Actions", glyph: Icon::Refresh, title: "Reconnect to Claude".into(), detail: String::new(), state: None, hint: None, command: Command::Reconnect });
        }
        // Go to: agents (busiest first), subprojects, projects.
        let mut agents: Vec<_> = self.agents.iter().filter(|a| !a.record.archived).collect();
        agents.sort_by_key(|a| (agent_state(a) != State::NeedsYou, !a.is_working(), std::cmp::Reverse(a.record.id)));
        for a in agents {
            let sub_name = self.subprojects.iter().find(|s| s.id == a.record.subproject_id).map(|s| s.name.clone()).unwrap_or_default();
            out.push(Entry { group: "Go to", glyph: Icon::Agent, title: a.record.name.clone(), detail: sub_name, state: Some(agent_state(a)), hint: None, command: Command::Agent(a.record.id) });
        }
        for s in &self.subprojects {
            let project = self.projects.iter().find(|p| p.id == s.project_id).map(|p| p.name.clone()).unwrap_or_default();
            out.push(Entry { group: "Go to", glyph: Icon::Layers, title: s.name.clone(), detail: project, state: None, hint: None, command: Command::Sub(s.id) });
        }
        for p in &self.projects {
            out.push(Entry { group: "Go to", glyph: Icon::Folder, title: p.name.clone(), detail: p.path.clone(), state: None, hint: None, command: Command::Project(p.id) });
        }
        // Appearance and app.
        for t in theme::THEMES {
            let active = self.theme_key == t.key;
            out.push(Entry {
                group: "Theme",
                glyph: if active { Icon::Check } else { Icon::Sliders },
                title: format!("Theme: {}", t.name),
                detail: t.blurb.into(),
                state: None,
                hint: None,
                command: Command::Theme(t.key),
            });
        }
        out.push(Entry { group: "Theme", glyph: Icon::Sliders, title: "Theme: follow Windows".into(), detail: "Carbon in dark mode, Paper in light".into(), state: None, hint: None, command: Command::Theme("system") });
        out.push(Entry { group: "App", glyph: Icon::Sidebar, title: "Toggle sidebar".into(), detail: String::new(), state: None, hint: crate::keys::hint("sidebar"), command: Command::ToggleSidebar });
        out.push(Entry { group: "App", glyph: Icon::Sliders, title: "Settings".into(), detail: String::new(), state: None, hint: crate::keys::hint("settings"), command: Command::Settings });
        out.push(Entry { group: "App", glyph: Icon::Keyboard, title: "Keyboard shortcuts".into(), detail: String::new(), state: None, hint: crate::keys::hint("shortcuts"), command: Command::Shortcuts });
        out.push(Entry { group: "App", glyph: Icon::Eye, title: if self.zen { "Leave zen mode".into() } else { "Zen mode".into() }, detail: "One conversation, nothing else".into(), state: None, hint: crate::keys::hint("zen"), command: Command::Zen });
        if let Some(project) = self.sel_project.and_then(|id| self.projects.iter().find(|p| p.id == id)) {
            out.push(Entry {
                group: "App",
                glyph: Icon::ArrowDown,
                title: format!("Export {} for analysis", project.name),
                detail: "A report to read or paste into a chat, and a zip with every conversation, the log and a screenshot (secrets masked)".into(),
                state: None,
                hint: None,
                command: Command::Export(crate::export::Scope::Project(project.id)),
            });
        }
        out.push(Entry {
            group: "App",
            glyph: Icon::ArrowDown,
            title: "Export everything for analysis".into(),
            detail: "Every project: a report and a zip, secrets masked".into(),
            state: None,
            hint: None,
            command: Command::Export(crate::export::Scope::Everything),
        });
        if let Some(sub_id) = self.sel_sub {
            out.push(Entry {
                group: "App",
                glyph: Icon::Board,
                title: "Open the dashboard".into(),
                detail: "This subproject: what waits for you, the agents and the lead".into(),
                state: None,
                hint: None,
                command: Command::Dashboard(sub_id),
            });
            if self.lead_of(sub_id).is_none() {
                out.push(Entry {
                    group: "App",
                    glyph: Icon::Flow,
                    title: "Start a lead for this subproject".into(),
                    detail: "An agent that oversees the others; you talk to it on the dashboard".into(),
                    state: None,
                    hint: None,
                    command: Command::StartLead(sub_id),
                });
            }
        }
        out.push(Entry {
            group: "App",
            glyph: if self.one_pane { Icon::Restore } else { Icon::Maximize },
            title: if self.one_pane { "Show agents side by side".into() } else { "Show one agent at a time".into() },
            detail: if self.one_pane {
                "Agents that work or need you get a pane next to the one you picked".into()
            } else {
                "Only the agent you picked, without others beside it".into()
            },
            state: None,
            hint: None,
            command: Command::OnePane(!self.one_pane),
        });
        out.push(Entry {
            group: "App",
            glyph: Icon::Diff,
            title: if self.changes_beside { "Don't show changes beside the agent".into() } else { "Show changes beside the agent".into() },
            detail: "A pane with the agent's diff next to its conversation, when there's room (its Changes tab has them either way)".into(),
            state: None,
            hint: None,
            command: Command::ChangesBeside(!self.changes_beside),
        });
        let sounds_on = self.sound.settings.on;
        out.push(Entry {
            group: "App",
            glyph: if sounds_on { Icon::SoundOff } else { Icon::Sound },
            title: if sounds_on { "Mute sounds".into() } else { "Turn sounds on".into() },
            detail: "Monaged's sounds for agents that need you or are done (Settings → Sounds)".into(),
            state: None,
            hint: None,
            command: Command::ToggleSounds,
        });
        let compact = ui::compact();
        out.push(Entry { group: "App", glyph: Icon::Lines, title: if compact { "Comfortable density".into() } else { "Compact density".into() }, detail: String::new(), state: None, hint: None, command: Command::Density(!compact) });
        out.push(Entry { group: "App", glyph: Icon::ArrowDown, title: "Check for updates".into(), detail: format!("Monaged {}", crate::update::current()), state: None, hint: None, command: Command::CheckUpdates });
        out.push(Entry { group: "App", glyph: Icon::Gauge, title: "Health check".into(), detail: "WSL, the agent, git, logins, disk".into(), state: None, hint: None, command: Command::Health });
        out.push(Entry { group: "App", glyph: Icon::Flag, title: "Setup guide".into(), detail: "Install WSL, Ubuntu and the Claude agent, sign in".into(), state: None, hint: None, command: Command::Setup });
        let has_playground = self.projects.iter().any(|p| p.path.ends_with("/monaged-playground"));
        out.push(Entry {
            group: "App",
            glyph: Icon::Flow,
            title: if has_playground { "Open the playground".into() } else { "Create the playground project".into() },
            detail: "A small app to try every feature on".into(),
            state: None,
            hint: None,
            command: Command::CreatePlayground,
        });
        if has_playground {
            out.push(Entry { group: "App", glyph: Icon::Flow, title: "Playground guide".into(), detail: "The tour of every feature".into(), state: None, hint: None, command: Command::Guide });
        }
        // Saved layouts of the project you're in.
        if let Some(project_id) = self.sel_project {
            for layout in self.layouts(project_id) {
                out.push(Entry { group: "Layouts", glyph: Icon::Sidebar, title: format!("Layout: {}", layout.name), detail: String::new(), state: None, hint: None, command: Command::Layout(project_id, layout.name.clone()) });
            }
            out.push(Entry { group: "Layouts", glyph: Icon::Plus, title: "Save this layout…".into(), detail: "Panes, widths and the inspector".into(), state: None, hint: None, command: Command::SaveLayout(project_id) });
            for layout in self.layouts(project_id) {
                out.push(Entry { group: "Layouts", glyph: Icon::Trash, title: format!("Delete layout: {}", layout.name), detail: String::new(), state: None, hint: None, command: Command::DeleteLayout(project_id, layout.name.clone()) });
            }
        }
        out
    }

    /// The entries matching what's typed, best first within each group.
    fn filtered(&self, cx: &Context<Self>) -> Vec<Entry> {
        let Some(p) = &self.palette else { return Vec::new() };
        let raw = p.input.read(cx).text().to_string();
        let query = raw.trim().to_lowercase();
        let mut scored: Vec<(usize, i32, Entry)> = Vec::new();
        if p.mode == PaletteMode::All {
            scored = self
                .entries()
                .into_iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let best = score(&query, &e.title).max(score_detail(&query, &e.detail));
                    best.map(|s| (i, s, e))
                })
                .collect();
            for (i, (path, full)) in self.search_files(&query, 8).into_iter().enumerate() {
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let folder = path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
                scored.push((10_000 + i, 500 - i as i32, Entry { group: "Files", glyph: Icon::Lines, title: name, detail: folder, state: None, hint: None, command: Command::OpenFile(full) }));
            }
        }
        let limit = if p.mode == PaletteMode::Conversations { 40 } else { 8 };
        for (i, hit) in self.search_conversations(&raw, limit).into_iter().enumerate() {
            let agent = self.agent(hit.agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
            scored.push((20_000 + i, 400 - i as i32, Entry {
                group: "In conversations",
                glyph: Icon::Ask,
                title: hit.snippet,
                detail: agent,
                state: None,
                hint: None,
                command: Command::Conversation(hit.agent_id, hit.item),
            }));
        }
        // One header per group: sort by group, then by how well it matched.
        if query.is_empty() {
            scored.sort_by_key(|(i, _, e)| (group_rank(e.group), *i));
        } else {
            scored.sort_by_key(|(i, s, e)| (group_rank(e.group), std::cmp::Reverse(*s), *i));
        }
        scored.into_iter().map(|(_, _, e)| e).take(80).collect()
    }

    pub(crate) fn on_palette_up(&mut self, _: &PaletteUp, _window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.menu_move(-1, cx);
            return;
        }
        if let Some(p) = &mut self.palette {
            p.selected = p.selected.saturating_sub(1);
            cx.notify();
        }
    }

    pub(crate) fn on_palette_down(&mut self, _: &PaletteDown, _window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.menu_move(1, cx);
            return;
        }
        let count = self.filtered(cx).len();
        if let Some(p) = &mut self.palette {
            p.selected = (p.selected + 1).min(count.saturating_sub(1));
            cx.notify();
        }
    }

    /// Enter in the palette.
    pub(crate) fn run_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.filtered(cx);
        let Some(entry) = self.palette.as_ref().and_then(|p| entries.get(p.selected).cloned()) else { return };
        self.run_command(entry.command, window, cx);
    }

    fn run_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.palette.as_ref().map(|p| p.input.read(cx).text().trim().to_string()).unwrap_or_default();
        self.palette = None;
        match command {
            Command::Project(id) => self.select_project(id, cx),
            Command::Sub(id) => self.select_sub(id, cx),
            Command::Agent(id) => self.open_agent(id, cx),
            Command::NewProject => self.open_project_form(window, cx),
            Command::NewSub(id) => self.open_sub_form(id, window, cx),
            Command::NewAgent(id) => self.add_agent(id, cx),
            Command::RunPipeline(id) => self.open_pipeline_form(id, window, cx),
            Command::OpenInZed(path) => self.open_in_zed(path, cx),
            Command::Push(id) => self.push_sub(id, false, cx),
            Command::Review(id) => self.review_agent(id, cx),
            Command::Inspector(tab) => {
                self.inspector_open = true;
                self.inspector_tab = tab;
            }
            Command::Theme(key) => self.set_theme(key, window, cx),
            Command::Settings => self.open_settings(cx),
            Command::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            Command::Reconnect => self.reconnect_now(cx),
            Command::Shortcuts => self.shortcuts_open = true,
            Command::OpenFile(path) => self.open_in_zed(path, cx),
            Command::Conversation(agent_id, item) => {
                self.open_hit(agent_id, item, query, window, cx);
                cx.notify();
                return;
            }
            Command::Layout(project_id, name) => self.apply_layout(project_id, &name, cx),
            Command::SaveLayout(project_id) => {
                self.ask_save_layout(project_id, window, cx);
                return;
            }
            Command::DeleteLayout(project_id, name) => self.delete_layout(project_id, &name, cx),
            Command::Zen => self.toggle_zen(cx),
            Command::OnePane(on) => self.set_one_pane(on, cx),
            Command::ChangesBeside(on) => self.set_changes_beside(on, cx),
            Command::Dashboard(sub_id) => self.select_sub(sub_id, cx),
            Command::Export(scope) => self.export(scope, cx),
            Command::StartLead(sub_id) => {
                self.select_sub(sub_id, cx);
                self.start_lead(sub_id, cx);
            }
            Command::ToggleSounds => {
                self.change_sounds(|s| s.on = !s.on, cx);
                let on = self.sound.settings.on;
                self.toast(crate::agent::Tone::Info, if on { "Sounds are on." } else { "Sounds are muted. Ctrl K → Turn sounds on brings them back." }, cx);
            }
            Command::Density(compact) => self.set_density(compact, cx),
            Command::Health => self.open_health(cx),
            Command::Setup => self.open_setup(cx),
            Command::SubChanges(id) => self.open_sub_changes(id, cx),
            Command::CheckUpdates => self.check_updates(false, cx),
            Command::CreatePlayground => self.create_playground(cx),
            Command::Guide => self.open_guide(cx),
            Command::ProjectSettings(id) => {
                self.open_project_settings(id, window, cx);
                return;
            }
        }
        if self.form.is_none() && self.palette.is_none() && !self.shortcuts_open {
            if let Some(id) = self.sel_agent {
                self.focus_request = Some(id);
            } else {
                window.focus(&self.focus);
            }
        }
        cx.notify();
    }

    pub(crate) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.as_ref()?;
        let entries = self.filtered(cx);
        let selected = p.selected.min(entries.len().saturating_sub(1));
        let mut list = div().flex().flex_col().py(px(4.));
        let mut last_group = "";
        for (i, entry) in entries.iter().enumerate() {
            if entry.group != last_group {
                last_group = entry.group;
                list = list.child(div().px(px(16.)).pt(px(10.)).pb(px(4.)).child(ui::label(entry.group)));
            }
            let on = i == selected;
            let command = entry.command.clone();
            list = list.child(
                div()
                    .id(("palette-row", i))
                    .h(px(36.))
                    .mx(px(6.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_size(px(13.5))
                    .text_color(rgb(if on { theme::text() } else { theme::text_2() }))
                    .when(on, |d| d.bg(rgb(theme::selected())))
                    .when(!on, |d| d.hover(|s| s.bg(rgb(theme::hover()))))
                    .child(icon(entry.glyph).text_color(rgb(if on { theme::text() } else { theme::text_3() })))
                    .child(div().flex_none().when(on, |d| d.font_weight(FontWeight::MEDIUM)).child(entry.title.clone()))
                    .when(!entry.detail.is_empty(), |d| d.child(div().min_w(px(0.)).truncate().text_color(rgb(theme::text_3())).child(format!("· {}", entry.detail))))
                    .child(div().flex_1())
                    .children(entry.state.map(|state| {
                        let word = match state {
                            State::Working => "working",
                            State::NeedsYou => "needs you",
                            State::Done => "idle",
                            State::Idle => "not started",
                        };
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(12.))
                            .text_color(rgb(if state == State::NeedsYou { theme::red_text() } else { theme::text_3() }))
                            .child(ui::status_dot(("palette-dot", i), state))
                            .child(word)
                    }))
                    .children(entry.hint.map(ui::kbd))
                    .when(on, |d| d.child(ui::kbd("Enter")))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_command(command.clone(), window, cx))),
            );
        }
        if entries.is_empty() {
            list = list.child(div().px(px(16.)).py(px(18.)).text_color(rgb(theme::text_3())).child("Nothing matches."));
        }
        Some(
            div()
                .id("palette-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(rgba(theme::scrim()))
                .flex()
                .justify_center()
                .pt(relative(0.12))
                .on_click(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                .child(ui::fade_in(
                    "palette-in",
                    div()
                        .id("palette")
                        .key_context("Palette")
                        .w(px(720.))
                        .max_h(px(520.))
                        .flex()
                        .flex_col()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgb(theme::line_strong()))
                        .bg(rgb(theme::raised()))
                        .shadow_lg()
                        .overflow_hidden()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .h(px(50.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .px(px(14.))
                                .border_b_1()
                                .border_color(rgb(theme::line()))
                                .child(icon(Icon::Search).text_color(rgb(theme::text_3())))
                                .child(div().flex_1().child(p.input.clone())),
                        )
                        .child(div().id("palette-list").flex_1().min_h(px(0.)).overflow_y_scroll().child(list))
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(14.))
                                .px(px(16.))
                                .py(px(9.))
                                .border_t_1()
                                .border_color(rgb(theme::line()))
                                .text_size(px(11.5))
                                .text_color(rgb(theme::text_3()))
                                .child(div().flex().items_center().gap(px(4.)).child(ui::kbd("↑")).child(ui::kbd("↓")).child("move"))
                                .child(div().flex().items_center().gap(px(4.)).child(ui::kbd("Enter")).child("run"))
                                .child(div().flex().items_center().gap(px(4.)).child(ui::kbd("Esc")).child("close")),
                        ),
                ))
                .into_any_element(),
        )
    }
}
