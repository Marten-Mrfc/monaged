//! Monaged: run Claude Code agents on your projects, inside WSL, from one
//! native window. Projects contain subprojects; each subproject runs agents
//! in their own git worktrees, all over one Agent Client Protocol connection.
//!
//! Usage: monaged [--distro Ubuntu] [--agent <path to claude-agent-acp in WSL>]

mod acp;
mod agent;
mod coord;
mod coord_ui;
mod diff;
mod dots;
mod feedback;
mod git;
mod hosting;
mod icons;
mod llm;
mod markdown;
mod menu;
mod merge_ui;
mod palette;
mod pipeline;
mod pipeline_ui;
mod review;
mod review_ui;
mod secrets;
mod shell;
mod store;
#[cfg(test)]
mod tests;
mod text_input;
mod theme;
mod ui;
mod usage;
mod usage_ui;
mod wsl;
mod zed;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    App, AppContext, Application, Bounds, Context, ElementId, Entity, FocusHandle, Focusable, KeyBinding, ScrollHandle,
    SharedString, Stateful, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions,
    prelude::*, px, size,
};
use serde_json::{Value, json};

use acp::{AgentConfig, AgentEvent, AgentWriter};
use agent::{AgentView, Item, Status, Tone, Turn};
use pipeline::{Pipeline, Role};
use store::{Project, Store, Subproject};
use text_input::TextInput;

actions!(
    monaged,
    [Submit, Cancel, TogglePalette, ToggleSidebar, ToggleInspector, OpenSettings, PaletteUp, PaletteDown, ShowShortcuts, NewAgentHere, PrevAgent, NextAgent]
);

/// Quoted so neither PowerShell nor cmd rewrites the `~`; bash expands it.
const LOGIN_COMMAND: &str = r#"wsl -d Ubuntu -- bash -lc "~/.local/share/claude-agent-acp/current/claude""#;

enum Conn {
    Starting,
    Handshaking,
    Ready,
    Failed(String),
}

enum Pending {
    Initialize,
    NewSession(i64),
    LoadSession(i64),
    Prompt(i64),
    /// Switching a session to its role's model; its first message waits.
    SetModel(i64),
}

#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    Clone,
    Existing,
    Empty,
}

#[derive(Clone, Copy, PartialEq)]
enum SubKind {
    Feature,
    Standing,
}

impl SubKind {
    fn as_str(self) -> &'static str {
        match self {
            SubKind::Feature => "feature",
            SubKind::Standing => "standing",
        }
    }
}

enum Form {
    NewProject {
        source: SourceKind,
        name: Entity<TextInput>,
        url: Entity<TextInput>,
        folder: Entity<TextInput>,
        busy: bool,
        error: Option<String>,
        /// The field the error is about (its label), shown under that field.
        invalid: Option<&'static str>,
    },
    NewSubproject {
        project_id: i64,
        kind: SubKind,
        name: Entity<TextInput>,
        /// What should be built (the pipeline's feature request).
        request: Entity<TextInput>,
        run_pipeline: bool,
        auto_mode: bool,
        busy: bool,
        error: Option<String>,
        invalid: Option<&'static str>,
    },
    StartPipeline {
        sub_id: i64,
        request: Entity<TextInput>,
        auto_mode: bool,
        error: Option<String>,
        invalid: Option<&'static str>,
    },
    Settings {
        error: Option<String>,
    },
}

/// What `spawn_agent` creates.
struct AgentSpec {
    name: String,
    role: Role,
    /// Work directly on the subproject's branch (the integrator) instead of an own branch.
    on_feature_branch: bool,
    /// Sent as the first message once the session is open.
    brief: Option<String>,
    select: bool,
}

struct Monaged {
    store: Store,
    config: AgentConfig,
    linux_user: Option<String>,
    projects: Vec<Project>,
    subprojects: Vec<Subproject>,
    agents: Vec<AgentView>,
    sel_project: Option<i64>,
    sel_sub: Option<i64>,
    sel_agent: Option<i64>,
    form: Option<Form>,
    /// The subproject an agent is being created in (its worktree is on the way).
    creating_agent: Option<i64>,
    // Review (Phase 2).
    api_key: Option<String>,
    cli_fallback: bool,
    auto_review: bool,
    /// Haiku spend so far: (API credit, Max plan equivalent) in USD.
    spend: (f64, f64),
    finished_turns: Vec<i64>,
    // Pipelines (Phase 3), by subproject id.
    pipelines: HashMap<i64, Pipeline>,
    /// Git inside WSL has no name/email, so builders can't commit.
    identity_missing: bool,
    // Merging (Phase 5).
    branch_info: HashMap<i64, merge_ui::BranchInfo>,
    merges: HashMap<i64, merge_ui::MergeState>,
    /// Git in WSL uses Windows' credential manager.
    creds_ok: Option<bool>,
    // Coordination (Phase 4).
    bridge: Option<coord::Bridge>,
    board: Vec<store::BoardEntry>,
    claims: Vec<(i64, String)>,
    asks: Vec<coord_ui::Ask>,
    next_ask: u64,
    _bridge: Task<()>,
    // Usage (Phase 6).
    meter: usage::Meter,
    budget: usage::Budget,
    plan_usage: usage::PlanUsage,
    /// Models the adapter offers: (value, display name).
    models: Vec<(String, String)>,
    /// Finished pipelines, newest first, for estimates.
    runs: Vec<usage::Run>,
    /// API-equivalent cost per subproject this week.
    sub_costs: HashMap<i64, f64>,
    /// Set while sending something the user asked for directly.
    bypass_budget: bool,
    _tick: Task<()>,
    // Agent connection (one ACP process hosts every session).
    conn: Conn,
    writer: Option<AgentWriter>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    sessions: HashMap<String, i64>,
    logged_in: Option<bool>,
    // UI.
    focus: FocusHandle,
    sidebar_open: bool,
    inspector_open: bool,
    inspector_tab: shell::InspectorTab,
    /// Projects folded shut in the sidebar tree.
    collapsed: HashSet<i64>,
    /// Agents opened beside the focused one (wide windows).
    pinned: Vec<i64>,
    /// One message box and scroll position per agent pane.
    inputs: HashMap<i64, Entity<TextInput>>,
    scrolls: HashMap<i64, ScrollHandle>,
    /// Put the cursor in this agent's message box on the next frame.
    focus_request: Option<i64>,
    palette: Option<palette::PaletteState>,
    menu: Option<menu::MenuState>,
    confirm: Option<feedback::Confirm>,
    shortcuts_open: bool,
    toasts: Vec<feedback::Toast>,
    next_toast: u64,
    /// Toasts raised while handling a message (no `cx` there); shown right after.
    queued_toasts: Vec<(Tone, String, Option<(&'static str, feedback::ToastAction)>)>,
    reconnect: feedback::Reconnect,
    settings_scroll: ScrollHandle,
    _clock: Task<()>,
    /// "system" or a theme key.
    theme_key: String,
    _appearance: Option<Subscription>,
    _bounds: Subscription,
    _events: Task<()>,
    _quit: Subscription,
}

impl Monaged {
    fn new(store: Store, config: AgentConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let projects = store.projects().unwrap_or_default();
        let subprojects = store.subprojects().unwrap_or_default();
        let mut reviews = store.reviews().unwrap_or_default();
        let agents = store
            .agents()
            .unwrap_or_default()
            .into_iter()
            .map(|record| {
                let mut view = AgentView::new(record);
                if let Some((summary, confidence)) = reviews.remove(&view.record.id) {
                    view.summary = summary;
                    view.confidence = confidence;
                }
                view
            })
            .collect();
        let cli_fallback = store.setting("cli_fallback").is_none_or(|v| v == "1");
        let auto_review = store.setting("auto_review").is_none_or(|v| v == "1");
        let spend = store.llm_spend().unwrap_or_default();
        let pipelines = store.pipelines().unwrap_or_default();
        let board = store.board().unwrap_or_default();
        let usage_state = usage_ui::load(&store);
        let costs = store.cost_by_agent().unwrap_or_default();
        let runs = store.runs().unwrap_or_default();
        let claims = store.claims().unwrap_or_default();
        // Agents' tool calls (via the MCP relay) arrive here.
        let (bridge, bridge_task) = match coord::start_bridge() {
            Ok((bridge, mut requests)) => {
                let task = cx.spawn(async move |view, cx| {
                    while let Some(request) = requests.next().await {
                        if view.update(cx, |this, cx| this.on_tool_request(request, cx)).is_err() {
                            break;
                        }
                    }
                });
                (Some(bridge), task)
            }
            Err(err) => {
                eprintln!("[coord] no coordination tools this run: {err:#}");
                (None, Task::ready(()))
            }
        };
        let theme_key = store.setting("theme").unwrap_or_else(|| "carbon".into());
        let sidebar_open = store.setting("ui:sidebar").is_none_or(|v| v == "1");
        // Remember the window's place and size (restored at the next start).
        let bounds_watch = cx.observe_window_bounds(window, |this: &mut Self, window, _cx| {
            let _ = this.store.set_setting("ui:window", &format_bounds(window.window_bounds()));
        });
        let inspector_open = store.setting("ui:inspector").map_or(f32::from(window.viewport_size().width) >= 1500., |v| v == "1");
        let appearance = cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            if this.theme_key == "system" {
                this.apply_theme(window);
                cx.notify();
            }
        });
        let quit = cx.on_app_quit(|this: &mut Self, _cx| {
            if let Some(writer) = &this.writer {
                writer.kill();
            }
            async {}
        });

        let distro = config.distro.clone();
        cx.spawn(async move |view, cx| {
            let user = cx.background_spawn(async move { git::linux_user(&distro) }).await;
            view.update(cx, |this, cx| {
                this.linux_user = user.ok();
                cx.notify();
            })
            .ok();
        })
        .detach();

        let mut this = Self {
            store,
            config,
            linux_user: None,
            sel_project: projects.first().map(|p| p.id),
            projects,
            subprojects,
            agents,
            sel_sub: None,
            sel_agent: None,
            form: None,
            creating_agent: None,
            api_key: secrets::load_api_key(),
            cli_fallback,
            auto_review,
            spend,
            finished_turns: Vec::new(),
            pipelines,
            identity_missing: false,
            branch_info: HashMap::new(),
            merges: HashMap::new(),
            creds_ok: None,
            bridge,
            board,
            claims,
            asks: Vec::new(),
            next_ask: 1,
            _bridge: bridge_task,
            meter: usage_state.meter,
            budget: usage_state.budget,
            plan_usage: usage_state.plan_usage,
            models: usage_state.models,
            runs,
            sub_costs: HashMap::new(),
            bypass_budget: false,
            _tick: usage_ui::start_tick(cx),
            conn: Conn::Starting,
            writer: None,
            next_id: 1,
            pending: HashMap::new(),
            sessions: HashMap::new(),
            logged_in: None,
            focus: cx.focus_handle(),
            sidebar_open,
            inspector_open,
            inspector_tab: shell::InspectorTab::Pipeline,
            collapsed: HashSet::new(),
            pinned: Vec::new(),
            inputs: HashMap::new(),
            scrolls: HashMap::new(),
            focus_request: None,
            palette: None,
            menu: None,
            confirm: None,
            shortcuts_open: false,
            toasts: Vec::new(),
            next_toast: 1,
            queued_toasts: Vec::new(),
            reconnect: feedback::Reconnect::default(),
            settings_scroll: ScrollHandle::new(),
            _clock: Self::start_clock(cx),
            theme_key,
            _appearance: Some(appearance),
            _bounds: bounds_watch,
            _events: Task::ready(()),
            _quit: quit,
        };
        for agent in &mut this.agents {
            agent.cost_total = costs.get(&agent.record.id).copied().unwrap_or(0.0);
            agent.cost_seen = this.store.setting(&format!("agent:{}:cost_seen", agent.record.id)).and_then(|v| v.parse().ok());
        }
        this.reload_sub_costs();
        this.apply_theme(window);
        window.focus(&this.focus);
        this.connect(cx);
        this.check_credentials(cx);
        if let Some(project_id) = this.sel_project {
            this.refresh_branches(project_id, cx);
        }
        this
    }

    /// (Re)starts the agent process inside WSL and wires its events into this view.
    fn connect(&mut self, cx: &mut Context<Self>) {
        if let Some(writer) = self.writer.take() {
            writer.kill();
        }
        self.conn = Conn::Starting;
        self.reconnect.at = None;
        self.drop_live_sessions();
        let mut events = acp::launch(self.config.clone());
        self._events = cx.spawn(async move |view, cx| {
            while let Some(event) = events.next().await {
                if view.update(cx, |this, cx| this.on_agent_event(event, cx)).is_err() {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn drop_live_sessions(&mut self) {
        self.pending.clear();
        self.sessions.clear();
        for agent in &mut self.agents {
            if agent.turn.take().is_some() {
                agent.notice(Tone::Warn, "Interrupted: the connection to Claude was lost.");
            }
            agent.live = false;
            agent.status = Status::Idle;
        }
    }

    fn agent_mut(&mut self, id: i64) -> Option<&mut AgentView> {
        self.agents.iter_mut().find(|agent| agent.record.id == id)
    }

    fn agent(&self, id: i64) -> Option<&AgentView> {
        self.agents.iter().find(|agent| agent.record.id == id)
    }

    /// Roughly: is this agent in a pane you can see right now?
    fn agent_on_screen(&self, id: i64) -> bool {
        let Some(agent) = self.agent(id) else { return false };
        self.form.is_none()
            && self.sel_sub == Some(agent.record.subproject_id)
            && (self.sel_agent == Some(id) || self.pinned.contains(&id) || agent.is_working())
    }

    // ---- JSON-RPC plumbing ------------------------------------------------

    fn send(&mut self, message: Value) {
        let Some(writer) = &self.writer else { return };
        if let Err(err) = writer.send(&message) {
            self.conn = Conn::Failed(format!("Lost the connection to Claude: {err:#}"));
        }
    }

    fn request(&mut self, method: &str, params: Value, kind: Pending) {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, kind);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }

    fn notify_agent(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn respond(&mut self, id: Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn respond_error(&mut self, id: Value, code: i64, message: &str) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }));
    }

    // ---- Agent events -----------------------------------------------------

    fn on_agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Started { writer } => {
                self.writer = Some(writer);
                self.conn = Conn::Handshaking;
                self.request(
                    "initialize",
                    json!({
                        "protocolVersion": 1,
                        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                        "clientInfo": { "name": "monaged", "version": env!("CARGO_PKG_VERSION") },
                    }),
                    Pending::Initialize,
                );
            }
            AgentEvent::Message(message) => self.on_message(message),
            AgentEvent::Log(line) => eprintln!("[agent] {line}"),
            AgentEvent::Failed(err) => {
                self.conn = Conn::Failed(err);
                self.writer = None;
                self.drop_live_sessions();
                self.schedule_reconnect(cx);
            }
            AgentEvent::Exited(code) => {
                let code = code.map_or("unknown".into(), |c| c.to_string());
                self.conn = Conn::Failed(format!("The Claude process stopped (exit code {code})."));
                self.writer = None;
                self.drop_live_sessions();
                self.schedule_reconnect(cx);
            }
        }
        for agent_id in std::mem::take(&mut self.finished_turns) {
            self.after_turn(agent_id, cx);
        }
        for (tone, text, action) in std::mem::take(&mut self.queued_toasts) {
            match action {
                Some((label, action)) => self.toast_action(tone, text, label, action, cx),
                None => self.toast(tone, text, cx),
            }
        }
        self.follow_all();
        cx.notify();
    }

    fn on_message(&mut self, message: Value) {
        let method = message.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => self.on_agent_request(&method, id, &message["params"]),
            (Some(method), None) => self.on_notification(&method, &message["params"]),
            (None, Some(id)) => self.on_response(&id, &message),
            (None, None) => {}
        }
    }

    fn on_response(&mut self, id: &Value, message: &Value) {
        let Some(kind) = id.as_u64().and_then(|id| self.pending.remove(&id)) else { return };

        if let Some(error) = message.get("error") {
            let text = error["message"].as_str().unwrap_or("unknown error").to_string();
            match kind {
                Pending::Initialize => self.conn = Conn::Failed(format!("Couldn't start Claude: {text}")),
                Pending::NewSession(agent_id) => {
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.status = Status::Failed(text.clone());
                        agent.notice(Tone::Error, format!("Couldn't start a session: {text}"));
                        let name = agent.record.name.clone();
                        self.queued_toasts.push((
                            Tone::Error,
                            format!("Couldn't start {name}'s session: {text}"),
                            Some(("Retry", feedback::ToastAction::RetrySession(agent_id))),
                        ));
                    }
                }
                Pending::LoadSession(agent_id) => {
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.notice(Tone::Warn, format!("Couldn't reopen the saved conversation ({text}). Starting a fresh one."));
                        agent.record.session_id = None;
                        agent.status = Status::Idle;
                    }
                    self.ensure_session(agent_id);
                }
                Pending::SetModel(agent_id) => {
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.notice(Tone::Warn, format!("Couldn't switch the model ({text}); using the default."));
                    }
                    self.flush_queued_prompt(agent_id);
                }
                Pending::Prompt(agent_id) => {
                    let hint = if self.logged_in == Some(false) { " You're not logged in yet (see the bar at the top)." } else { "" };
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.turn = None;
                        agent.notice(Tone::Error, format!("Claude couldn't answer: {text}.{hint}"));
                    }
                }
            }
            return;
        }

        let result = &message["result"];
        match kind {
            Pending::Initialize => {
                self.conn = Conn::Ready;
                self.reconnect.attempts = 0;
                // The selected agent, plus any agent with a message waiting (e.g. a role brief).
                let mut open: Vec<i64> = self.agents.iter().filter(|a| a.queued_prompt.is_some()).map(|a| a.record.id).collect();
                open.extend(self.sel_agent);
                for agent_id in open {
                    self.ensure_session(agent_id);
                }
            }
            Pending::NewSession(agent_id) => {
                let Some(session_id) = result["sessionId"].as_str().map(str::to_owned) else { return };
                self.sessions.insert(session_id.clone(), agent_id);
                if let Err(err) = self.store.set_agent_session(agent_id, &session_id) {
                    eprintln!("[app] could not save session id: {err:#}");
                }
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.record.session_id = Some(session_id);
                    agent.status = Status::Ready;
                    agent.live = true;
                    let text = format!("Ready on branch {} in {}", agent.record.branch, agent.record.worktree);
                    agent.notice(Tone::Info, text);
                }
                self.note_models(&result["configOptions"]);
                if !self.apply_model(agent_id, &result["configOptions"]) {
                    self.flush_queued_prompt(agent_id);
                }
            }
            Pending::LoadSession(agent_id) => {
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.status = Status::Ready;
                    agent.live = true;
                    agent.cost_baseline = agent.cost_seen.is_none();
                    agent.notice(Tone::Info, "Reopened the saved conversation.");
                }
                self.note_models(&result["configOptions"]);
                if !self.apply_model(agent_id, &result["configOptions"]) {
                    self.flush_queued_prompt(agent_id);
                }
            }
            Pending::SetModel(agent_id) => {
                let model = result["configOptions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|o| o["id"] == "model")
                    .and_then(|o| o["currentValue"].as_str())
                    .map(|v| self.model_name(v));
                if let Some(agent) = self.agent_mut(agent_id)
                    && let Some(model) = model
                {
                    agent.notice(Tone::Info, format!("Using {model} for this role (Settings → Models)."));
                }
                self.flush_queued_prompt(agent_id);
            }
            Pending::Prompt(agent_id) => {
                let reason = result["stopReason"].as_str().unwrap_or("end_turn").to_string();
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.finish_turn(&reason);
                }
                self.record_turn(agent_id, &result["usage"]);
                if reason != "cancelled" {
                    self.finished_turns.push(agent_id);
                }
            }
        }
    }

    fn on_notification(&mut self, method: &str, params: &Value) {
        match method {
            "session/update" => {
                let session_id = params["sessionId"].as_str().unwrap_or_default();
                if let Some(&agent_id) = self.sessions.get(session_id) {
                    if params["update"]["sessionUpdate"] == "usage_update" {
                        self.on_usage_update(agent_id, &params["update"]);
                    }
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.apply_update(&params["update"]);
                    }
                }
            }
            "auth/status_update" => match params["state"].as_str() {
                Some("unauthenticated") => self.logged_in = Some(false),
                Some("authenticated") => self.logged_in = Some(true),
                _ => {}
            },
            _ => {}
        }
    }

    fn on_agent_request(&mut self, method: &str, id: Value, params: &Value) {
        // Monaged's own coordination tools never need the user's OK.
        if method == "session/request_permission" && params["toolCall"].to_string().contains("mcp__monaged__") {
            let options = params["options"].as_array().cloned().unwrap_or_default();
            let pick = ["allow_always", "allow_once"]
                .iter()
                .find_map(|kind| options.iter().find(|o| o["kind"].as_str() == Some(kind)))
                .and_then(|o| o["optionId"].as_str().map(str::to_owned));
            if let Some(option) = pick {
                self.respond(id, json!({ "outcome": { "outcome": "selected", "optionId": option } }));
                return;
            }
        }
        if method == "session/request_permission" {
            let session_id = params["sessionId"].as_str().unwrap_or_default();
            if let Some(&agent_id) = self.sessions.get(session_id)
                && let Some(agent) = self.agent_mut(agent_id)
            {
                agent.add_permission_request(id, params);
                let (name, sub_id) = (agent.record.name.clone(), agent.record.subproject_id);
                if !self.agent_on_screen(agent_id) {
                    self.queued_toasts.push((
                        Tone::Warn,
                        format!("{name} asks for permission"),
                        Some(("Open", feedback::ToastAction::Open { sub_id, agent_id: Some(agent_id) })),
                    ));
                }
                return;
            }
        }
        self.respond_error(id, -32601, &format!("{method} is not supported by Monaged yet"));
    }

    /// Opens the agent's session on the current connection: reopens a saved
    /// one (history is replayed) or creates a new one in its worktree.
    fn ensure_session(&mut self, agent_id: i64) {
        if !matches!(self.conn, Conn::Ready) {
            return;
        }
        let mcp = self.mcp_servers(agent_id);
        let Some(agent) = self.agent_mut(agent_id) else { return };
        // Archived agents' worktrees are gone; their history stays read-only.
        if agent.live || agent.record.archived || matches!(agent.status, Status::Starting | Status::Loading) {
            return;
        }
        let cwd = agent.record.worktree.clone();
        match agent.record.session_id.clone() {
            Some(session_id) => {
                agent.status = Status::Loading;
                agent.items.clear();
                self.sessions.insert(session_id.clone(), agent_id);
                self.request(
                    "session/load",
                    json!({ "sessionId": session_id, "cwd": cwd, "mcpServers": mcp }),
                    Pending::LoadSession(agent_id),
                );
            }
            None => {
                agent.status = Status::Starting;
                self.request("session/new", json!({ "cwd": cwd, "mcpServers": mcp }), Pending::NewSession(agent_id));
            }
        }
    }

    // ---- Navigation -------------------------------------------------------

    fn select_project(&mut self, project_id: i64, cx: &mut Context<Self>) {
        self.sel_project = Some(project_id);
        self.sel_sub = None;
        self.sel_agent = None;
        self.form = None;
        self.refresh_branches(project_id, cx);
        cx.notify();
    }

    fn select_sub(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return };
        let project_id = sub.project_id;
        self.sel_project = Some(project_id);
        self.sel_sub = Some(sub_id);
        self.form = None;
        self.refresh_branches(project_id, cx);
        self.refresh_request(sub_id, cx);
        let first_agent =
            self.agents.iter().find(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| a.record.id);
        self.sel_agent = first_agent;
        if let Some(agent_id) = first_agent {
            self.ensure_session(agent_id);
            self.focus_request = Some(agent_id);
            if let Some(handle) = self.scrolls.get(&agent_id) {
                handle.scroll_to_bottom();
            }
        }
        cx.notify();
    }

    fn select_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        self.sel_agent = Some(agent_id);
        self.focus_request = Some(agent_id);
        self.ensure_session(agent_id);
        if let Some(handle) = self.scrolls.get(&agent_id) {
            handle.scroll_to_bottom();
        }
        cx.notify();
    }

    fn open_in_zed(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(user) = self.linux_user.clone() else {
            self.toast(Tone::Warn, "Still finding your WSL user name; try again in a second.", cx);
            return;
        };
        match zed::open(&self.config.distro, &user, &path) {
            Ok(()) => self.toast(Tone::Info, format!("Opening {} in Zed", last_segment(&path)), cx),
            Err(err) => self.toast_error(format!("Couldn't open Zed: {err:#}"), cx),
        }
    }

    // ---- Creating things --------------------------------------------------

    fn open_project_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new("e.g. HR app", cx));
        let url = cx.new(|cx| TextInput::new("git@gitlab.com:team/hr-app.git  or  https://github.com/you/repo.git", cx));
        let folder = cx.new(|cx| TextInput::new("Leave empty for ~/code/<name>", cx));
        window.focus(&name.focus_handle(cx));
        self.form = Some(Form::NewProject { source: SourceKind::Clone, name, url, folder, busy: false, error: None, invalid: None });
        cx.notify();
    }

    fn open_sub_form(&mut self, project_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new("e.g. Company car reservation", cx));
        let request = cx.new(|cx| TextInput::new("e.g. Employees can reserve a company car for a date range; show it on the dashboard", cx));
        window.focus(&name.focus_handle(cx));
        self.sel_project = Some(project_id);
        self.form = Some(Form::NewSubproject {
            project_id,
            kind: SubKind::Feature,
            name,
            request,
            run_pipeline: true,
            auto_mode: false,
            busy: false,
            error: None,
            invalid: None,
        });
        cx.notify();
    }

    fn open_pipeline_form(&mut self, sub_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let request = cx.new(|cx| TextInput::new("What should be built?", cx));
        window.focus(&request.focus_handle(cx));
        self.form = Some(Form::StartPipeline { sub_id, request, auto_mode: false, error: None, invalid: None });
        cx.notify();
    }

    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        match &mut self.form {
            Some(Form::NewProject { source, name, url, folder, busy, error, invalid }) => {
                if *busy {
                    return;
                }
                let url_text = url.read(cx).text().trim().to_string();
                let folder_text = folder.read(cx).text().trim().to_string();
                let mut name_text = name.read(cx).text().trim().to_string();
                if name_text.is_empty() {
                    let from = if *source == SourceKind::Clone { &url_text } else { &folder_text };
                    name_text = last_segment(from);
                }
                let problem = match source {
                    SourceKind::Clone if url_text.is_empty() => Some(("Git URL", "Paste the Git URL to clone.", url.clone())),
                    SourceKind::Existing if folder_text.is_empty() => Some(("Folder in WSL", "Enter the WSL folder, e.g. ~/code/hr-app.", folder.clone())),
                    _ if name_text.is_empty() => Some(("Name", "Give the project a name.", name.clone())),
                    _ => None,
                };
                if let Some((field, problem, input)) = problem {
                    *error = Some(problem.into());
                    *invalid = Some(field);
                    input.update(cx, |input, cx| input.set_invalid(true, cx));
                    cx.notify();
                    return;
                }
                *invalid = None;
                let folder_path = if folder_text.is_empty() { format!("~/code/{}", git::slug(&name_text)) } else { folder_text };
                let git_source = match source {
                    SourceKind::Clone => git::Source::Clone(url_text),
                    SourceKind::Existing => git::Source::Existing,
                    SourceKind::Empty => git::Source::Empty,
                };
                *busy = true;
                *error = None;
                cx.spawn(async move |view, cx| {
                    let d = distro.clone();
                    let prepared = cx.background_spawn(async move { git::prepare_project(&d, &folder_path, &git_source) }).await;
                    view.update(cx, |this, cx| {
                        let identity_missing = prepared.as_ref().is_ok_and(|p| p.identity_missing);
                        match prepared.and_then(|p| this.store.add_project(&name_text, &distro, &p.path, &p.base_branch)) {
                            Ok(project) => {
                                let id = project.id;
                                this.projects.push(project);
                                this.projects.sort_by_key(|p| p.name.to_lowercase());
                                this.select_project(id, cx);
                                this.toast(Tone::Info, format!("Added {name_text}"), cx);
                                if identity_missing {
                                    this.identity_missing = true;
                                    this.toast_action(
                                        Tone::Warn,
                                        "Git inside WSL has no name or email yet, so agents can't commit.",
                                        "Fix in Settings",
                                        feedback::ToastAction::OpenSettings,
                                        cx,
                                    );
                                }
                            }
                            Err(err) => {
                                if let Some(Form::NewProject { busy, error, .. }) = &mut this.form {
                                    *busy = false;
                                    *error = Some(format!("{err:#}"));
                                }
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Some(Form::NewSubproject { project_id, kind, name, request, run_pipeline, auto_mode, busy, error, invalid }) => {
                if *busy {
                    return;
                }
                let request = request.read(cx).text().trim().to_string();
                let (run_pipeline, auto_mode) = (*run_pipeline, *auto_mode);
                let sub_name = name.read(cx).text().trim().to_string();
                if sub_name.is_empty() {
                    *error = Some("Give the subproject a name.".into());
                    *invalid = Some("Name");
                    name.update(cx, |input, cx| input.set_invalid(true, cx));
                    cx.notify();
                    return;
                }
                let Some(project) = self.projects.iter().find(|p| p.id == *project_id).cloned() else { return };
                let (project_id, kind) = (*project_id, *kind);
                let branch = format!("{}/{}", kind.as_str(), git::slug(&sub_name));
                *busy = true;
                *error = None;
                cx.spawn(async move |view, cx| {
                    let (repo, b, base) = (project.path.clone(), branch.clone(), project.base_branch.clone());
                    let created = cx.background_spawn(async move { git::ensure_branch(&distro, &repo, &b, &base) }).await;
                    view.update(cx, |this, cx| {
                        match created.and_then(|_| this.store.add_subproject(project_id, &sub_name, kind.as_str(), &branch)) {
                            Ok(sub) => {
                                let id = sub.id;
                                this.subprojects.push(sub);
                                this.form = None;
                                this.select_sub(id, cx);
                                if run_pipeline {
                                    let request = if request.is_empty() { sub_name.clone() } else { request.clone() };
                                    this.start_pipeline(id, request, auto_mode, cx);
                                } else {
                                    this.add_agent(id, cx);
                                }
                            }
                            Err(err) => {
                                if let Some(Form::NewSubproject { busy, error, .. }) = &mut this.form {
                                    *busy = false;
                                    *error = Some(format!("{err:#}"));
                                }
                            }
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Some(Form::StartPipeline { sub_id, request, auto_mode, error, invalid }) => {
                let text = request.read(cx).text().trim().to_string();
                if text.is_empty() {
                    *error = Some("Describe what should be built.".into());
                    *invalid = Some("What should be built?");
                    request.update(cx, |input, cx| input.set_invalid(true, cx));
                    cx.notify();
                    return;
                }
                let (sub_id, auto_mode) = (*sub_id, *auto_mode);
                self.form = None;
                self.start_pipeline(sub_id, text, auto_mode, cx);
            }
            Some(Form::Settings { .. }) | None => {}
        }
        cx.notify();
    }

    fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.refresh_spend();
        self.form = Some(Form::Settings { error: None });
        cx.notify();
    }

    /// "+ Agent": a free agent in its own worktree.
    fn add_agent(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        if self.creating_agent.is_some() {
            return;
        }
        let number = self.agents.iter().filter(|a| a.record.subproject_id == sub_id).count() + 1;
        self.creating_agent = Some(sub_id);
        let spec = AgentSpec { name: format!("agent-{number}"), role: Role::Free, on_feature_branch: false, brief: None, select: true };
        self.spawn_agent(sub_id, spec, cx);
    }

    /// Creates an agent: its worktree (own branch, or the feature branch for the
    /// integrator), its database row, its session, and queues its brief.
    fn spawn_agent(&mut self, sub_id: i64, spec: AgentSpec, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        let name = self.unique_agent_name(sub_id, &spec.name);
        let sub_slug = git::slug(&sub.name);
        let worktree = git::worktree_path(&project.path, &sub_slug, &name);
        let branch = if spec.on_feature_branch { sub.branch.clone() } else { format!("agent/{sub_slug}/{name}") };
        let role = spec.role.to_db();
        self.creating_agent = Some(sub_id);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let (distro, repo, wt, br, from) =
                (project.distro.clone(), project.path.clone(), worktree.clone(), branch.clone(), sub.branch.clone());
            let added = cx.background_spawn(async move { git::add_worktree(&distro, &repo, &wt, &br, &from) }).await;
            view.update(cx, |this, cx| {
                this.creating_agent = None;
                match added.and_then(|_| this.store.add_agent(sub_id, &name, &branch, &worktree, &role)) {
                    Ok(record) => {
                        let id = record.id;
                        let mut agent = AgentView::new(record);
                        agent.queued_prompt = spec.brief;
                        this.agents.push(agent);
                        if spec.select && this.sel_sub == Some(sub_id) {
                            this.select_agent(id, cx);
                        } else {
                            this.ensure_session(id);
                        }
                    }
                    Err(err) => this.toast_error(format!("Couldn't create {name}'s worktree: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Talking to an agent ----------------------------------------------

    /// Sends a message to an agent, or queues it until its session is open and idle.
    fn send_prompt(&mut self, agent_id: i64, text: String) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let session = agent.record.session_id.clone().filter(|_| agent.live && agent.turn.is_none());
        let Some(session_id) = session else {
            agent.queued_prompt = Some(match agent.queued_prompt.take() {
                Some(earlier) => format!("{earlier}\n\n{text}"),
                None => text,
            });
            self.ensure_session(agent_id);
            return;
        };
        if !self.bypass_budget
            && let Some(block) = self.budget_block()
        {
            self.pause_prompt(agent_id, text, &block);
            return;
        }
        let text = self.with_board_updates(agent_id, text);
        let Some(agent) = self.agent_mut(agent_id) else { return };
        agent.items.push(Item::User(text.clone()));
        agent.turn = Some(Turn { started: Instant::now(), first_words: None });
        self.request(
            "session/prompt",
            json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] }),
            Pending::Prompt(agent_id),
        );
        if let Some(handle) = self.scrolls.get(&agent_id) {
            handle.scroll_to_bottom();
        }
    }

    fn flush_queued_prompt(&mut self, agent_id: i64) {
        let ready = self.agent(agent_id).is_some_and(|a| a.live && a.turn.is_none());
        if ready && let Some(prompt) = self.agent_mut(agent_id).and_then(|a| a.queued_prompt.take()) {
            self.send_prompt(agent_id, prompt);
        }
    }

    /// The agent whose message box has the cursor (else the focused pane).
    fn composer_agent(&self, window: &Window, cx: &Context<Self>) -> Option<i64> {
        self.inputs.iter().find(|(_, input)| input.focus_handle(cx).is_focused(window)).map(|(id, _)| *id).or(self.sel_agent)
    }

    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.accept_confirm(window, cx);
            return;
        }
        if self.menu.is_some() {
            self.run_menu_selection(window, cx);
            return;
        }
        if self.shortcuts_open {
            self.toggle_shortcuts(window, cx);
            return;
        }
        if self.palette.is_some() {
            self.run_palette_selection(window, cx);
            return;
        }
        if self.form.is_some() {
            self.submit_form(cx);
            return;
        }
        let Some(agent_id) = self.composer_agent(window, cx) else { return };
        let Some(input) = self.inputs.get(&agent_id).cloned() else { return };
        let text = input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.is_working() {
            self.toast(Tone::Warn, format!("{} is still working. Press Esc to stop it first.", agent.record.name), cx);
            return;
        }
        input.update(cx, |input, cx| input.clear(cx));
        // Typing your own answer replaces any half-answered question cards.
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.answers.clear();
        }
        self.bypass_budget = true;
        self.send_prompt(agent_id, text);
        self.bypass_budget = false;
        cx.notify();
    }

    /// Esc closes the innermost thing first: a dialog, a menu, the palette, a
    /// form; otherwise it stops the agent you're typing to.
    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm.is_some() {
            self.close_confirm(window, cx);
            return;
        }
        if self.menu.is_some() {
            self.close_menu(window, cx);
            return;
        }
        if self.shortcuts_open {
            self.toggle_shortcuts(window, cx);
            return;
        }
        if self.palette.is_some() {
            self.close_palette(window, cx);
            return;
        }
        if self.form.take().is_some() {
            self.focus_request = self.sel_agent;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        let Some(agent_id) = self.composer_agent(window, cx) else { return };
        self.stop_agent(agent_id, cx);
    }

    fn on_show_shortcuts(&mut self, _: &ShowShortcuts, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_shortcuts(window, cx);
    }

    fn on_new_agent_here(&mut self, _: &NewAgentHere, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(sub_id) = self.sel_sub {
            self.form = None;
            self.add_agent(sub_id, cx);
        }
    }

    fn on_prev_agent(&mut self, _: &PrevAgent, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_agent(-1, cx);
    }

    fn on_next_agent(&mut self, _: &NextAgent, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_agent(1, cx);
    }

    /// Alt Up / Alt Down: the previous or next agent in this subproject.
    fn step_agent(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(sub_id) = self.sel_sub else { return };
        let ids: Vec<i64> =
            self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| a.record.id).collect();
        if ids.is_empty() {
            return;
        }
        let at = self.sel_agent.and_then(|id| ids.iter().position(|&i| i == id));
        let next = match at {
            Some(at) => (at as isize + delta).rem_euclid(ids.len() as isize) as usize,
            None => 0,
        };
        self.form = None;
        self.select_agent(ids[next], cx);
    }

    /// Opens a failed agent's session again, or restarts a live one (its
    /// conversation is reloaded from Claude's saved history).
    pub(crate) fn retry_session(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        if !matches!(self.conn, Conn::Ready) {
            self.toast_action(Tone::Warn, "Claude isn't connected yet.", "Reconnect", feedback::ToastAction::Reconnect, cx);
            return;
        }
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.is_working() {
            return;
        }
        if let Some(session_id) = agent.record.session_id.clone() {
            self.sessions.remove(&session_id);
        }
        let Some(agent) = self.agent_mut(agent_id) else { return };
        agent.live = false;
        agent.status = Status::Idle;
        self.ensure_session(agent_id);
        cx.notify();
    }

    fn on_toggle_palette(&mut self, _: &TogglePalette, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(window, cx);
    }

    fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        let _ = self.store.set_setting("ui:sidebar", if self.sidebar_open { "1" } else { "0" });
        cx.notify();
    }

    fn on_toggle_inspector(&mut self, _: &ToggleInspector, _window: &mut Window, cx: &mut Context<Self>) {
        self.inspector_open = !self.inspector_open;
        let _ = self.store.set_setting("ui:inspector", if self.inspector_open { "1" } else { "0" });
        cx.notify();
    }

    fn on_open_settings(&mut self, _: &OpenSettings, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(cx);
    }

    /// Picks the palette for `theme_key` ("system" follows Windows' light/dark mode).
    fn apply_theme(&self, window: &mut Window) {
        let key = if self.theme_key == "system" {
            match window.appearance() {
                gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => "paper",
                _ => "carbon",
            }
        } else {
            self.theme_key.as_str()
        };
        theme::set(key);
        window.refresh();
    }

    pub(crate) fn set_theme(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_key = key.to_string();
        let _ = self.store.set_setting("theme", key);
        self.apply_theme(window);
        cx.notify();
    }

    fn choose_permission(&mut self, agent_id: i64, index: usize, option_id: Option<String>, cx: &mut Context<Self>) {
        let response = (|| {
            let agent = self.agents.iter_mut().find(|a| a.record.id == agent_id)?;
            let Some(Item::Permission { request_id, options, chosen, .. }) = agent.items.get_mut(index) else { return None };
            if chosen.is_some() {
                return None;
            }
            let label = option_id
                .as_ref()
                .and_then(|id| options.iter().find(|o| &o.id == id))
                .map_or_else(|| "Cancelled".to_string(), |o| o.name.clone());
            *chosen = Some(label);
            let outcome = match &option_id {
                Some(id) => json!({ "outcome": "selected", "optionId": id }),
                None => json!({ "outcome": "cancelled" }),
            };
            Some((request_id.clone(), outcome))
        })();
        if let Some((request_id, outcome)) = response {
            self.respond(request_id, json!({ "outcome": outcome }));
        }
        cx.notify();
    }
}

impl Render for Monaged {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_shell(window, cx)
    }
}

// ---- Small UI helpers ------------------------------------------------------

/// Older call sites pick a button by its background; map them onto the kit.
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, bg: u32) -> Stateful<gpui::Div> {
    let kind = if bg == theme::PRIMARY_BG {
        ui::Kind::Primary
    } else if bg == theme::red_soft() {
        ui::Kind::Danger
    } else {
        ui::Kind::Secondary
    };
    ui::button(id, kind, None, label)
}

fn tone_color(tone: Tone) -> u32 {
    match tone {
        Tone::Info => theme::text_3(),
        Tone::Warn => theme::red_text(),
        Tone::Error => theme::red_text(),
    }
}

/// "x,y,w,h" in logical pixels, with "max:" in front when maximized.
fn parse_bounds(text: &str) -> Option<WindowBounds> {
    let maximized = text.starts_with("max:");
    let numbers: Vec<f32> = text.trim_start_matches("max:").split(',').filter_map(|n| n.trim().parse().ok()).collect();
    let [x, y, w, h] = numbers[..] else { return None };
    if w < 640. || h < 400. {
        return None;
    }
    let bounds = Bounds { origin: gpui::point(px(x), px(y)), size: size(px(w), px(h)) };
    Some(if maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
}

fn format_bounds(bounds: WindowBounds) -> String {
    let (prefix, b) = match bounds {
        WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => ("max:", b),
        WindowBounds::Windowed(b) => ("", b),
    };
    format!("{prefix}{},{},{},{}", f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height))
}

/// "git@gitlab.com:team/hr-app.git" or "~/code/hr-app" -> "hr-app"
fn last_segment(text: &str) -> String {
    let trimmed = text.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn parse_args() -> AgentConfig {
    let mut config = AgentConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--distro" => config.distro = args.next().unwrap_or(config.distro),
            "--agent" => config.agent_cmd = args.next().unwrap_or(config.agent_cmd),
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    config
}

fn main() {
    // Started by the Claude adapter inside WSL as an agent's MCP server.
    let args: Vec<String> = std::env::args().collect();
    if let Some(relay) = coord::parse_relay_args(&args) {
        if let Err(err) = coord::run_relay(relay) {
            eprintln!("monaged relay: {err:#}");
            std::process::exit(1);
        }
        return;
    }
    let config = parse_args();
    let store = match Store::open() {
        Ok(store) => store,
        Err(err) => {
            eprintln!("Monaged could not open its database: {err:#}");
            std::process::exit(1);
        }
    };
    let saved_bounds = store.setting("ui:window").and_then(|v| parse_bounds(&v));
    Application::new().with_assets(icons::Assets).run(move |cx: &mut App| {
        cx.bind_keys(text_input::key_bindings());
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("TextInput")),
            // Enter also submits a form when focus isn't in a text field.
            KeyBinding::new("enter", Submit, Some("Monaged")),
            KeyBinding::new("escape", Cancel, None),
            KeyBinding::new("ctrl-k", TogglePalette, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-alt-b", ToggleInspector, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("ctrl-/", ShowShortcuts, None),
            KeyBinding::new("ctrl-n", NewAgentHere, None),
            KeyBinding::new("alt-up", PrevAgent, None),
            KeyBinding::new("alt-down", NextAgent, None),
            KeyBinding::new("up", PaletteUp, Some("Palette")),
            KeyBinding::new("down", PaletteDown, Some("Palette")),
            KeyBinding::new("up", PaletteUp, Some("Menu")),
            KeyBinding::new("down", PaletteDown, Some("Menu")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Where you left the window last time (maximized, or its size and place).
        let window_bounds =
            saved_bounds.unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, size(px(1480.), px(920.)), cx)));
        cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                // Monaged draws its own title bar (with Windows' snap-aware controls).
                titlebar: Some(TitlebarOptions { title: Some("Monaged".into()), appears_transparent: true, ..Default::default() }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Monaged::new(store, config, window, cx)),
        )
        .expect("failed to open the window");
        cx.activate(true);
    });
}
