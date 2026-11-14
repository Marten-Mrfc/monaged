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
mod git;
mod hosting;
mod llm;
mod markdown;
mod merge_ui;
mod pipeline;
mod pipeline_ui;
mod review;
mod review_ui;
mod secrets;
mod store;
#[cfg(test)]
mod tests;
mod text_input;
mod theme;
mod usage;
mod usage_ui;
mod wsl;
mod zed;

use std::collections::HashMap;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    AnyElement, App, AppContext, Application, Bounds, ClipboardItem, Context, ElementId, Entity,
    FocusHandle, Focusable, FontWeight, KeyBinding, ScrollHandle, SharedString, Stateful,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div,
    prelude::*, px, rgb, size,
};
use serde_json::{Value, json};

use acp::{AgentConfig, AgentEvent, AgentWriter};
use agent::{AgentTab, AgentView, Item, Status, Tone, Turn};
use pipeline::{Pipeline, Role};
use store::{Project, Store, Subproject};
use text_input::TextInput;

actions!(monaged, [Submit, Cancel]);

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
    },
    StartPipeline {
        sub_id: i64,
        request: Entity<TextInput>,
        auto_mode: bool,
        error: Option<String>,
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
    banner: Option<(Tone, String)>,
    creating_agent: bool,
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
    /// A promotion into the top branch waiting for its second click.
    promote_confirm: Option<(i64, String, String)>,
    /// Git in WSL uses Windows' credential manager.
    creds_ok: Option<bool>,
    // Coordination (Phase 4).
    bridge: Option<coord::Bridge>,
    board: Vec<store::BoardEntry>,
    claims: Vec<(i64, String)>,
    asks: Vec<coord_ui::Ask>,
    next_ask: u64,
    /// The subproject view shows the board instead of an agent.
    board_open: bool,
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
    input: Entity<TextInput>,
    focus: FocusHandle,
    scroll: ScrollHandle,
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
        let input = cx.new(|cx| TextInput::new("Message this agent. Enter to send, Esc to stop.", cx));
        window.focus(&input.focus_handle(cx));
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
            banner: None,
            creating_agent: false,
            api_key: secrets::load_api_key(),
            cli_fallback,
            auto_review,
            spend,
            finished_turns: Vec::new(),
            pipelines,
            identity_missing: false,
            branch_info: HashMap::new(),
            merges: HashMap::new(),
            promote_confirm: None,
            creds_ok: None,
            bridge,
            board,
            claims,
            asks: Vec::new(),
            next_ask: 1,
            board_open: false,
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
            input,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _events: Task::ready(()),
            _quit: quit,
        };
        for agent in &mut this.agents {
            agent.cost_total = costs.get(&agent.record.id).copied().unwrap_or(0.0);
            agent.cost_seen = this.store.setting(&format!("agent:{}:cost_seen", agent.record.id)).and_then(|v| v.parse().ok());
        }
        this.reload_sub_costs();
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
            }
            AgentEvent::Exited(code) => {
                let code = code.map_or("unknown".into(), |c| c.to_string());
                self.conn = Conn::Failed(format!("The Claude process stopped (exit code {code})."));
                self.writer = None;
                self.drop_live_sessions();
            }
        }
        for agent_id in std::mem::take(&mut self.finished_turns) {
            self.after_turn(agent_id, cx);
        }
        self.scroll.scroll_to_bottom();
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
                    let hint = if self.logged_in == Some(false) { " You're not logged in yet (see the banner)." } else { "" };
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
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn select_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        self.sel_agent = Some(agent_id);
        self.board_open = false;
        self.ensure_session(agent_id);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn open_in_zed(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(user) = self.linux_user.clone() else {
            self.banner = Some((Tone::Warn, "Still finding your WSL user name; try again in a second.".into()));
            cx.notify();
            return;
        };
        if let Err(err) = zed::open(&self.config.distro, &user, &path) {
            self.banner = Some((Tone::Error, format!("{err:#}")));
            cx.notify();
        }
    }

    // ---- Creating things --------------------------------------------------

    fn open_project_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| TextInput::new("e.g. HR app", cx));
        let url = cx.new(|cx| TextInput::new("git@gitlab.com:team/hr-app.git  or  https://github.com/you/repo.git", cx));
        let folder = cx.new(|cx| TextInput::new("Leave empty for ~/code/<name>", cx));
        window.focus(&name.focus_handle(cx));
        self.form = Some(Form::NewProject { source: SourceKind::Clone, name, url, folder, busy: false, error: None });
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
        });
        cx.notify();
    }

    fn open_pipeline_form(&mut self, sub_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let request = cx.new(|cx| TextInput::new("What should be built?", cx));
        window.focus(&request.focus_handle(cx));
        self.form = Some(Form::StartPipeline { sub_id, request, auto_mode: false, error: None });
        cx.notify();
    }

    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        match &mut self.form {
            Some(Form::NewProject { source, name, url, folder, busy, error }) => {
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
                    SourceKind::Clone if url_text.is_empty() => Some("Paste the Git URL to clone."),
                    SourceKind::Existing if folder_text.is_empty() => Some("Enter the WSL folder, e.g. ~/code/hr-app."),
                    _ if name_text.is_empty() => Some("Give the project a name."),
                    _ => None,
                };
                if let Some(problem) = problem {
                    *error = Some(problem.into());
                    cx.notify();
                    return;
                }
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
                                if identity_missing {
                                    this.banner = Some((
                                        Tone::Warn,
                                        "Git inside WSL has no name/email yet, so agents can't commit. Run in WSL: \
                                         git config --global user.name \"Your Name\" && git config --global user.email you@example.com"
                                            .into(),
                                    ));
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
            Some(Form::NewSubproject { project_id, kind, name, request, run_pipeline, auto_mode, busy, error }) => {
                if *busy {
                    return;
                }
                let request = request.read(cx).text().trim().to_string();
                let (run_pipeline, auto_mode) = (*run_pipeline, *auto_mode);
                let sub_name = name.read(cx).text().trim().to_string();
                if sub_name.is_empty() {
                    *error = Some("Give the subproject a name.".into());
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
            Some(Form::StartPipeline { sub_id, request, auto_mode, error }) => {
                let text = request.read(cx).text().trim().to_string();
                if text.is_empty() {
                    *error = Some("Describe what should be built.".into());
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
        if self.creating_agent {
            return;
        }
        let number = self.agents.iter().filter(|a| a.record.subproject_id == sub_id).count() + 1;
        self.creating_agent = true;
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
        self.banner = Some((Tone::Info, format!("Creating a worktree for {name}…")));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let (distro, repo, wt, br, from) =
                (project.distro.clone(), project.path.clone(), worktree.clone(), branch.clone(), sub.branch.clone());
            let added = cx.background_spawn(async move { git::add_worktree(&distro, &repo, &wt, &br, &from) }).await;
            view.update(cx, |this, cx| {
                this.creating_agent = false;
                match added.and_then(|_| this.store.add_agent(sub_id, &name, &branch, &worktree, &role)) {
                    Ok(record) => {
                        this.banner = None;
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
                    Err(err) => this.banner = Some((Tone::Error, format!("Couldn't create {name}'s worktree: {err:#}"))),
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
        self.scroll.scroll_to_bottom();
    }

    fn flush_queued_prompt(&mut self, agent_id: i64) {
        let ready = self.agent(agent_id).is_some_and(|a| a.live && a.turn.is_none());
        if ready && let Some(prompt) = self.agent_mut(agent_id).and_then(|a| a.queued_prompt.take()) {
            self.send_prompt(agent_id, prompt);
        }
    }

    fn on_submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        if self.form.is_some() {
            self.submit_form(cx);
            return;
        }
        let Some(agent_id) = self.sel_agent else { return };
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.is_working() {
            self.banner = Some((Tone::Warn, "This agent is still working. Press Esc to stop it first.".into()));
            cx.notify();
            return;
        }
        self.banner = None;
        self.input.update(cx, |input, cx| input.clear(cx));
        // Typing your own answer replaces any half-answered question cards.
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.answers.clear();
        }
        self.bypass_budget = true;
        self.send_prompt(agent_id, text);
        self.bypass_budget = false;
        cx.notify();
    }

    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.take().is_some() {
            window.focus(&self.input.focus_handle(cx));
            cx.notify();
            return;
        }
        let Some(agent_id) = self.sel_agent else { return };
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
            .filter_map(|(i, item)| matches!(item, Item::Permission { chosen: None, .. }).then_some(i))
            .collect();
        if let Some(session_id) = session_id {
            self.notify_agent("session/cancel", json!({ "sessionId": session_id }));
        }
        for index in open {
            self.choose_permission(agent_id, index, None, cx);
        }
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

    // ---- Rendering --------------------------------------------------------

    fn conn_label(&self) -> (u32, SharedString) {
        match &self.conn {
            Conn::Starting => (theme::WARN, "Starting WSL…".into()),
            Conn::Handshaking => (theme::WARN, "Connecting to Claude…".into()),
            Conn::Ready if self.logged_in == Some(false) => (theme::WARN, "Not logged in".into()),
            Conn::Ready => (theme::GOOD, "Claude connected".into()),
            Conn::Failed(reason) => {
                let short: String = reason.chars().take(40).collect();
                (theme::BAD, format!("Disconnected: {short}").into())
            }
        }
    }

    fn sub_activity(&self, sub_id: i64) -> Option<u32> {
        if self.pipeline_needs_you(sub_id) {
            return Some(theme::WARN);
        }
        let agents = self.agents.iter().filter(|a| a.record.subproject_id == sub_id);
        let mut color = None;
        for agent in agents {
            if agent.needs_attention() {
                return Some(theme::WARN);
            }
            if agent.is_working() {
                color = Some(theme::ACCENT);
            }
        }
        color
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (dot, label) = self.conn_label();
        let mut list = div().flex().flex_col().gap_0p5().px_2().pb_4();
        for project in &self.projects {
            let pid = project.id;
            let selected = self.sel_project == Some(pid) && self.sel_sub.is_none() && self.form.is_none();
            list = list.child(
                nav_row(("project", pid as usize), selected, 0.)
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().text_color(rgb(theme::MUTED)).child("▣"))
                    .child(div().flex_1().child(project.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_project(pid, cx))),
            );
            for sub in self.subprojects.iter().filter(|s| s.project_id == pid) {
                let sid = sub.id;
                let selected = self.sel_sub == Some(sid) && self.form.is_none();
                let mark = if sub.kind == "standing" { "◇" } else { "◆" };
                list = list.child(
                    nav_row(("sub", sid as usize), selected, 1.)
                        .child(div().text_color(rgb(theme::MUTED)).child(mark))
                        .child(div().flex_1().child(sub.name.clone()))
                        .children(self.sub_activity(sid).map(|color| div().size_2().rounded_full().bg(rgb(color))))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sid, cx))),
                );
            }
            list = list.child(
                nav_row(("new-sub", pid as usize), false, 1.)
                    .text_color(rgb(theme::MUTED))
                    .child("+ New subproject")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_sub_form(pid, window, cx))),
            );
        }

        div()
            .w(px(260.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::SIDEBAR))
            .border_r_1()
            .border_color(rgb(theme::BORDER))
            .text_size(px(13.))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("monaged"))
                    .child(div().size_2().rounded_full().bg(rgb(dot))),
            )
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(rgb(theme::MUTED))
                    .child("PROJECTS")
                    .child(
                        div()
                            .id("add-project")
                            .px_1p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::HOVER)))
                            .child("+ Add")
                            .on_click(cx.listener(|this, _, window, cx| this.open_project_form(window, cx))),
                    ),
            )
            .child(div().id("sidebar-list").flex_1().min_h(px(0.)).overflow_y_scroll().child(list))
            .children(self.render_usage_meter(cx))
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(theme::BORDER))
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(rgb(theme::MUTED))
                    .child(div().flex_1().min_w(px(0.)).child(label))
                    .children(matches!(self.conn, Conn::Failed(_)).then(|| {
                        button("reconnect", "Reconnect", theme::ALLOW_BG).on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                    }))
                    .child(
                        div()
                            .id("open-settings")
                            .px_1p5()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::HOVER)))
                            .child("Settings")
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
                    ),
            )
    }

    fn render_login_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.logged_in != Some(false) {
            return None;
        }
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(rgb(0x3a3222))
                .border_b_1()
                .border_color(rgb(theme::WARN))
                .text_size(px(13.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child("Claude isn't logged in inside WSL. Run this once in a terminal, type /login, then click Reconnect:")
                        .child(div().font_family("Consolas").text_color(rgb(theme::WARN)).child(LOGIN_COMMAND)),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap_2()
                        .child(button("copy-login", "Copy command", theme::INPUT_BG).on_click(cx.listener(|_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(LOGIN_COMMAND.to_string()))
                        })))
                        .child(button("login-reconnect", "Reconnect", theme::ALLOW_BG).on_click(cx.listener(|this, _, _, cx| this.connect(cx)))),
                )
                .into_any_element(),
        )
    }

    fn render_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (tone, text) = self.banner.as_ref()?;
        let color = tone_color(*tone);
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(theme::BORDER))
                .text_size(px(13.))
                .text_color(rgb(color))
                .child(div().flex_1().min_w(px(0.)).child(text.clone()))
                .child(
                    div()
                        .id("dismiss-banner")
                        .cursor_pointer()
                        .text_color(rgb(theme::MUTED))
                        .child("✕")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.banner = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(Form::Settings { error }) = &self.form {
            return self.render_settings(error.as_ref(), cx);
        }
        if let Some(form) = &self.form {
            return self.render_form(form, cx);
        }
        if let Some(sub_id) = self.sel_sub {
            return self.render_subproject(sub_id, cx);
        }
        if let Some(project_id) = self.sel_project {
            return self.render_project(project_id, cx);
        }
        self.render_welcome(cx)
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .p_8()
            .flex()
            .flex_col()
            .gap_3()
            .max_w(px(620.))
            .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).child("Welcome to Monaged"))
            .child(div().text_color(rgb(theme::MUTED)).child(
                "One place per project. Add a project (a git repo inside WSL), split it into subprojects, \
                 and let Claude agents work on them side by side, each in its own git worktree.",
            ))
            .child(
                div().flex().child(
                    button("welcome-add", "Add your first project", theme::ALLOW_BG)
                        .on_click(cx.listener(|this, _, window, cx| this.open_project_form(window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_project(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = self.projects.iter().find(|p| p.id == project_id) else { return div().into_any_element() };
        let subs: Vec<&Subproject> = self.subprojects.iter().filter(|s| s.project_id == project_id).collect();
        let path = project.path.clone();
        let mut cards = div().flex().flex_col().gap_2();
        if subs.is_empty() {
            cards = cards.child(div().text_color(rgb(theme::MUTED)).child(
                "No subprojects yet. A subproject is a feature (it gets its own branch) or a standing concern such as security.",
            ));
        }
        for sub in subs {
            let sid = sub.id;
            let agent_count = self.agents.iter().filter(|a| a.record.subproject_id == sid).count();
            let working = self.agents.iter().filter(|a| a.record.subproject_id == sid && a.is_working()).count();
            cards = cards.child(
                div()
                    .id(("card", sid as usize))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .rounded_lg()
                    .bg(rgb(theme::PANEL))
                    .border_1()
                    .border_color(rgb(theme::BORDER))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(theme::ACCENT)))
                    .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(sub.name.clone()))
                    .child(badge(&sub.kind))
                    .child(div().font_family("Consolas").text_size(px(12.)).text_color(rgb(theme::MUTED)).child(sub.branch.clone()))
                    .child(div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(match working {
                        0 => format!("{agent_count} agent{}", if agent_count == 1 { "" } else { "s" }),
                        n => format!("{n} working"),
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sid, cx))),
            );
        }
        div()
            .id("project-view")
            .flex_1()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).child(project.name.clone()))
                            .child(
                                div()
                                    .font_family("Consolas")
                                    .text_size(px(12.))
                                    .text_color(rgb(theme::MUTED))
                                    .child(format!("{} · {} · base branch {}", project.distro, project.path, project.base_branch)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button("project-zed", "Open in Zed", theme::INPUT_BG).on_click(cx.listener(move |this, _, _, cx| {
                                this.open_in_zed(path.clone(), cx)
                            })))
                            .child(button("project-new-sub", "+ New subproject", theme::ALLOW_BG).on_click(cx.listener(
                                move |this, _, window, cx| this.open_sub_form(project_id, window, cx),
                            ))),
                    ),
            )
            .child(self.render_branch_panel(project_id, cx))
            .child(switch_like(
                "project-auto-start",
                "Auto-start small, low-risk plans with confidence 4–5 (in subprojects with auto mode on). \
                 Large features, migrations, auth and data changes always wait for you.",
                self.project_auto_start(project_id),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_project_auto_start(project_id, cx))))
            .child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child("SUBPROJECTS"))
            .child(cards)
            .into_any_element()
    }

    fn render_subproject(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return div().into_any_element() };
        let project_path = self.projects.iter().find(|p| p.id == sub.project_id).map(|p| p.path.clone()).unwrap_or_default();
        let selected = self.sel_agent.and_then(|id| self.agent(id)).filter(|a| a.record.subproject_id == sub_id);
        let zed_path = selected.map(|a| a.record.worktree.clone()).unwrap_or(project_path);
        let zed_label = if selected.is_some() { "Open worktree in Zed" } else { "Open in Zed" };

        let mut tabs = div().flex().items_center().gap_1().px_4().py_2().border_b_1().border_color(rgb(theme::BORDER));
        let archived = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && a.record.archived).count();
        let board_count = self.board_count(sub_id);
        tabs = tabs.child(
            div()
                .id("board-tab")
                .flex()
                .items_center()
                .gap_1p5()
                .px_3()
                .py_1()
                .rounded_md()
                .text_size(px(13.))
                .cursor_pointer()
                .when(self.board_open, |d| d.bg(rgb(theme::SELECTED)))
                .hover(|s| s.bg(rgb(theme::HOVER)))
                .child("Board")
                .when(board_count > 0, |d| d.child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child(board_count.to_string())))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.board_open = true;
                    cx.notify();
                })),
        );
        tabs = tabs.child(div().w(px(1.)).h(px(16.)).mx_1().bg(rgb(theme::BORDER)));
        for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived) {
            let aid = agent.record.id;
            let is_selected = Some(aid) == self.sel_agent && !self.board_open;
            let color = agent_color(agent);
            tabs = tabs.child(
                div()
                    .id(("agent-tab", aid as usize))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .text_size(px(13.))
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(rgb(theme::SELECTED)))
                    .hover(|s| s.bg(rgb(theme::HOVER)))
                    .child(div().size_2().rounded_full().bg(rgb(color)))
                    .child(agent.record.name.clone())
                    .children(agent.confidence.as_ref().map(|c| {
                        div().text_size(px(11.)).text_color(rgb(review_ui::score_color(c.score))).child(format!("{}/5", c.score))
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_agent(aid, cx))),
            );
        }
        tabs = tabs.child(
            div()
                .id("add-agent")
                .px_3()
                .py_1()
                .rounded_md()
                .text_size(px(13.))
                .text_color(rgb(theme::MUTED))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme::HOVER)))
                .child(if self.creating_agent { "Creating…" } else { "+ Agent" })
                .on_click(cx.listener(move |this, _, _, cx| this.add_agent(sub_id, cx))),
        );

        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w(px(0.))
                    .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).child(sub.name.clone()))
                    .child(badge(&sub.kind))
                    .child(div().font_family("Consolas").text_size(px(12.)).text_color(rgb(theme::MUTED)).child(sub.branch.clone()))
                    .children(self.sub_usage_label(sub_id).map(|l| div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(l))),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(
                        self.pipelines.get(&sub_id).is_none_or(|p| matches!(p.stage, pipeline::Stage::Done | pipeline::Stage::Stopped)),
                        |d| {
                            d.child(button("run-pipeline", "Run pipeline", theme::ALLOW_BG).on_click(cx.listener(
                                move |this, _, window, cx| this.open_pipeline_form(sub_id, window, cx),
                            )))
                        },
                    )
                    .child(button("sub-zed", zed_label, theme::INPUT_BG).on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(zed_path.clone(), cx)))),
            );
        let panel = self.render_pipeline_panel(sub_id, cx);

        let body: AnyElement = match selected {
            _ if self.board_open => self.render_board(sub_id, cx),
            Some(agent) => self.render_agent(agent, cx),
            None => div()
                .flex_1()
                .p_8()
                .text_color(rgb(theme::MUTED))
                .child("No agents yet. Click \"+ Agent\" to start one in its own worktree.")
                .into_any_element(),
        };

        let tabs = tabs.when(archived > 0, |d| {
            d.child(div().pl_2().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(format!("{archived} cleaned up")))
        });
        let merge = self.render_merge_card(sub_id, cx);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(header)
            .children(panel)
            .children(merge)
            .child(tabs)
            .child(body)
            .into_any_element()
    }

    fn render_agent(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let body = match agent.tab {
            AgentTab::Conversation => self.render_conversation(agent, cx),
            AgentTab::Changes => self.render_changes(agent, cx),
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(self.render_agent_bar(agent, cx))
            .child(body)
            .into_any_element()
    }

    fn render_conversation(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let agent_id = agent.record.id;
        let busy = agent.is_working();
        let status_line = match &agent.status {
            Status::Idle => Some((Tone::Info, "Not connected yet.".to_string())),
            Status::Starting => Some((Tone::Info, "Starting a Claude session in this worktree…".to_string())),
            Status::Loading => Some((Tone::Info, "Reopening the saved conversation…".to_string())),
            Status::Failed(err) => Some((Tone::Error, format!("Failed: {err}"))),
            Status::Ready => None,
        };
        let items: Vec<AnyElement> = agent.items.iter().enumerate().map(|(i, item)| self.render_item(agent_id, i, item, cx)).collect();
        let action_button = if busy {
            button("stop", "Stop", theme::DENY_BG).on_click(cx.listener(|this, _, window, cx| this.on_cancel(&Cancel, window, cx)))
        } else {
            button("send", "Send", theme::ALLOW_BG).on_click(cx.listener(|this, _, window, cx| this.on_submit(&Submit, window, cx)))
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(
                div().id("transcript").flex_1().min_h(px(0.)).overflow_y_scroll().track_scroll(&self.scroll).child(
                    div().flex().flex_col().items_center().child(
                        div()
                            .w_full()
                            .max_w(px(900.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .children(items)
                            .children(status_line.map(|(tone, text)| {
                                div().text_size(px(12.)).text_color(rgb(tone_color(tone))).child(text)
                            })),
                    ),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .bg(rgb(theme::PANEL))
                    .border_t_1()
                    .border_color(rgb(theme::BORDER))
                    .child(div().flex_1().child(self.input.clone()))
                    .child(action_button),
            )
            .into_any_element()
    }

    fn render_item(&self, agent_id: i64, index: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        match item {
            Item::User(text) if text.starts_with(pipeline::BRIEF_HEADER) => pipeline_ui::render_brief(text),
            Item::User(text) if text.starts_with("[monaged-ask #") || text.contains(coord::DIGEST_HEADER) => coord_ui::render_user_message(text),
            Item::User(text) => div().px_3().py_2().rounded_lg().bg(rgb(theme::USER_BG)).child(text.clone()).into_any_element(),
            Item::Agent(text) if pipeline::contains_directive(text) => self.render_agent_reply(agent_id, index, text, cx),
            Item::Agent(text) => review_ui::render_markdown(text),
            Item::Thought(text) => div()
                .pl_3()
                .border_l_2()
                .border_color(rgb(theme::BORDER))
                .text_size(px(13.))
                .text_color(rgb(theme::MUTED))
                .italic()
                .child(text.clone())
                .into_any_element(),
            Item::Tool { title, kind, status, .. } => {
                let (icon, color) = match status.as_str() {
                    "completed" => ("●", theme::GOOD),
                    "failed" => ("✕", theme::BAD),
                    "in_progress" => ("◐", theme::ACCENT),
                    _ => ("○", theme::MUTED),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(theme::PANEL))
                    .text_size(px(13.))
                    .child(div().text_color(rgb(color)).child(icon))
                    .child(div().font_family("Consolas").flex_1().child(match title.strip_prefix("mcp__monaged__") {
                        Some(tool) => format!("Monaged · {}", tool.replace('_', " ")),
                        None => title.clone(),
                    }))
                    .child(div().text_color(rgb(theme::MUTED)).child(if title.starts_with("mcp__monaged__") {
                        "coordination".to_string()
                    } else {
                        kind.clone()
                    }))
                    .into_any_element()
            }
            Item::Plan(entries) => div()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme::BORDER))
                .text_size(px(13.))
                .child(div().text_color(rgb(theme::MUTED)).child("Plan"))
                .children(entries.iter().map(|entry| {
                    let (mark, color) = match entry.status.as_str() {
                        "completed" => ("☑", theme::GOOD),
                        "in_progress" => ("◐", theme::ACCENT),
                        _ => ("☐", theme::MUTED),
                    };
                    div().flex().gap_2().child(div().text_color(rgb(color)).child(mark)).child(entry.content.clone())
                }))
                .into_any_element(),
            Item::Permission { title, options, chosen, .. } => {
                let card = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(if chosen.is_some() { theme::BORDER } else { theme::WARN }))
                    .child(format!("Claude wants to: {title}"));
                match chosen {
                    Some(answer) => card
                        .child(div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(format!("→ {answer}")))
                        .into_any_element(),
                    None => card
                        .child(div().flex().gap_2().children(options.iter().enumerate().map(|(j, option)| {
                            let bg = if option.kind.starts_with("allow") { theme::ALLOW_BG } else { theme::DENY_BG };
                            let option_id = option.id.clone();
                            button(("perm", (agent_id as usize) * 4096 + index * 16 + j), option.name.clone(), bg).on_click(cx.listener(
                                move |this, _, _, cx| this.choose_permission(agent_id, index, Some(option_id.clone()), cx),
                            ))
                        })))
                        .into_any_element(),
                }
            }
            Item::Notice(tone, text) => {
                div().text_size(px(12.)).text_color(rgb(tone_color(*tone))).child(text.clone()).into_any_element()
            }
        }
    }

    fn render_form(&self, form: &Form, cx: &mut Context<Self>) -> AnyElement {
        let (title, body, busy, error, submit_label): (&str, AnyElement, bool, Option<String>, &str) = match form {
            Form::NewProject { source, name, url, folder, busy, error } => {
                let source = *source;
                let choice = |id: &'static str, label: &'static str, kind: SourceKind, cx: &mut Context<Self>| {
                    chip(id, label, source == kind).on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(Form::NewProject { source, name, url, .. }) = &mut this.form {
                            *source = kind;
                            // The fields shift when the source changes; put the cursor where typing makes sense.
                            let target = if kind == SourceKind::Clone { url } else { name };
                            window.focus(&target.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                };
                let folder_hint = match source {
                    SourceKind::Existing => "The folder inside WSL, e.g. ~/code/hr-app. It becomes a git repo if it isn't one yet.",
                    SourceKind::Clone => "Where to clone it. Leave empty for ~/code/<name>. Uses your git/SSH setup inside WSL.",
                    SourceKind::Empty => "Where to create the new repo. Leave empty for ~/code/<name>.",
                };
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(choice("src-clone", "Clone from GitLab / GitHub", SourceKind::Clone, cx))
                            .child(choice("src-existing", "Existing WSL folder", SourceKind::Existing, cx))
                            .child(choice("src-empty", "New empty repo", SourceKind::Empty, cx)),
                    )
                    .when(source == SourceKind::Clone, |d| d.child(field("Git URL", None, url.clone())))
                    .child(field("Name", Some("Leave empty to use the repo or folder name."), name.clone()))
                    .child(field("Folder in WSL", Some(folder_hint), folder.clone()))
                    .into_any_element();
                ("Add a project", body, *busy, error.clone(), if *busy { "Working…" } else { "Add project" })
            }
            Form::NewSubproject { kind, name, request, run_pipeline, auto_mode, busy, error, .. } => {
                let (kind, run, auto) = (*kind, *run_pipeline, *auto_mode);
                let choice = |id: &'static str, label: &'static str, value: SubKind, cx: &mut Context<Self>| {
                    chip(id, label, kind == value).on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(Form::NewSubproject { kind, name, .. }) = &mut this.form {
                            *kind = value;
                            window.focus(&name.focus_handle(cx));
                        }
                        cx.notify();
                    }))
                };
                let hint = match kind {
                    SubKind::Feature => "A goal like \"company car reservation\". Gets the branch feature/<name>; its agents branch off it.",
                    SubKind::Standing => "A long-running concern like \"security\". Gets the branch standing/<name>.",
                };
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(choice("kind-feature", "Feature", SubKind::Feature, cx))
                            .child(choice("kind-standing", "Standing", SubKind::Standing, cx)),
                    )
                    .child(field("Name", Some(hint), name.clone()))
                    .child(switch_row("toggle-run-pipeline", "Run the pipeline: research → verify → plan → build", run, cx, |form| {
                        if let Form::NewSubproject { run_pipeline, .. } = form {
                            *run_pipeline = !*run_pipeline;
                        }
                    }))
                    .when(run, |d| {
                        d.child(field("What should be built?", Some("The researcher starts from this. Leave empty to use the name."), request.clone()))
                            .child(switch_row(
                                "toggle-auto-mode",
                                "Auto mode: agents' questions get their recommended answer, confident small plans start by themselves",
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
                ("New subproject", body, *busy, error.clone(), label)
            }
            Form::StartPipeline { request, auto_mode, error, .. } => {
                let body = div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(field("What should be built?", Some("A researcher starts from this, then two verifiers, a planner and the builders."), request.clone()))
                    .child(switch_row(
                        "toggle-auto-mode",
                        "Auto mode: agents' questions get their recommended answer, confident small plans start by themselves",
                        *auto_mode,
                        cx,
                        |form| {
                            if let Form::StartPipeline { auto_mode, .. } = form {
                                *auto_mode = !*auto_mode;
                            }
                        },
                    ))
                    .into_any_element();
                ("Run the pipeline", body, false, error.clone(), "Start the research")
            }
            Form::Settings { error } => return self.render_settings(error.as_ref(), cx),
        };

        div()
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .max_w(px(680.))
            .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(title))
            .child(body)
            .children(error.map(|e| div().text_size(px(13.)).text_color(rgb(theme::BAD)).child(e)))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("form-submit", submit_label, if busy { theme::INPUT_BG } else { theme::ALLOW_BG })
                            .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                    )
                    .child(button("form-cancel", "Cancel", theme::INPUT_BG).on_click(cx.listener(|this, _, window, cx| this.on_cancel(&Cancel, window, cx)))),
            )
            .into_any_element()
    }
}

impl Render for Monaged {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Monaged")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::on_cancel))
            .size_full()
            .flex()
            .bg(rgb(theme::BG))
            .text_color(rgb(theme::TEXT))
            .text_size(px(14.))
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(self.render_login_banner(cx))
                    .children(self.render_paused_bar(cx))
                    .children(self.render_banner(cx))
                    .child(self.render_content(cx)),
            )
    }
}

// ---- Small UI helpers ------------------------------------------------------

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, bg: u32) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .px_3()
        .py_1p5()
        .rounded_md()
        .bg(rgb(bg))
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
        .child(label.into())
}

fn chip(id: &'static str, label: &'static str, selected: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_md()
        .border_1()
        .text_size(px(13.))
        .cursor_pointer()
        .border_color(rgb(if selected { theme::ACCENT } else { theme::BORDER }))
        .when(selected, |d| d.bg(rgb(theme::SELECTED)))
        .hover(|s| s.bg(rgb(theme::HOVER)))
        .child(label)
}

fn nav_row(id: impl Into<ElementId>, selected: bool, indent: f32) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .pl(px(8. + indent * 16.))
        .pr_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |d| d.bg(rgb(theme::SELECTED)))
        .hover(|s| s.bg(rgb(theme::HOVER)))
}

fn field(label: &'static str, hint: Option<&'static str>, input: Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(label))
        .child(input)
        .children(hint.map(|h| div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(h)))
}

/// A labelled on/off switch; the caller attaches the click handler.
fn switch_like(id: &'static str, label: &'static str, on: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .text_size(px(13.))
        .child(
            div()
                .flex_none()
                .w(px(34.))
                .h(px(18.))
                .rounded_full()
                .p(px(2.))
                .flex()
                .when(on, |d| d.justify_end().bg(rgb(theme::ALLOW_BG)))
                .when(!on, |d| d.bg(rgb(theme::BORDER)))
                .child(div().size(px(14.)).rounded_full().bg(rgb(theme::TEXT))),
        )
        .child(label)
}

/// A switch that flips a field of the open form.
fn switch_row(
    id: &'static str,
    label: &'static str,
    on: bool,
    cx: &mut Context<Monaged>,
    flip: impl Fn(&mut Form) + 'static,
) -> Stateful<gpui::Div> {
    switch_like(id, label, on).on_click(cx.listener(move |this, _, _, cx| {
        if let Some(form) = &mut this.form {
            flip(form);
        }
        cx.notify();
    }))
}

fn badge(kind: &str) -> impl IntoElement {
    let color = if kind == "standing" { theme::WARN } else { theme::ACCENT };
    div()
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .text_size(px(11.))
        .child(kind.to_string())
}

fn tone_color(tone: Tone) -> u32 {
    match tone {
        Tone::Info => theme::MUTED,
        Tone::Warn => theme::WARN,
        Tone::Error => theme::BAD,
    }
}

fn agent_color(agent: &AgentView) -> u32 {
    if agent.needs_attention() {
        return theme::WARN;
    }
    if agent.is_working() {
        return theme::ACCENT;
    }
    match agent.status {
        Status::Ready => theme::GOOD,
        Status::Failed(_) => theme::BAD,
        Status::Starting | Status::Loading => theme::WARN,
        Status::Idle => theme::MUTED,
    }
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
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys(text_input::key_bindings());
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("TextInput")),
            // Enter also submits a form when focus isn't in a text field.
            KeyBinding::new("enter", Submit, Some("Monaged")),
            KeyBinding::new("escape", Cancel, None),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1240.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("Monaged".into()), ..Default::default() }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Monaged::new(store, config, window, cx)),
        )
        .expect("failed to open the window");
        cx.activate(true);
    });
}
