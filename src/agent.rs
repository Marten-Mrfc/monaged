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

/// One of the session's settings, as the agent offers them (ACP `configOptions`):
/// mode, model, effort, fast mode… Monaged shows whatever the agent reports,
/// so another ACP agent's settings appear the same way.
#[derive(Clone, Debug)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub current: String,
    pub choices: Vec<ConfigChoice>,
}

#[derive(Clone, Debug)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
    pub description: String,
}

impl ConfigOption {
    pub fn current_name(&self) -> String {
        self.choices.iter().find(|c| c.value == self.current).map(|c| c.name.clone()).unwrap_or_else(|| self.current.clone())
    }

    /// An on/off setting (fast mode, ultracode).
    pub fn is_toggle(&self) -> bool {
        self.choices.len() == 2 && self.choices.iter().any(|c| c.value == "on") && self.choices.iter().any(|c| c.value == "off")
    }

    pub fn parse_all(value: &Value) -> Vec<ConfigOption> {
        value.as_array().into_iter().flatten().filter_map(ConfigOption::parse).collect()
    }

    fn parse(value: &Value) -> Option<ConfigOption> {
        let id = value["id"].as_str()?.to_string();
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let current = match &value["currentValue"] {
            Value::String(s) => s.clone(),
            Value::Bool(b) => if *b { "on".into() } else { "off".into() },
            other => other.to_string(),
        };
        let mut choices: Vec<ConfigChoice> = value["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| ConfigChoice { value: text(&o["value"]), name: text(&o["name"]), description: text(&o["description"]) })
            .collect();
        if value["type"] == "boolean" && choices.is_empty() {
            choices = vec![
                ConfigChoice { value: "on".into(), name: "On".into(), description: String::new() },
                ConfigChoice { value: "off".into(), name: "Off".into(), description: String::new() },
            ];
        }
        Some(ConfigOption { name: text(&value["name"]), description: text(&value["description"]), category: text(&value["category"]), id, current, choices })
    }
}

/// A slash command the agent offers (ACP `available_commands_update`).
#[derive(Clone, Debug)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

/// Something you attach to your next message.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub kind: AttachmentKind,
    /// For images: shown as a thumbnail.
    pub preview: Option<std::sync::Arc<gpui::Image>>,
}

#[derive(Clone, Debug)]
pub enum AttachmentKind {
    /// Base64 image data.
    Image { mime: String, data: String },
    /// A text file's contents, sent as an embedded resource.
    Text { uri: String, text: String },
}

impl Attachment {
    /// The ACP content block for this attachment.
    pub fn content_block(&self) -> Value {
        match &self.kind {
            AttachmentKind::Image { mime, data } => serde_json::json!({ "type": "image", "mimeType": mime, "data": data }),
            AttachmentKind::Text { uri, text } => {
                serde_json::json!({ "type": "resource", "resource": { "uri": uri, "mimeType": "text/plain", "text": text } })
            }
        }
    }
}

/// What a tool call carries beyond its one-line title.
#[derive(Clone, Debug, Default)]
pub struct ToolDetail {
    /// The tool's input, pretty-printed.
    pub input: Option<String>,
    pub locations: Vec<String>,
    pub content: Vec<ToolContent>,
}

#[derive(Clone, Debug)]
pub enum ToolContent {
    Text(String),
    Diff { path: String, old: Option<String>, new: String },
    Terminal(String),
}

impl ToolDetail {
    fn merge(&mut self, update: &Value) {
        if !update["rawInput"].is_null() {
            let raw = &update["rawInput"];
            self.input = Some(crate::env::mask(&serde_json::to_string_pretty(raw).unwrap_or_else(|_| raw.to_string())));
        }
        if let Some(locations) = update["locations"].as_array() {
            self.locations = locations
                .iter()
                .filter_map(|l| {
                    let path = l["path"].as_str()?;
                    Some(match l["line"].as_u64() {
                        Some(line) => format!("{path}:{line}"),
                        None => path.to_string(),
                    })
                })
                .collect();
        }
        if let Some(content) = update["content"].as_array() {
            self.content = content
                .iter()
                .filter_map(|c| match c["type"].as_str()? {
                    "content" => content_text(&c["content"]).map(ToolContent::Text),
                    "diff" => Some(ToolContent::Diff {
                        path: c["path"].as_str().unwrap_or_default().to_string(),
                        old: c["oldText"].as_str().map(crate::env::mask),
                        new: crate::env::mask(c["newText"].as_str().unwrap_or_default()),
                    }),
                    "terminal" => c["terminalId"].as_str().map(|t| ToolContent::Terminal(t.to_string())),
                    _ => None,
                })
                .collect();
        }
    }
}

/// A form the agent asks you to fill in (ACP `elicitation/create`): Claude's
/// own question cards, or an MCP server asking for input or a sign-in link.
#[derive(Clone, Debug)]
pub struct Elicitation {
    pub request_id: Value,
    pub message: String,
    /// A link to open instead of a form (MCP sign-in).
    pub url: Option<String>,
    pub fields: Vec<FormField>,
    /// Chosen values by field key (strings, or arrays for multiple choice).
    pub values: HashMap<String, Value>,
    pub state: FormState,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FormState {
    Open,
    /// What you answered, as one line.
    Answered(String),
    Skipped,
    Withdrawn,
}

#[derive(Clone, Debug)]
pub struct FormField {
    pub key: String,
    pub title: String,
    pub description: String,
    pub kind: FieldKind,
}

#[derive(Clone, Debug)]
pub enum FieldKind {
    Single(Vec<ConfigChoice>),
    Multi(Vec<ConfigChoice>),
    Text,
    Bool,
    Number,
}

impl Elicitation {
    pub fn parse(request_id: Value, params: &Value) -> Elicitation {
        let schema = &params["requestedSchema"];
        let mut keys: Vec<&String> = schema["properties"].as_object().map(|o| o.keys().collect()).unwrap_or_default();
        // "question_10" after "question_2": natural order, as the agent numbered them.
        keys.sort_by_key(|k| natural_key(k));
        let fields = keys
            .into_iter()
            .map(|key| {
                let prop = &schema["properties"][key.as_str()];
                let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
                let choices = |list: &Value| -> Vec<ConfigChoice> {
                    list.as_array()
                        .into_iter()
                        .flatten()
                        .map(|o| match o {
                            Value::String(s) => ConfigChoice { value: s.clone(), name: s.clone(), description: String::new() },
                            o => ConfigChoice {
                                value: o["const"].as_str().map(str::to_owned).unwrap_or_else(|| o["const"].to_string()),
                                name: o["title"].as_str().or(o["const"].as_str()).unwrap_or_default().to_string(),
                                description: text(&o["description"]),
                            },
                        })
                        .collect()
                };
                let kind = match prop["type"].as_str().unwrap_or("string") {
                    "array" => {
                        let items = &prop["items"];
                        let list = if !items["anyOf"].is_null() { &items["anyOf"] } else if !items["oneOf"].is_null() { &items["oneOf"] } else { &items["enum"] };
                        FieldKind::Multi(choices(list))
                    }
                    "boolean" => FieldKind::Bool,
                    "number" | "integer" => FieldKind::Number,
                    _ if !prop["oneOf"].is_null() => FieldKind::Single(choices(&prop["oneOf"])),
                    _ if !prop["enum"].is_null() => FieldKind::Single(choices(&prop["enum"])),
                    _ => FieldKind::Text,
                };
                FormField { key: key.clone(), title: text(&prop["title"]), description: text(&prop["description"]), kind }
            })
            .collect();
        Elicitation {
            request_id,
            message: params["message"].as_str().unwrap_or_default().to_string(),
            url: (params["mode"] == "url").then(|| params["url"].as_str().unwrap_or_default().to_string()),
            fields,
            values: HashMap::new(),
            state: FormState::Open,
        }
    }

    /// One line describing the answers, for the transcript.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        for field in &self.fields {
            match self.values.get(&field.key) {
                Some(Value::String(s)) if !s.trim().is_empty() => parts.push(s.trim().to_string()),
                Some(Value::Array(a)) if !a.is_empty() => {
                    parts.push(a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                }
                Some(Value::Bool(b)) => parts.push(if *b { "yes".into() } else { "no".into() }),
                Some(Value::Number(n)) => parts.push(n.to_string()),
                _ => {}
            }
        }
        if parts.is_empty() { "Answered".into() } else { parts.join(" · ") }
    }
}

fn natural_key(key: &str) -> (String, u64, String) {
    let digits: String = key.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let prefix: String = key.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let rest = key[prefix.len() + digits.len()..].to_string();
    (prefix, digits.parse().unwrap_or(0), rest)
}

pub enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool { id: String, title: String, kind: String, status: String, detail: ToolDetail },
    Plan(Vec<PlanEntry>),
    Permission { request_id: Value, title: String, options: Vec<PermOption>, chosen: Option<String> },
    Notice(Tone, String),
    /// The agent asks you to fill in a form (questions, input, a sign-in link).
    Elicit(Elicitation),
    /// Files and images sent with the message before this item (name, image).
    Attachments(Vec<(String, Option<std::sync::Arc<gpui::Image>>)>),
    /// An image the agent showed.
    AgentImage(std::sync::Arc<gpui::Image>),
}

/// Your pick on one question.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// One option (or several, on a question that allows it).
    Options(Vec<usize>),
    /// "Something else": the words are in the question's text box.
    Other,
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
    /// Secrets whose values are in the diff (by name).
    pub changes_secrets: Vec<String>,
    /// Secrets whose values showed up in the conversation, not reported yet.
    pub leaks: Vec<String>,
    pub selected_file: usize,
    pub loading_changes: bool,
    pub summary: Option<Summary>,
    pub confidence: Option<Confidence>,
    pub summarizing: bool,
    pub reviewing: bool,
    /// A review is running but the quality gate needs one of what's there now.
    pub review_again: bool,
    /// It worked since its last review: the score may not fit what's there now.
    pub review_stale: bool,
    pub review_error: Option<String>,
    // Pipeline (Phase 3).
    /// A message to send as soon as the session is open (e.g. the role brief).
    pub queued_prompt: Option<String>,
    /// What you picked on open questions so far: (item index, question index) -> pick.
    pub answers: HashMap<(usize, usize), Pick>,
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
    // The agent protocol's own features.
    /// Mode, model, effort and whatever else the agent lets you set.
    pub config: Vec<ConfigOption>,
    pub commands: Vec<SlashCommand>,
    /// Files and images waiting to go with the next message.
    pub attachments: Vec<Attachment>,
    /// The title the agent gave the session.
    pub title: Option<String>,
    /// The agent is compacting its context.
    pub compacting: bool,
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
            changes_secrets: Vec::new(),
            leaks: Vec::new(),
            selected_file: 0,
            loading_changes: false,
            summary: None,
            confidence: None,
            summarizing: false,
            reviewing: false,
            review_again: false,
            review_stale: false,
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
            config: Vec::new(),
            commands: Vec::new(),
            attachments: Vec::new(),
            title: None,
            compacting: false,
        }
    }

    pub fn config_option(&self, id: &str) -> Option<&ConfigOption> {
        self.config.iter().find(|o| o.id == id)
    }

    /// An open form waiting for your answer.
    pub fn open_form(&self) -> bool {
        self.items.iter().any(|i| matches!(i, Item::Elicit(e) if e.state == FormState::Open))
    }

    /// Takes the agent's configOptions (from session/new, session/load or an update).
    pub fn set_config(&mut self, value: &Value) {
        let parsed = ConfigOption::parse_all(value);
        if !parsed.is_empty() {
            self.config = parsed;
        }
    }

    /// The messages you typed yourself, oldest first (for Up in the message box).
    pub fn typed_messages(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::User(t) if !t.starts_with(crate::pipeline::BRIEF_HEADER) && !t.starts_with("[monaged-") && !t.contains(crate::coord::DIGEST_HEADER) => {
                    Some(t.trim().to_string())
                }
                _ => None,
            })
            .filter(|t| !t.is_empty() && t.len() < 4000)
            .collect()
    }

    pub fn role(&self) -> crate::pipeline::Role {
        crate::pipeline::Role::from_db(&self.record.role)
    }

    /// The first thing the user asked this agent.
    pub fn task(&self) -> String {
        self.items.iter().find_map(|i| if let Item::User(t) = i { Some(t.clone()) } else { None }).unwrap_or_default()
    }

    /// What you said after the task, oldest first (the reviewer reads these).
    pub fn follow_ups(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| if let Item::User(t) = i { Some(t) } else { None })
            .skip(1)
            .filter(|t| !t.starts_with("[monaged-") && !t.contains(crate::coord::DIGEST_HEADER))
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
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
    /// Secret values are masked on the way in; ones that showed up are noted.
    pub fn apply_update(&mut self, update: &Value) {
        self.apply_update_inner(update);
        self.leaks.extend(crate::env::take_found());
    }

    fn apply_update_inner(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str().unwrap_or_default() {
            "user_message_chunk" => {
                // Images you attached come back with the history: show them, not "[image]".
                if update["content"]["type"] == "image"
                    && let Some(image) = crate::media::from_content(&update["content"])
                {
                    match self.items.last_mut() {
                        Some(Item::Attachments(list)) => list.push(("image".into(), Some(image))),
                        _ => self.items.push(Item::Attachments(vec![("image".into(), Some(image))])),
                    }
                    return;
                }
                if let Some(text) = content_text(&update["content"]) {
                    match self.items.last_mut() {
                        // A secret can be split across chunks: mask the whole message.
                        Some(Item::User(existing)) => {
                            existing.push_str(&text);
                            crate::env::mask_in_place(existing);
                        }
                        _ => self.items.push(Item::User(text)),
                    }
                }
            }
            "agent_message_chunk" => {
                if update["content"]["type"] == "image"
                    && let Some(image) = crate::media::from_content(&update["content"])
                {
                    self.items.push(Item::AgentImage(image));
                    return;
                }
                if let Some(text) = content_text(&update["content"]) {
                    self.mark_first_words();
                    match self.items.last_mut() {
                        Some(Item::Agent(existing)) => {
                            existing.push_str(&text);
                            crate::env::mask_in_place(existing);
                        }
                        _ => self.items.push(Item::Agent(text)),
                    }
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = content_text(&update["content"]) {
                    match self.items.last_mut() {
                        Some(Item::Thought(existing)) => {
                            existing.push_str(&text);
                            crate::env::mask_in_place(existing);
                        }
                        _ => self.items.push(Item::Thought(text)),
                    }
                }
            }
            "tool_call" => {
                self.mark_first_words();
                let mut detail = ToolDetail::default();
                detail.merge(update);
                self.items.push(Item::Tool {
                    id: update["toolCallId"].as_str().unwrap_or_default().to_owned(),
                    title: crate::env::mask(update["title"].as_str().unwrap_or("Tool")),
                    kind: update["kind"].as_str().unwrap_or_default().to_owned(),
                    status: update["status"].as_str().unwrap_or("pending").to_owned(),
                    detail,
                });
            }
            "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or_default();
                let existing = self.items.iter_mut().rev().find_map(|item| match item {
                    Item::Tool { id: tool_id, title, status, detail, .. } if tool_id == id => Some((title, status, detail)),
                    _ => None,
                });
                if let Some((title, status, detail)) = existing {
                    if let Some(new_title) = update["title"].as_str() {
                        *title = new_title.to_owned();
                    }
                    if let Some(new_status) = update["status"].as_str() {
                        *status = new_status.to_owned();
                    }
                    detail.merge(update);
                }
            }
            "config_option_update" => {
                if update["configOptions"].is_array() {
                    self.set_config(&update["configOptions"]);
                } else if let (Some(id), Some(value)) = (update["configId"].as_str(), update["value"].as_str())
                    && let Some(option) = self.config.iter_mut().find(|o| o.id == id) {
                        option.current = value.to_string();
                    }
            }
            "current_mode_update" => {
                if let Some(mode) = update["currentModeId"].as_str()
                    && let Some(option) = self.config.iter_mut().find(|o| o.id == "mode")
                {
                    option.current = mode.to_string();
                }
            }
            "available_commands_update" => {
                self.commands = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| {
                        Some(SlashCommand {
                            name: c["name"].as_str()?.to_string(),
                            description: c["description"].as_str().unwrap_or_default().to_string(),
                            hint: c["input"]["hint"].as_str().map(str::to_owned),
                        })
                    })
                    .collect();
            }
            "session_info_update" => {
                if let Some(title) = update["title"].as_str().filter(|t| !t.trim().is_empty()) {
                    self.title = Some(title.to_string());
                }
            }
            "compaction_update" => {
                let state = update["state"].as_str().or(update["status"].as_str()).unwrap_or_default();
                let done = matches!(state, "completed" | "complete" | "done" | "finished" | "failed");
                if self.compacting && done {
                    self.notice(Tone::Info, "Compacted the conversation to free up context.");
                }
                self.compacting = !done;
            }
            "notice" => {
                let tone = match update["severity"].as_str() {
                    Some("error") => Tone::Error,
                    Some("warning" | "warn") => Tone::Warn,
                    _ => Tone::Info,
                };
                let title = update["title"].as_str().unwrap_or_default();
                let text = match update["description"].as_str() {
                    Some(d) if !d.is_empty() => format!("{title}: {d}"),
                    _ => title.to_string(),
                };
                if !text.is_empty() {
                    self.notice(tone, text);
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
        "text" => content["text"].as_str().map(crate::env::mask),
        "image" => Some("[image]".into()),
        "resource_link" => Some(format!("[{}]", content["name"].as_str().unwrap_or("link"))),
        _ => None,
    }
}
