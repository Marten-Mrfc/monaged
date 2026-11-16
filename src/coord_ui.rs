//! Phase 4: coordination inside the app. Agents call Monaged's MCP tools
//! (see `coord.rs`); the requests land here. The board, file claims, the
//! Haiku librarian, routing questions between agents, and the Board view.

use std::sync::mpsc::Sender;
use std::time::Instant;

use gpui::{AnyElement, Context, FontWeight, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::agent::Tone;
use crate::coord::{self, ToolReply, ToolRequest};
use crate::pipeline::{self, Directive, Role};
use crate::icons::{Icon, icon};
use crate::review_ui::render_markdown;
use crate::store::BoardEntry;
use crate::{Monaged, llm, theme, ui};

/// A question from one agent to another that hasn't been answered yet.
pub(crate) struct Ask {
    pub id: u64,
    pub from: i64,
    pub to: i64,
    pub question: String,
    /// The asker's tool call, still waiting (until it times out).
    pub reply: Option<Sender<ToolReply>>,
    pub at: Instant,
}

impl Ask {
    pub(crate) fn waiting(&self) -> bool {
        self.reply.is_some() && self.at.elapsed() < coord::ASK_TIMEOUT
    }
}

const KINDS: [&str; 3] = ["contract", "decision", "note"];

impl Monaged {
    /// The `monaged` MCP server for this agent's session (empty if the bridge failed).
    /// Monaged's own coordination server, plus the project's (Project settings → MCP servers).
    pub(crate) fn mcp_servers(&self, agent_id: i64) -> Value {
        let mut servers = Vec::new();
        if let Some(bridge) = &self.bridge {
            let lead = self.agent(agent_id).is_some_and(|a| a.role() == Role::Lead);
            servers.push(coord::mcp_server_spec(bridge, agent_id, lead));
        }
        servers.extend(self.project_mcp_specs(agent_id));
        Value::Array(servers)
    }

    pub(crate) fn on_tool_request(&mut self, request: ToolRequest, cx: &mut Context<Self>) {
        let ToolRequest { agent, tool, arguments, reply, debug } = request;
        if debug {
            return self.on_debug_request(&tool, &arguments, reply, cx);
        }
        if self.agent(agent).is_none() {
            let _ = reply.send(ToolReply::error("Monaged doesn't know this agent any more."));
            return;
        }
        let answer = match tool.as_str() {
            "board_read" => self.tool_board_read(agent, &arguments),
            "board_post" => self.tool_board_post(agent, &arguments),
            "claim_files" => self.tool_claim_files(agent, &arguments),
            "ask" => return self.tool_ask(agent, &arguments, reply, cx),
            "env_list" => self.tool_env_list(agent),
            "run_with_env" => return self.tool_run_with_env(agent, &arguments, reply, cx),
            other => self.lead_tool(agent, other, &arguments, cx).unwrap_or_else(|| ToolReply::error(format!("Unknown Monaged tool: {other}"))),
        };
        let _ = reply.send(answer);
        cx.notify();
    }

    // ---- Lookups -------------------------------------------------------------------

    pub(crate) fn sub_of(&self, agent_id: i64) -> Option<i64> {
        self.agent(agent_id).map(|a| a.record.subproject_id)
    }

    pub(crate) fn agent_name(&self, agent_id: i64) -> String {
        self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_else(|| "an agent".into())
    }

    /// The agent's own subproject, or the one it named (same project only).
    fn target_sub(&self, agent_id: i64, named: Option<&str>) -> Result<i64, String> {
        let own = self.sub_of(agent_id).ok_or("unknown agent")?;
        let Some(name) = named.map(str::trim).filter(|n| !n.is_empty()) else { return Ok(own) };
        let project = self.subprojects.iter().find(|s| s.id == own).map(|s| s.project_id);
        let siblings: Vec<_> = self.subprojects.iter().filter(|s| Some(s.project_id) == project).collect();
        let wanted = name.to_lowercase();
        siblings
            .iter()
            .find(|s| s.name.to_lowercase() == wanted || crate::git::slug(&s.name) == crate::git::slug(name))
            .map(|s| s.id)
            .ok_or_else(|| {
                let names: Vec<&str> = siblings.iter().map(|s| s.name.as_str()).collect();
                format!("There's no subproject called \"{name}\". Subprojects in this project: {}.", names.join(", "))
            })
    }

    /// Paths an agent may edit: its workstream's `owns` from the plan, plus its claims.
    pub(crate) fn owned_paths(&self, agent_id: i64) -> Vec<(String, &'static str)> {
        let Some(agent) = self.agent(agent_id) else { return Vec::new() };
        let mut out = Vec::new();
        if let Role::Builder(workstream) = agent.role()
            && let Some(plan) = self.pipelines.get(&agent.record.subproject_id).and_then(|p| p.plan.as_ref())
            && let Some(ws) = pipeline::workstreams(plan).into_iter().find(|w| w.name == workstream)
        {
            for path in ws.value["owns"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                out.push((path.to_string(), "owned"));
            }
        }
        for (_, pattern) in self.claims.iter().filter(|(id, _)| *id == agent_id) {
            out.push((pattern.clone(), "claimed"));
        }
        out
    }

    fn role_label(&self, agent_id: i64) -> String {
        match self.agent(agent_id).map(|a| a.role()) {
            Some(Role::Builder(ws)) => format!("builder for {ws}"),
            Some(Role::Researcher) => "researcher".into(),
            Some(Role::Verifier(n)) => format!("verifier {n}"),
            Some(Role::Planner) => "planner".into(),
            Some(Role::Integrator) => "integrator".into(),
            _ => "agent".into(),
        }
    }

    /// One line per agent in the subproject: who they are and what they own.
    fn roster(&self, sub_id: i64, me: i64) -> String {
        let sub_name = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.name.clone()).unwrap_or_default();
        let mut out = format!("Agents in \"{sub_name}\":\n");
        for a in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived) {
            let id = a.record.id;
            let state = if a.is_working() { "working" } else { "idle" };
            let owns: Vec<String> = self.owned_paths(id).into_iter().map(|(p, _)| p).collect();
            let owns = if owns.is_empty() { String::new() } else { format!("; owns {}", owns.join(", ")) };
            let you = if id == me { " (you)" } else { "" };
            out.push_str(&format!("- {}{you}: {}, {state}{owns}\n", a.record.name, self.role_label(id)));
        }
        out
    }

    fn board_seen(&self, agent_id: i64) -> i64 {
        self.store.setting(&format!("agent:{agent_id}:board_seen")).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn mark_board_seen(&mut self, agent_id: i64) {
        let Some(sub_id) = self.sub_of(agent_id) else { return };
        let last = self.board.iter().filter(|e| e.subproject_id == sub_id).map(|e| e.id).max().unwrap_or(0);
        if last > self.board_seen(agent_id) {
            let _ = self.store.set_setting(&format!("agent:{agent_id}:board_seen"), &last.to_string());
        }
    }

    /// Entries on the agent's own board it hasn't seen, split into ones that
    /// touch its files (or concern everyone) and the rest.
    fn unseen_entries(&self, agent_id: i64) -> (Vec<&BoardEntry>, Vec<&BoardEntry>) {
        let Some(sub_id) = self.sub_of(agent_id) else { return (Vec::new(), Vec::new()) };
        let seen = self.board_seen(agent_id);
        let mine: Vec<String> = self.owned_paths(agent_id).into_iter().map(|(p, _)| p).collect();
        let name = self.agent_name(agent_id);
        self.board
            .iter()
            .filter(|e| e.subproject_id == sub_id && e.id > seen && e.agent_id != Some(agent_id))
            .filter(|e| e.delivered_to != Some(agent_id))
            .partition(|e| match e.kind.as_str() {
                // Contracts and decisions are interfaces: they matter most to the
                // agents that *use* the files, not the ones that own them.
                "contract" | "decision" => true,
                "answer" => e.title.starts_with(&format!("{name} →")),
                _ => e.paths.is_empty() || mine.is_empty() || e.paths.iter().any(|p| mine.iter().any(|m| coord::overlaps(p, m))),
            })
    }

    /// Adds unseen board entries to a message that is about to be sent.
    pub(crate) fn with_board_updates(&mut self, agent_id: i64, text: String) -> String {
        if self.bridge.is_none() || text.starts_with("[monaged-ask #") {
            return text;
        }
        let (relevant, others) = self.unseen_entries(agent_id);
        if relevant.is_empty() && others.is_empty() {
            return text;
        }
        let digest = coord::digest(&relevant, &others);
        self.mark_board_seen(agent_id);
        text + &digest
    }

    /// "(3 new board entries …)" for the end of a tool reply.
    fn unseen_hint(&self, agent_id: i64) -> String {
        let (relevant, others) = self.unseen_entries(agent_id);
        match relevant.len() + others.len() {
            0 => String::new(),
            n => format!("\n\n({n} new board entr{} since you last read it; call board_read.)", if n == 1 { "y" } else { "ies" }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn post(
        &mut self,
        sub_id: i64,
        agent_id: Option<i64>,
        author: &str,
        kind: &str,
        title: &str,
        body: &str,
        paths: &[String],
        delivered_to: Option<i64>,
    ) -> Option<i64> {
        match self.store.board_post(sub_id, agent_id, author, kind, title, body, paths, delivered_to) {
            Ok(entry) => {
                let id = entry.id;
                self.board.push(entry);
                Some(id)
            }
            Err(err) => {
                eprintln!("[board] couldn't save: {err:#}");
                None
            }
        }
    }

    // ---- Tools -----------------------------------------------------------------------

    fn tool_board_read(&mut self, agent_id: i64, args: &Value) -> ToolReply {
        let own = self.sub_of(agent_id).unwrap_or_default();
        let sub_id = match self.target_sub(agent_id, args["subproject"].as_str()) {
            Ok(id) => id,
            Err(err) => return ToolReply::error(err),
        };
        let all = args["all"].as_bool().unwrap_or(false) || sub_id != own;
        let seen = self.board_seen(agent_id);
        let mut out = self.roster(sub_id, agent_id);
        if all
            && let Some(plan) = self.pipelines.get(&sub_id).and_then(|p| p.plan.as_ref())
            && let Some(contracts) = plan["contracts"].as_array().filter(|c| !c.is_empty())
        {
            out.push_str("\nContracts from the plan:\n");
            for c in contracts {
                out.push_str(&format!("- {}\n", c.as_str().map_or_else(|| c.to_string(), str::to_owned)));
            }
        }
        let entries: Vec<&BoardEntry> = self
            .board
            .iter()
            .filter(|e| e.subproject_id == sub_id && (all || (e.id > seen && e.agent_id != Some(agent_id))))
            .collect();
        out.push('\n');
        if entries.is_empty() {
            out.push_str(if all {
                "The board is empty."
            } else {
                "Nothing new on the board since you last read it or got it with a message from Monaged. Pass all=true to see everything."
            });
        } else {
            for e in &entries {
                out.push_str(&coord::format_entry(e, 2500));
                out.push('\n');
            }
        }
        if sub_id == own {
            self.mark_board_seen(agent_id);
        }
        ToolReply::ok(out)
    }

    fn tool_board_post(&mut self, agent_id: i64, args: &Value) -> ToolReply {
        let Some(sub_id) = self.sub_of(agent_id) else { return ToolReply::error("unknown agent") };
        let kind = args["kind"].as_str().filter(|k| KINDS.contains(k)).unwrap_or("note");
        let title = args["title"].as_str().unwrap_or_default().trim();
        let body = args["body"].as_str().unwrap_or_default().trim();
        if title.is_empty() || body.is_empty() {
            return ToolReply::error("board_post needs a title and a body.");
        }
        let paths: Vec<String> = args["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(coord::normalize_path)
            .filter(|p| !p.is_empty())
            .take(30)
            .collect();
        let author = self.agent_name(agent_id);
        let Some(id) = self.post(sub_id, Some(agent_id), &author, kind, title, body, &paths, None) else {
            return ToolReply::error("Monaged couldn't save the entry.");
        };
        // The poster has seen everything up to its own entry.
        let _ = self.store.set_setting(&format!("agent:{agent_id}:board_seen"), &self.board_seen(agent_id).max(id - 1).to_string());
        ToolReply::ok(format!("Posted #{id}. The other agents get it with their next message (notes only when they touch their files).{}", self.unseen_hint(agent_id)))
    }

    fn tool_claim_files(&mut self, agent_id: i64, args: &Value) -> ToolReply {
        let paths: Vec<String> = args["paths"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(coord::normalize_path)
            .filter(|p| !p.is_empty())
            .take(50)
            .collect();
        if paths.is_empty() {
            return ToolReply::error("claim_files needs at least one path.");
        }
        let project = self.sub_of(agent_id).and_then(|s| self.subprojects.iter().find(|x| x.id == s)).map(|s| s.project_id);
        let mut warnings = Vec::new();
        let others: Vec<(i64, String, i64)> = self
            .agents
            .iter()
            .filter(|a| a.record.id != agent_id && !a.record.archived)
            .filter(|a| self.subprojects.iter().any(|s| s.id == a.record.subproject_id && Some(s.project_id) == project))
            .map(|a| (a.record.id, a.record.name.clone(), a.record.subproject_id))
            .collect();
        let my_sub = self.sub_of(agent_id);
        for (other, name, sub) in others {
            for (pattern, how) in self.owned_paths(other) {
                for path in paths.iter().filter(|p| coord::overlaps(p, &pattern)) {
                    let place = if Some(sub) == my_sub {
                        String::new()
                    } else {
                        let sub_name = self.subprojects.iter().find(|s| s.id == sub).map(|s| s.name.clone()).unwrap_or_default();
                        format!(" in \"{sub_name}\"")
                    };
                    warnings.push(format!("- {path} overlaps {pattern}, {how} by {name}{place}"));
                }
            }
        }
        for path in &paths {
            if self.store.add_claim(agent_id, path).is_ok() && !self.claims.iter().any(|(id, p)| *id == agent_id && p == path) {
                self.claims.push((agent_id, path.clone()));
            }
        }
        let mut out = format!("Claimed {} path{}.", paths.len(), if paths.len() == 1 { "" } else { "s" });
        if !warnings.is_empty() {
            warnings.dedup();
            out.push_str("\n\nHeads-up, other agents already have these:\n");
            out.push_str(&warnings.join("\n"));
            out.push_str("\n\nDon't edit them without agreeing first: `ask` the owner, or leave the change for them.");
        }
        out.push_str(&self.unseen_hint(agent_id));
        ToolReply::ok(out)
    }

    fn tool_ask(&mut self, from: i64, args: &Value, reply: Sender<ToolReply>, cx: &mut Context<Self>) {
        let question = args["question"].as_str().unwrap_or_default().trim().to_string();
        let to = args["to"].as_str().unwrap_or_default().trim().trim_start_matches('@').to_string();
        if question.is_empty() || to.is_empty() {
            let _ = reply.send(ToolReply::error("ask needs `to` and `question`."));
            return;
        }
        let sub_id = match self.target_sub(from, args["subproject"].as_str()) {
            Ok(id) => id,
            Err(err) => {
                let _ = reply.send(ToolReply::error(err));
                return;
            }
        };
        let board: Vec<BoardEntry> = self.board.iter().filter(|e| e.subproject_id == sub_id).cloned().collect();
        let plan = self.pipelines.get(&sub_id).and_then(|p| p.plan.clone());
        let Some(backend) = self.backend().filter(|_| !board.is_empty() || plan.is_some()) else {
            return self.route_ask(from, sub_id, to, question, reply, cx);
        };
        // The librarian (Haiku) tries the board first: cheap, and no agent is interrupted.
        let asker = self.agent_name(from);
        let prompt = coord::librarian_prompt(&asker, &to, &question, &board, plan.as_ref());
        cx.spawn(async move |view, cx| {
            let via = backend.via();
            let result = cx
                .background_spawn(async move {
                    llm::complete_json(&backend, coord::LIBRARIAN_SYSTEM, &prompt, &coord::librarian_schema())
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok((value, usage)) => {
                        this.log_haiku(from, "librarian", via, &usage);
                        this.refresh_spend();
                        let answer = value["answer"].as_str().unwrap_or_default().trim().to_string();
                        if value["found"].as_bool() == Some(true) && !answer.is_empty() {
                            let title = coord::answer_title(&asker, &to, &question);
                            let text = format!("Answered from the board by Monaged's librarian (not by {to}): {answer}\n\nIf that doesn't settle it, ask again with more detail.");
                            let delivered = reply.send(ToolReply::ok(text)).is_ok();
                            this.post(sub_id, None, "librarian", "answer", &title, &answer, &[], delivered.then_some(from));
                            cx.notify();
                            return;
                        }
                    }
                    Err(err) => eprintln!("[librarian] {err:#}"),
                }
                this.route_ask(from, sub_id, to, question, reply, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Finds the agent a question is for: by name, by workstream, or "lead".
    pub(crate) fn find_target(&self, sub_id: i64, to: &str) -> Option<i64> {
        let wanted = to.to_lowercase();
        let candidates: Vec<_> = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).collect();
        if let Some(a) = candidates.iter().find(|a| a.record.name.to_lowercase() == wanted) {
            return Some(a.record.id);
        }
        // The newest builder for a workstream is the one doing it now.
        if let Some(a) = candidates.iter().rev().find(|a| matches!(a.role(), Role::Builder(ws) if ws == crate::git::slug(to))) {
            return Some(a.record.id);
        }
        if matches!(wanted.as_str(), "lead" | "integrator" | "planner") {
            for role in [Role::Lead, Role::Integrator, Role::Planner] {
                if let Some(a) = candidates.iter().rev().find(|a| a.role() == role) {
                    return Some(a.record.id);
                }
            }
        }
        None
    }

    fn route_ask(&mut self, from: i64, sub_id: i64, to: String, question: String, reply: Sender<ToolReply>, cx: &mut Context<Self>) {
        let Some(target) = self.find_target(sub_id, &to) else {
            let names: Vec<String> = self
                .agents
                .iter()
                .filter(|a| a.record.subproject_id == sub_id && !a.record.archived && a.record.id != from)
                .map(|a| a.record.name.clone())
                .collect();
            let _ = reply.send(ToolReply::error(format!(
                "There's no agent called \"{to}\" here. You can ask: {}, or \"lead\" for the integrator or planner.",
                if names.is_empty() { "nobody yet".into() } else { names.join(", ") }
            )));
            return;
        };
        if target == from {
            let _ = reply.send(ToolReply::error("That's you."));
            return;
        }
        let target_name = self.agent_name(target);
        if let Some(block) = self.budget_block() {
            let _ = reply.send(ToolReply::ok(format!(
                "{target_name} can't be asked right now: {} Make a sensible assumption, post it with board_post (kind \"decision\"), and carry on.",
                block.message()
            )));
            return;
        }
        // Two agents waiting on each other would both time out.
        if self.asks.iter().any(|a| a.from == target && a.waiting()) {
            let _ = reply.send(ToolReply::ok(format!(
                "{target_name} is itself waiting for an answer, so it can't reply now. Make a sensible assumption, post it \
                 with board_post (kind \"decision\"), and carry on."
            )));
            return;
        }
        let id = self.next_ask;
        self.next_ask += 1;
        let asker = self.agent_name(from);
        let busy = self.agent(target).is_some_and(|a| a.is_working());
        self.asks.push(Ask { id, from, to: target, question: question.clone(), reply: Some(reply), at: Instant::now() });
        if let Some(agent) = self.agent_mut(from) {
            let when = if busy { " It's busy, so the question waits until its current turn ends." } else { "" };
            agent.notice(Tone::Info, format!("Asked {target_name}: \"{}\".{when}", crate::review::clip(&question, 200)));
        }
        self.send_prompt(target, coord::ask_message(id, &asker, &question));
        cx.notify();
    }

    /// The agent finished a turn: if that turn answered questions, send the
    /// answers back and record them on the board. True if it did.
    pub(crate) fn resolve_asks(&mut self, agent_id: i64) -> bool {
        let Some(agent) = self.agent(agent_id) else { return false };
        let request = agent.last_request();
        let ids: Vec<u64> =
            self.asks.iter().filter(|a| a.to == agent_id && request.contains(&coord::ask_marker(a.id))).map(|a| a.id).collect();
        if ids.is_empty() {
            return false;
        }
        let reply_text = agent.last_reply();
        let answers: Vec<String> = pipeline::directives(&reply_text)
            .into_iter()
            .filter_map(|d| if let Directive::Answer(v) = d { v["answer"].as_str().map(str::to_owned) } else { None })
            .collect();
        let fallback = crate::review::clip(reply_text.trim(), 1500);
        let to_name = self.agent_name(agent_id);
        for (i, id) in ids.iter().enumerate() {
            let Some(pos) = self.asks.iter().position(|a| a.id == *id) else { continue };
            let ask = self.asks.remove(pos);
            let answer = answers.get(i).or(answers.last()).cloned().unwrap_or_else(|| fallback.clone());
            let from_name = self.agent_name(ask.from);
            let title = coord::answer_title(&from_name, &to_name, &ask.question);
            let delivered = ask.reply.is_some_and(|tx| tx.send(ToolReply::ok(format!("{to_name} answered: {answer}"))).is_ok());
            self.post(agent_id_sub(self, agent_id), Some(agent_id), &to_name, "answer", &title, &answer, &[], delivered.then_some(ask.from));
            if let Some(agent) = self.agent_mut(ask.from) {
                let note = if delivered { "Answer sent back." } else { "It came too late for the waiting call, so it's on the board." };
                agent.notice(Tone::Info, format!("{to_name} answered. {note}"));
            }
        }
        true
    }

    // ---- Rendering ---------------------------------------------------------------------

    pub(crate) fn board_count(&self, sub_id: i64) -> usize {
        self.board.iter().filter(|e| e.subproject_id == sub_id).count()
    }

    pub(crate) fn remove_board_entry(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.store.delete_board_entry(id).is_ok() {
            self.board.retain(|e| e.id != id);
        }
        cx.notify();
    }

    /// The board, as sections of the Pipeline tab: questions in flight, the
    /// entries (newest first; the latest three until you ask for all), and
    /// who owns which files. Nothing when there's nothing to show, except a
    /// hint while builders work.
    pub(crate) fn render_board_sections(&self, sub_id: i64, building: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out: Vec<AnyElement> = Vec::new();

        // Questions in flight.
        let open: Vec<&Ask> = self.asks.iter().filter(|a| self.sub_of(a.to) == Some(sub_id)).collect();
        if !open.is_empty() {
            out.push(
                ui::panel_section("Open questions")
                    .children(open.iter().map(|a| {
                        let secs = a.at.elapsed().as_secs();
                        let state = if a.waiting() { format!("{}m{:02}s", secs / 60, secs % 60) } else { "timed out".into() };
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(ui::status_dot(("ask-dot", a.id as usize), if a.waiting() { ui::State::Working } else { ui::State::Idle }))
                                    .child(ui::one_line(div().flex_1()).child(ui::mono(format!("{} → {}", self.agent_name(a.from), self.agent_name(a.to)))))
                                    .child(ui::mono(state)),
                            )
                            .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(crate::review::clip(&a.question, 200)))
                    }))
                    .into_any_element(),
            );
        }

        let entries: Vec<&BoardEntry> = self.board.iter().filter(|e| e.subproject_id == sub_id).rev().collect();
        let total = entries.len();
        let all = self.board_all.contains(&sub_id);
        if total > 0 || building {
            let mut section = ui::panel_section(if total > 0 { format!("Board · {total}") } else { "Board".to_string() });
            if total == 0 {
                section = section.child(ui::meta("Builders post contracts, decisions and answers here as they work."));
            }
            for e in entries.iter().take(if all { usize::MAX } else { 3 }) {
                let id = e.id;
                section = section.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .pt(px(4.))
                        .group("board-entry")
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .px(px(5.))
                                        .rounded(px(3.))
                                        .border_1()
                                        .border_color(rgb(if e.kind == "contract" { theme::text_3() } else { theme::line_strong() }))
                                        .font_family(theme::MONO)
                                        .text_size(px(10.))
                                        .text_color(rgb(if e.kind == "contract" { theme::text() } else { theme::text_3() }))
                                        .child(e.kind.clone()),
                                )
                                .child(ui::one_line(div().flex_1()).child(ui::mono(format!("{} · {}", e.author, ago(e.at)))))
                                .child(
                                    ui::icon_button(SharedString::from(format!("board-remove-{id}")), Icon::X)
                                        .size(px(20.))
                                        .invisible()
                                        .group_hover("board-entry", |s| s.visible())
                                        .tooltip(crate::shell::tip("Remove from the board"))
                                        .on_click(cx.listener(move |this, _, _, cx| this.remove_board_entry(id, cx))),
                                ),
                        )
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(e.title.clone()))
                        .when(!e.paths.is_empty(), |d| d.child(ui::mono(e.paths.join(", "))))
                        .child(div().text_size(px(13.)).child(render_markdown(&e.body))),
                );
            }
            if total > 3 {
                section = section.child(
                    ui::action_row("board-all", if all { Icon::ChevronDown } else { Icon::ChevronRight }, if all { "Show the latest three".to_string() } else { format!("Show all {total}") }, None).on_click(
                        cx.listener(move |this, _, _, cx| {
                            if !this.board_all.remove(&sub_id) {
                                this.board_all.insert(sub_id);
                            }
                            cx.notify();
                        }),
                    ),
                );
            }
            out.push(section.into_any_element());
        }

        // Who owns what.
        let owners: Vec<AnyElement> = self
            .agents
            .iter()
            .filter(|a| a.record.subproject_id == sub_id && !a.record.archived)
            .filter_map(|a| {
                let owned = self.owned_paths(a.record.id);
                (!owned.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().text_size(px(12.5)).text_color(rgb(theme::text())).child(a.record.name.clone()))
                        .child(div().flex().flex_col().gap(px(2.)).children(owned.into_iter().map(|(p, how)| {
                            ui::one_line(div())
                                .font_family(theme::MONO)
                                .text_size(px(11.5))
                                .text_color(rgb(if how == "claimed" { theme::text_3() } else { theme::text_2() }))
                                .child(if how == "claimed" { format!("{p} (claimed)") } else { p })
                        })))
                        .into_any_element()
                })
            })
            .collect();
        if !owners.is_empty() {
            out.push(ui::panel_section("Who owns which files").children(owners).into_any_element());
        }
        out
    }
}

/// A message Monaged sent on an agent's behalf: another agent's question, or
/// a message with board updates attached (shown folded).
pub(crate) fn render_user_message(text: &str) -> AnyElement {
    if text.starts_with("[monaged-ask #") {
        let first = text.lines().next().unwrap_or_default();
        let asker = first.split("] ").nth(1).and_then(|r| r.split(" asks").next()).unwrap_or("Another agent");
        let question: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("> ").or_else(|| (l == ">").then_some(""))).collect();
        return ui::card()
            .gap(px(6.))
            .child(div().flex().items_center().gap(px(8.)).child(icon(Icon::Ask).size(px(14.))).child(ui::label(format!("Question from {asker}"))))
            .child(div().text_size(px(13.5)).text_color(rgb(theme::text())).child(question.join("\n")))
            .into_any_element();
    }
    let (message, digest) = text.split_once(&format!("\n\n---\n{}", coord::DIGEST_HEADER)).unwrap_or((text, ""));
    let entries = digest.lines().filter(|l| l.starts_with('#') || l.starts_with("- #")).count();
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap(px(4.))
        .child(
            div()
                .max_w(gpui::relative(0.85))
                .px(px(13.))
                .py(px(9.))
                .rounded(px(10.))
                .bg(rgb(theme::raised()))
                .border_1()
                .border_color(rgb(theme::line()))
                .text_size(px(14.))
                .line_height(px(21.))
                .child(if crate::markdown::looks_like_markdown(message) { render_markdown(message) } else { message.to_string().into_any_element() }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.5))
                .text_color(rgb(theme::text_3()))
                .child(icon(Icon::Board).size(px(12.)).text_color(rgb(theme::text_3())))
                .child(format!("{entries} board update{} attached by Monaged", if entries == 1 { "" } else { "s" })),
        )
        .into_any_element()
}

fn agent_id_sub(this: &Monaged, agent_id: i64) -> i64 {
    this.agent(agent_id).map(|a| a.record.subproject_id).unwrap_or_default()
}

fn ago(at: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(at);
    match (now - at).max(0) {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    }
}
