//! Letting Claude look at Monaged: an MCP server (`monaged.exe --mcp-debug`)
//! that Claude Desktop or Claude Code starts. It forwards each tool call to
//! the running Monaged window over the local bridge, with a debug token kept
//! in `%APPDATA%\Monaged\debug.json` while Settings → Connect Claude is on.
//! Reading is all it does unless you also allow actions there.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use crate::agent::{AgentView, Tone};
use crate::coord::ToolReply;
use crate::export::{self, Scope, mask_all};
use crate::pipeline::Role;
use crate::shell::{agent_state, role_label};
use crate::ui::State;
use crate::{Monaged, review};

/// Settings keys.
pub const SETTING_ON: &str = "debug:mcp";
pub const SETTING_ACTIONS: &str = "debug:actions";

/// Where the running window leaves its port and debug token.
pub fn debug_file() -> PathBuf {
    crate::store::data_dir().join("debug.json")
}

pub fn is_debug_args(args: &[String]) -> bool {
    args.get(1).map(String::as_str) == Some("--mcp-debug")
}

const READ_TOOLS: [(&str, &str); 10] = [
    ("status", "Monaged's state: version, WSL and Claude connection, agents by state, usage and budget, what's on screen, what waits for the user."),
    ("projects", "Every project with its subprojects (pipeline stage, confidence) and agents (id, role, state, score)."),
    ("subproject", "One subproject in full: the request, the pipeline (stage, plan, quality gate), its agents, the coordination board, what waits for the user and the timeline. Pass `subproject` (id or name)."),
    ("agent", "One agent: its role, state, summary, confidence with findings and checks, and its conversation (the last `items`, default 30). Pass `agent` (id or name)."),
    ("changes", "An agent's diff against where its branch started (secrets masked). Pass `agent`."),
    ("inbox", "Everything that waits for the user, across projects."),
    ("log", "The end of Monaged's log (`lines`, default 200; `search` to keep only matching lines)."),
    ("screenshot", "A picture of Monaged's window as it is now. Use `show` first to pick what's on screen."),
    ("show", "Change what Monaged shows (this moves the user's view): `project`, `subproject` (its dashboard), `agent`, or `page` (settings, usage, health, guide)."),
    ("export", "Write the export for analysis (a Markdown report and a zip) for `project` (id or name), or everything; returns where it went and the report."),
];

const ACTION_TOOLS: [(&str, &str); 3] = [
    ("send_message", "Send an agent a message as if the user typed it (`agent`, `text`). Only when the user allowed actions in Settings → Connect Claude."),
    ("stop_agent", "Stop an agent's current turn (`agent`). Only when actions are allowed."),
    ("review_agent", "Run the tests and a review of an agent's work (`agent`). Only when actions are allowed."),
];

pub fn tool_list() -> Value {
    let schema = |name: &str| -> Value {
        let props = match name {
            "subproject" => json!({ "subproject": { "type": "string" } }),
            "agent" => json!({ "agent": { "type": "string" }, "items": { "type": "integer" } }),
            "changes" | "stop_agent" | "review_agent" => json!({ "agent": { "type": "string" } }),
            "log" => json!({ "lines": { "type": "integer" }, "search": { "type": "string" } }),
            "show" => json!({ "project": { "type": "string" }, "subproject": { "type": "string" }, "agent": { "type": "string" }, "page": { "type": "string", "enum": ["settings", "usage", "connect", "health", "guide"] } }),
            "export" => json!({ "project": { "type": "string" } }),
            "send_message" => json!({ "agent": { "type": "string" }, "text": { "type": "string" } }),
            _ => json!({}),
        };
        let required: Vec<&str> = match name {
            "subproject" => vec!["subproject"],
            "agent" | "changes" | "stop_agent" | "review_agent" => vec!["agent"],
            "send_message" => vec!["agent", "text"],
            _ => vec![],
        };
        json!({ "type": "object", "properties": props, "required": required })
    };
    Value::Array(
        READ_TOOLS
            .iter()
            .chain(ACTION_TOOLS.iter())
            .map(|(name, description)| json!({ "name": name, "description": description, "inputSchema": schema(name) }))
            .collect(),
    )
}

// ---- The relay (runs as `monaged.exe --mcp-debug`, started by Claude) --------------

pub fn run_relay() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = message.get("id").cloned() else { continue };
        let method = message["method"].as_str().unwrap_or_default();
        let result = match method {
            "initialize" => json!({
                "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "monaged", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Monaged runs Claude Code agents on the user's projects inside WSL. These tools read what's happening in the running app: start with `status` or `projects`, then `subproject` or `agent`. `screenshot` shows the window (use `show` to pick the screen first; it moves the user's view, so say so). Secrets are masked."
            }),
            "tools/list" => json!({ "tools": tool_list() }),
            "ping" => json!({}),
            "tools/call" => {
                let tool = message["params"]["name"].as_str().unwrap_or_default();
                let arguments = message["params"]["arguments"].clone();
                match call_window(tool, arguments) {
                    Ok(reply) => {
                        let mut content = Vec::new();
                        if let Some(image) = reply["image"].as_str() {
                            content.push(json!({ "type": "image", "data": image, "mimeType": "image/png" }));
                        }
                        content.push(json!({ "type": "text", "text": reply["text"].as_str().unwrap_or_default() }));
                        json!({ "content": content, "isError": reply["is_error"].as_bool().unwrap_or(false) })
                    }
                    Err(err) => json!({ "content": [{ "type": "text", "text": format!("{err:#}") }], "isError": true }),
                }
            }
            _ => {
                let error = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} isn't supported") } });
                writeln!(stdout, "{error}")?;
                stdout.flush()?;
                continue;
            }
        };
        writeln!(stdout, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": result }))?;
        stdout.flush()?;
    }
    Ok(())
}

fn call_window(tool: &str, arguments: Value) -> Result<Value> {
    let text = std::fs::read_to_string(debug_file())
        .map_err(|_| anyhow::anyhow!("Monaged isn't running, or Settings → Connect Claude is off. Open Monaged and turn it on."))?;
    let info: Value = serde_json::from_str(&text).context("debug.json is damaged; turn Connect Claude off and on again")?;
    let port = info["port"].as_u64().filter(|p| *p > 0 && *p <= u16::MAX as u64).context("debug.json has no port")? as u16;
    let token = info["token"].as_str().context("debug.json has no token")?;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|_| anyhow::anyhow!("Monaged isn't running (or was restarted with Connect Claude off). Open it and try again."))?;
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    writeln!(stream, "{}", json!({ "token": token, "debug": true, "tool": tool, "arguments": arguments }))?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("unreadable reply from Monaged")
}

// ---- Setting it up ---------------------------------------------------------------

/// Writes or removes debug.json for this run's bridge.
pub fn publish(port: u16, token: &str, on: bool) -> Result<()> {
    let path = debug_file();
    if !on {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = json!({ "port": port, "token": token, "pid": std::process::id(), "version": env!("CARGO_PKG_VERSION") });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)?;
    Ok(())
}

/// Claude Desktop's config file.
pub fn claude_desktop_config() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|a| PathBuf::from(a).join("Claude").join("claude_desktop_config.json"))
}

/// Adds (or updates) the `monaged` server in a Claude Desktop config, keeping everything else.
pub fn add_to_desktop_config(existing: Option<&str>, exe: &str) -> Result<String> {
    let mut config: Value = match existing.map(str::trim).filter(|t| !t.is_empty()) {
        Some(text) => serde_json::from_str(text).context("Claude Desktop's config isn't valid JSON, so Monaged left it alone")?,
        None => json!({}),
    };
    let Some(root) = config.as_object_mut() else { bail!("Claude Desktop's config isn't a JSON object, so Monaged left it alone") };
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else { bail!("mcpServers in Claude Desktop's config isn't an object") };
    servers.insert("monaged".into(), json!({ "command": exe, "args": ["--mcp-debug"] }));
    Ok(serde_json::to_string_pretty(&config)?)
}

pub fn install_desktop() -> Result<PathBuf> {
    let path = claude_desktop_config().context("APPDATA isn't set")?;
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let existing = std::fs::read_to_string(&path).ok();
    let updated = add_to_desktop_config(existing.as_deref(), &exe)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(old) = &existing {
        let _ = std::fs::write(path.with_extension("json.bak"), old);
    }
    std::fs::write(&path, updated)?;
    Ok(path)
}

/// The config without Monaged's server (None: nothing to change).
pub fn remove_from_desktop_config(text: &str) -> Option<String> {
    let mut config: Value = serde_json::from_str(text).ok()?;
    config.get_mut("mcpServers")?.as_object_mut()?.remove("monaged")?;
    serde_json::to_string_pretty(&config).ok()
}

/// Uninstalling: Claude Desktop shouldn't keep starting a program that's gone.
pub fn uninstall_desktop() -> Result<bool> {
    let Some(path) = claude_desktop_config() else { return Ok(false) };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(false) };
    match remove_from_desktop_config(&text) {
        Some(updated) => {
            std::fs::write(&path, updated)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The command that adds Monaged to Claude Code on Windows.
pub fn claude_code_windows_command() -> String {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| "monaged.exe".into());
    format!("claude mcp add --scope user monaged -- \"{exe}\" --mcp-debug")
}

/// Adds Monaged to Claude Code inside WSL (its CLI is next to the agent).
pub fn install_claude_code_wsl(distro: &str, agent_cmd: &str) -> Result<String> {
    let exe = std::env::current_exe()?;
    let wsl_exe = crate::coord::windows_to_wsl_path(&exe.to_string_lossy());
    let cli = crate::onboarding::claude_cli(agent_cmd);
    let cli = if cli.starts_with("~/") { crate::wsl::shell_path(&cli) } else { crate::wsl::quote(&cli) };
    let script = format!(
        "c={cli}; command -v \"$c\" >/dev/null 2>&1 || c=claude\n\"$c\" mcp remove --scope user monaged >/dev/null 2>&1\n\"$c\" mcp add --scope user monaged -- {} --mcp-debug 2>&1",
        crate::wsl::quote(&wsl_exe)
    );
    crate::wsl::run(distro, &script)
}

// ---- The tools, inside the window ---------------------------------------------------

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty()).or(None)
}

fn number_arg(args: &Value, key: &str) -> Option<i64> {
    args[key].as_i64().or_else(|| args[key].as_str().and_then(|s| s.trim().parse().ok()))
}

impl Monaged {
    pub(crate) fn debug_on(&self) -> bool {
        self.store.setting(SETTING_ON).as_deref() == Some("1")
    }

    pub(crate) fn debug_actions(&self) -> bool {
        self.store.setting(SETTING_ACTIONS).as_deref() == Some("1")
    }

    /// Writes (or removes) debug.json to match the setting.
    pub(crate) fn publish_debug(&self) {
        let on = self.debug_on();
        let result = match &self.bridge {
            Some(bridge) => publish(bridge.port, &bridge.debug_token, on),
            None => publish(0, "", false),
        };
        if let Err(err) = result {
            eprintln!("[debug] couldn't write debug.json: {err:#}");
        }
    }

    pub(crate) fn set_debug(&mut self, on: bool, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(SETTING_ON, if on { "1" } else { "0" });
        if !on {
            let _ = self.store.set_setting(SETTING_ACTIONS, "0");
        }
        self.publish_debug();
        cx.notify();
    }

    pub(crate) fn set_debug_actions(&mut self, on: bool, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(SETTING_ACTIONS, if on { "1" } else { "0" });
        cx.notify();
    }

    pub(crate) fn connect_claude_desktop(&mut self, cx: &mut Context<Self>) {
        self.set_debug(true, cx);
        match install_desktop() {
            Ok(path) => self.toast(Tone::Info, format!("Added Monaged to Claude Desktop ({}). Restart Claude Desktop, then ask it about Monaged.", path.display()), cx),
            Err(err) => self.toast_error(format!("Couldn't add Monaged to Claude Desktop: {err:#}"), cx),
        }
    }

    pub(crate) fn connect_claude_code_wsl(&mut self, cx: &mut Context<Self>) {
        self.set_debug(true, cx);
        let (distro, agent_cmd) = (self.config.distro.clone(), self.config.agent_cmd.clone());
        self.progress("connect-claude", "Adding Monaged to Claude Code in WSL…", cx);
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { install_claude_code_wsl(&distro, &agent_cmd) }).await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(out) if !out.to_lowercase().contains("error") => this.settle(
                        "connect-claude",
                        Tone::Info,
                        "Added: run `claude` in a WSL terminal and ask it about Monaged (/mcp lists the tools).",
                        cx,
                    ),
                    Ok(out) => this.settle("connect-claude", Tone::Error, format!("Claude Code said: {}", out.trim()), cx),
                    Err(err) => this.settle("connect-claude", Tone::Error, format!("Couldn't add Monaged to Claude Code: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Finds an agent by id or name ("name", "subproject/name").
    fn debug_find_agent(&self, wanted: &str) -> Result<i64, String> {
        if let Ok(id) = wanted.parse::<i64>()
            && self.agent(id).is_some()
        {
            return Ok(id);
        }
        let lower = wanted.to_lowercase();
        let (sub, name) = lower.rsplit_once('/').map_or((None, lower.as_str()), |(s, n)| (Some(s), n));
        let matches: Vec<&AgentView> = self
            .agents
            .iter()
            .filter(|a| a.record.name.to_lowercase() == name)
            .filter(|a| sub.is_none_or(|s| self.subprojects.iter().any(|x| x.id == a.record.subproject_id && x.name.to_lowercase() == s)))
            .collect();
        match matches.as_slice() {
            [one] => Ok(one.record.id),
            [] => Err(format!("No agent \"{wanted}\". `projects` lists them with their ids.")),
            many => Err(format!(
                "\"{wanted}\" matches {} agents; pass the id: {}",
                many.len(),
                many.iter().map(|a| format!("{} (id {})", a.record.name, a.record.id)).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    fn debug_find_sub(&self, wanted: &str) -> Result<i64, String> {
        if let Ok(id) = wanted.parse::<i64>()
            && self.subprojects.iter().any(|s| s.id == id)
        {
            return Ok(id);
        }
        let matches: Vec<_> = self.subprojects.iter().filter(|s| s.name.eq_ignore_ascii_case(wanted) || s.branch.eq_ignore_ascii_case(wanted)).collect();
        match matches.as_slice() {
            [one] => Ok(one.id),
            [] => Err(format!("No subproject \"{wanted}\". `projects` lists them.")),
            _ => Err(format!("\"{wanted}\" matches several subprojects; pass the id.")),
        }
    }

    fn debug_find_project(&self, wanted: &str) -> Result<i64, String> {
        if let Ok(id) = wanted.parse::<i64>()
            && self.projects.iter().any(|p| p.id == id)
        {
            return Ok(id);
        }
        self.projects.iter().find(|p| p.name.eq_ignore_ascii_case(wanted)).map(|p| p.id).ok_or_else(|| format!("No project \"{wanted}\"."))
    }

    fn debug_status(&self) -> String {
        let count = |s: State| self.agents.iter().filter(|a| !a.record.archived && agent_state(a) == s).count();
        let screen = match (&self.form, self.sel_agent, self.sel_sub, self.sel_project) {
            (Some(_), _, _, _) => "a page (settings, a form, the guide or similar)".to_string(),
            (None, Some(id), _, _) => format!("agent {} (id {id})", self.agent_name(id)),
            (None, None, Some(s), _) => format!("the dashboard of subproject id {s}"),
            (None, None, None, Some(p)) => format!("the overview of project id {p}"),
            _ => "the welcome screen".to_string(),
        };
        let usage = {
            let week = self.plan_usage.week.as_ref().map(|w| format!("{:.0}% of the plan's week used", w.utilization * 100.0)).unwrap_or_else(|| "the plan's week isn't known yet".into());
            let share = format!("Monaged's share about {:.1}%", self.meter.live_share() * 100.0);
            let blocked = self.budget_block().map(|b| format!("; automatic work is paused: {}", b.message())).unwrap_or_default();
            format!("{week}; {share}{blocked}")
        };
        let mut out = format!(
            "Monaged {} on {}. WSL distro {}. The Claude agent connection is {}.\n\
             Agents: {} working, {} waiting for the user, {} idle, {} not started ({} archived).\n\
             On screen: {screen}.\n\
             Reviews: {}; the quality bar is {}/5; summaries and reviews after every turn {}.\n\
             Usage: {usage}.\n\
             Actions for Claude (send messages, stop, review): {}.",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            self.config.distro,
            if matches!(self.conn, crate::Conn::Ready) { "up" } else { "down" },
            count(State::Working),
            count(State::NeedsYou),
            count(State::Done),
            count(State::Idle),
            self.agents.iter().filter(|a| a.record.archived).count(),
            if self.api_key.is_some() { "with an API key" } else if self.cli_fallback { "on the Claude plan" } else { "off" },
            self.min_score(),
            if self.auto_review { "on" } else { "off" },
            if self.debug_actions() { "allowed" } else { "not allowed (Settings → Connect Claude)" },
        );
        let inbox = self.inbox();
        if !inbox.is_empty() {
            out.push_str(&format!("\nWaiting for the user ({}): {}", inbox.len(), inbox.iter().map(|i| format!("{} ({})", i.title, i.detail)).collect::<Vec<_>>().join("; ")));
        }
        out
    }

    fn debug_projects(&self) -> String {
        if self.projects.is_empty() {
            return "No projects yet.".into();
        }
        let mut out = String::new();
        for p in &self.projects {
            out.push_str(&format!("Project {} (id {}): `{}`, base `{}`\n", p.name, p.id, p.path, p.base_branch));
            for s in self.subprojects.iter().filter(|s| s.project_id == p.id) {
                let stage = self.pipelines.get(&s.id).map(|x| format!(", pipeline {}", x.stage.label())).unwrap_or_default();
                let score = self.sub_confidence(s.id).map(|(c, _)| format!(", confidence {c}/5")).unwrap_or_default();
                out.push_str(&format!("  Subproject {} (id {}, {}, `{}`{stage}{score})\n", s.name, s.id, s.kind, s.branch));
                for a in self.agents.iter().filter(|a| a.record.subproject_id == s.id) {
                    let state = match agent_state(a) {
                        State::NeedsYou => "needs the user",
                        State::Working => "working",
                        State::Done => "idle",
                        State::Idle => "not started",
                    };
                    let score = a.confidence.as_ref().map(|c| format!(", {}/5", c.score)).unwrap_or_default();
                    out.push_str(&format!(
                        "    Agent {} (id {}, {}, {state}{score}{})\n",
                        a.record.name,
                        a.record.id,
                        role_label(&a.role()),
                        if a.record.archived { ", archived" } else { "" }
                    ));
                }
            }
        }
        out
    }

    fn debug_agent(&self, id: i64, items: usize) -> String {
        let Some(a) = self.agent(id) else { return "Gone.".into() };
        let mut out = format!(
            "Agent {} (id {id}), {}, subproject id {}. Branch `{}`, worktree `{}`.{}\n",
            a.record.name,
            role_label(&a.role()),
            a.record.subproject_id,
            a.record.branch,
            a.record.worktree,
            a.model.as_deref().map(|m| format!(" Model {m}.")).unwrap_or_default()
        );
        out.push_str(&format!("State: {:?}; working: {}; paused by budget: {}.\n", a.status_word(), a.is_working(), a.paused));
        if let Some(s) = &a.summary {
            out.push_str(&format!("Summary ({}): {} Next: {}\n", s.status, s.text, s.next_step));
        }
        if let Some(c) = &a.confidence {
            out.push_str(&format!("Confidence {}/5 ({}).\n", c.score, c.verdict));
            for r in &c.reasons {
                out.push_str(&format!("- {r}\n"));
            }
            for i in &c.issues {
                out.push_str(&format!("- Finding [{}] {}: {}\n", i.severity, i.file, i.problem));
            }
            out.push_str(&format!("Checks: {}\n", review::clip(&c.checks, 3000)));
        }
        if let Some(e) = &a.review_error {
            out.push_str(&format!("Review error: {e}\n"));
        }
        if let Some(q) = &a.queued_prompt {
            out.push_str(&format!("Queued message: {}\n", review::clip(q, 500)));
        }
        let start = a.items.len().saturating_sub(items);
        out.push_str(&format!("\nConversation ({} items in memory; the last {}):\n\n", a.items.len(), a.items.len() - start));
        out.push_str(&export::transcript_from_items(&a.items[start..], true));
        if a.items.is_empty() {
            out.push_str("(Not open in this run. `export` reads it from Claude's session files.)");
        }
        out
    }

    fn debug_log(lines: usize, search: Option<&str>) -> String {
        let text = std::fs::read(crate::store::data_dir().join("monaged.log")).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
        let kept: Vec<&str> = text.lines().filter(|l| search.is_none_or(|s| l.to_lowercase().contains(&s.to_lowercase()))).collect();
        let tail = &kept[kept.len().saturating_sub(lines)..];
        if tail.is_empty() { "The log is empty (debug builds write to the console instead).".into() } else { tail.join("\n") }
    }

    /// A debug tool call from Claude (see the module comment).
    pub(crate) fn on_debug_request(&mut self, tool: &str, args: &Value, reply: Sender<ToolReply>, window_cx: &mut Context<Self>) {
        let cx = window_cx;
        let send = |reply: &Sender<ToolReply>, r: ToolReply| {
            let _ = reply.send(ToolReply { text: mask_all(&r.text), ..r });
        };
        if !self.debug_on() {
            send(&reply, ToolReply::error("Connect Claude is off in Monaged's settings."));
            return;
        }
        if ACTION_TOOLS.iter().any(|(n, _)| *n == tool) && !self.debug_actions() {
            send(&reply, ToolReply::error("The user hasn't allowed actions (Settings → Connect Claude → Let Claude act too). Reading works."));
            return;
        }
        let agent_arg = || arg(args, "agent").ok_or_else(|| "Pass `agent` (an id or a name).".to_string());
        let result: Result<String, String> = match tool {
            "status" => Ok(self.debug_status()),
            "projects" => Ok(self.debug_projects()),
            "inbox" => {
                let inbox = self.inbox();
                Ok(if inbox.is_empty() {
                    "Nothing waits for the user.".into()
                } else {
                    inbox.iter().map(|i| format!("- {}: {} (subproject id {}{})", i.title, i.detail, i.sub_id, i.agent_id.map(|a| format!(", agent id {a}")).unwrap_or_default())).collect::<Vec<_>>().join("\n")
                })
            }
            "subproject" => arg(args, "subproject").ok_or_else(|| "Pass `subproject`.".to_string()).and_then(|s| self.debug_find_sub(s)).map(|sub_id| {
                let mut out = self.team_status(sub_id);
                if let Some(p) = self.pipelines.get(&sub_id) {
                    out.push_str(&format!("\n\nRequest:\n{}", p.request));
                    if let Some(plan) = &p.plan {
                        out.push_str(&format!("\n\nPlan:\n{}", serde_json::to_string_pretty(plan).unwrap_or_default()));
                    }
                }
                let board: Vec<String> = self.board.iter().filter(|e| e.subproject_id == sub_id).map(|e| format!("- [{}] {} ({}): {}", e.kind, e.title, e.author, review::clip(&e.body, 400))).collect();
                if !board.is_empty() {
                    out.push_str(&format!("\n\nBoard:\n{}", board.join("\n")));
                }
                let events = self.store.timeline(sub_id, 40).unwrap_or_default();
                if !events.is_empty() {
                    out.push_str("\n\nTimeline (newest first):");
                    for e in events {
                        out.push_str(&format!("\n- {} {} {}", export::civil(e.at.max(0) as u64), e.kind, e.data.replace('\n', " ")));
                    }
                }
                out
            }),
            "agent" => agent_arg().and_then(|a| self.debug_find_agent(a)).map(|id| self.debug_agent(id, number_arg(args, "items").unwrap_or(30).clamp(1, 500) as usize)),
            "changes" => agent_arg().and_then(|a| self.debug_find_agent(a)).map(|id| {
                let a = self.agent(id);
                match a.map(|a| a.changes_raw.as_str()).filter(|d| !d.is_empty()) {
                    Some(diff) => review::clip(diff, 60_000),
                    None => "No diff loaded for this agent yet (open its Changes tab, or `review_agent`).".into(),
                }
            }),
            "log" => Ok(Self::debug_log(number_arg(args, "lines").unwrap_or(200).clamp(1, 5000) as usize, arg(args, "search"))),
            "show" => self.debug_show(args, cx),
            "screenshot" => {
                cx.spawn(async move |_, cx| {
                    // One frame so a `show` just before is on screen.
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    let shot = cx.background_spawn(async move { export::screenshot_png() }).await;
                    let r = match shot {
                        Ok(png) => {
                            use base64::Engine as _;
                            ToolReply { text: "Monaged's window right now.".into(), is_error: false, image: Some(base64::engine::general_purpose::STANDARD.encode(png)) }
                        }
                        Err(err) => ToolReply::error(format!("Couldn't take a screenshot: {err:#}")),
                    };
                    let _ = reply.send(r);
                })
                .detach();
                return;
            }
            "export" => {
                let scope = match arg(args, "project") {
                    Some(p) => match self.debug_find_project(p) {
                        Ok(id) => Scope::Project(id),
                        Err(e) => return send(&reply, ToolReply::error(e)),
                    },
                    None => Scope::Everything,
                };
                let plan = self.export_plan(scope);
                cx.spawn(async move |_, cx| {
                    let written = cx.background_spawn(async move { export::write_export(plan, None) }).await;
                    let r = match written {
                        Ok(w) => ToolReply::ok(format!(
                            "Wrote {} and {} (on the user's Windows PC).\n\n{}",
                            w.report.display(),
                            w.zip.display(),
                            review::clip(&w.report_text, 60_000)
                        )),
                        Err(err) => ToolReply::error(format!("Couldn't export: {err:#}")),
                    };
                    let _ = reply.send(r);
                })
                .detach();
                return;
            }
            "send_message" => agent_arg().and_then(|a| self.debug_find_agent(a)).and_then(|id| {
                let text = arg(args, "text").ok_or("Pass `text`.")?.to_string();
                self.send_prompt(id, text);
                let _ = self.store.log_event(self.agent(id).map(|a| a.record.subproject_id).unwrap_or(0), Some(id), "debug", "Claude (Connect Claude) sent a message");
                Ok(format!("Sent to {}.", self.agent_name(id)))
            }),
            "stop_agent" => agent_arg().and_then(|a| self.debug_find_agent(a)).map(|id| {
                self.stop_agent(id, cx);
                format!("Stopping {}.", self.agent_name(id))
            }),
            "review_agent" => agent_arg().and_then(|a| self.debug_find_agent(a)).map(|id| {
                self.review_agent(id, cx);
                format!("Reviewing {}; ask `agent` for the score in a minute.", self.agent_name(id))
            }),
            other => Err(format!("Unknown tool: {other}")),
        };
        send(&reply, match result {
            Ok(text) => ToolReply::ok(text),
            Err(e) => ToolReply::error(e),
        });
        cx.notify();
    }

    fn debug_show(&mut self, args: &Value, cx: &mut Context<Self>) -> Result<String, String> {
        if let Some(page) = arg(args, "page") {
            match page {
                "settings" => self.open_settings(cx),
                "connect" => self.open_settings_page("Connect Claude", cx),
                "usage" => self.open_settings_page("Usage and budget", cx),
                "health" => self.form = Some(crate::Form::Health),
                "guide" => self.form = Some(crate::Form::Guide),
                other => return Err(format!("No page \"{other}\" (settings, usage, connect, health, guide).")),
            }
            cx.notify();
            return Ok(format!("Showing {page}."));
        }
        if let Some(agent) = arg(args, "agent") {
            let id = self.debug_find_agent(agent)?;
            self.open_agent(id, cx);
            return Ok(format!("Showing agent {}.", self.agent_name(id)));
        }
        if let Some(sub) = arg(args, "subproject") {
            let id = self.debug_find_sub(sub)?;
            self.form = None;
            self.select_sub(id, cx);
            return Ok("Showing the subproject's dashboard.".into());
        }
        if let Some(project) = arg(args, "project") {
            let id = self.debug_find_project(project)?;
            self.form = None;
            self.select_project(id, cx);
            return Ok("Showing the project's overview.".into());
        }
        Err("Pass `project`, `subproject`, `agent` or `page`.".into())
    }
}

trait StatusWord {
    fn status_word(&self) -> String;
}

impl StatusWord for AgentView {
    fn status_word(&self) -> String {
        match &self.status {
            crate::agent::Status::Idle => "idle (no session open)".into(),
            crate::agent::Status::Starting => "starting".into(),
            crate::agent::Status::Loading => "reopening its session".into(),
            crate::agent::Status::Ready => if self.role() == Role::Lead { "ready (lead)".into() } else { "ready".into() },
            crate::agent::Status::Failed(e) => format!("failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_config_keeps_what_was_there() {
        let before = r#"{ "mcpServers": { "files": { "command": "x" } }, "theme": "dark" }"#;
        let after: Value = serde_json::from_str(&add_to_desktop_config(Some(before), r"C:\Monaged\monaged.exe").unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["mcpServers"]["files"]["command"], "x");
        assert_eq!(after["mcpServers"]["monaged"]["command"], r"C:\Monaged\monaged.exe");
        assert_eq!(after["mcpServers"]["monaged"]["args"][0], "--mcp-debug");
        // A missing or empty file starts fresh; a broken one is left alone.
        assert!(add_to_desktop_config(None, "m").unwrap().contains("mcpServers"));
        assert!(add_to_desktop_config(Some("  "), "m").is_ok());
        assert!(add_to_desktop_config(Some("{ not json"), "m").is_err());
        assert!(add_to_desktop_config(Some("[1]"), "m").is_err());
        // Uninstalling takes only Monaged's entry out.
        let with = add_to_desktop_config(Some(before), "m").unwrap();
        let without: Value = serde_json::from_str(&remove_from_desktop_config(&with).unwrap()).unwrap();
        assert!(without["mcpServers"].get("monaged").is_none());
        assert_eq!(without["mcpServers"]["files"]["command"], "x");
        assert!(remove_from_desktop_config(before).is_none(), "nothing to change");
        assert!(remove_from_desktop_config("not json").is_none());
    }

    #[test]
    fn the_debug_tools_are_listed_with_schemas() {
        let tools = tool_list();
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"status") && names.contains(&"screenshot") && names.contains(&"send_message"));
        let agent = tools.as_array().unwrap().iter().find(|t| t["name"] == "agent").unwrap();
        assert_eq!(agent["inputSchema"]["required"][0], "agent");
        assert!(is_debug_args(&["monaged.exe".into(), "--mcp-debug".into()]));
        assert!(!is_debug_args(&["monaged.exe".into()]));
    }
}
