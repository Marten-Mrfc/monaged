//! The inspector's Run tab: start a worktree's dev server and watch it, and
//! run its tests.
//! - Commands come from Project settings → Run and test, else from the repo
//!   (monaged.json, package.json, pyproject.toml, Cargo.toml, go.mod, a Makefile).
//! - The dev server runs in its own process group in WSL, so Stop ends it and
//!   everything it started. Its output streams in; the port is read from it
//!   and opens in your browser (WSL forwards localhost to Windows).
//! - Tests run once, or three times to find flaky ones (a test that both
//!   passed and failed). Failures can go straight to the agent.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::time::Instant;

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, Context, ScrollHandle, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::agent::Tone;
use crate::git_ui::GitTarget;
use crate::icons::{Icon, icon_sized};
use crate::shell::tip;
use crate::ui::{self, Kind, State};
use crate::wsl::{quote, run, shell_path};
use crate::{Monaged, theme};

const MAX_LOG: usize = 2000;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RunConfig {
    pub dev: String,
    pub test: String,
    pub port: Option<u16>,
    /// Where the commands came from ("Project settings", "package.json"…).
    pub source: String,
}

pub(crate) struct DevServer {
    pub child: Option<Child>,
    pub pgid: Option<u32>,
    pub running: bool,
    pub exit: Option<String>,
    pub port: Option<u16>,
    pub logs: VecDeque<String>,
    pub started: Instant,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TestResult {
    pub passed: bool,
    pub summary: String,
    pub failing: Vec<String>,
    /// Failed in some runs and passed in others.
    pub flaky: Vec<String>,
    pub output: String,
    pub runs: usize,
    pub seconds: f32,
}

#[derive(Default)]
pub(crate) struct RunState {
    pub config: Option<RunConfig>,
    pub dev: Option<DevServer>,
    pub testing: bool,
    pub tests: Option<TestResult>,
    pub show_output: bool,
    pub scroll: ScrollHandle,
}

/// Finds the commands in a worktree.
fn detect(distro: &str, worktree: &str) -> RunConfig {
    let out = run(
        distro,
        &format!(
            "cd {} || exit 0\nfor f in monaged.json package.json Cargo.toml pyproject.toml pytest.ini manage.py go.mod Makefile pnpm-lock.yaml yarn.lock; do [ -f \"$f\" ] && echo \"=== $f\" && head -c 20000 \"$f\" && echo; done; command -v pytest >/dev/null && echo '=== has-pytest'; true",
            shell_path(worktree)
        ),
    )
    .unwrap_or_default();
    let mut files: HashMap<String, String> = HashMap::new();
    let mut current = String::new();
    for line in out.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            current = name.trim().to_string();
            files.insert(current.clone(), String::new());
        } else if let Some(body) = files.get_mut(&current) {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(v) = files.get("monaged.json").and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        return RunConfig {
            dev: v["dev"].as_str().unwrap_or_default().to_string(),
            test: v["test"].as_str().unwrap_or_default().to_string(),
            port: v["port"].as_u64().map(|p| p as u16),
            source: "monaged.json".into(),
        };
    }
    if let Some(v) = files.get("package.json").and_then(|t| serde_json::from_str::<Value>(t).ok()) {
        let pm = if files.contains_key("pnpm-lock.yaml") { "pnpm" } else if files.contains_key("yarn.lock") { "yarn" } else { "npm" };
        let scripts = &v["scripts"];
        let dev = if scripts["dev"].is_string() { format!("{pm} run dev") } else if scripts["start"].is_string() { format!("{pm} start") } else { String::new() };
        let test = if scripts["test"].is_string() { format!("{pm} test") } else { String::new() };
        return RunConfig { dev, test, port: None, source: "package.json".into() };
    }
    if files.contains_key("Cargo.toml") {
        return RunConfig { dev: "cargo run".into(), test: "cargo test".into(), port: None, source: "Cargo.toml".into() };
    }
    if files.contains_key("go.mod") {
        return RunConfig { dev: "go run .".into(), test: "go test ./...".into(), port: None, source: "go.mod".into() };
    }
    if let Some(make) = files.get("Makefile") {
        let has = |t: &str| make.lines().any(|l| l.starts_with(&format!("{t}:")));
        if has("dev") || has("test") {
            return RunConfig {
                dev: if has("dev") { "make dev".into() } else { String::new() },
                test: if has("test") { "make test".into() } else { String::new() },
                port: None,
                source: "Makefile".into(),
            };
        }
    }
    if files.contains_key("pyproject.toml") || files.contains_key("pytest.ini") || files.contains_key("manage.py") {
        let test = if files.contains_key("has-pytest") { "python3 -m pytest -q" } else { "python3 -m unittest discover -v" };
        let dev = if files.contains_key("manage.py") { "python3 manage.py runserver" } else { "" };
        return RunConfig { dev: dev.into(), test: test.into(), port: None, source: "Python project".into() };
    }
    RunConfig::default()
}

/// A port the server says it listens on.
fn find_port(line: &str) -> Option<u16> {
    let lower = line.to_lowercase();
    for marker in ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::]:", "port "] {
        if let Some(at) = lower.find(marker) {
            let digits: String = lower[at + marker.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = digits.parse::<u16>()
                && port >= 80
            {
                return Some(port);
            }
        }
    }
    None
}

/// Failing test names and a one-line summary from common runners' output.
fn parse_tests(output: &str, ok: bool) -> (String, Vec<String>) {
    let mut failing: Vec<String> = Vec::new();
    let mut summary = String::new();
    for line in output.lines() {
        let t = line.trim();
        // pytest: "FAILED path::test - why"; unittest ends with "FAILED (failures=1)", not a name.
        let name = if let Some(rest) = t.strip_prefix("FAILED ").filter(|r| !r.starts_with('(')) {
            Some(rest.split(" - ").next().unwrap_or(rest).to_string())
        } else if let Some(rest) = t.strip_prefix("FAIL: ").or_else(|| t.strip_prefix("ERROR: ")) {
            Some(rest.to_string())
        } else if let Some(rest) = t.strip_prefix("--- FAIL: ") {
            Some(rest.split_whitespace().next().unwrap_or(rest).to_string())
        } else if t.starts_with("test ") && t.ends_with("FAILED") {
            Some(t.trim_start_matches("test ").trim_end_matches("... FAILED").trim().to_string())
        } else { t.strip_prefix("✕ ").or_else(|| t.strip_prefix("× ")).map(|rest| rest.to_string()) };
        if let Some(n) = name
            && !failing.contains(&n)
        {
            failing.push(n);
        }
        let lower = t.to_lowercase();
        if (lower.contains(" passed") || lower.contains(" failed")) && (lower.starts_with('=') || lower.starts_with("tests:") || lower.starts_with("test result:"))
            || lower.starts_with("ran ") && lower.contains(" test")
            || lower == "ok" || lower.starts_with("failed (")
        {
            summary = if summary.is_empty() { t.trim_matches('=').trim().to_string() } else { format!("{summary} · {}", t.trim_matches('=').trim()) };
        }
    }
    if summary.is_empty() {
        summary = if ok { "Tests passed".into() } else { "Tests failed".into() };
    }
    (summary, failing)
}

fn run_tests(distro: &str, worktree: &str, command: &str, times: usize, exports: &str) -> anyhow::Result<TestResult> {
    let start = Instant::now();
    let mut seen_fail: HashMap<String, usize> = HashMap::new();
    let mut last = TestResult::default();
    let mut any_failed = false;
    for _ in 0..times.max(1) {
        let script = format!("cd {} || exit 1\n{exports}/usr/bin/timeout 900 /bin/bash -c {} 2>&1\necho \"MONAGED_EXIT $?\"", shell_path(worktree), quote(command));
        let out = if exports.is_empty() { crate::wsl::run_heavy(distro, &script)? } else { crate::wsl::run_clean(distro, &script)? };
        let out = crate::env::mask_names(&out).0;
        let (body, code) = out.rsplit_once("MONAGED_EXIT ").map(|(b, c)| (b.to_string(), c.trim().parse::<i32>().unwrap_or(1))).unwrap_or((out.clone(), 1));
        let ok = code == 0;
        any_failed |= !ok;
        let (summary, failing) = parse_tests(&body, ok);
        for f in &failing {
            *seen_fail.entry(f.clone()).or_default() += 1;
        }
        last = TestResult { passed: ok, summary, failing, flaky: Vec::new(), output: body, runs: times.max(1), seconds: 0. };
    }
    let runs = times.max(1);
    if runs > 1 {
        last.flaky = seen_fail.iter().filter(|(_, n)| **n < runs).map(|(k, _)| k.clone()).collect();
        last.failing = seen_fail.iter().filter(|(_, n)| **n == runs).map(|(k, _)| k.clone()).collect();
        last.passed = !any_failed;
    }
    last.seconds = start.elapsed().as_secs_f32();
    Ok(last)
}

impl Monaged {
    pub(crate) fn run_dir(&self, sub_id: i64) -> Option<String> {
        let target = self.run_targets.get(&sub_id).copied().unwrap_or_else(|| self.default_run_target(sub_id));
        match target {
            GitTarget::Agent(id) => self.agent(id).filter(|a| !a.record.archived).map(|a| a.record.worktree.clone()),
            GitTarget::Project => {
                let sub = self.subprojects.iter().find(|s| s.id == sub_id)?;
                self.projects.iter().find(|p| p.id == sub.project_id).map(|p| p.path.clone())
            }
        }
    }

    fn default_run_target(&self, sub_id: i64) -> GitTarget {
        match self.sel_agent.and_then(|id| self.agent(id)) {
            Some(a) if a.record.subproject_id == sub_id && !a.record.archived => GitTarget::Agent(a.record.id),
            _ => GitTarget::Project,
        }
    }

    /// The commands for a worktree: Project settings first, else detected.
    pub(crate) fn prepare_run(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(dir) = self.run_dir(sub_id) else { return };
        if self.run_states.get(&dir).is_some_and(|r| r.config.is_some()) {
            return;
        }
        self.run_states.entry(dir.clone()).or_default();
        let project_id = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id).unwrap_or(0);
        let saved = self.store.setting(&format!("run:{project_id}")).and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
        let distro = self.config.distro.clone();
        let d = dir.clone();
        cx.spawn(async move |view, cx| {
            let detected = cx.background_spawn(async move { detect(&distro, &d) }).await;
            view.update(cx, |this, cx| {
                let mut config = detected;
                let pick = |s: &Value| s.as_str().filter(|v| !v.trim().is_empty()).map(str::to_owned);
                if let Some(dev) = pick(&saved["dev"]) {
                    config.dev = dev;
                    config.source = "Project settings".into();
                }
                if let Some(test) = pick(&saved["test"]) {
                    config.test = test;
                    config.source = "Project settings".into();
                }
                if let Some(port) = saved["port"].as_u64() {
                    config.port = Some(port as u16);
                }
                this.run_states.entry(dir).or_default().config = Some(config);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn start_dev(&mut self, dir: String, cx: &mut Context<Self>) {
        let Some(config) = self.run_states.get(&dir).and_then(|r| r.config.clone()) else { return };
        if config.dev.trim().is_empty() {
            self.toast(Tone::Warn, "No dev server command. Set one in Project settings → Run and test.", cx);
            return;
        }
        let exports = self.run_exports(&dir);
        // Variables go in the script over stdin: never in a command line or a file.
        // With any, the shell skips startup files (see wsl::clean_prelude).
        let clean = !exports.expose().is_empty();
        let script = crate::env::Secret::new(format!(
            "export GIT_TERMINAL_PROMPT=0\n{}cd {} || exit 1\n{}exec 2>&1\nexec /usr/bin/setsid --wait /bin/bash -c {} < /dev/null\n",
            if clean { crate::wsl::clean_prelude(&self.config.distro) } else { String::new() },
            shell_path(&dir),
            exports.expose(),
            quote(&format!("echo \"MONAGED_PGID $$\"; {}", config.dev))
        ));
        let mut command = if clean { crate::wsl::clean_command(&self.config.distro) } else {
            let mut c = crate::wsl::command(&self.config.distro);
            c.args(["--", "bash", "-l", "-s"]);
            c
        };
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                self.toast_error(format!("Couldn't start the dev server: {err}"), cx);
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(script.expose().as_bytes());
        }
        let (tx, mut rx) = unbounded::<Option<String>>();
        if let Some(stdout) = child.stdout.take() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.unbounded_send(Some(line)).is_err() {
                        return;
                    }
                }
                let _ = tx.unbounded_send(None);
            });
        }
        let state = self.run_states.entry(dir.clone()).or_default();
        state.dev = Some(DevServer { child: Some(child), pgid: None, running: true, exit: None, port: config.port, logs: VecDeque::new(), started: Instant::now() });
        cx.notify();
        cx.spawn(async move |view, cx| {
            while let Some(event) = rx.next().await {
                let alive = view
                    .update(cx, |this, cx| {
                        let Some(dev) = this.run_states.get_mut(&dir).and_then(|r| r.dev.as_mut()) else { return };
                        match event {
                            Some(line) => {
                                if let Some(pgid) = line.strip_prefix("MONAGED_PGID ").and_then(|p| p.trim().parse().ok()) {
                                    dev.pgid = Some(pgid);
                                    return;
                                }
                                if dev.port.is_none() {
                                    dev.port = find_port(&line);
                                }
                                dev.logs.push_back(crate::env::mask_names(&line).0);
                                while dev.logs.len() > MAX_LOG {
                                    dev.logs.pop_front();
                                }
                            }
                            None => {
                                dev.running = false;
                                let code = dev.child.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
                                dev.exit = Some(match code {
                                    Some(0) | None => "stopped".into(),
                                    Some(c) => format!("exited with code {c}"),
                                });
                            }
                        }
                        if let Some(r) = this.run_states.get(&dir) {
                            r.scroll.scroll_to_bottom();
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn stop_dev(&mut self, dir: &str) {
        let Some(dev) = self.run_states.get_mut(dir).and_then(|r| r.dev.as_mut()) else { return };
        if let Some(pgid) = dev.pgid {
            let distro = self.config.distro.clone();
            std::thread::spawn(move || {
                let _ = run(&distro, &format!("kill -TERM -- -{pgid} 2>/dev/null; sleep 1; kill -KILL -- -{pgid} 2>/dev/null; true"));
            });
        }
        if let Some(child) = dev.child.as_mut() {
            let _ = child.kill();
        }
        dev.running = false;
        dev.exit = Some("stopped".into());
    }

    /// Ends every dev server (Monaged is closing).
    pub(crate) fn stop_all_dev_servers(&mut self) {
        let dirs: Vec<String> = self.run_states.iter().filter(|(_, r)| r.dev.as_ref().is_some_and(|d| d.running)).map(|(d, _)| d.clone()).collect();
        for dir in dirs {
            if let Some(pgid) = self.run_states.get(&dir).and_then(|r| r.dev.as_ref()).and_then(|d| d.pgid) {
                let _ = run(&self.config.distro, &format!("kill -TERM -- -{pgid} 2>/dev/null; true"));
            }
            self.stop_dev(&dir);
        }
    }

    fn start_tests(&mut self, dir: String, times: usize, cx: &mut Context<Self>) {
        let Some(config) = self.run_states.get(&dir).and_then(|r| r.config.clone()) else { return };
        if config.test.trim().is_empty() {
            self.toast(Tone::Warn, "No test command. Set one in Project settings → Run and test.", cx);
            return;
        }
        let state = self.run_states.entry(dir.clone()).or_default();
        if state.testing {
            return;
        }
        state.testing = true;
        let distro = self.config.distro.clone();
        // The project's variables, exported in the script (stdin), never on a command line.
        let exports = self.run_exports(&dir);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let d = dir.clone();
            let result = cx.background_spawn(async move { run_tests(&distro, &d, &config.test, times, exports.expose()) }).await;
            view.update(cx, |this, cx| {
                let state = this.run_states.entry(dir).or_default();
                state.testing = false;
                match result {
                    Ok(r) => {
                        let line = if !r.flaky.is_empty() {
                            format!("{} flaky test{} found over {} runs", r.flaky.len(), if r.flaky.len() == 1 { "" } else { "s" }, r.runs)
                        } else if r.passed {
                            format!("Tests passed · {}", r.summary)
                        } else {
                            format!("Tests failed · {}", r.summary)
                        };
                        state.tests = Some(r);
                        this.toast(Tone::Info, line, cx);
                    }
                    Err(err) => this.toast_error(format!("Couldn't run the tests: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sends the failing tests and their output to the agent.
    fn send_failures(&mut self, agent_id: i64, dir: &str, cx: &mut Context<Self>) {
        let Some(result) = self.run_states.get(dir).and_then(|r| r.tests.clone()) else { return };
        let tail: String = result.output.lines().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        let mut text = format!("The tests fail in your worktree ({}). Please fix them.\n", result.summary);
        if !result.failing.is_empty() {
            text.push_str(&format!("\nFailing: {}\n", result.failing.join(", ")));
        }
        if !result.flaky.is_empty() {
            text.push_str(&format!("Flaky (passed and failed across runs): {}\n", result.flaky.join(", ")));
        }
        text.push_str(&format!("\nLast lines of the output:\n```\n{tail}\n```"));
        self.bypass_budget = true;
        self.send_prompt(agent_id, text);
        self.bypass_budget = false;
        self.toast(Tone::Info, "Sent the failures to the agent.", cx);
        cx.notify();
    }

    pub(crate) fn render_run_inspector(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let target = self.run_targets.get(&sub_id).copied().unwrap_or_else(|| self.default_run_target(sub_id));
        let Some(dir) = self.run_dir(sub_id) else { return ui::panel_section("Run").child(ui::meta("No worktree to run in.")).into_any_element() };
        let project_id = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id).unwrap_or(0);
        let state = self.run_states.get(&dir);
        let config = state.and_then(|s| s.config.clone());

        // Which worktree: a menu (following the selected agent unless you pick one).
        let target_name = match target {
            GitTarget::Agent(id) => self.agent(id).map(|a| a.record.name.clone()).unwrap_or_else(|| "agent".into()),
            GitTarget::Project => "project checkout".into(),
        };
        let elsewhere = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.record.worktree != dir)
            .filter(|a| self.run_states.get(&a.record.worktree).and_then(|r| r.dev.as_ref()).is_some_and(|d| d.running))
            .count();
        let picker = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex_none().text_size(px(12.5)).text_color(rgb(theme::text_3())).child(if elsewhere > 0 { format!("Worktree · {elsewhere} more running") } else { "Worktree".to_string() }))
            .child(div().flex_1())
            .child(ui::menu_chip("run-target", target_name).flex_shrink().min_w(px(0.)).max_w(px(240.)).on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                this.open_menu(crate::menu::MenuTarget::Worktree(sub_id, true), event.position(), window, cx)
            })));

        let Some(config) = config else {
            return ui::panel_section("Run").child(picker).child(ui::skeleton_text("run-sk", 3)).into_any_element();
        };

        // Dev server.
        let dev = state.and_then(|s| s.dev.as_ref());
        let running = dev.is_some_and(|d| d.running);
        let d1 = dir.clone();
        let d2 = dir.clone();
        let mut server = ui::panel_section("Dev server")
            .child(picker)
            .child(div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_2())).child(if config.dev.is_empty() { "No command".to_string() } else { config.dev.clone() }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::status_dot("dev-dot", if running { State::Working } else if dev.and_then(|d| d.exit.as_deref()).is_some_and(|e| e != "stopped") { State::NeedsYou } else { State::Idle }))
                    .child(div().flex_1().text_size(px(12.5)).text_color(rgb(theme::text_2())).child(match dev {
                        Some(d) if d.running => format!("Running for {}s", d.started.elapsed().as_secs()),
                        Some(d) => d.exit.clone().unwrap_or_else(|| "stopped".into()),
                        None => "Not running".into(),
                    }))
                    .child(if running {
                        ui::button("dev-stop", Kind::Secondary, Some(Icon::Stop), "Stop").on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_dev(&d1);
                            cx.notify();
                        }))
                    } else {
                        ui::button("dev-start", Kind::Primary, Some(Icon::Play), "Start").on_click(cx.listener(move |this, _, _, cx| this.start_dev(d2.clone(), cx)))
                    }),
            );
        if let Some(port) = dev.and_then(|d| d.port).filter(|_| running) {
            let url = format!("http://localhost:{port}");
            server = server.child(ui::button("dev-open", Kind::Secondary, Some(Icon::External), format!("Open {url}")).on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url))));
        }
        if let Some(dev) = dev.filter(|d| !d.logs.is_empty()) {
            let scroll = state.map(|s| s.scroll.clone()).unwrap_or_default();
            server = server.child(
                div()
                    .id("dev-logs")
                    .h(px(220.))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(rgb(theme::code_bg()))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .font_family(theme::MONO)
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(rgb(theme::text_2()))
                    .children(dev.logs.iter().rev().take(400).collect::<Vec<_>>().into_iter().rev().enumerate().map(|(i, l)| {
                        let bad = l.to_lowercase().contains("error") || l.contains("Traceback");
                        div().id(("log", i)).text_color(rgb(if bad { theme::red_text() } else { theme::text_2() })).child(l.clone())
                    })),
            );
        }

        // Tests.
        let testing = state.is_some_and(|s| s.testing);
        let result = state.and_then(|s| s.tests.clone());
        let (d3, d4, d5, d6) = (dir.clone(), dir.clone(), dir.clone(), dir.clone());
        let mut tests = ui::panel_section("Tests")
            .child(div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_2())).child(if config.test.is_empty() { "No command".to_string() } else { config.test.clone() }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(if testing {
                        ui::busy_button("tests-run", Kind::Primary, "Running…").into_any_element()
                    } else {
                        ui::button("tests-run", Kind::Primary, Some(Icon::Play), "Run tests").on_click(cx.listener(move |this, _, _, cx| this.start_tests(d3.clone(), 1, cx))).into_any_element()
                    })
                    .when(!testing, |d| {
                        d.child(ui::button("tests-flaky", Kind::Secondary, None, "Run 3× (find flaky)").tooltip(tip("Runs the tests three times; tests that pass and fail are flaky")).on_click(cx.listener(move |this, _, _, cx| this.start_tests(d4.clone(), 3, cx))))
                    }),
            );
        if let Some(r) = &result {
            tests = tests
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon_sized(if r.passed && r.flaky.is_empty() { Icon::Check } else { Icon::Alert }, 14.).text_color(rgb(if r.passed && r.flaky.is_empty() { theme::text_2() } else { theme::red_text() })))
                        .child(div().flex_1().min_w(px(0.)).text_size(px(12.5)).child(format!("{} · {:.1}s{}", r.summary, r.seconds, if r.runs > 1 { format!(" · {} runs", r.runs) } else { String::new() }))),
                )
                .when(!r.failing.is_empty(), |d| {
                    d.child(ui::label("Failing")).children(r.failing.iter().take(20).map(|f| div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::red_text())).child(f.clone())))
                })
                .when(!r.flaky.is_empty(), |d| {
                    d.child(ui::label("Flaky")).children(r.flaky.iter().map(|f| div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text())).child(format!("{f}  (passed and failed)"))))
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(ui::button("tests-output", Kind::Ghost, None, if state.is_some_and(|s| s.show_output) { "Hide output" } else { "Show output" }).on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(s) = this.run_states.get_mut(&d5) {
                                s.show_output = !s.show_output;
                            }
                            cx.notify();
                        })))
                        .when(!(r.passed && r.flaky.is_empty()), |d| {
                            let agent = match target {
                                GitTarget::Agent(id) => Some(id),
                                GitTarget::Project => None,
                            };
                            d.children(agent.map(|agent_id| {
                                let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
                                ui::button("tests-send", Kind::Secondary, Some(Icon::Send), format!("Send failures to {name}")).on_click(cx.listener(move |this, _, _, cx| this.send_failures(agent_id, &d6, cx)))
                            }))
                        }),
                )
                .when(state.is_some_and(|s| s.show_output), |d| {
                    d.child(
                        div()
                            .id("tests-output-box")
                            .max_h(px(300.))
                            .overflow_y_scroll()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .bg(rgb(theme::code_bg()))
                            .font_family(theme::MONO)
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(rgb(theme::text_2()))
                            .children(r.output.lines().rev().take(200).collect::<Vec<_>>().into_iter().rev().map(|l| div().child(l.to_string()))),
                    )
                });
        }

        let footer = ui::panel_section("Commands").child(ui::meta(format!("From {}.", if config.source.is_empty() { "nowhere yet" } else { config.source.as_str() }))).child(
            div().flex().child(ui::button("run-config", Kind::Ghost, Some(Icon::Sliders), "Change in Project settings").on_click(cx.listener(move |this, _, window, cx| this.open_project_settings(project_id, window, cx)))),
        );
        let env = ui::panel_section("Environment").child(self.render_run_env(project_id, &dir, cx));
        div().flex().flex_col().child(server).child(tests).child(env).child(footer).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_ports_and_failures() {
        assert_eq!(super::find_port("  Local:   http://localhost:5173/"), Some(5173));
        assert_eq!(super::find_port("Serving on 0.0.0.0:8765"), Some(8765));
        assert_eq!(super::find_port("nothing here"), None);
        let (summary, failing) = super::parse_tests("FAIL: test_total (tests.test_orders.OrderTests)\n----\nRan 5 tests in 0.01s\n\nFAILED (failures=1)", false);
        assert_eq!(failing, vec!["test_total (tests.test_orders.OrderTests)".to_string()]);
        assert!(summary.contains("Ran 5 tests"));
        let (_, f) = super::parse_tests("FAILED tests/test_x.py::test_y - AssertionError\n===== 1 failed, 3 passed in 0.2s =====", false);
        assert_eq!(f, vec!["tests/test_x.py::test_y".to_string()]);
    }
}
