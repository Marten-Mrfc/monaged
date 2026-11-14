//! Phase 4: how agents reach Monaged mid-task.
//!
//! Every agent session gets an MCP server: `monaged.exe --mcp-relay …`,
//! started by the Claude adapter inside WSL through WSL's Windows interop.
//! The relay speaks MCP (newline-delimited JSON-RPC) on stdio and forwards
//! each tool call over a token-protected localhost TCP connection to the
//! running Monaged window ("the bridge"). Windows-to-Windows localhost always
//! works, so WSL's NAT networking doesn't matter.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use serde_json::{Value, json};

/// How long an `ask` may wait for another agent before the asker moves on.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(8 * 60);

/// The tools every agent gets. Descriptions steer agents to be frugal.
pub fn tool_list() -> Value {
    let mut tools = base_tools();
    if let Some(list) = tools.as_array_mut() {
        list.extend(crate::env::tool_defs());
    }
    tools
}

fn base_tools() -> Value {
    json!([
        {
            "name": "board_read",
            "description": "Read your subproject's coordination board: contracts (API shapes, data models), decisions, notes and earlier Q&A from the other agents. By default returns only entries you haven't seen. Read it before you ask anything. Pass `subproject` to read another subproject's board in this project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": { "type": "boolean", "description": "Return every entry, not just unseen ones." },
                    "subproject": { "type": "string", "description": "Name of another subproject in this project." }
                }
            }
        },
        {
            "name": "board_post",
            "description": "Post something other agents need: a contract (an API shape, data model or function signature you provide or depend on), a decision, or a short note. Keep it brief and concrete. Other agents get contracts and decisions with their next message; notes only when they touch their files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["contract", "decision", "note"] },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Files or globs this is about." }
                },
                "required": ["kind", "title", "body"]
            }
        },
        {
            "name": "ask",
            "description": "Ask another agent a question when you're blocked and the board doesn't answer it. A librarian answers from the board first (cheap); otherwise the agent is asked and its short answer comes back here, which can take minutes. `to` is an agent name (see the board), or \"lead\" for the planner/integrator.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string" },
                    "question": { "type": "string" },
                    "subproject": { "type": "string", "description": "When the agent works in another subproject." }
                },
                "required": ["to", "question"]
            }
        },
        {
            "name": "claim_files",
            "description": "Tell Monaged which files or globs you're about to edit. It warns you if another agent owns or claimed them, so you can coordinate instead of conflicting.",
            "inputSchema": {
                "type": "object",
                "properties": { "paths": { "type": "array", "items": { "type": "string" } } },
                "required": ["paths"]
            }
        }
    ])
}

// ---- The relay (runs as `monaged.exe --mcp-relay`) -------------------------------

pub struct RelayArgs {
    pub port: u16,
    pub token: String,
    pub agent: i64,
}

pub fn parse_relay_args(args: &[String]) -> Option<RelayArgs> {
    if args.get(1).map(String::as_str) != Some("--mcp-relay") {
        return None;
    }
    let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    Some(RelayArgs { port: value("--port")?.parse().ok()?, token: value("--token")?, agent: value("--agent")?.parse().ok()? })
}

pub fn run_relay(args: RelayArgs) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = message.get("id").cloned() else { continue }; // notifications need no reply
        let method = message["method"].as_str().unwrap_or_default();
        let reply = match method {
            "initialize" => json!({
                "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "monaged", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Monaged coordinates the agents working on this project. Read the board before asking; post contracts others need."
            }),
            "tools/list" => json!({ "tools": tool_list() }),
            "ping" => json!({}),
            "tools/call" => {
                let tool = message["params"]["name"].as_str().unwrap_or_default();
                let arguments = message["params"]["arguments"].clone();
                let (text, is_error) = match call_bridge(&args, tool, arguments) {
                    Ok(reply) => (reply["text"].as_str().unwrap_or_default().to_string(), reply["is_error"].as_bool().unwrap_or(false)),
                    Err(err) => (format!("Monaged isn't reachable: {err:#}. Carry on without coordination tools."), true),
                };
                json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
            }
            _ => {
                let error = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} isn't supported") } });
                writeln!(stdout, "{error}")?;
                stdout.flush()?;
                continue;
            }
        };
        writeln!(stdout, "{}", json!({ "jsonrpc": "2.0", "id": id, "result": reply }))?;
        stdout.flush()?;
    }
    Ok(())
}

fn call_bridge(args: &RelayArgs, tool: &str, arguments: Value) -> Result<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", args.port)).context("can't connect to the Monaged window")?;
    stream.set_read_timeout(Some(ASK_TIMEOUT + Duration::from_secs(30)))?;
    let request = json!({ "token": args.token, "agent": args.agent, "tool": tool, "arguments": arguments });
    writeln!(stream, "{request}")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("unreadable reply from Monaged")
}

// ---- The bridge (runs inside the Monaged window) ------------------------------------

pub struct ToolRequest {
    pub agent: i64,
    pub tool: String,
    pub arguments: Value,
    pub reply: mpsc::Sender<ToolReply>,
}

pub struct ToolReply {
    pub text: String,
    pub is_error: bool,
}

impl ToolReply {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }
}

pub struct Bridge {
    pub port: u16,
    pub token: String,
}

/// Listens on a random localhost port; each connection is one tool call.
pub fn start_bridge() -> Result<(Bridge, UnboundedReceiver<ToolRequest>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("couldn't open the local coordination port")?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let (tx, rx) = unbounded();
    let expected = token.clone();
    thread::Builder::new().name("monaged-bridge".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (tx, expected) = (tx.clone(), expected.clone());
            let _ = thread::Builder::new().name("monaged-tool".into()).spawn(move || {
                let _ = serve(stream, &tx, &expected);
            });
        }
    })?;
    Ok((Bridge { port, token }, rx))
}

fn serve(stream: TcpStream, tx: &futures::channel::mpsc::UnboundedSender<ToolRequest>, token: &str) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    let reply = if request["token"].as_str() != Some(token) {
        ToolReply::error("bad token")
    } else {
        let (reply_tx, reply_rx) = mpsc::channel();
        tx.unbounded_send(ToolRequest {
            agent: request["agent"].as_i64().unwrap_or(0),
            tool: request["tool"].as_str().unwrap_or_default().to_string(),
            arguments: request["arguments"].clone(),
            reply: reply_tx,
        })?;
        reply_rx.recv_timeout(ASK_TIMEOUT).unwrap_or_else(|_| {
            ToolReply::ok("No answer in time. Continue with your best assumption and post it on the board with board_post.")
        })
    };
    let mut stream = stream;
    writeln!(stream, "{}", json!({ "text": reply.text, "is_error": reply.is_error }))?;
    stream.flush()?;
    Ok(())
}

fn random_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..2)
        .map(|i| {
            let mut hasher = RandomState::new().build_hasher();
            hasher.write_u64(i ^ std::process::id() as u64);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

/// The MCP server entry for ACP's `session/new` / `session/load`.
pub fn mcp_server_spec(bridge: &Bridge, agent: i64) -> Value {
    let exe = std::env::current_exe().map(|p| windows_to_wsl_path(&p.to_string_lossy())).unwrap_or_default();
    json!({
        "name": "monaged",
        "command": exe,
        "args": ["--mcp-relay", "--port", bridge.port.to_string(), "--token", bridge.token, "--agent", agent.to_string()],
        "env": []
    })
}

// ---- Board text ---------------------------------------------------------------------

use crate::store::BoardEntry;

/// Marks a message that carries another agent's question, so the answer can
/// be routed back when the turn ends: `[monaged-ask #7]`.
pub fn ask_marker(id: u64) -> String {
    format!("[monaged-ask #{id}]")
}

pub fn ask_message(id: u64, from: &str, question: &str) -> String {
    format!(
        "{} {from} asks you (via Monaged):\n\n> {}\n\nReply with only the answer (1-5 sentences) inside this block, then \
         stop. Don't start other work in this turn. If you don't know, say so.\n\n```monaged-answer\n{{\"answer\": \"...\"}}\n```",
        ask_marker(id),
        question.replace('\n', "\n> ")
    )
}

/// Title of an `answer` entry; also how an asker recognises its own answers.
pub fn answer_title(from: &str, to: &str, question: &str) -> String {
    format!("{from} → {to}: {}", crate::review::clip(question.trim(), 300))
}

pub fn format_entry(e: &BoardEntry, max_body: usize) -> String {
    let paths = if e.paths.is_empty() { String::new() } else { format!("; paths: {}", e.paths.join(", ")) };
    format!("#{} [{}] {} (by {}{paths})\n{}\n", e.id, e.kind, e.title, e.author, crate::review::clip(e.body.trim(), max_body))
}

pub const DIGEST_HEADER: &str = "New on the coordination board (from Monaged):";

/// Appended to an agent's next message: relevant entries in full, the rest as one-liners.
pub fn digest(relevant: &[&BoardEntry], others: &[&BoardEntry]) -> String {
    const FULL: usize = 6;
    const BRIEF: usize = 10;
    let mut out = format!("\n\n---\n{DIGEST_HEADER}\n\n");
    for e in relevant.iter().take(FULL) {
        out.push_str(&format_entry(e, 700));
        out.push('\n');
    }
    let mut rest: Vec<&BoardEntry> = relevant.iter().skip(FULL).copied().collect();
    rest.extend(others.iter().copied());
    for e in rest.iter().take(BRIEF) {
        out.push_str(&format!("- #{} [{}] {} (by {})\n", e.id, e.kind, e.title, e.author));
    }
    if rest.len() > BRIEF {
        out.push_str(&format!("- …and {} more\n", rest.len() - BRIEF));
    }
    if !rest.is_empty() {
        out.push_str("\nCall board_read with all=true if one of these matters to you.\n");
    }
    out
}

pub const LIBRARIAN_SYSTEM: &str = "You are the librarian for a team of coding agents building one feature together. \
An agent asks a question. Answer it using ONLY the coordination board and plan you're given. If they clearly answer it, \
reply in 1-3 sentences and cite entry numbers like #12. If they don't, set found to false and leave answer empty. Never \
guess and never use outside knowledge: a wrong answer is worse than none, because the question then goes to the agent \
who knows.";

pub fn librarian_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "found": { "type": "boolean" }, "answer": { "type": "string" } },
        "required": ["found", "answer"],
        "additionalProperties": false
    })
}

pub fn librarian_prompt(asker: &str, to: &str, question: &str, board: &[BoardEntry], plan: Option<&Value>) -> String {
    let mut out = String::new();
    if let Some(plan) = plan {
        let workstreams: Vec<Value> = plan["workstreams"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| json!({ "name": w["name"], "goal": w["goal"], "owns": w["owns"] }))
            .collect();
        let short = json!({ "title": plan["title"], "summary": plan["summary"], "contracts": plan["contracts"], "workstreams": workstreams });
        out.push_str(&format!("## Plan\n{}\n\n", serde_json::to_string_pretty(&short).unwrap_or_default()));
    }
    out.push_str("## Board\n");
    if board.is_empty() {
        out.push_str("(empty)\n");
    }
    for e in board.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev() {
        out.push_str(&format_entry(e, 1500));
        out.push('\n');
    }
    out.push_str(&format!("\n## Question from {asker} (addressed to {to})\n{question}\n"));
    out
}

// ---- Paths: ownership and claims ---------------------------------------------------

pub fn normalize_path(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let mut path = path.as_str();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.trim_start_matches('/').trim_end_matches('/').to_string()
}

fn has_wildcard(p: &str) -> bool {
    p.contains(['*', '?'])
}

/// Glob match with `**` (any number of directories), `*` and `?` within one segment.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(&"**") => (0..=path.len()).any(|i| match_segments(&pattern[1..], &path[i..])),
        Some(p) => !path.is_empty() && match_segment(p.as_bytes(), path[0].as_bytes()) && match_segments(&pattern[1..], &path[1..]),
    }
}

fn match_segment(p: &[u8], t: &[u8]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some(b'*') => (0..=t.len()).any(|i| match_segment(&p[1..], &t[i..])),
        Some(b'?') => !t.is_empty() && match_segment(&p[1..], &t[1..]),
        Some(c) => t.first() == Some(c) && match_segment(&p[1..], &t[1..]),
    }
}

/// The directory before the first wildcard: `src/api/**` -> `src/api`.
fn literal_dir(pattern: &str) -> &str {
    match pattern.find(['*', '?']) {
        Some(i) => pattern[..i].rsplit_once('/').map_or("", |(dir, _)| dir),
        None => pattern,
    }
}

fn is_under(dir: &str, path: &str) -> bool {
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/'))
}

/// Could these two paths or globs touch the same file? Errs towards yes.
pub fn overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (normalize_path(a), normalize_path(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    match (has_wildcard(&a), has_wildcard(&b)) {
        (false, false) => is_under(&a, &b) || is_under(&b, &a),
        (true, false) => glob_match(&a, &b) || is_under(&b, literal_dir(&a)),
        (false, true) => glob_match(&b, &a) || is_under(&a, literal_dir(&b)),
        (true, true) => {
            let (da, db) = (literal_dir(&a), literal_dir(&b));
            is_under(da, db) || is_under(db, da)
        }
    }
}

/// `O:\Projects\app\monaged.exe` -> `/mnt/o/Projects/app/monaged.exe`
pub fn windows_to_wsl_path(path: &str) -> String {
    let path = path.trim_start_matches(r"\\?\");
    let mut chars = path.chars();
    match (chars.next(), chars.next()) {
        (Some(drive), Some(':')) => format!("/mnt/{}{}", drive.to_ascii_lowercase(), chars.as_str().replace('\\', "/")),
        _ => path.replace('\\', "/"),
    }
}
