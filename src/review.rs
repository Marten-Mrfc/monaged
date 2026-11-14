//! Summaries and the 1-5 confidence score for an agent's work.
//!
//! Research shows coding agents are systematically overconfident about their
//! own work, so the score never relies on the agent's opinion. It combines:
//! - automated checks (the project's tests), the strongest signal;
//! - an adversarial review by Haiku that hunts for concrete problems;
//! - the reviewer's verdict on whether the task was done.

use anyhow::Result;
use serde_json::{Value, json};

use crate::llm::{self, Backend, Usage};
use crate::wsl::{quote, run};

#[derive(Clone)]
pub struct Summary {
    pub text: String,
    /// "in_progress", "done", "blocked" or "needs_input".
    pub status: String,
    pub next_step: String,
}

#[derive(Clone)]
pub enum Checks {
    NotFound,
    Unavailable(String),
    Passed(String),
    Failed { command: String, tail: String },
}

impl Checks {
    pub fn describe(&self) -> String {
        match self {
            Checks::NotFound => "No automated checks found (no test setup detected).".into(),
            Checks::Unavailable(why) => format!("Checks couldn't run: {why}"),
            Checks::Passed(command) => format!("`{command}` passed."),
            Checks::Failed { command, tail } => format!("`{command}` FAILED. Last output:\n{tail}"),
        }
    }
}

#[derive(Clone)]
pub struct Issue {
    pub severity: String,
    pub file: String,
    pub problem: String,
}

#[derive(Clone)]
pub struct Confidence {
    pub score: u8,
    pub verdict: String,
    pub reasons: Vec<String>,
    pub issues: Vec<Issue>,
    pub checks: String,
}

// ---- Summary ---------------------------------------------------------------

const SUMMARY_SYSTEM: &str = "You keep a short, always-current status summary of one coding agent's work for a \
project dashboard. Write for the developer who owns the project: plain language, concrete, no filler. \
summary: 2-3 short sentences on what the agent has done so far and where it stands; name the files it touched. \
status: in_progress, done, blocked or needs_input. next_step: one short sentence on what happens next or what the \
developer must decide. If a previous summary is given, update it instead of starting over.";

pub struct TurnInfo<'a> {
    pub task: &'a str,
    pub last_request: &'a str,
    pub agent_reply: &'a str,
    pub tools: &'a [String],
    pub diffstat: &'a str,
    pub previous: Option<&'a str>,
}

pub fn summarize(backend: &Backend, turn: &TurnInfo) -> Result<(Summary, Usage)> {
    let schema = json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "status": { "type": "string", "enum": ["in_progress", "done", "blocked", "needs_input"] },
            "next_step": { "type": "string" }
        },
        "required": ["summary", "status", "next_step"],
        "additionalProperties": false
    });
    let tools = turn.tools.iter().rev().take(30).rev().cloned().collect::<Vec<_>>().join("\n");
    let prompt = format!(
        "Original task:\n{}\n\nLatest request:\n{}\n\nTools the agent used this turn:\n{}\n\nAgent's final reply:\n{}\n\n\
         Files changed so far (vs. where the agent started):\n{}\n\nPrevious summary:\n{}",
        clip(turn.task, 2000),
        clip(turn.last_request, 2000),
        if tools.is_empty() { "(none)".into() } else { tools },
        clip(turn.agent_reply, 4000),
        if turn.diffstat.is_empty() { "(no changes)" } else { turn.diffstat },
        turn.previous.unwrap_or("(none yet)"),
    );
    let (value, usage) = llm::complete_json(backend, SUMMARY_SYSTEM, &prompt, &schema)?;
    Ok((
        Summary {
            text: value["summary"].as_str().unwrap_or_default().to_string(),
            status: value["status"].as_str().unwrap_or("in_progress").to_string(),
            next_step: value["next_step"].as_str().unwrap_or_default().to_string(),
        },
        usage,
    ))
}

// ---- Checks ----------------------------------------------------------------

/// Runs the project's tests in the worktree (10 min max): `command` from
/// Project settings when set, else the one in monaged.json (as the Run tab
/// does), else a detected one. Python runs pytest only when it's importable,
/// else unittest, so a missing pytest isn't counted as failing tests.
pub fn run_checks(distro: &str, worktree: &str, command: Option<&str>) -> Checks {
    let script = format!(
        r#"cd {wt} || exit 1
cmd={given}
if [ -n "$cmd" ]; then :
elif [ -f monaged.json ] && command -v python3 >/dev/null; then
  cmd=$(python3 -c 'import json; print(json.load(open("monaged.json")).get("test", ""))' 2>/dev/null)
fi
if [ -n "$cmd" ]; then :
elif [ -f Cargo.toml ]; then cmd='cargo test --quiet'
elif [ -f package.json ] && grep -q '"test"' package.json; then cmd='npm test --silent'
elif [ -f go.mod ]; then cmd='go test ./...'
elif [ -f pyproject.toml ] || [ -f pytest.ini ] || [ -d tests ]; then
  if python3 -c 'import pytest' 2>/dev/null; then cmd='python3 -m pytest -q'
  elif command -v pytest >/dev/null; then cmd='pytest -q'
  else cmd='python3 -m unittest -q'; fi
elif [ -f Makefile ] && grep -qE '^test:' Makefile; then cmd='make test'
else echo MONAGED_NO_CHECKS; exit 0; fi
echo "MONAGED_CMD $cmd"
out=$(timeout 600 bash -lc "$cmd" 2>&1); code=$?
echo "MONAGED_EXIT $code"
printf '%s\n' "$out" | tail -n 30"#,
        wt = quote(worktree),
        given = quote(command.unwrap_or_default()),
    );
    let out = match run(distro, &script) {
        Ok(out) => out,
        Err(err) => return Checks::Unavailable(format!("{err:#}")),
    };
    if out.contains("MONAGED_NO_CHECKS") {
        return Checks::NotFound;
    }
    let command = out.lines().find_map(|l| l.strip_prefix("MONAGED_CMD ")).unwrap_or("tests").to_string();
    let code: i32 = out.lines().find_map(|l| l.strip_prefix("MONAGED_EXIT ")).and_then(|c| c.trim().parse().ok()).unwrap_or(-1);
    let tail = out.lines().skip_while(|l| !l.starts_with("MONAGED_EXIT")).skip(1).collect::<Vec<_>>().join("\n");
    match code {
        0 => Checks::Passed(command),
        127 => Checks::Unavailable(format!("`{command}` isn't installed in WSL")),
        // pytest: no tests were collected.
        5 if command.contains("pytest") => Checks::NotFound,
        124 => Checks::Failed { command, tail: "timed out after 10 minutes".into() },
        _ => Checks::Failed { command, tail },
    }
}

// ---- Adversarial review ----------------------------------------------------

const REVIEW_SYSTEM: &str = "You are a skeptical senior code reviewer. Your job is to find concrete reasons why this \
change would NOT do what the task asked, or would break something. Assume nothing works until the diff shows it. \
Report only problems you can point to in the diff (which file, what is wrong). Do not invent issues and do not pad \
with style nits. Severity: high = it will fail or break existing behaviour; medium = a likely bug or a missing piece \
the task needs; low = a minor risk. verdict: works, partly, broken, or unclear (unclear only when the diff is too \
partial to judge). summary: one sentence.";

const MAX_DIFF_CHARS: usize = 120_000;

pub fn review(backend: &Backend, task: &str, diff: &str, checks: &Checks) -> Result<(Confidence, Usage)> {
    let schema = json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["works", "partly", "broken", "unclear"] },
            "issues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["high", "medium", "low"] },
                        "file": { "type": "string" },
                        "problem": { "type": "string" }
                    },
                    "required": ["severity", "file", "problem"],
                    "additionalProperties": false
                }
            },
            "summary": { "type": "string" }
        },
        "required": ["verdict", "issues", "summary"],
        "additionalProperties": false
    });
    let (diff_text, diff_note) = if diff.len() > MAX_DIFF_CHARS {
        let mut cut = MAX_DIFF_CHARS;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        (&diff[..cut], format!("\n[The diff is longer: only the first {MAX_DIFF_CHARS} of {} characters are shown.]", diff.len()))
    } else {
        (diff, String::new())
    };
    let prompt = format!(
        "Task given to the agent:\n{}\n\nAutomated checks:\n{}\n\nDiff (the agent's worktree vs. where its branch started):\n```diff\n{}\n```{}",
        clip(task, 3000),
        checks.describe(),
        diff_text,
        diff_note,
    );
    let (value, usage) = llm::complete_json(backend, REVIEW_SYSTEM, &prompt, &schema)?;
    let issues: Vec<Issue> = value["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| Issue {
            severity: i["severity"].as_str().unwrap_or("low").to_string(),
            file: i["file"].as_str().unwrap_or_default().to_string(),
            problem: i["problem"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    let verdict = value["verdict"].as_str().unwrap_or("unclear").to_string();
    let (score, mut reasons) = score(checks, &verdict, &issues);
    if let Some(summary) = value["summary"].as_str().filter(|s| !s.is_empty()) {
        reasons.push(format!("Reviewer: {summary}"));
    }
    if !diff_note.is_empty() {
        reasons.push("The diff was too large to review in full; only the first part was reviewed.".into());
    }
    Ok((Confidence { score, verdict, reasons, issues, checks: checks.describe() }, usage))
}

/// Combines the signals into 1-5. Checks and the verdict cap the score;
/// concrete issues lower it.
pub fn score(checks: &Checks, verdict: &str, issues: &[Issue]) -> (u8, Vec<String>) {
    let mut reasons = Vec::new();
    let mut cap: f32 = 5.0;
    match checks {
        Checks::Passed(command) => reasons.push(format!("✓ Checks passed (`{command}`)")),
        Checks::Failed { command, .. } => {
            cap = cap.min(2.0);
            reasons.push(format!("✕ Checks failed (`{command}`)"));
        }
        Checks::Unavailable(why) => {
            cap = cap.min(4.0);
            reasons.push(format!("? Checks couldn't run: {why}"));
        }
        Checks::NotFound => {
            cap = cap.min(4.0);
            reasons.push("? No tests found, so this rests on the review alone".into());
        }
    }
    match verdict {
        "works" => reasons.push("✓ Reviewer: does what the task asked".into()),
        "partly" => {
            cap = cap.min(3.0);
            reasons.push("~ Reviewer: only partly does what the task asked".into());
        }
        "broken" => {
            cap = cap.min(2.0);
            reasons.push("✕ Reviewer: this doesn't work".into());
        }
        _ => {
            cap = cap.min(4.0);
            reasons.push("? Reviewer couldn't judge from the diff".into());
        }
    }
    let high = issues.iter().filter(|i| i.severity == "high").count();
    let medium = issues.iter().filter(|i| i.severity == "medium").count();
    if high > 0 {
        reasons.push(format!("✕ {high} serious issue{} found", if high == 1 { "" } else { "s" }));
    }
    if medium > 0 {
        reasons.push(format!("~ {medium} likely problem{} found", if medium == 1 { "" } else { "s" }));
    }
    let raw = 5.0 - 1.5 * high as f32 - 0.5 * medium.min(4) as f32;
    (raw.min(cap).round().clamp(1.0, 5.0) as u8, reasons)
}

/// Keeps the start and end of long text, marking the cut.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut head = max * 2 / 3;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (max - head);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n[… {} characters left out …]\n{}", &text[..head], tail - head, &text[tail..])
}

// ---- Persistence helpers ---------------------------------------------------

pub fn issues_to_json(issues: &[Issue]) -> Value {
    Value::Array(issues.iter().map(|i| json!({ "severity": i.severity, "file": i.file, "problem": i.problem })).collect())
}

pub fn issues_from_json(value: &Value) -> Vec<Issue> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| Issue {
            severity: i["severity"].as_str().unwrap_or("low").to_string(),
            file: i["file"].as_str().unwrap_or_default().to_string(),
            problem: i["problem"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}
