//! Exporting for analysis: one Markdown report that any chat (or person) can
//! read, and a zip with everything in full (every conversation, the pipeline
//! data as JSON, git's view, the app log and a screenshot of the window).
//! Secret values and anything that looks like a token are masked first.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::{AppContext as _, Context};
use serde_json::{Value, json};

use crate::agent::{AgentView, Item, Tone, ToolContent};
use crate::pipeline;
use crate::shell::{agent_state, role_label};
use crate::ui::State;
use crate::{Monaged, review, wsl};

/// The single report stays readable in a chat: transcripts in it are cut to this.
const REPORT_TRANSCRIPT_CHARS: usize = 12_000;
/// A Claude session file read from WSL, at most.
const MAX_SESSION_BYTES: usize = 8_000_000;

// ---- Masking ---------------------------------------------------------------------

/// Prefixes of tokens and keys that must never leave the machine.
const TOKEN_PREFIXES: [&str; 12] =
    ["sk-ant-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "AKIA", "AIza"];

/// Replaces anything shaped like a token or key (known prefixes, bearer
/// headers, private key blocks, `password=…`) with `[masked]`.
pub fn scrub_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_key_block = false;
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
            in_key_block = true;
            out.push_str("[masked private key]");
            continue;
        }
        if in_key_block {
            if line.contains("-----END") {
                in_key_block = false;
            }
            continue;
        }
        out.push_str(&scrub_line(line));
    }
    out
}

fn scrub_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while !rest.is_empty() {
        // The next word start where a token begins.
        let hit = TOKEN_PREFIXES
            .iter()
            .filter_map(|p| rest.find(p).map(|i| (i, *p)))
            .filter(|(i, _)| *i == 0 || !rest[..*i].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric()))
            .min_by_key(|(i, _)| *i);
        let Some((at, prefix)) = hit else {
            out.push_str(rest);
            break;
        };
        let tail = &rest[at + prefix.len()..];
        let len = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(tail.len());
        out.push_str(&rest[..at]);
        if len >= 12 {
            out.push_str("[masked token]");
        } else {
            out.push_str(&rest[at..at + prefix.len() + len]);
        }
        rest = &tail[len..];
    }
    // "Authorization: Bearer xyz", "password=xyz", "token: xyz"
    let lower = out.to_lowercase();
    for key in ["bearer ", "password=", "password: ", "passwd=", "secret=", "token=", "api_key=", "apikey="] {
        if let Some(at) = lower.find(key) {
            let start = at + key.len();
            let value_len = out[start..].find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '&').unwrap_or(out.len() - start);
            if value_len >= 6 {
                out.replace_range(start..start + value_len, "[masked]");
            }
        }
    }
    out
}

/// Vault values by name, then token shapes.
pub fn mask_all(text: &str) -> String {
    scrub_tokens(&crate::env::mask_names(text).0)
}

// ---- Transcripts -----------------------------------------------------------------

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut} […]")
}

fn quote_block(text: &str) -> String {
    text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n")
}

/// An agent's conversation as Markdown, from what Monaged has in memory.
pub fn transcript_from_items(items: &[Item], full: bool) -> String {
    let limit = |n: usize| if full { usize::MAX } else { n };
    let mut out = Vec::new();
    for item in items {
        let block = match item {
            Item::User(text) if text.starts_with(pipeline::BRIEF_HEADER) => {
                let title = text.lines().next().unwrap_or_default().trim_start_matches('#').trim();
                if full { format!("**{title}** (sent by Monaged)\n\n{}", quote_block(text)) } else { format!("**{title}** (sent by Monaged, {} lines)", text.lines().count()) }
            }
            Item::User(text) if text.starts_with(crate::lead::UPDATE_PREFIX) => format!("_Update from Monaged:_ {}", text.trim_start_matches(crate::lead::UPDATE_PREFIX).trim()),
            Item::User(text) => format!("**You:**\n\n{}", quote_block(&clip_chars(text, limit(4000)))),
            Item::Agent(text) => format!("**Agent:**\n\n{}", clip_chars(text, limit(6000))),
            Item::Thought(text) => format!("_Thinking:_ {}", clip_chars(&text.replace('\n', " "), limit(600))),
            Item::Tool { title, kind, status, detail, .. } => {
                let mut line = format!("- Tool `{kind}`: {title} ({status})");
                if full {
                    if let Some(input) = &detail.input {
                        line.push_str(&format!("\n  Input: `{}`", clip_chars(&input.replace('\n', " "), 600)));
                    }
                    for content in &detail.content {
                        match content {
                            ToolContent::Text(t) | ToolContent::Terminal(t) => {
                                line.push_str(&format!("\n\n```\n{}\n```", clip_chars(t, 3000)));
                            }
                            ToolContent::Diff { path, old, new } => {
                                let removed = old.as_deref().map_or(0, |o| o.lines().count());
                                line.push_str(&format!("\n  Edited {path} ({} lines, was {removed})", new.lines().count()));
                            }
                        }
                    }
                }
                line
            }
            Item::Plan(entries) => {
                let list: Vec<String> = entries.iter().map(|e| format!("  - [{}] {}", e.status, e.content)).collect();
                format!("- Plan:\n{}", list.join("\n"))
            }
            Item::Permission { title, chosen, .. } => format!("- Asked permission: {title} → {}", chosen.as_deref().unwrap_or("not answered yet")),
            Item::Notice(tone, text) => format!("_{}: {text}_", if matches!(tone, Tone::Info) { "Note" } else { "Warning" }),
            Item::Elicit(form) => format!("- Asked you (form): {}", form.message),
            Item::Attachments(files) => format!("- Attached: {}", files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")),
            Item::AgentImage(_) => "- (showed an image)".to_string(),
        };
        out.push(block);
    }
    out.join("\n\n")
}

/// A Claude Code session file (`~/.claude/projects/…/<id>.jsonl`) as Markdown.
pub fn transcript_from_session(jsonl: &str) -> String {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let kind = entry["type"].as_str().unwrap_or_default();
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let content = &entry["message"]["content"];
        let mut parts = Vec::new();
        if let Some(text) = content.as_str() {
            parts.push(text.to_string());
        }
        for block in content.as_array().into_iter().flatten() {
            match block["type"].as_str().unwrap_or_default() {
                "text" => parts.push(block["text"].as_str().unwrap_or_default().to_string()),
                "thinking" => parts.push(format!("_Thinking:_ {}", clip_chars(&block["thinking"].as_str().unwrap_or_default().replace('\n', " "), 600))),
                "tool_use" => parts.push(format!(
                    "- Tool `{}`: `{}`",
                    block["name"].as_str().unwrap_or("?"),
                    clip_chars(&block["input"].to_string(), 600)
                )),
                "tool_result" => {
                    let text = match &block["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(items) => items.iter().filter_map(|i| i["text"].as_str()).collect::<Vec<_>>().join("\n"),
                        _ => String::new(),
                    };
                    let error = if block["is_error"].as_bool().unwrap_or(false) { " (error)" } else { "" };
                    parts.push(format!("  Result{error}:\n\n```\n{}\n```", clip_chars(text.trim(), 3000)));
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            continue;
        }
        let who = if kind == "user" {
            if parts.iter().all(|p| p.starts_with("  Result")) { "" } else { "**You (or Monaged):**\n\n" }
        } else {
            "**Agent:**\n\n"
        };
        out.push(format!("{who}{}", parts.join("\n\n")));
    }
    out.join("\n\n")
}

// ---- Zip -------------------------------------------------------------------------

/// A zip archive (deflated), for a handful of text files and a PNG.
pub fn zip(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    // 2026-01-01 00:00 in DOS time; dates inside the zip don't matter here.
    let (dos_time, dos_date): (u16, u16) = (0, ((2026 - 1980) << 9) | (1 << 5) | 1);
    for (name, data) in files {
        let crc = crc32fast::hash(data);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data)?;
        let packed = encoder.finish()?;
        anyhow::ensure!(packed.len() < u32::MAX as usize && data.len() < u32::MAX as usize, "{name} is too large for a zip");
        let offset = out.len() as u32;
        let name_bytes = name.as_bytes();
        // Local file header; bit 11: the name is UTF-8.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&(1u16 << 11).to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&dos_time.to_le_bytes());
        out.extend_from_slice(&dos_date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(&packed);
        // Its central directory entry.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&(1u16 << 11).to_le_bytes());
        central.extend_from_slice(&8u16.to_le_bytes());
        central.extend_from_slice(&dos_time.to_le_bytes());
        central.extend_from_slice(&dos_date.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0u8; 8]); // extra, comment, disk, internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, files.len() as u16);
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    Ok(out)
}

/// RGBA pixels as a PNG file.
pub fn png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = ::png::Encoder::new(&mut out, width, height);
        encoder.set_color(::png::ColorType::Rgba);
        encoder.set_depth(::png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

/// A screenshot of Monaged's window as PNG.
pub fn screenshot_png() -> Result<Vec<u8>> {
    let (width, height, rgba) = crate::native::capture_own_window()?;
    png(width, height, &rgba)
}

// ---- Gathering -------------------------------------------------------------------

/// What an export is about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Project(i64),
    Everything,
}

/// An agent whose conversation isn't in memory: read its session from WSL.
struct Fetch {
    path: String,
    name: String,
    session_id: String,
    distro: String,
}

/// Everything gathered on the UI thread; the rest happens in the background.
pub struct Plan {
    title: String,
    report: String,
    files: Vec<(String, Vec<u8>)>,
    fetch: Vec<Fetch>,
    git: Vec<(String, String, String)>,
    pub data: Value,
}

/// A safe file name part.
fn file_part(text: &str) -> String {
    let cleaned: String = text.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() { "untitled".into() } else { trimmed.chars().take(60).collect() }
}

/// "2026-11-15 at 13:05".
fn human_now() -> String {
    let label = now_label();
    let (date, time) = label.split_once(' ').unwrap_or((&label, ""));
    format!("{date} at {}", time.replace('-', ":"))
}

fn now_label() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    civil(secs)
}

/// "2026-11-15 13-05" from seconds since 1970 (UTC).
pub fn civil(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}-{:02}", rem / 3600, (rem % 3600) / 60)
}

impl Monaged {
    fn agent_state_word(agent: &AgentView) -> &'static str {
        match agent_state(agent) {
            State::NeedsYou => "needs you",
            State::Working => "working",
            State::Done => "idle",
            State::Idle => "not started",
        }
    }

    /// The report and data for a scope, from what Monaged has in memory.
    pub(crate) fn export_plan(&self, scope: Scope) -> Plan {
        let projects: Vec<&crate::store::Project> = self.projects.iter().filter(|p| scope == Scope::Everything || scope == Scope::Project(p.id)).collect();
        let title = match (scope, projects.first()) {
            (Scope::Project(_), Some(p)) => p.name.clone(),
            _ => "all projects".into(),
        };
        let costs = self.store.cost_by_agent().unwrap_or_default();
        let mut md = String::new();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut fetch = Vec::new();
        let mut git = Vec::new();
        let mut data_projects = Vec::new();

        md.push_str(&format!("# Monaged export: {title}\n\n"));
        md.push_str(&format!(
            "Exported {} UTC by Monaged {} on {}. WSL distro: {}. Secret values and anything that looks like a token are masked.\n\n",
            human_now(),
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            self.config.distro
        ));
        md.push_str("This report is made to be read by a person or pasted into a chat with Claude. The zip next to it has every conversation in full, the pipeline data as JSON, git's view of each project, Monaged's log and a screenshot.\n\n");

        // What waits for you.
        let inbox: Vec<_> = self.inbox().into_iter().filter(|i| projects.iter().any(|p| self.subprojects.iter().any(|s| s.id == i.sub_id && s.project_id == p.id))).collect();
        md.push_str("## Waiting for you\n\n");
        if inbox.is_empty() {
            md.push_str("Nothing.\n\n");
        } else {
            for i in &inbox {
                md.push_str(&format!("- {}: {}\n", i.title, i.detail));
            }
            md.push('\n');
        }

        for project in &projects {
            md.push_str(&format!("## Project: {}\n\n", project.name));
            md.push_str(&format!("- Repository: `{}` in {}\n- Base branch: `{}`\n", project.path, project.distro, project.base_branch));
            let chain = self.chain(project.id);
            if !chain.is_empty() {
                let rules: Vec<String> = chain.iter().map(|t| format!("{t} ({:?})", self.policy(project.id, t))).collect();
                md.push_str(&format!("- Branches merge in this order: {}\n", rules.join(" → ")));
            }
            md.push_str(&format!("- Quality bar: {}/5\n\n", self.min_score()));
            git.push((project.name.clone(), project.distro.clone(), project.path.clone()));
            let mut data_subs = Vec::new();
            for sub in self.subprojects.iter().filter(|s| s.project_id == project.id) {
                md.push_str(&format!("### Subproject: {} ({}, `{}`)\n\n", sub.name, sub.kind, sub.branch));
                let p = self.pipelines.get(&sub.id);
                if let Some(p) = p {
                    md.push_str(&format!("**Pipeline:** {} (auto mode {}). ", p.stage.label(), if p.auto_mode { "on" } else { "off" }));
                    if let Some(note) = &p.note {
                        md.push_str(&format!("Status: {note}"));
                    }
                    md.push_str(&format!("\n\n**Request:**\n\n{}\n\n", quote_block(p.request.trim())));
                    if let Some(research) = &p.research {
                        md.push_str(&format!("**Research:** {}\n\n", research["summary"].as_str().unwrap_or("(no summary)")));
                    }
                    for (n, v) in p.verifications.iter().enumerate().filter(|(_, v)| !v.is_null()) {
                        md.push_str(&format!("- Verifier {}: {} (confidence {})\n", n + 1, v["verdict"].as_str().unwrap_or("?"), v["confidence"]));
                    }
                    if let Some(plan) = &p.plan {
                        md.push_str(&format!("\n**Plan:** {}\n\n```json\n{}\n```\n\n", pipeline::plan_title(plan), serde_json::to_string_pretty(plan).unwrap_or_default()));
                    }
                    let g = &p.gate;
                    if !(g.pending.is_empty() && g.fixing.is_empty() && g.held.is_empty() && p.done.is_empty()) {
                        md.push_str("**Quality gate:**\n\n");
                        md.push_str(&format!("- Passed: {}\n", if p.done.is_empty() { "none".into() } else { p.done.join(", ") }));
                        for (name, why) in &g.held {
                            md.push_str(&format!("- Waiting for you: {name}: {why}\n"));
                        }
                        if !g.fixing.is_empty() {
                            md.push_str(&format!("- Fixing findings: {}\n", g.fixing.join(", ")));
                        }
                        if !g.pending.is_empty() {
                            md.push_str(&format!("- In review: {}\n", g.pending.join(", ")));
                        }
                        let rounds: Vec<String> = g.rounds.iter().map(|(n, r)| format!("{n} {r}")).collect();
                        if !rounds.is_empty() {
                            md.push_str(&format!("- Fix rounds used: {}\n", rounds.join(", ")));
                        }
                        md.push('\n');
                    }
                }
                if let Some((score, from)) = self.sub_confidence(sub.id) {
                    md.push_str(&format!("**Confidence:** {score}/5 ({from}).\n\n"));
                }
                // Agents.
                let agents: Vec<&AgentView> = self.agents.iter().filter(|a| a.record.subproject_id == sub.id).collect();
                let mut data_agents = Vec::new();
                for agent in &agents {
                    let r = &agent.record;
                    let role = role_label(&agent.role());
                    md.push_str(&format!(
                        "#### Agent {} ({role}{})\n\n- State: {}{}\n- Branch `{}`, worktree `{}`\n",
                        r.name,
                        if r.archived { ", archived" } else { "" },
                        Self::agent_state_word(agent),
                        agent.model.as_deref().map(|m| format!(", model {m}")).unwrap_or_default(),
                        r.branch,
                        r.worktree
                    ));
                    if let Some(cost) = costs.get(&r.id).copied().filter(|c| *c > 0.0) {
                        md.push_str(&format!("- Cost so far: ${cost:.2} (API-equivalent)\n"));
                    }
                    if let Some(s) = &agent.summary {
                        md.push_str(&format!("- Summary ({}): {}{}\n", s.status, s.text, if s.next_step.is_empty() { String::new() } else { format!(" Next: {}", s.next_step) }));
                    }
                    if let Some(c) = &agent.confidence {
                        md.push_str(&format!("- Confidence {}/5, verdict {}\n", c.score, c.verdict));
                        for reason in &c.reasons {
                            md.push_str(&format!("  - {reason}\n"));
                        }
                        for issue in &c.issues {
                            md.push_str(&format!("  - Finding [{}] {}: {}\n", issue.severity, issue.file, issue.problem));
                        }
                        md.push_str(&format!("  - Checks: {}\n", review::clip(&c.checks, 1500).replace('\n', "\n    ")));
                    }
                    if let Some(error) = &agent.review_error {
                        md.push_str(&format!("- Review error: {error}\n"));
                    }
                    if let crate::agent::Status::Failed(error) = &agent.status {
                        md.push_str(&format!("- Session failed: {error}\n"));
                    }
                    let path = format!("conversations/{}/{}/{}.md", file_part(&project.name), file_part(&sub.name), file_part(&r.name));
                    if agent.items.is_empty() {
                        if let Some(session_id) = r.session_id.clone() {
                            md.push_str(&format!("\nConversation: not open in this run; the zip has it (`{path}`) when Claude still has the session.\n\n"));
                            fetch.push(Fetch { path: path.clone(), name: r.name.clone(), session_id, distro: project.distro.clone() });
                        } else {
                            md.push_str("\nNo conversation yet.\n\n");
                        }
                    } else {
                        let short = transcript_from_items(&agent.items, false);
                        let shown = if short.chars().count() > REPORT_TRANSCRIPT_CHARS {
                            let skip = short.chars().count() - REPORT_TRANSCRIPT_CHARS;
                            format!("[… the start is in the zip …]\n\n{}", short.chars().skip(skip).collect::<String>())
                        } else {
                            short
                        };
                        md.push_str(&format!("\n<details><summary>Conversation ({} items; in full in the zip)</summary>\n\n{shown}\n\n</details>\n\n", agent.items.len()));
                        files.push((path, format!("# {} ({role})\n\n{}\n", r.name, transcript_from_items(&agent.items, true)).into_bytes()));
                    }
                    data_agents.push(json!({
                        "id": r.id, "name": r.name, "role": r.role, "branch": r.branch, "worktree": r.worktree, "archived": r.archived,
                        "state": Self::agent_state_word(agent), "model": agent.model, "cost_usd": costs.get(&r.id),
                        "summary": agent.summary.as_ref().map(|s| json!({ "text": s.text, "status": s.status, "next_step": s.next_step })),
                        "confidence": agent.confidence.as_ref().map(|c| json!({ "score": c.score, "verdict": c.verdict, "reasons": c.reasons, "issues": review::issues_to_json(&c.issues), "checks": c.checks })),
                        "review_error": agent.review_error, "items": agent.items.len(),
                    }));
                }
                // The board.
                let board: Vec<_> = self.board.iter().filter(|e| e.subproject_id == sub.id).collect();
                if !board.is_empty() {
                    md.push_str("**Coordination board:**\n\n");
                    for e in &board {
                        md.push_str(&format!("- [{}] {} ({}): {}\n", e.kind, e.title, e.author, review::clip(&e.body, 600).replace('\n', " ")));
                    }
                    md.push('\n');
                }
                // The timeline.
                let events = self.store.timeline(sub.id, 150).unwrap_or_default();
                if !events.is_empty() {
                    md.push_str("**Timeline** (newest first):\n\n");
                    for e in events.iter().take(80) {
                        let who = e.agent_id.map(|id| self.agent_name(id)).unwrap_or_default();
                        md.push_str(&format!("- {} {} {}{}\n", civil(e.at.max(0) as u64), e.kind, if who.is_empty() { String::new() } else { format!("{who}: ") }, e.data.replace('\n', " ")));
                    }
                    md.push('\n');
                }
                data_subs.push(json!({
                    "id": sub.id, "name": sub.name, "kind": sub.kind, "branch": sub.branch,
                    "pipeline": p.map(|p| json!({
                        "stage": p.stage.as_str(), "request": p.request, "auto_mode": p.auto_mode, "note": p.note,
                        "research": p.research, "verifications": p.verifications, "plan": p.plan, "done": p.done, "gate": p.gate.to_json(),
                    })),
                    "agents": data_agents,
                    "board": board.iter().map(|e| json!({ "kind": e.kind, "title": e.title, "author": e.author, "body": e.body, "paths": e.paths })).collect::<Vec<_>>(),
                    "timeline": events.iter().map(|e| json!({ "at": e.at, "kind": e.kind, "agent": e.agent_id, "text": e.data })).collect::<Vec<_>>(),
                }));
            }
            data_projects.push(json!({ "id": project.id, "name": project.name, "path": project.path, "distro": project.distro, "base_branch": project.base_branch, "subprojects": data_subs }));
        }

        // Settings that say how Monaged is set up (nothing secret).
        let settings = self.export_settings();
        md.push_str("## Settings\n\n");
        for (key, value) in &settings {
            md.push_str(&format!("- `{key}`: {}\n", review::clip(value, 200).replace('\n', " ")));
        }
        md.push_str(&format!(
            "\nReviews: {}; summaries and reviews after every turn: {}; Haiku spend: ${:.4} API, ${:.4} on the plan.\n",
            if self.api_key.is_some() { "with an API key" } else if self.cli_fallback { "on the Claude plan (CLI)" } else { "off" },
            if self.auto_review { "on" } else { "off" },
            self.spend.0,
            self.spend.1
        ));

        let data = json!({
            "monaged": env!("CARGO_PKG_VERSION"),
            "exported_utc": now_label(),
            "distro": self.config.distro,
            "projects": data_projects,
            "inbox": inbox.iter().map(|i| json!({ "title": i.title, "detail": i.detail, "subproject": i.sub_id, "agent": i.agent_id })).collect::<Vec<_>>(),
            "settings": settings,
        });
        Plan { title, report: md, files, fetch, git, data }
    }

    /// Settings worth knowing, without anything that could be secret.
    pub(crate) fn export_settings(&self) -> BTreeMap<String, String> {
        let secretish = ["token", "key", "secret", "password", "credential", "auth"];
        self.store
            .all_settings()
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| !secretish.iter().any(|s| k.to_lowercase().contains(s)))
            .map(|(k, v)| (k, mask_all(&v)))
            .collect()
    }

    /// Exports and shows the result. Writes to Downloads (or Monaged's data folder).
    pub(crate) fn export(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.exporting {
            self.toast(Tone::Info, "An export is already being made.", cx);
            return;
        }
        let plan = self.export_plan(scope);
        self.exporting = true;
        self.progress("export", format!("Exporting {}: conversations, git, the log and a screenshot…", plan.title), cx);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { write_export(plan, None) }).await;
            view.update(cx, |this, cx| {
                this.exporting = false;
                match result {
                    Ok(out) => {
                        let _ = crate::native::reveal_in_explorer(&out.zip);
                        this.settle_action(
                            "export",
                            Tone::Info,
                            format!("Saved to {}: the report (.md) to read or paste into a chat, and a zip with everything.", out.dir.display()),
                            "Copy the report",
                            crate::feedback::ToastAction::Copy(out.report_text.clone()),
                            cx,
                        );
                        this.chime(crate::sound::Cue::Success);
                    }
                    Err(err) => this.settle("export", Tone::Error, format!("Couldn't export: {err:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Where an export went.
pub struct Written {
    pub dir: PathBuf,
    pub zip: PathBuf,
    pub report: PathBuf,
    pub report_text: String,
}

/// The slow part: conversations from WSL, git, the log, a screenshot; then the files.
pub fn write_export(mut plan: Plan, dir: Option<PathBuf>) -> Result<Written> {
    // Conversations that weren't open: Claude keeps them in ~/.claude/projects.
    for f in &plan.fetch {
        // Session ids are UUIDs; anything else isn't looked up (it goes into a glob).
        let safe = !f.session_id.is_empty() && f.session_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let script = format!(
            "f=$(ls -t \"$HOME\"/.claude/projects/*/{id}.jsonl 2>/dev/null | head -n 1); [ -n \"$f\" ] && head -c {MAX_SESSION_BYTES} \"$f\"",
            id = f.session_id
        );
        let text = if safe { wsl::run(&f.distro, &script).unwrap_or_default() } else { String::new() };
        let body = if text.trim().is_empty() {
            "Claude no longer has this session (or WSL couldn't be reached).".to_string()
        } else {
            transcript_from_session(&text)
        };
        plan.files.push((f.path.clone(), format!("# {}\n\n{body}\n", f.name).into_bytes()));
    }
    // Git's view of each project.
    let mut git_text = String::new();
    for (name, distro, path) in &plan.git {
        let script = format!(
            "cd {} || exit 0\necho '$ git status --short --branch'; git status --short --branch 2>&1 | head -n 60\n\
             echo; echo '$ git worktree list'; git worktree list 2>&1 | head -n 40\n\
             echo; echo '$ git branch -vv'; git branch -vv 2>&1 | head -n 60\n\
             echo; echo '$ git log --oneline --graph --all -n 60'; git log --oneline --graph --all -n 60 2>&1",
            wsl::shell_path(path)
        );
        let out = wsl::run(distro, &script).unwrap_or_else(|e| format!("(couldn't run git: {e:#})"));
        git_text.push_str(&format!("# {name} ({path})\n\n{out}\n\n"));
    }
    if !git_text.is_empty() {
        plan.files.push(("git.txt".into(), git_text.into_bytes()));
    }
    // Monaged's own log.
    let data_dir = crate::store::data_dir();
    for name in ["monaged.log", "monaged.old.log"] {
        if let Ok(log) = std::fs::read(data_dir.join(name)) {
            let text = String::from_utf8_lossy(&log);
            let tail: Vec<&str> = text.lines().rev().take(2000).collect();
            plan.files.push((name.into(), tail.into_iter().rev().collect::<Vec<_>>().join("\n").into_bytes()));
        }
    }
    plan.files.push(("data.json".into(), serde_json::to_vec_pretty(&plan.data)?));
    plan.files.push(("README.md".into(), plan.report.clone().into_bytes()));
    // Mask everything text; then the screenshot.
    for (name, bytes) in plan.files.iter_mut() {
        if !name.ends_with(".png") {
            *bytes = mask_all(&String::from_utf8_lossy(bytes)).into_bytes();
        }
    }
    let report_text = mask_all(&plan.report);
    if let Ok(png) = screenshot_png() {
        plan.files.push(("screenshot.png".into(), png));
    }
    plan.files.sort_by(|a, b| (a.0 != "README.md", &a.0).cmp(&(b.0 != "README.md", &b.0)));

    let dir = dir.or_else(crate::native::downloads_dir).filter(|d| d.is_dir()).unwrap_or_else(crate::store::data_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let base = format!("Monaged export - {} - {}", file_part(&plan.title), now_label());
    let zip_path = dir.join(format!("{base}.zip"));
    let report_path = dir.join(format!("{base}.md"));
    std::fs::write(&zip_path, zip(&plan.files)?).with_context(|| format!("couldn't write {}", zip_path.display()))?;
    std::fs::write(&report_path, &report_text).with_context(|| format!("couldn't write {}", report_path.display()))?;
    Ok(Written { dir, zip: zip_path, report: report_path, report_text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_and_keys_are_masked() {
        let text = "export GH=ghp_0123456789abcdefghijABCDEFGHIJ012345 and sk-ant-api03-abcdefghijklmnop\n\
                    Authorization: Bearer abcdef0123456789\npassword=hunter2hunter2\nghp_short is fine\n\
                    -----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\nBBBB\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let out = scrub_tokens(text);
        assert!(!out.contains("ghp_0123") && !out.contains("sk-ant-api03") && !out.contains("abcdef0123456789") && !out.contains("hunter2"));
        assert!(!out.contains("AAAA") && out.contains("[masked private key]") && out.ends_with("after"));
        assert!(out.contains("ghp_short is fine"));
        // Words that merely contain a prefix aren't touched.
        assert_eq!(scrub_tokens("a mask-ant-thing"), "a mask-ant-thing");
    }

    #[test]
    fn a_zip_lists_its_files() {
        let files = vec![("README.md".to_string(), b"# hi\n".to_vec()), ("a/b.txt".to_string(), vec![b'x'; 5000])];
        let bytes = zip(&files).unwrap();
        assert_eq!(&bytes[..4], &[0x50, 0x4b, 0x03, 0x04]);
        // The end record says two files.
        let end = bytes.len() - 22;
        assert_eq!(&bytes[end..end + 4], &[0x50, 0x4b, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([bytes[end + 10], bytes[end + 11]]), 2);
        // Deflate made the repetitive file small.
        assert!(bytes.len() < 1000);
    }

    #[test]
    fn claude_sessions_read_as_markdown() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"Fix the coupon bug"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"look at orders.py"},{"type":"text","text":"Found it."},{"type":"tool_use","name":"Edit","input":{"file_path":"orders.py"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok","is_error":false}]}}
{"type":"summary","summary":"x"}
not json"#;
        let md = transcript_from_session(jsonl);
        assert!(md.contains("**You (or Monaged):**\n\nFix the coupon bug"));
        assert!(md.contains("**Agent:**") && md.contains("Found it.") && md.contains("Tool `Edit`"));
        assert!(md.contains("Result:") && !md.contains("summary"));
    }

    #[test]
    fn transcripts_from_memory() {
        let items = vec![
            Item::User(format!("{} Builder (api)\n\nlots\nof\nlines", pipeline::BRIEF_HEADER)),
            Item::User("Use UUIDs".into()),
            Item::Agent("Done.".into()),
            Item::Notice(Tone::Warn, "Paused".into()),
        ];
        let short = transcript_from_items(&items, false);
        assert!(short.contains("Builder (api)** (sent by Monaged, 5 lines)"));
        assert!(short.contains("**You:**\n\n> Use UUIDs") && short.contains("**Agent:**\n\nDone.") && short.contains("_Warning: Paused_"));
        assert!(transcript_from_items(&items, true).contains("> lots"));
    }

    #[test]
    fn dates_and_names() {
        assert_eq!(civil(0), "1970-01-01 00-00");
        assert_eq!(civil(1_794_747_900), "2026-11-15 13-05");
        assert_eq!(file_part("Order history / v2"), "Order-history---v2");
        assert_eq!(file_part("///"), "untitled");
    }
}
