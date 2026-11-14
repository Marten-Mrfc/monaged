//! One agent's conversation state, built from ACP `session/update`s.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::diff::FileDiff;
use crate::review::{Confidence, Summary};
use crate::store::AgentRecord;

#[derive(Clone, Copy)]
pub enum Tone {
    Info,
    Warn,
    Error,
}

pub struct PlanEntry {
    pub content: String,
    pub status: String,
}

pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

pub enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool { id: String, title: String, kind: String, status: String },
    Plan(Vec<PlanEntry>),
    Permission { request_id: Value, title: String, options: Vec<PermOption>, chosen: Option<String> },
    Notice(Tone, String),
}

pub struct Turn {
    pub started: Instant,
    pub first_words: Option<Duration>,
}

#[derive(Clone, PartialEq)]
pub enum Status {
    /// Has (or will get) a session, but it isn't open in this run yet.
    Idle,
    /// Creating its worktree or a new session.
    Starting,
    /// Re-opening a saved session (`session/load` replays its history).
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
pub enum AgentTab {
    Conversation,
    Changes,
}

pub struct AgentView {
    pub record: AgentRecord,
    pub items: Vec<Item>,
    pub turn: Option<Turn>,
    pub status: Status,
    /// The session is open on the current agent connection.
    pub live: bool,
    // Review (Phase 2).
    pub tab: AgentTab,
    pub changes: Option<Vec<FileDiff>>,
    pub changes_raw: String,
    pub changes_truncated: bool,
    pub selected_file: usize,
    pub loading_changes: bool,
    pub summary: Option<Summary>,
    pub confidence: Option<Confidence>,
    pub summarizing: bool,
    pub reviewing: bool,
    pub review_error: Option<String>,
    // Pipeline (Phase 3).
    /// A message to send as soon as the session is open (e.g. the role brief).
    pub queued_prompt: Option<String>,
    /// Chosen options for open question cards: (item index, question index) -> option.
    pub answers: HashMap<(usize, usize), usize>,
    /// Already reminded once to deliver its result block.
    pub nudged: bool,
    // Usage (Phase 6).
    /// The model the session reports, e.g. `claude-sonnet-5-5`.
    pub model: Option<String>,
    /// The session's cumulative cost as last reported (it only grows).
    pub cost_seen: Option<f64>,
    /// Cost of the turn in progress.
    pub turn_cost: f64,
    /// API-equivalent cost of all its turns.
    pub cost_total: f64,
    /// Its next message waits for the budget.
    pub paused: bool,
    /// Reopened a session from before cost tracking: its first cost figure
    /// includes old turns, so it only sets the baseline.
    pub cost_baseline: bool,
}

impl AgentView {
    pub fn new(record: AgentRecord) -> Self {
        Self {
            record,
            items: Vec::new(),
            turn: None,
            status: Status::Idle,
            live: false,
            tab: AgentTab::Conversation,
            changes: None,
            changes_raw: String::new(),
            changes_truncated: false,
            selected_file: 0,
            loading_changes: false,
            summary: None,
            confidence: None,
            summarizing: false,
            reviewing: false,
            review_error: None,
            queued_prompt: None,
            answers: HashMap::new(),
            nudged: false,
            model: None,
            cost_seen: None,
            turn_cost: 0.0,
            cost_total: 0.0,
            paused: false,
            cost_baseline: false,
        }
    }

    pub fn role(&self) -> crate::pipeline::Role {
        crate::pipeline::Role::from_db(&self.record.role)
    }

    /// The first thing the user asked this agent.
    pub fn task(&self) -> String {
        self.items.iter().find_map(|i| if let Item::User(t) = i { Some(t.clone()) } else { None }).unwrap_or_default()
    }

    fn last_user_index(&self) -> Option<usize> {
        self.items.iter().rposition(|i| matches!(i, Item::User(_)))
    }

    pub fn last_request(&self) -> String {
        self.last_user_index().and_then(|i| if let Item::User(t) = &self.items[i] { Some(t.clone()) } else { None }).unwrap_or_default()
    }

    /// The agent's text since the last request.
    pub fn last_reply(&self) -> String {
        let start = self.last_user_index().map_or(0, |i| i + 1);
        self.items[start..]
            .iter()
            .filter_map(|i| if let Item::Agent(t) = i { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Tool calls since the last request, as "title (status)".
    pub fn tools_this_turn(&self) -> Vec<String> {
        let start = self.last_user_index().map_or(0, |i| i + 1);
        self.items[start..]
            .iter()
            .filter_map(|i| if let Item::Tool { title, status, .. } = i { Some(format!("{title} ({status})")) } else { None })
            .collect()
    }

    pub fn is_working(&self) -> bool {
        self.turn.is_some()
    }

    pub fn needs_attention(&self) -> bool {
        self.items.iter().any(|item| matches!(item, Item::Permission { chosen: None, .. }))
    }

    pub fn notice(&mut self, tone: Tone, text: impl Into<String>) {
        self.items.push(Item::Notice(tone, text.into()));
    }

    fn mark_first_words(&mut self) {
        if let Some(turn) = &mut self.turn
            && turn.first_words.is_none()
        {
            turn.first_words = Some(turn.started.elapsed());
        }
    }

    /// Applies one `session/update` (live, or replayed by `session/load`).
    pub fn apply_update(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "user_message_chunk" => {
                if let Some(text) = content_text(&update["content"]) {
                    match self.items.last_mut() {
                        Some(Item::User(existing)) => existing.push_str(&text),
                        _ => self.items.push(Item::User(text)),
                    }
                }
            }
            "agent_message_chunk" => {
                if let Some(text) = content_text(&update["content"]) {
                    self.mark_first_words();
                    match self.items.last_mut() {
                        Some(Item::Agent(existing)) => existing.push_str(&text),
                        _ => self.items.push(Item::Agent(text)),
                    }
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = content_text(&update["content"]) {
                    match self.items.last_mut() {
                        Some(Item::Thought(existing)) => existing.push_str(&text),
                        _ => self.items.push(Item::Thought(text)),
                    }
                }
            }
            "tool_call" => {
                self.mark_first_words();
                self.items.push(Item::Tool {
                    id: update["toolCallId"].as_str().unwrap_or_default().to_owned(),
                    title: update["title"].as_str().unwrap_or("Tool").to_owned(),
                    kind: update["kind"].as_str().unwrap_or_default().to_owned(),
                    status: update["status"].as_str().unwrap_or("pending").to_owned(),
                });
            }
            "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                let existing = self.items.iter_mut().rev().find_map(|item| match item {
                    Item::Tool { id: tool_id, title, status, .. } if tool_id == id => Some((title, status)),
                    _ => None,
                });
                if let Some((title, status)) = existing {
                    if let Some(new_title) = update["title"].as_str() {
                        *title = new_title.to_owned();
                    }
                    if let Some(new_status) = update["status"].as_str() {
                        *status = new_status.to_owned();
                    }
                }
            }
            "plan" => {
                let entries: Vec<PlanEntry> = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|entry| PlanEntry {
                        content: entry["content"].as_str().unwrap_or_default().to_owned(),
                        status: entry["status"].as_str().unwrap_or("pending").to_owned(),
                    })
                    .collect();
                // Plans are re-sent in full; update the current turn's plan in place.
                let last_user = self.items.iter().rposition(|item| matches!(item, Item::User(_))).unwrap_or(0);
                match self.items.iter().rposition(|item| matches!(item, Item::Plan(_))) {
                    Some(index) if index > last_user => self.items[index] = Item::Plan(entries),
                    _ => self.items.push(Item::Plan(entries)),
                }
            }
            _ => {}
        }
    }

    pub fn add_permission_request(&mut self, request_id: Value, params: &Value) {
        let tool_id = params["toolCall"]["toolCallId"].as_str().unwrap_or_default();
        let title = params["toolCall"]["title"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                self.items.iter().rev().find_map(|item| match item {
                    Item::Tool { id, title, .. } if id == tool_id => Some(title.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Run a tool".into());
        let options = params["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|option| PermOption {
                id: option["optionId"].as_str().unwrap_or_default().to_owned(),
                name: option["name"].as_str().unwrap_or("?").to_owned(),
                kind: option["kind"].as_str().unwrap_or_default().to_owned(),
            })
            .collect();
        self.items.push(Item::Permission { request_id, title, options, chosen: None });
    }

    /// Ends the running turn and records a timing summary.
    pub fn finish_turn(&mut self, stop_reason: &str) {
        let Some(turn) = self.turn.take() else { return };
        let total = turn.started.elapsed().as_secs_f32();
        let summary = match turn.first_words {
            Some(first) => format!("Done in {total:.1} s · first words after {:.1} s", first.as_secs_f32()),
            None => format!("Done in {total:.1} s"),
        };
        match stop_reason {
            "end_turn" => self.notice(Tone::Info, summary),
            "cancelled" => self.notice(Tone::Warn, format!("Stopped after {total:.1} s")),
            other => self.notice(Tone::Warn, format!("{summary} (stopped: {other})")),
        }
    }
}

fn content_text(content: &Value) -> Option<String> {
    match content["type"].as_str()? {
        "text" => content["text"].as_str().map(str::to_owned),
        "image" => Some("[image]".into()),
        "resource_link" => Some(format!("[{}]", content["name"].as_str().unwrap_or("link"))),
        _ => None,
    }
}
