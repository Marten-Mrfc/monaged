//! Small, cheap Claude calls (Claude Haiku 5.5) for summaries and reviews.
//!
//! Two backends:
//! - **API key** (preferred): raw HTTP to the Messages API. Rust has no
//!   official Anthropic SDK, so this follows the documented REST shape.
//!   Paid from the user's API credit, so it doesn't touch the Max plan.
//! - **Claude CLI** fallback: `claude -p` inside WSL with `--json-schema`.
//!   Counts against the Max plan's usage.
//!
//! Both ask for structured JSON output, so replies always parse.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::wsl;

pub const MODEL: &str = "claude-haiku-5-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
/// Claude Haiku 5.5 list prices in $ per million tokens (prompts up to 100K tokens).
const INPUT_PER_M: f64 = 0.10;
const OUTPUT_PER_M: f64 = 0.50;
const CLAUDE_CLI: &str = "~/.local/share/claude-agent-acp/current/claude";

#[derive(Clone)]
pub enum Backend {
    Api { key: String },
    Cli { distro: String },
}

impl Backend {
    pub fn via(&self) -> &'static str {
        match self {
            Backend::Api { .. } => "api",
            Backend::Cli { .. } => "max",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// Sends one request and returns the parsed JSON object that matches `schema`.
pub fn complete_json(backend: &Backend, system: &str, prompt: &str, schema: &Value) -> Result<(Value, Usage)> {
    match backend {
        Backend::Api { key } => via_api(key, system, prompt, schema),
        Backend::Cli { distro } => via_cli(distro, system, prompt, schema),
    }
}

fn via_api(key: &str, system: &str, prompt: &str, schema: &Value) -> Result<(Value, Usage)> {
    let body = json!({
        "model": MODEL,
        // Short structured replies; room for adaptive thinking at low effort.
        "max_tokens": 4000,
        "system": system,
        "output_config": {
            "effort": "low",
            "format": { "type": "json_schema", "schema": schema },
        },
        "messages": [{ "role": "user", "content": prompt }],
    });
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();

    let mut last_error = anyhow!("no attempt made");
    for attempt in 0..3u64 {
        if attempt > 0 {
            sleep(Duration::from_secs(2 * attempt));
        }
        let sent = agent
            .post(API_URL)
            .header("x-api-key", key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .send_json(&body);
        let mut response = match sent {
            Ok(response) => response,
            Err(err) => {
                last_error = anyhow!("couldn't reach the Anthropic API: {err}");
                continue;
            }
        };
        let status = response.status().as_u16();
        let reply: Value = response.body_mut().read_json().context("the Anthropic API sent an unreadable reply")?;
        let message = reply["error"]["message"].as_str().unwrap_or("unknown error").to_string();
        match status {
            200 => {}
            401 => bail!("the API key was rejected (401). Check it in Settings."),
            429 | 500..=599 => {
                last_error = anyhow!("Anthropic API {status}: {message}");
                continue;
            }
            _ => bail!("Anthropic API {status}: {message}"),
        }
        match reply["stop_reason"].as_str() {
            Some("refusal") => bail!("Claude declined this request"),
            Some("max_tokens") => bail!("the reply was cut off (max_tokens)"),
            _ => {}
        }
        let text: String = reply["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect();
        let value: Value = serde_json::from_str(&text).context("the reply wasn't valid JSON")?;
        let usage = &reply["usage"];
        let input = usage["input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
        let output = usage["output_tokens"].as_u64().unwrap_or(0);
        let cost = input as f64 * INPUT_PER_M / 1e6 + output as f64 * OUTPUT_PER_M / 1e6;
        return Ok((value, Usage { input_tokens: input, output_tokens: output, cost_usd: cost }));
    }
    Err(last_error)
}

fn via_cli(distro: &str, system: &str, prompt: &str, schema: &Value) -> Result<(Value, Usage)> {
    // Everything travels inside quoted heredocs, so no escaping is needed.
    let clean = |text: &str| text.replace("MONAGED_EOF", "MONAGED-EOF");
    let script = format!(
        "schema=$(cat <<'MONAGED_EOF'\n{schema}\nMONAGED_EOF\n)\n\
         system=$(cat <<'MONAGED_EOF'\n{system}\nMONAGED_EOF\n)\n\
         {CLAUDE_CLI} -p --model {MODEL} --effort low --output-format json --json-schema \"$schema\" \
           --tools \"\" --no-session-persistence --strict-mcp-config --system-prompt \"$system\" <<'MONAGED_EOF'\n\
         {prompt}\nMONAGED_EOF",
        schema = schema,
        system = clean(system),
        prompt = clean(prompt),
    );
    let out = wsl::run(distro, &script)?;
    let line = out.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or(&out);
    let reply: Value = serde_json::from_str(line).context("the Claude CLI sent an unreadable reply")?;
    if reply["is_error"].as_bool() == Some(true) {
        bail!("Claude CLI: {}", reply["result"].as_str().unwrap_or("unknown error"));
    }
    let value = match reply.get("structured_output") {
        Some(v) if v.is_object() => v.clone(),
        _ => serde_json::from_str(reply["result"].as_str().unwrap_or_default()).context("the reply wasn't valid JSON")?,
    };
    let usage = &reply["usage"];
    let input = usage["input_tokens"].as_u64().unwrap_or(0)
        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
        + usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
    Ok((
        value,
        Usage {
            input_tokens: input,
            output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
            cost_usd: reply["total_cost_usd"].as_f64().unwrap_or(0.0),
        },
    ))
}
