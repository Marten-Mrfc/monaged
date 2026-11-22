//! Plans as documents. The planner writes the plan as a full Markdown
//! document (with diagrams and tables) above its `monaged-plan` block; each
//! revision is kept as a version. You read it on the plan page
//! (`plan_view.rs`), comment on any part of it, and send the comments to the
//! planner, whose answer is the next version.

use serde_json::Value;

use crate::markdown::{self, Block};

#[derive(Clone, Debug)]
pub struct PlanVersion {
    pub version: u32,
    pub document: String,
    pub plan: Value,
    pub at: i64,
}

#[derive(Clone, Debug)]
pub struct PlanComment {
    pub id: i64,
    pub sub_id: i64,
    /// The version it was written on.
    pub version: u32,
    /// The block of that version's document it's about; None: the whole plan.
    pub block: Option<usize>,
    /// What that block said, so it still reads right in later versions.
    pub quote: String,
    pub body: String,
    /// Sent to the planner while this version was the latest.
    pub sent_in: Option<u32>,
    pub at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct PlanBook {
    pub versions: Vec<PlanVersion>,
    pub comments: Vec<PlanComment>,
}

impl PlanBook {
    pub fn latest(&self) -> Option<&PlanVersion> {
        self.versions.last()
    }

    pub fn version(&self, n: u32) -> Option<&PlanVersion> {
        self.versions.iter().find(|v| v.version == n)
    }

    pub fn next_version(&self) -> u32 {
        self.latest().map_or(1, |v| v.version + 1)
    }

    /// Written but not sent yet.
    pub fn unsent(&self) -> Vec<&PlanComment> {
        self.comments.iter().filter(|c| c.sent_in.is_none()).collect()
    }

    /// Sent and not answered by a newer version yet.
    pub fn waiting(&self) -> bool {
        let latest = self.latest().map_or(0, |v| v.version);
        self.comments.iter().any(|c| c.sent_in.is_some_and(|v| v >= latest))
    }
}

/// Where a comment stands, in words.
pub fn comment_state(c: &PlanComment, latest: u32) -> String {
    match c.sent_in {
        None => "not sent yet".into(),
        Some(v) if latest > v => format!("answered in v{}", v + 1),
        Some(_) => "sent to the planner".into(),
    }
}

/// The document part of the planner's reply: everything but the
/// `monaged-*` blocks, starting at its first heading when it has one.
pub fn document_from_reply(reply: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in reply.lines() {
        let t = line.trim_start();
        if !skipping && t.starts_with("```monaged-") {
            skipping = true;
            continue;
        }
        if skipping {
            if t.starts_with("```") {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    let text = out.join("\n");
    let start = text.lines().position(|l| l.starts_with("# ")).or_else(|| text.lines().position(|l| l.starts_with("## ")));
    let text = match start {
        Some(i) => text.lines().skip(i).collect::<Vec<_>>().join("\n"),
        None => text,
    };
    text.trim().to_string()
}

/// A document from the plan's own fields, for plans without one (older
/// planners, or a planner that only sent its block).
pub fn document_from_plan(plan: &Value) -> String {
    let strings = |key: &str| -> Vec<String> { plan[key].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect() };
    let cell = |s: &str| s.replace('|', "\\|").replace('\n', " ");
    let mut doc = format!("# {}\n\n", crate::pipeline::plan_title(plan));
    if let Some(summary) = plan["summary"].as_str() {
        doc.push_str(&format!("{summary}\n\n"));
    }
    let streams = crate::pipeline::workstreams(plan);
    if !streams.is_empty() {
        doc.push_str("## Workstreams\n\n| Workstream | Goal | Owns | Depends on |\n|---|---|---|---|\n");
        for w in &streams {
            let owns = w.value["owns"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|o| format!("`{o}`")).collect::<Vec<_>>().join(", ");
            let deps = w.depends_on().join(", ");
            doc.push_str(&format!("| {} | {} | {} | {} |\n", w.name, cell(w.value["goal"].as_str().unwrap_or_default()), owns, if deps.is_empty() { "—".into() } else { deps }));
        }
        doc.push('\n');
        if let Some(diagram) = crate::diagram::build_order(&streams).filter(|_| streams.len() > 1) {
            doc.push_str(&format!("### Build order\n\n```mermaid\n{diagram}```\n\n"));
        }
        for w in &streams {
            let steps: Vec<&str> = w.value["steps"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if !steps.is_empty() {
                doc.push_str(&format!("### {}\n\n", w.name));
                for step in steps {
                    doc.push_str(&format!("1. {step}\n"));
                }
                doc.push('\n');
            }
        }
    }
    let contracts = strings("contracts");
    if !contracts.is_empty() {
        doc.push_str("## Contracts\n\n");
        for c in contracts {
            doc.push_str(&format!("- {c}\n"));
        }
        doc.push('\n');
    }
    let tests = strings("tests");
    if !tests.is_empty() {
        doc.push_str("## Test plan\n\n");
        for t in tests {
            doc.push_str(&format!("- [ ] {t}\n"));
        }
        doc.push('\n');
    }
    if let Some(reason) = plan["confidence_reason"].as_str() {
        doc.push_str(&format!("## Confidence\n\n{}/5: {reason}\n", plan["confidence"].as_u64().unwrap_or(0)));
    }
    doc.trim_end().to_string()
}

/// The document to keep for a plan: the planner's own when it wrote one
/// worth the name, else one made from the plan's fields.
pub fn usable_document(reply: &str, plan: &Value) -> String {
    let doc = document_from_reply(reply);
    let headings = doc.lines().filter(|l| l.starts_with('#')).count();
    if headings >= 2 && doc.len() >= 200 { doc } else { document_from_plan(plan) }
}

/// A block as plain text (for quotes and the outline).
pub fn block_text(block: &Block) -> String {
    match block {
        Block::Heading(_, i) | Block::Paragraph(i) | Block::Quote(i) => i.text.clone(),
        Block::Item { content, .. } => content.text.clone(),
        Block::Code(code, lang) if lang.eq_ignore_ascii_case("mermaid") => format!("[diagram] {}", code.lines().next().unwrap_or_default()),
        Block::Code(code, _) => code.clone(),
        Block::Table { rows, .. } => rows.first().map(|r| r.iter().map(|c| c.text.clone()).collect::<Vec<_>>().join(" | ")).map(|h| format!("[table] {h}")).unwrap_or_default(),
        Block::Rule => String::new(),
    }
}

/// Headings (levels 1–3) by block index, for the outline.
pub fn outline(blocks: &[Block]) -> Vec<(usize, u8, String)> {
    blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| if let Block::Heading(level, inline) = b { (*level <= 3).then(|| (i, *level, inline.text.clone())) } else { None })
        .collect()
}

/// The heading a block sits under.
pub fn section_of(blocks: &[Block], index: usize) -> Option<String> {
    blocks[..=index.min(blocks.len().saturating_sub(1))].iter().rev().find_map(|b| if let Block::Heading(_, i) = b { Some(i.text.clone()) } else { None })
}

pub fn parse(document: &str) -> Vec<Block> {
    markdown::parse(document)
}

/// What the planner gets for your comments.
pub fn comments_message(version: u32, comments: &[(Option<String>, String, String)]) -> String {
    let mut text = format!(
        "The developer reviewed your plan (version {version}) and left {} comment{}. Revise the plan so it answers every one: \
         change what they ask for, or say why not. Start the document with a short \"## Changes in this version\" section that \
         answers each comment by its number. Then reply with the full updated plan document (every section, not only what \
         changed, with its diagrams and tables) and a new ```monaged-plan``` block. Ask with a question block only if a \
         comment leaves something only the developer can decide.\n",
        comments.len(),
        if comments.len() == 1 { "" } else { "s" }
    );
    for (n, (section, quote, body)) in comments.iter().enumerate() {
        let place = match (section, quote.trim()) {
            (_, "") => "On the whole plan".to_string(),
            (Some(s), q) if s.trim() != q => format!("On \"{}\" (in \"{s}\")", crate::review::clip(q, 240).replace('\n', " ")),
            (_, q) => format!("On \"{}\"", crate::review::clip(q, 240).replace('\n', " ")),
        };
        text.push_str(&format!("\n{}. {place}:\n{}\n", n + 1, body.trim()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_document_is_the_reply_without_its_blocks() {
        let reply = "Here is the plan.\n\n# Car reservations\n\nIntro.\n\n## Approach\n\n```mermaid\nflowchart LR\n a --> b\n```\n\n```monaged-plan\n{\"title\": \"x\"}\n```\n";
        let doc = document_from_reply(reply);
        assert!(doc.starts_with("# Car reservations"));
        assert!(doc.contains("```mermaid") && !doc.contains("monaged-plan") && !doc.contains("Here is the plan"));
    }

    #[test]
    fn a_plan_without_a_document_gets_one() {
        let plan = json!({
            "title": "Car reservations", "summary": "Reserve a car.", "confidence": 4, "confidence_reason": "small",
            "contracts": ["POST /reservations"], "tests": ["overlap is refused"],
            "workstreams": [{ "name": "core", "goal": "model | rules", "owns": ["src/core/**"], "steps": ["add the model"] },
                            { "name": "api", "goal": "endpoints", "owns": ["src/api/**"], "depends_on": ["core"] }]
        });
        assert!(usable_document("Just the block.\n```monaged-plan\n{}\n```", &plan).starts_with("# Car reservations"));
        let doc = document_from_plan(&plan);
        assert!(doc.contains("| core | model \\| rules | `src/core/**` | — |"));
        assert!(doc.contains("| api | endpoints | `src/api/**` | core |"));
        assert!(doc.contains("```mermaid\nflowchart LR") && doc.contains("- [ ] overlap is refused") && doc.contains("4/5: small"));
        let blocks = parse(&doc);
        let heads: Vec<String> = outline(&blocks).into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(heads[..3], ["Car reservations".to_string(), "Workstreams".into(), "Build order".into()]);
    }

    #[test]
    fn comments_go_to_the_planner_numbered_with_where_they_are() {
        let text = comments_message(
            2,
            &[
                (Some("Approach".into()), "Use a queue for reminders.".into(), "Why not a cron job?".into()),
                (None, String::new(), "Keep the UI out of this one.".into()),
            ],
        );
        assert!(text.contains("version 2") && text.contains("2 comments") && text.contains("## Changes in this version"));
        assert!(text.contains("1. On \"Use a queue for reminders.\" (in \"Approach\"):\nWhy not a cron job?"));
        assert!(text.contains("2. On the whole plan:\nKeep the UI out of this one."));
    }

    #[test]
    fn comments_say_where_they_stand() {
        let c = PlanComment { id: 1, sub_id: 1, version: 1, block: None, quote: String::new(), body: "x".into(), sent_in: None, at: 0 };
        assert_eq!(comment_state(&c, 1), "not sent yet");
        let sent = PlanComment { sent_in: Some(1), ..c };
        assert_eq!(comment_state(&sent, 1), "sent to the planner");
        assert_eq!(comment_state(&sent, 2), "answered in v2");
        let book = PlanBook { versions: vec![PlanVersion { version: 1, document: String::new(), plan: Value::Null, at: 0 }], comments: vec![sent] };
        assert!(book.waiting() && book.unsent().is_empty() && book.next_version() == 2);
    }
}
