//! A project's own settings page (project menu → Project settings):
//! - MCP servers: tools the project's agents get in their sessions (stdio
//!   commands run in WSL, or HTTP), each with a switch and a test that lists
//!   the server's tools;
//! - Knowledge: the project's CLAUDE.md, skills, subagents and commands,
//!   edited in place, plus shared notes every agent of the project reads at
//!   once (a CLAUDE.md above all its worktrees);
//! - Run and test: the commands the Run tab uses (else it finds them itself).

use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::agent::Tone;
use crate::icons::{Icon, icon_sized};
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::wsl::{quote, run, shell_path};
use crate::{Monaged, theme};

/// One MCP server for a project's agents.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct McpServer {
    pub name: String,
    /// A command line (stdio, run in WSL) or an http(s) URL.
    pub target: String,
    /// KEY=value pairs separated by ';'.
    pub env: String,
    pub enabled: bool,
}

impl McpServer {
    fn is_http(&self) -> bool {
        self.target.starts_with("http://") || self.target.starts_with("https://")
    }

    /// The ACP `mcpServers` entry.
    pub fn spec(&self) -> Value {
        let pairs: Vec<(String, String)> =
            self.env.split(';').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).filter(|(k, _)| !k.is_empty()).collect();
        if self.is_http() {
            return json!({ "type": "http", "name": self.name, "url": self.target, "headers": pairs.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>() });
        }
        let mut words = split_command(&self.target).into_iter();
        let command = words.next().unwrap_or_default();
        json!({
            "name": self.name,
            "command": command,
            "args": words.collect::<Vec<_>>(),
            "env": pairs.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
        })
    }
}

/// Splits a command line on spaces, keeping "quoted parts" together.
fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            (_, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

pub(crate) fn load_servers(store: &crate::store::Store, project_id: i64) -> Vec<McpServer> {
    store
        .setting(&format!("mcp:{project_id}"))
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(McpServer {
                            name: s["name"].as_str()?.to_string(),
                            target: s["target"].as_str()?.to_string(),
                            env: s["env"].as_str().unwrap_or_default().to_string(),
                            enabled: s["enabled"].as_bool().unwrap_or(true),
                        })
                    })
                    .collect()
            })
        })
        .unwrap_or_default()
}

pub(crate) fn save_servers(store: &crate::store::Store, project_id: i64, servers: &[McpServer]) {
    let v: Vec<Value> = servers.iter().map(|s| json!({ "name": s.name, "target": s.target, "env": s.env, "enabled": s.enabled })).collect();
    let _ = store.set_setting(&format!("mcp:{project_id}"), &Value::Array(v).to_string());
}

/// Starts a stdio server, asks for its tools, and returns their names.
fn test_server(distro: &str, cwd: &str, server: &McpServer) -> anyhow::Result<Vec<String>> {
    if server.is_http() {
        anyhow::bail!("HTTP servers are checked when a session starts.");
    }
    let env: String = server
        .env
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| format!("{}={} ", k.trim(), quote(v.trim())))
        .collect();
    let script = format!(
        "cd {cwd} 2>/dev/null || cd ~\n\
         ( printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"monaged\",\"version\":\"1\"}}}}}}' \
           '{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}' '{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}}'; sleep 6 ) \
           | {env}timeout 15 {cmd} 2>/dev/null | grep -m1 -E '\"id\" *: *2[^0-9]'",
        cwd = shell_path(cwd),
        cmd = server.target,
    );
    // No answer at all makes grep fail without a message: say what that means.
    let out = run(distro, &script).map_err(|e| {
        let text = format!("{e:#}");
        if text.trim().is_empty() { anyhow::anyhow!("The server didn't answer tools/list. Is the command right?") } else { e }
    })?;
    let reply: Value = serde_json::from_str(out.trim()).map_err(|_| anyhow::anyhow!("The server didn't answer tools/list. Is the command right?"))?;
    Ok(reply["result"]["tools"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(str::to_owned)).collect())
}

/// The knowledge files in a project: (label, path in WSL).
fn list_knowledge(distro: &str, repo: &str, shared: &str) -> anyhow::Result<Vec<(String, String)>> {
    let out = run(
        distro,
        &format!(
            "cd {repo} || exit 1\nfor f in CLAUDE.md .claude/skills/*/SKILL.md .claude/agents/*.md .claude/commands/*.md; do [ -f \"$f\" ] && echo \"$f\"; done; true",
            repo = shell_path(repo)
        ),
    )?;
    let mut files = vec![("Shared notes for every agent".to_string(), shared.to_string())];
    for rel in out.lines().filter(|l| !l.is_empty()) {
        let label = if rel == "CLAUDE.md" {
            "CLAUDE.md (project memory)".to_string()
        } else if let Some(name) = rel.strip_prefix(".claude/skills/").and_then(|r| r.strip_suffix("/SKILL.md")) {
            format!("Skill: {name}")
        } else if let Some(name) = rel.strip_prefix(".claude/agents/").and_then(|r| r.strip_suffix(".md")) {
            format!("Subagent: {name}")
        } else if let Some(name) = rel.strip_prefix(".claude/commands/").and_then(|r| r.strip_suffix(".md")) {
            format!("Command: /{name}")
        } else {
            rel.to_string()
        };
        files.push((label, format!("{}/{}", repo.trim_end_matches('/'), rel)));
    }
    Ok(files)
}

fn read_file(distro: &str, path: &str) -> anyhow::Result<String> {
    run(distro, &format!("cat {} 2>/dev/null || true", shell_path(path)))
}

/// Writes text to a file in WSL (base64, so any content survives).
pub(crate) fn write_file(distro: &str, path: &str, text: &str) -> anyhow::Result<()> {
    let data = crate::attach::base64(text.as_bytes());
    run(distro, &format!("mkdir -p \"$(dirname {p})\" && printf '%s' {d} | base64 -d > {p}", p = shell_path(path), d = quote(&data)))?;
    Ok(())
}

/// The page's state.
pub(crate) struct ProjectPage {
    pub project_id: i64,
    pub servers: Vec<McpServer>,
    pub server_name: Entity<TextInput>,
    pub server_target: Entity<TextInput>,
    pub server_env: Entity<TextInput>,
    /// Test results by server name: Ok(tools) or Err(message); None while running.
    pub tests: std::collections::HashMap<String, Option<Result<Vec<String>, String>>>,
    pub files: Option<Vec<(String, String)>>,
    /// The file open in the editor: (path, editor, saved text).
    pub open: Option<(String, Entity<TextInput>, String)>,
    pub run_dev: Entity<TextInput>,
    pub run_test: Entity<TextInput>,
    pub run_port: Entity<TextInput>,
}

impl Monaged {
    fn project_repo(&self, project_id: i64) -> Option<String> {
        self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone())
    }

    /// Where the shared notes live: a CLAUDE.md above all the project's worktrees.
    pub(crate) fn shared_notes_path(&self, project_id: i64) -> Option<String> {
        let repo = self.project_repo(project_id)?;
        let repo = repo.trim_end_matches('/');
        let (parent, name) = repo.rsplit_once('/').unwrap_or(("", repo));
        Some(format!("{parent}/.monaged/{name}/CLAUDE.md"))
    }

    /// The project's MCP servers that are switched on, as ACP specs.
    pub(crate) fn project_mcp_specs(&self, agent_id: i64) -> Vec<Value> {
        let Some(project_id) = self.agent(agent_id).and_then(|a| self.subprojects.iter().find(|s| s.id == a.record.subproject_id)).map(|s| s.project_id) else {
            return Vec::new();
        };
        // `${NAME}` in env values and headers comes from the project's environment.
        load_servers(&self.store, project_id)
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| {
                let mut spec = s.spec();
                for key in ["env", "headers"] {
                    if let Some(list) = spec[key].as_array_mut() {
                        for pair in list {
                            if let Some(value) = pair["value"].as_str() {
                                pair["value"] = Value::String(self.interpolate_env(project_id, value));
                            }
                        }
                    }
                }
                spec
            })
            .collect()
    }

    pub(crate) fn open_project_settings(&mut self, project_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let input = |placeholder: &str, text: String, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| {
                let mut i = TextInput::new(placeholder, cx);
                i.set_text(text, cx);
                i
            })
        };
        let run = self.store.setting(&format!("run:{project_id}")).and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
        let page = ProjectPage {
            project_id,
            servers: load_servers(&self.store, project_id),
            server_name: input("Name, e.g. docs", String::new(), cx),
            server_target: input("Command in WSL (python3 tools/menu_mcp.py) or an https:// URL", String::new(), cx),
            server_env: input("Environment, optional: KEY=value; OTHER=value", String::new(), cx),
            tests: Default::default(),
            files: None,
            open: None,
            run_dev: input("Dev server command, e.g. npm run dev (empty: detect)", run["dev"].as_str().unwrap_or_default().to_string(), cx),
            run_test: input("Test command, e.g. pytest -q (empty: detect)", run["test"].as_str().unwrap_or_default().to_string(), cx),
            run_port: input("Port, e.g. 3000 (empty: read it from the output)", run["port"].as_u64().map(|p| p.to_string()).unwrap_or_default(), cx),
        };
        self.form = Some(crate::Form::Project(Box::new(page)));
        // The first field on the page: a variable's name.
        self.open_env_ui(project_id, window, cx);
        self.load_knowledge(cx);
        cx.notify();
    }

    fn page(&mut self) -> Option<&mut ProjectPage> {
        match &mut self.form {
            Some(crate::Form::Project(page)) => Some(page),
            _ => None,
        }
    }

    fn add_server(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        let (name, target, env) = (page.server_name.read(cx).text().trim().to_string(), page.server_target.read(cx).text().trim().to_string(), page.server_env.read(cx).text().trim().to_string());
        if name.is_empty() || target.is_empty() {
            self.toast(Tone::Warn, "Give the server a name and a command (or URL).", cx);
            return;
        }
        let Some(page) = self.page() else { return };
        if page.servers.iter().any(|s| s.name == name) {
            self.toast(Tone::Warn, format!("There's already a server called {name}."), cx);
            return;
        }
        page.servers.push(McpServer { name: name.clone(), target, env, enabled: true });
        let (project_id, servers) = (page.project_id, page.servers.clone());
        for input in [page.server_name.clone(), page.server_target.clone(), page.server_env.clone()] {
            input.update(cx, |i, cx| i.clear(cx));
        }
        save_servers(&self.store, project_id, &servers);
        self.test_mcp(&name, cx);
        self.toast(Tone::Info, format!("Added {name}. New sessions get it; restart an agent's session to give it to that agent."), cx);
        cx.notify();
    }

    fn test_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        let Some(page) = self.page() else { return };
        let Some(server) = page.servers.iter().find(|s| s.name == name).cloned() else { return };
        page.tests.insert(name.to_string(), None);
        let project_id = page.project_id;
        let cwd = self.project_repo(project_id).unwrap_or_default();
        let name = name.to_string();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { test_server(&distro, &cwd, &server) }).await;
            view.update(cx, |this, cx| {
                if let Some(page) = this.page() {
                    page.tests.insert(name, Some(result.map_err(|e| format!("{e:#}"))));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn change_server(&mut self, name: &str, remove: bool, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        if remove {
            page.servers.retain(|s| s.name != name);
        } else if let Some(s) = page.servers.iter_mut().find(|s| s.name == name) {
            s.enabled = !s.enabled;
        }
        let (project_id, servers) = (page.project_id, page.servers.clone());
        save_servers(&self.store, project_id, &servers);
        cx.notify();
    }

    fn load_knowledge(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        let project_id = page.project_id;
        let (Some(repo), Some(shared)) = (self.project_repo(project_id), self.shared_notes_path(project_id)) else { return };
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let files = cx.background_spawn(async move { list_knowledge(&distro, &repo, &shared) }).await;
            view.update(cx, |this, cx| {
                if let Some(page) = this.page() {
                    page.files = Some(files.unwrap_or_default());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_knowledge(&mut self, path: String, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let p = path.clone();
            let text = cx.background_spawn(async move { read_file(&distro, &p) }).await.unwrap_or_default();
            view.update(cx, |this, cx| {
                let editor = cx.new(|cx| {
                    let mut input = TextInput::new("Write here. Markdown, with frontmatter for skills, subagents and commands.", cx).multiline().max_lines(28);
                    input.set_text(text.clone(), cx);
                    input
                });
                if let Some(page) = this.page() {
                    page.open = Some((path, editor, text));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_knowledge(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        let Some((path, editor, _)) = &page.open else { return };
        let (path, text, project_id) = (path.clone(), editor.read(cx).text().to_string(), page.project_id);
        let repo = self.project_repo(project_id).unwrap_or_default();
        let distro = self.config.distro.clone();
        let in_repo = path.starts_with(&format!("{}/", repo.trim_end_matches('/')));
        let rel = path.trim_start_matches(&format!("{}/", repo.trim_end_matches('/'))).to_string();
        cx.spawn(async move |view, cx| {
            let t = text.clone();
            let p = path.clone();
            let result = cx
                .background_spawn(async move {
                    write_file(&distro, &p, &t)?;
                    if commit && in_repo {
                        run(&distro, &format!("cd {} && git add -- {r} && git commit -q -m {m} -- {r}", shell_path(&repo), r = quote(&rel), m = quote(&format!("Update {rel} (from Monaged)"))))?;
                    }
                    anyhow::Ok(())
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        if let Some(page) = this.page()
                            && let Some(open) = &mut page.open
                        {
                            open.2 = text;
                        }
                        this.toast(Tone::Info, if commit { "Saved and committed." } else { "Saved." }, cx);
                    }
                    Err(err) => this.toast_error(format!("Couldn't save: {err:#}"), cx),
                }
                this.load_knowledge(cx);
            })
            .ok();
        })
        .detach();
    }

    /// A new skill, subagent or command from a template.
    fn new_knowledge(&mut self, kind: &'static str, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        let (project_id, count) = (page.project_id, page.files.as_ref().map_or(0, |f| f.len()));
        let Some(repo) = self.project_repo(project_id) else { return };
        let slug = format!("new-{kind}-{count}");
        let (path, text) = match kind {
            "skill" => (
                format!("{repo}/.claude/skills/{slug}/SKILL.md"),
                format!("---\nname: {slug}\ndescription: When to use this skill, in one sentence (Claude reads this to decide).\n---\n\n# {slug}\n\nSteps and conventions the agent should follow.\n"),
            ),
            "agent" => (
                format!("{repo}/.claude/agents/{slug}.md"),
                format!("---\nname: {slug}\ndescription: What this subagent is for, and when to hand it work.\ntools: Read, Grep, Glob\n---\n\nYou are a focused helper. Describe its job, its limits and what it returns.\n"),
            ),
            _ => (format!("{repo}/.claude/commands/{slug}.md"), "---\ndescription: What /this command does\n---\n\nThe prompt this command sends. $ARGUMENTS is what follows the command.\n".to_string()),
        };
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let (p, t) = (path.clone(), text.clone());
            let result = cx.background_spawn(async move { write_file(&distro, &p, &t) }).await;
            view.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.load_knowledge(cx);
                    this.open_knowledge(path, cx);
                    this.toast(Tone::Info, "Created from a template; rename it in its frontmatter and file name.", cx);
                }
                Err(err) => this.toast_error(format!("{err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn save_run_config(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.page() else { return };
        let (dev, test, port) = (page.run_dev.read(cx).text().trim().to_string(), page.run_test.read(cx).text().trim().to_string(), page.run_port.read(cx).text().trim().parse::<u64>().ok());
        let project_id = page.project_id;
        let _ = self.store.set_setting(&format!("run:{project_id}"), &json!({ "dev": dev, "test": test, "port": port }).to_string());
        self.toast(Tone::Info, "Saved. The Run tab uses these commands.", cx);
    }

    pub(crate) fn render_project_page(&self, page: &ProjectPage, cx: &mut Context<Self>) -> AnyElement {
        let project = self.projects.iter().find(|p| p.id == page.project_id);
        let name = project.map(|p| p.name.clone()).unwrap_or_default();
        let section = |title: &'static str, blurb: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .py(px(24.))
                .border_t_1()
                .border_color(rgb(theme::line()))
                .child(div().flex().flex_col().gap(px(4.)).child(ui::heading(title)).child(div().text_size(px(13.)).text_color(rgb(theme::text_3())).child(blurb)))
        };

        // MCP servers.
        let mut mcp = section("MCP servers", "Tools this project's agents get: a command that runs in WSL (stdio) or an HTTP URL. Changes reach a session when it starts or restarts. Put secrets in Environment and write ${NAME} here.");
        for (i, server) in page.servers.iter().enumerate() {
            let (n1, n2, n3) = (server.name.clone(), server.name.clone(), server.name.clone());
            let test = page.tests.get(&server.name);
            mcp = mcp.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .bg(rgb(theme::panel()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(ui::switch(SharedString::from(format!("mcp-on-{i}")), "", server.enabled).on_click(cx.listener(move |this, _, _, cx| this.change_server(&n1, false, cx))))
                            .child(div().w(px(160.)).flex_none().text_size(px(13.5)).child(server.name.clone()))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(server.target.clone()))
                            .child(ui::button(("mcp-test", i), Kind::Ghost, None, "Test").on_click(cx.listener(move |this, _, _, cx| this.test_mcp(&n2, cx))))
                            .child(ui::icon_button(("mcp-remove", i), Icon::Trash).on_click(cx.listener(move |this, _, _, cx| this.change_server(&n3, true, cx)))),
                    )
                    .children(test.map(|t| match t {
                        None => div().flex().items_center().gap(px(6.)).child(ui::spinner(("mcp-spin", i), 12., theme::text_3())).child(ui::meta("Starting it and asking for its tools…")).into_any_element(),
                        Some(Ok(tools)) => ui::meta(format!("Works: {} tool{}: {}", tools.len(), if tools.len() == 1 { "" } else { "s" }, tools.join(", "))).into_any_element(),
                        Some(Err(err)) => div().text_size(px(12.)).text_color(rgb(theme::red_text())).child(err.clone()).into_any_element(),
                    })),
            );
        }
        mcp = mcp.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(div().flex().gap(px(8.)).child(div().w(px(200.)).flex_none().child(page.server_name.clone())).child(div().flex_1().child(page.server_target.clone())))
                .child(div().flex().gap(px(8.)).child(div().flex_1().child(page.server_env.clone())).child(ui::button("mcp-add", Kind::Secondary, Some(Icon::Plus), "Add server").on_click(cx.listener(|this, _, _, cx| this.add_server(cx))))),
        );

        // Knowledge.
        let open_path = page.open.as_ref().map(|(p, _, _)| p.clone());
        let list = div().w(px(280.)).flex_none().flex().flex_col().gap(px(1.)).children(match &page.files {
            None => vec![ui::skeleton_text("knowledge-sk", 4).into_any_element()],
            Some(files) => files
                .iter()
                .enumerate()
                .map(|(i, (label, path))| {
                    let p = path.clone();
                    ui::row(("knowledge", i), open_path.as_deref() == Some(path.as_str()))
                        .child(icon_sized(if i == 0 { Icon::Layers } else { Icon::Lines }, 13.).text_color(rgb(theme::text_3())))
                        .child(div().flex_1().min_w(px(0.)).truncate().child(label.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_knowledge(p.clone(), cx)))
                        .into_any_element()
                })
                .collect(),
        });
        let editor: AnyElement = match &page.open {
            None => ui::meta("Pick a file, or start a new skill, subagent or command.").into_any_element(),
            Some((path, editor, saved)) => {
                let dirty = editor.read(cx).text() != saved.as_str();
                let in_repo = project.is_some_and(|p| path.starts_with(&p.path));
                let file = path.clone();
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(path.clone())).when(dirty, |d| d.child(ui::mono("unsaved"))))
                    .child(div().p(px(10.)).rounded(px(6.)).border_1().border_color(rgb(theme::line_strong())).bg(rgb(theme::raised())).font_family(theme::MONO).text_size(px(12.5)).child(editor.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(ui::button("knowledge-save", Kind::Primary, None, "Save").on_click(cx.listener(|this, _, _, cx| this.save_knowledge(false, cx))))
                            .when(in_repo, |d| d.child(ui::button("knowledge-commit", Kind::Secondary, Some(Icon::Commit), "Save and commit").on_click(cx.listener(|this, _, _, cx| this.save_knowledge(true, cx)))))
                            .child(ui::button("knowledge-zed", Kind::Ghost, Some(Icon::External), "Open in Zed").on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(file.clone(), cx)))),
                    )
                    .into_any_element()
            }
        };
        let knowledge = section(
            "Knowledge",
            "What the agents read: CLAUDE.md, skills, subagents and commands from the repo (an agent sees them through its branch), and shared notes that reach every agent of this project at once.",
        )
        .child(
            div()
                .flex()
                .gap(px(6.))
                .child(ui::button("new-skill", Kind::Secondary, Some(Icon::Plus), "Skill").on_click(cx.listener(|this, _, _, cx| this.new_knowledge("skill", cx))))
                .child(ui::button("new-agent-def", Kind::Secondary, Some(Icon::Plus), "Subagent").on_click(cx.listener(|this, _, _, cx| this.new_knowledge("agent", cx))))
                .child(ui::button("new-command", Kind::Secondary, Some(Icon::Plus), "Command").on_click(cx.listener(|this, _, _, cx| this.new_knowledge("command", cx)))),
        )
        .child(div().flex().items_start().gap(px(16.)).child(list).child(editor));

        // Run and test.
        let runs = section("Run and test", "The Run tab starts the dev server and runs the tests in a worktree. Leave a field empty and Monaged looks at monaged.json, package.json, pyproject.toml, Cargo.toml or a Makefile.")
            .child(ui::field("Dev server", None, page.run_dev.clone()))
            .child(ui::field("Tests", None, page.run_test.clone()))
            .child(div().w(px(240.)).child(ui::field("Port", None, page.run_port.clone())))
            .child(div().flex().child(ui::button("run-save", Kind::Primary, None, "Save").on_click(cx.listener(|this, _, _, cx| this.save_run_config(cx)))));

        div()
            .id("project-settings")
            .flex_1()
            .overflow_y_scroll()
            .child(
                div().flex().justify_center().px(px(32.)).py(px(32.)).child(
                    div()
                        .w(px(1000.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(12.))
                                .pb(px(20.))
                                .child(div().flex_1().flex().flex_col().gap(px(4.)).child(ui::title(format!("{name} settings"))).child(ui::meta("For this project only.")))
                                .child(ui::button("project-settings-close", Kind::Secondary, None, "Done").on_click(cx.listener(|this, _, window, cx| this.on_cancel(&crate::Cancel, window, cx)))),
                        )
                        .children(self.render_env_section(cx))
                        .child(mcp)
                        .child(knowledge)
                        .child(runs),
                ),
            )
            .into_any_element()
    }
}
