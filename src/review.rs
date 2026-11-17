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
    /// The tests fail, but every failing test failed on the base branch too:
    /// it was broken before this change.
    FailedBefore { command: String, tail: String },
}

impl Checks {
    /// Reads `describe`'s text back (saved reviews keep only that), to score again.
    pub fn from_description(text: &str) -> Checks {
        if let Some(why) = text.strip_prefix("Checks couldn't run: ") {
            return Checks::Unavailable(why.to_string());
        }
        if text.starts_with("No automated checks found") {
            return Checks::NotFound;
        }
        let command = || text.strip_prefix('`').and_then(|t| t.split_once('`')).map(|(c, _)| c.to_string()).unwrap_or_else(|| "tests".into());
        if let Some((_, tail)) = text.split_once(BEFORE_MARK) {
            return Checks::FailedBefore { command: command(), tail: tail.to_string() };
        }
        if let Some((_, tail)) = text.split_once(" FAILED. Last output:\n") {
            return Checks::Failed { command: command(), tail: tail.to_string() };
        }
        if text.ends_with(" passed.") {
            return Checks::Passed(command());
        }
        Checks::Unavailable("unknown".into())
    }

    pub fn describe(&self) -> String {
        match self {
            Checks::NotFound => "No automated checks found (no test setup detected).".into(),
            Checks::Unavailable(why) => format!("Checks couldn't run: {why}"),
            Checks::Passed(command) => format!("`{command}` passed."),
            Checks::Failed { command, tail } => format!("`{command}` FAILED. Last output:\n{tail}"),
            Checks::FailedBefore { command, tail } => format!("`{command}`{BEFORE_MARK}{tail}"),
        }
    }
}

const BEFORE_MARK: &str = " fails, but every failing test failed on the base branch too (broken before this change; \
not this agent's to fix unless the task says so). Last output:\n";

/// The lines that name a failing test, normalised so the same failure on two
/// runs compares equal (timings and messages left out).
pub fn failure_lines(output: &str) -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    for line in output.lines() {
        let line = line.trim();
        let failing = line.starts_with("FAIL:")
            || line.starts_with("ERROR:")
            || line.starts_with("FAILED ")
            || line.starts_with("--- FAIL:")
            || line.starts_with('✕')
            || line.starts_with('×')
            || (line.starts_with("test ") && line.ends_with("FAILED"))
            || line.starts_with("FAIL ");
        // Summaries ("FAILED (failures=2)", "test result: FAILED") count, not name.
        if !failing || line.starts_with("FAILED (") || line.starts_with("test result:") {
            continue;
        }
        let line = line.split(" - ").next().unwrap_or(line);
        let mut clean = String::new();
        let mut depth = 0;
        for c in line.chars() {
            match c {
                '(' if line.contains("ms)") || line.contains("s)") => depth += 1,
                ')' if depth > 0 => depth -= 1,
                _ if depth == 0 => clean.push(c),
                _ => {}
            }
        }
        // "FAIL\tpkg\t0.01s": the timing at the end.
        let clean: Vec<&str> = clean.split_whitespace().filter(|w| !(w.ends_with('s') && w.trim_end_matches("ms").trim_end_matches('s').parse::<f64>().is_ok())).collect();
        let clean = clean.join(" ");
        if !clean.is_empty() {
            found.insert(clean);
        }
    }
    found
}

/// Failing now vs. on the base branch: only pre-existing failures when every
/// failing test now failed before too (and there's something to compare).
pub fn only_failed_before(now: &str, before: &str) -> bool {
    let (now, before) = (failure_lines(now), failure_lines(before));
    !now.is_empty() && now.is_subset(&before)
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
/// How a test command runs: as is, or (when the project uses Claude Code's
/// sandbox) in bubblewrap without Windows drives or WSL interop, so tests the
/// agent wrote can't reach outside the sandbox through the review.
fn runner(fence: bool) -> &'static str {
    if fence {
        "command -v bwrap >/dev/null 2>&1 || { echo MONAGED_NO_FENCE; exit 0; }\n\
         set -- bwrap --dev-bind / / --tmpfs /mnt; [ -d /run/WSL ] && set -- \"$@\" --tmpfs /run/WSL; set -- \"$@\" --die-with-parent bash -lc\n"
    } else {
        "set -- bash -lc\n"
    }
}

/// One test run at a time: two full suites (plus their base-branch runs) at
/// once made timing tests fail that pass on a quiet machine, and those
/// failures look new. The next review waits for the one before it.
static CHECKS_RUNNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn run_checks(distro: &str, worktree: &str, command: Option<&str>, base: Option<&str>, fence: bool) -> Checks {
    let _one_at_a_time = CHECKS_RUNNING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let script = format!(
        r#"cd {wt} || exit 1
cmd={given}
if [ -n "$cmd" ]; then :
elif [ -f monaged.json ] && command -v python3 >/dev/null; then
  cmd=$(python3 -c 'import json; print(json.load(open("monaged.json")).get("test", ""))' 2>/dev/null)
fi
{js}
if [ -n "$cmd" ]; then :
elif [ -f Cargo.toml ]; then cmd='cargo test --quiet'
elif [ -n "$pm" ] && grep -q '"test"' package.json; then
  if [ "$pm" = npm ]; then cmd='npm test --silent'; else cmd="$pm run test"; fi
elif [ -f go.mod ]; then cmd='go test ./...'
elif [ -f pyproject.toml ] || [ -f pytest.ini ] || [ -d tests ]; then
  if python3 -c 'import pytest' 2>/dev/null; then cmd='python3 -m pytest -q'
  elif command -v pytest >/dev/null; then cmd='pytest -q'
  elif grep -rqsE '^\s*(import|from) pytest' --include='*.py' --exclude-dir=.git --exclude-dir=node_modules --exclude-dir=.venv --exclude-dir=venv . ; then echo MONAGED_NO_RUNNER pytest; exit 0
  else cmd='python3 -m unittest -q'; fi
elif [ -f Makefile ] && grep -qE '^test:' Makefile; then cmd='make test'
else echo MONAGED_NO_CHECKS; exit 0; fi
echo "MONAGED_CMD $cmd"
# A JavaScript test run needs the package manager and the installed dependencies.
case "${{cmd%% *}}" in npm|npx|pnpm|pnpx|yarn|bun|bunx|node|vitest|jest|turbo|nx) ;; *) pm= ;; esac
# The program that runs the tests (and the package manager) must be a Linux one:
# Windows' npm or node, found through interop, can't run a project inside WSL.
for tool in "${{cmd%% *}}" $pm; do
  # Paths, VAR=value prefixes, and tools that live in node_modules/.bin.
  case "$tool" in *=*|*/*|''|vitest|jest|turbo|nx) continue ;; esac
  where=$(command -v "$tool" 2>/dev/null)
  case "$where" in
    '') echo "MONAGED_MISSING $tool"; exit 0 ;;
    /mnt/*) echo "MONAGED_WINDOWS $tool"; exit 0 ;;
  esac
done
{runner}if [ -n "$pm" ] && [ ! -d node_modules ]; then
  echo "MONAGED_INSTALL $install"
  iout=$(timeout 600 "$@" "$install" 2>&1); icode=$?
  if [ $icode -ne 0 ]; then echo "MONAGED_INSTALL_FAILED $icode"; printf '%s\n' "$iout" | tail -n 15; exit 0; fi
fi
out=$(timeout 600 "$@" "$cmd" 2>&1); code=$?
{strip}
printf '%s\n' "$out" | grep -E '^\s*(FAIL|ERROR:|--- FAIL|✕|×|test .*FAILED$)' | head -n 200 | sed 's/^/MONAGED_FAILLINE /'
echo "MONAGED_EXIT $code"
printf '%s\n' "$out" | tail -n 30"#,
        strip = STRIP_COLORS,
        wt = quote(worktree),
        given = quote(command.unwrap_or_default()),
        js = JS_PACKAGE_MANAGER,
        runner = runner(fence),
    );
    let out = match run(distro, &script) {
        Ok(out) => out,
        Err(err) => return Checks::Unavailable(format!("{err:#}")),
    };
    read_checks(&out, |command| {
        let now: String = out.lines().filter_map(|l| l.strip_prefix("MONAGED_FAILLINE ")).collect::<Vec<_>>().join("\n");
        base.and_then(|base| base_failures(distro, worktree, base, command, fence)).is_some_and(|before| only_failed_before(&now, &before))
    })
}

/// Takes terminal colours out of `$out`. Test runners colour their output
/// even into a pipe (vitest's " FAIL " starts with an escape code), which
/// hid every failing test's name, so failures that were there before the
/// change looked new.
const STRIP_COLORS: &str = r#"out=$(printf '%s\n' "$out" | sed 's/\x1b\[[0-9;?]*[A-Za-z]//g')"#;

/// The same for text read back in Rust (a review saved before colours were taken out).
fn strip_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&n) = chars.peek() {
                chars.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Works out a JavaScript project's package manager (`$pm`, empty for
/// none) and how to install its dependencies (`$install`), from the lock
/// file or package.json's `packageManager`.
const JS_PACKAGE_MANAGER: &str = r#"pm=; install=
if [ -f package.json ]; then
  if [ -f bun.lock ] || [ -f bun.lockb ] || grep -qE '"packageManager": *"bun@' package.json; then pm=bun; install='bun install --frozen-lockfile'
  elif [ -f pnpm-lock.yaml ] || grep -qE '"packageManager": *"pnpm@' package.json; then pm=pnpm; install='pnpm install --frozen-lockfile'
  elif [ -f yarn.lock ] || grep -qE '"packageManager": *"yarn@' package.json; then pm=yarn; install='yarn install --frozen-lockfile'
  elif [ -f package-lock.json ]; then pm=npm; install='npm ci'
  else pm=npm; install='npm install'; fi
fi"#;

/// Reads `run_checks`' output. `failed_before` says whether the failing tests
/// of `command` failed on the base branch too (only asked when tests fail).
fn read_checks(out: &str, failed_before: impl FnOnce(&str) -> bool) -> Checks {
    let out = &strip_colors(out);
    if out.contains("MONAGED_NO_FENCE") {
        return Checks::Unavailable("this project uses the sandbox, and running the tests fenced needs bubblewrap in WSL (the setup guide's extras install it)".into());
    }
    if out.contains("MONAGED_NO_CHECKS") {
        return Checks::NotFound;
    }
    if let Some(runner) = out.lines().find_map(|l| l.strip_prefix("MONAGED_NO_RUNNER ")) {
        return Checks::Unavailable(format!("the tests need {runner}, which isn't installed in WSL"));
    }
    if let Some(tool) = out.lines().find_map(|l| l.strip_prefix("MONAGED_MISSING ")) {
        return Checks::Unavailable(format!("the tests need `{tool}`, which isn't installed in WSL (install it inside WSL; a Windows copy can't run them)"));
    }
    if let Some(tool) = out.lines().find_map(|l| l.strip_prefix("MONAGED_WINDOWS ")) {
        return Checks::Unavailable(format!(
            "WSL only finds the Windows `{tool}` (through interop), which can't run a project inside WSL. Install `{tool}` inside WSL"
        ));
    }
    let command = out.lines().find_map(|l| l.strip_prefix("MONAGED_CMD ")).unwrap_or("tests").to_string();
    if out.lines().any(|l| l.starts_with("MONAGED_INSTALL_FAILED")) {
        let install = out.lines().find_map(|l| l.strip_prefix("MONAGED_INSTALL ")).unwrap_or("installing the dependencies");
        let tail: Vec<&str> = out.lines().skip_while(|l| !l.starts_with("MONAGED_INSTALL_FAILED")).skip(1).filter(|l| !l.trim().is_empty()).collect();
        let last = tail.iter().rev().take(3).rev().copied().collect::<Vec<_>>().join(" · ");
        return Checks::Unavailable(format!("the worktree had no dependencies installed, and `{install}` failed: {}", clip(&last, 400)));
    }
    let code: i32 = out.lines().find_map(|l| l.strip_prefix("MONAGED_EXIT ")).and_then(|c| c.trim().parse().ok()).unwrap_or(-1);
    let tail = out.lines().skip_while(|l| !l.starts_with("MONAGED_EXIT")).skip(1).collect::<Vec<_>>().join("\n");
    match code {
        0 => Checks::Passed(command),
        127 => Checks::Unavailable(format!("`{command}` isn't installed in WSL")),
        // pytest and unittest (Python 3.12+): no tests were found.
        5 if command.contains("pytest") || command.contains("unittest") => Checks::NotFound,
        124 => Checks::Failed { command, tail: "timed out after 10 minutes".into() },
        // Failing tests say which; a runner that stops in silence never got going.
        _ if tail.trim().is_empty() => {
            Checks::Unavailable(format!("`{command}` stopped (exit code {code}) without printing anything, so it most likely couldn't start"))
        }
        _ if WINDOWS_SIGNS.iter().any(|s| tail.contains(s)) => Checks::Unavailable(format!(
            "`{command}` ended up running Windows programs, which can't run a project inside WSL: {}",
            clip(tail.lines().find(|l| WINDOWS_SIGNS.iter().any(|s| l.contains(s))).unwrap_or_default().trim(), 300)
        )),
        // Were these tests failing before the agent started? Then they aren't its doing.
        _ if failed_before(&command) => Checks::FailedBefore { command, tail },
        _ => Checks::Failed { command, tail },
    }
}

/// Output that shows a Windows program ran instead of a Linux one.
const WINDOWS_SIGNS: [&str; 3] =
    ["is not recognized as an internal or external command", "UNC paths are not supported", "CMD.EXE was started with the above path"];

/// Base-branch results, by (worktree's repo, base commit, command): a
/// failing review would otherwise run the base's tests again every time.
static BASE_RESULTS: std::sync::Mutex<Vec<(String, Option<String>)>> = std::sync::Mutex::new(Vec::new());

/// The failing-test lines of the same command on the base branch (in a
/// throwaway checkout that borrows the worktree's installed dependencies),
/// or None when that couldn't run or passed.
fn base_failures(distro: &str, worktree: &str, base: &str, command: &str, fence: bool) -> Option<String> {
    let sha = run(distro, &format!("cd {} && git rev-parse --verify --quiet {}^{{commit}}", quote(worktree), quote(base))).ok()?;
    let sha = sha.trim().lines().last()?.to_string();
    let key = format!("{distro}\n{sha}\n{command}\n{fence}");
    if let Some((_, cached)) = BASE_RESULTS.lock().ok()?.iter().find(|(k, _)| *k == key) {
        return cached.clone();
    }
    let script = format!(
        r#"cd {wt} || exit 1
wt=$(pwd -P)
{runner}tmp=$(mktemp -d) || exit 1
if ! git worktree add --detach -q "$tmp/base" {sha} >/dev/null 2>&1; then rm -rf "$tmp"; echo MONAGED_NO_BASE; exit 0; fi
cd "$tmp/base"
# Installed dependencies aren't in git: borrow the worktree's (a monorepo has
# node_modules in its packages too).
for d in .venv venv vendor; do [ -e "$wt/$d" ] && [ ! -e "$d" ] && ln -s "$wt/$d" "$d"; done
(cd "$wt" && find . -maxdepth 3 -name node_modules -prune -type d 2>/dev/null) | while IFS= read -r d; do
  [ ! -e "$d" ] && [ -d "$(dirname "$d")" ] && ln -s "$wt/$d" "$d"
done
[ -d "$wt/target" ] && export CARGO_TARGET_DIR="$wt/target"
out=$(timeout 600 "$@" {cmd} 2>&1); code=$?
{strip}
cd "$wt"
git worktree remove --force "$tmp/base" >/dev/null 2>&1
rm -rf "$tmp"
git worktree prune >/dev/null 2>&1
echo "MONAGED_EXIT $code"
printf '%s\n' "$out" | grep -E '^\s*(FAIL|ERROR:|--- FAIL|✕|×|test .*FAILED$)' | head -n 200"#,
        wt = quote(worktree),
        sha = quote(&sha),
        cmd = quote(command),
        runner = runner(fence),
        strip = STRIP_COLORS,
    );
    let out = run(distro, &script).ok()?;
    let code: i32 = out.lines().find_map(|l| l.strip_prefix("MONAGED_EXIT ")).and_then(|c| c.trim().parse().ok())?;
    let result = (code != 0).then(|| out.lines().skip_while(|l| !l.starts_with("MONAGED_EXIT")).skip(1).collect::<Vec<_>>().join("\n"));
    if let Ok(mut cache) = BASE_RESULTS.lock() {
        if cache.len() > 32 {
            cache.remove(0);
        }
        cache.push((key, result.clone()));
    }
    result
}

// ---- Adversarial review ----------------------------------------------------

const REVIEW_SYSTEM: &str = "You are a skeptical senior code reviewer. Your job is to find concrete reasons why this \
change would NOT do what the task asked, or would break something. Assume nothing works until the code shows it.

You get: the task and the user's later messages (later ones can change or add to the task); the project's notes \
(its conventions and decisions: follow them, and don't report something they explain or allow); the agent's own \
account of its work (a claim to check against the code, never evidence by itself); findings the user already ruled \
out, with their reasons (don't report them again unless the change made them real); the automated checks; the diff; \
and the full current text of the changed files.

Report only problems you can point to in the code shown (which file, what is wrong). Use the full files to check that \
what the diff calls, imports or changes really exists and fits. When something only looks wrong because it depends on \
code you can't see, leave it out, or report it as low and say what you couldn't check. Do not invent issues and do not \
pad with style nits. Severity: high = it will fail or break existing behaviour; medium = a likely bug or a missing \
piece the task needs; low = a minor risk. verdict: works, partly, broken, or unclear (unclear only when what's shown \
is too partial to judge). summary: one sentence.

Every finding is a defect. Leave out anything you concluded is fine, acceptable, documented, deliberate or merely \
unverified; a finding that ends in \"no defect confirmed\" or \"this is fine\" is not a finding. The automated checks \
are the evidence about tests: don't report that the agent didn't or couldn't run them, and when the checks couldn't \
run, judge the code itself. Text like [masked] or [masked token] is Monaged hiding a secret from you, not the code. \
confirmed: true only when the code shown proves the problem; false when it rests on code, files or behaviour you \
couldn't see (unconfirmed findings count as minor).

Judge the change, not the conversation: what the later messages ask the agent to run, report or say is process, not \
part of the change, and never a finding. Give \"broken\" or \"partly\" only with a high or medium finding that shows \
it; when the diff is too large to see whole and nothing you saw is wrong, say \"unclear\".";

/// A finding you said isn't a problem, and why (the next reviews are told).
#[derive(Clone, Debug, PartialEq)]
pub struct Settled {
    pub file: String,
    pub problem: String,
    pub reason: String,
}

pub fn settled_to_json(list: &[Settled]) -> Value {
    Value::Array(list.iter().map(|s| json!({ "file": s.file, "problem": s.problem, "reason": s.reason })).collect())
}

pub fn settled_from_json(text: &str) -> Vec<Settled> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|v| Settled {
            file: v["file"].as_str().unwrap_or_default().to_string(),
            problem: v["problem"].as_str().unwrap_or_default().to_string(),
            reason: v["reason"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// What the reviewer gets besides the diff and the checks, so it judges the
/// change the way someone who knows the project would.
#[derive(Clone, Debug, Default)]
pub struct ReviewContext {
    /// The agent's first message: the task.
    pub task: String,
    /// What you said after that, oldest first.
    pub follow_ups: Vec<String>,
    /// The agent's own account of its last turn.
    pub report: String,
    /// The project's CLAUDE.md and the shared notes for its agents.
    pub knowledge: String,
    /// Changed files' current text: (path, text).
    pub files: Vec<(String, String)>,
    /// Changed files left out for size.
    pub files_left_out: usize,
    pub settled: Vec<Settled>,
}

/// Room for the changed files' full text in one review (characters).
const MAX_FILES_CHARS: usize = 80_000;
/// The longest single file read in full.
const MAX_FILE_CHARS: usize = 40_000;

/// The script that reads the project notes and the changed files from the
/// worktree: each text file between markers, binary files skipped.
pub fn context_script(worktree: &str, shared_notes: Option<&str>, paths: &[String], marker: &str) -> String {
    let mut s = format!(
        "cd {} || exit 1\nroot=$(pwd -P)\n\
         inside() {{ case \"$(realpath -- \"$1\" 2>/dev/null)\" in \"$root\"/*) return 0 ;; *) return 1 ;; esac; }}\n\
         emit() {{ inside \"$1\" || return 0; show \"$@\"; }}\n\
         show() {{ [ -f \"$1\" ] || return 0; [ -L \"$1\" ] && return 0; if [ -s \"$1\" ] && ! grep -Iq . \"$1\" 2>/dev/null; then return 0; fi; echo \"{marker}_BEGIN $2\"; head -c \"$3\" \"$1\"; echo; [ \"$(wc -c < \"$1\")\" -gt \"$3\" ] && echo '[… the rest of this file is left out …]'; echo \"{marker}_END\"; }}\n",
        quote(worktree)
    );
    s.push_str("emit CLAUDE.md knowledge:CLAUDE.md 8000\n");
    if let Some(notes) = shared_notes {
        // The shared notes live next to the repo, outside the worktree.
        s.push_str(&format!("show {} knowledge:shared 6000\n", crate::wsl::shell_path(notes)));
    }
    for path in paths {
        s.push_str(&format!("emit {} {} {}\n", quote(path), quote(&format!("file:{path}")), MAX_FILE_CHARS));
    }
    s.push_str("true\n");
    s
}

/// Reads `context_script`'s output: (label, text) in order.
pub fn parse_context(out: &str, marker: &str) -> Vec<(String, String)> {
    let (begin, end) = (format!("{marker}_BEGIN "), format!("{marker}_END"));
    let mut found = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    for line in out.lines() {
        if let Some(label) = line.strip_prefix(&begin) {
            current = Some((label.to_string(), Vec::new()));
        } else if line == end {
            if let Some((label, lines)) = current.take() {
                found.push((label, lines.join("\n").trim_end().to_string()));
            }
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    found
}

/// Reads the notes and changed files for a review (within the size budget).
pub fn read_context(distro: &str, worktree: &str, shared_notes: Option<&str>, changed: &[String]) -> (String, Vec<(String, String)>, usize) {
    let marker = format!("MONAGED_CTX_{}", std::process::id());
    let Ok(out) = run(distro, &context_script(worktree, shared_notes, changed, &marker)) else { return (String::new(), Vec::new(), changed.len()) };
    let mut knowledge = Vec::new();
    let mut files = Vec::new();
    let mut used = 0;
    for (label, text) in parse_context(&out, &marker) {
        if let Some(which) = label.strip_prefix("knowledge:") {
            if !text.trim().is_empty() {
                let title = if which == "shared" { "Shared notes for this project's agents" } else { "CLAUDE.md" };
                knowledge.push(format!("## {title}\n{text}"));
            }
        } else if let Some(path) = label.strip_prefix("file:")
            && used + text.len() <= MAX_FILES_CHARS
        {
            used += text.len();
            files.push((path.to_string(), text));
        }
    }
    let left_out = changed.len().saturating_sub(files.len());
    (knowledge.join("\n\n"), files, left_out)
}

/// The review's prompt: everything the reviewer gets, in the order it reads best.
pub fn review_prompt(context: &ReviewContext, diff: &str, checks: &Checks) -> (String, String) {
    let (diff_text, diff_note) = if diff.len() > MAX_DIFF_CHARS {
        let mut cut = MAX_DIFF_CHARS;
        while !diff.is_char_boundary(cut) {
            cut -= 1;
        }
        (&diff[..cut], format!("\n[The diff is longer: only the first {MAX_DIFF_CHARS} of {} characters are shown.]", diff.len()))
    } else {
        (diff, String::new())
    };
    let mut prompt = format!("Task given to the agent:\n{}\n", clip(&context.task, 3000));
    if !context.follow_ups.is_empty() {
        prompt.push_str("\nThe user's later messages to the agent, oldest first:\n");
        for message in context.follow_ups.iter().rev().take(8).rev() {
            prompt.push_str(&format!("- {}\n", clip(message, 800).replace('\n', "\n  ")));
        }
    }
    if !context.knowledge.trim().is_empty() {
        prompt.push_str(&format!("\nThe project's notes (its conventions and decisions):\n{}\n", clip(&context.knowledge, 10_000)));
    }
    if !context.report.trim().is_empty() {
        prompt.push_str(&format!("\nThe agent's own account of its last turn (a claim to check, not evidence):\n{}\n", clip(&context.report, 2500)));
    }
    if !context.settled.is_empty() {
        prompt.push_str("\nFindings the user ruled out before (don't report them again unless this change made them real):\n");
        for s in context.settled.iter().rev().take(25) {
            let why = if s.reason.trim().is_empty() { "no reason given".to_string() } else { s.reason.clone() };
            prompt.push_str(&format!("- {}: {} (user: {})\n", s.file, clip(&s.problem, 300), clip(&why, 300)));
        }
    }
    prompt.push_str(&format!("\nAutomated checks:\n{}\n\nDiff (the agent's worktree vs. where its branch started):\n```diff\n{diff_text}\n```{diff_note}\n", checks.describe()));
    if !context.files.is_empty() {
        prompt.push_str("\nThe changed files as they are now:\n");
        for (path, text) in &context.files {
            prompt.push_str(&format!("\n--- {path} ---\n{text}\n"));
        }
        if context.files_left_out > 0 {
            prompt.push_str(&format!("\n[{} more changed file(s) left out for size.]\n", context.files_left_out));
        }
    }
    (prompt, diff_note)
}

const MAX_DIFF_CHARS: usize = 120_000;

pub fn review(backend: &Backend, context: &ReviewContext, diff: &str, checks: &Checks) -> Result<(Confidence, Usage)> {
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
                        "problem": { "type": "string" },
                        "confirmed": { "type": "boolean" }
                    },
                    "required": ["severity", "file", "problem", "confirmed"],
                    "additionalProperties": false
                }
            },
            "summary": { "type": "string" }
        },
        "required": ["verdict", "issues", "summary"],
        "additionalProperties": false
    });
    let (prompt, diff_note) = review_prompt(context, diff, checks);
    let (value, usage) = llm::complete_json(backend, REVIEW_SYSTEM, &prompt, &schema)?;
    let issues = read_issues(&value["issues"]);
    let (verdict, unbacked) = settle_verdict(value["verdict"].as_str().unwrap_or("unclear"), &issues);
    let (score, mut reasons) = score(checks, &verdict, &issues);
    if let Some(said) = unbacked {
        reasons.push(format!("– The reviewer said \"{said}\" without naming anything serious, so it counts as unclear"));
    }
    if let Some(summary) = value["summary"].as_str().filter(|s| !s.is_empty()) {
        reasons.push(format!("Reviewer: {summary}"));
    }
    if !diff_note.is_empty() {
        reasons.push("The diff was too large to review in full; only the first part was reviewed.".into());
    }
    Ok((Confidence { score, verdict, reasons, issues, checks: checks.describe() }, usage))
}

/// The reviewer's findings, minus the ones that aren't defects: a finding
/// the code doesn't prove becomes low (it's shown, but doesn't cost score),
/// and one that talks itself out of being a problem is dropped.
fn read_issues(value: &Value) -> Vec<Issue> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let problem = i["problem"].as_str().unwrap_or_default().trim().to_string();
            let lower = problem.to_lowercase();
            if problem.is_empty() || NOT_A_DEFECT.iter().any(|s| lower.contains(s)) {
                return None;
            }
            let mut severity = i["severity"].as_str().unwrap_or("low").to_string();
            let mut problem = problem;
            if i["confirmed"].as_bool() == Some(false) && severity != "low" {
                severity = "low".into();
                problem = format!("(Not confirmed in the code shown.) {problem}");
            }
            Some(Issue { severity, file: i["file"].as_str().unwrap_or_default().to_string(), problem })
        })
        .collect()
}

/// "broken" or "partly" has to rest on a high or medium finding. Without
/// one (only minor or unconfirmed findings), the reviewer couldn't show what
/// doesn't work, and the verdict counts as unclear, as it does once you rule
/// out the last serious finding. Returns the verdict to score and, when it
/// was changed, what the reviewer said.
pub fn settle_verdict(said: &str, issues: &[Issue]) -> (String, Option<String>) {
    let serious = issues.iter().any(|i| matches!(i.severity.as_str(), "high" | "medium"));
    if matches!(said, "broken" | "partly") && !serious {
        return ("unclear".into(), Some(said.to_string()));
    }
    (said.to_string(), None)
}

/// A finding that says this about itself isn't one.
const NOT_A_DEFECT: [&str; 5] = ["no concrete defect", "no defect confirmed", "not a defect", "so this is fine", "rather than a defect"];

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
        Checks::FailedBefore { command, .. } => {
            cap = cap.min(4.0);
            reasons.push(format!("~ Checks fail (`{command}`), but only tests that failed before this change too"));
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

/// Reasons `score` writes start with one of these marks; the rest (the
/// reviewer's summary, notes) are kept when scoring again.
const SCORE_MARKS: [&str; 5] = ["✓ ", "✕ ", "~ ", "? ", "– "];

/// You said a finding isn't a problem: drop it and score again. Once no high
/// or medium finding is left, the reviewer's "broken"/"partly" verdict no
/// longer stands on anything shown, so it counts as unclear until the next review.
pub fn rule_out(c: &mut Confidence, index: usize) -> Option<Issue> {
    if index >= c.issues.len() {
        return None;
    }
    let issue = c.issues.remove(index);
    let ruled_before = c
        .reasons
        .iter()
        .find_map(|r| r.strip_prefix("– You ruled out ").and_then(|t| t.split_whitespace().next()).and_then(|n| n.parse::<usize>().ok()))
        .unwrap_or(0);
    let serious_left = c.issues.iter().any(|i| matches!(i.severity.as_str(), "high" | "medium"));
    if !serious_left && matches!(c.verdict.as_str(), "broken" | "partly") {
        c.verdict = "unclear".into();
    }
    let (score, mut reasons) = score(&Checks::from_description(&c.checks), &c.verdict, &c.issues);
    let ruled = ruled_before + 1;
    reasons.push(format!("– You ruled out {ruled} finding{}; review again to confirm", if ruled == 1 { "" } else { "s" }));
    reasons.extend(c.reasons.iter().filter(|r| !SCORE_MARKS.iter().any(|m| r.starts_with(m))).cloned());
    c.score = score;
    c.reasons = reasons;
    Some(issue)
}

/// The message that asks the agent to fix what the review found, or None
/// when there's nothing to fix.
pub fn fix_prompt(c: &Confidence) -> Option<String> {
    let failed = match Checks::from_description(&c.checks) {
        Checks::Failed { command, tail } => Some((command, tail)),
        _ => None,
    };
    if c.issues.is_empty() && failed.is_none() {
        return None;
    }
    let mut text = String::from(
        "Monaged's review of your changes found the problems below. Check each one against the code first: \
         if one isn't a real problem, say so and why instead of changing code. Fix the rest, then run the tests.\n",
    );
    if let Some((command, tail)) = failed {
        text.push_str(&format!("\nThe tests failed (`{command}`). The end of their output:\n```\n{}\n```\n", clip(tail.trim(), 2500)));
    }
    if !c.issues.is_empty() {
        text.push_str("\nFindings:\n");
        for (n, issue) in c.issues.iter().enumerate() {
            let place = if issue.file.trim().is_empty() { String::new() } else { format!(" {}:", issue.file.trim()) };
            text.push_str(&format!("{}. [{}]{} {}\n", n + 1, issue.severity, place, issue.problem.trim()));
        }
    }
    Some(text.trim_end().to_string())
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

#[cfg(test)]
mod checks_tests {
    use super::*;

    fn read(out: &str) -> Checks {
        read_checks(out, |_| false)
    }

    #[test]
    fn a_windows_npm_is_an_environment_problem_not_failing_tests() {
        // Mobietrain: every builder got 1-2/5 because WSL ran Windows' npm.
        let c = read("MONAGED_CMD npm test --silent\nMONAGED_WINDOWS npm\n");
        assert!(matches!(&c, Checks::Unavailable(why) if why.contains("Windows `npm`")), "{}", c.describe());
        assert!(matches!(read("MONAGED_CMD bun run test\nMONAGED_MISSING bun\n"), Checks::Unavailable(why) if why.contains("`bun`")));
    }

    #[test]
    fn a_runner_that_stops_in_silence_couldnt_start() {
        let c = read("MONAGED_CMD npm test --silent\nMONAGED_EXIT 1\n\n");
        assert!(matches!(&c, Checks::Unavailable(why) if why.contains("without printing anything")), "{}", c.describe());
        let c = read("MONAGED_CMD npm test\nMONAGED_EXIT 1\n'turbo' is not recognized as an internal or external command,\n");
        assert!(matches!(&c, Checks::Unavailable(why) if why.contains("Windows programs")), "{}", c.describe());
    }

    #[test]
    fn real_failures_still_fail() {
        let c = read("MONAGED_CMD bun run test\nMONAGED_FAILLINE FAIL src/a.test.ts\nMONAGED_EXIT 1\n FAIL src/a.test.ts > adds\nTests 1 failed | 9 passed\n");
        assert!(matches!(&c, Checks::Failed { command, .. } if command == "bun run test"), "{}", c.describe());
        let c = read_checks("MONAGED_CMD bun run test\nMONAGED_EXIT 1\n FAIL x\n", |_| true);
        assert!(matches!(c, Checks::FailedBefore { .. }));
        assert!(matches!(read("MONAGED_CMD cargo test --quiet\nMONAGED_EXIT 0\nok\n"), Checks::Passed(c) if c == "cargo test --quiet"));
    }

    #[test]
    fn coloured_failures_are_read_like_plain_ones() {
        // vitest colours " FAIL " even into a pipe; that hid every failing test.
        let line = "\u{1b}[41m\u{1b}[1m FAIL \u{1b}[22m\u{1b}[49m \u{1b}[30m\u{1b}[45m tools \u{1b}[49m\u{1b}[39m app/x.test.ts\u{1b}[2m > \u{1b}[22madds";
        assert_eq!(strip_colors(line), " FAIL   tools  app/x.test.ts > adds");
        assert_eq!(failure_lines(&strip_colors(line)).len(), 1);
        let c = read(&format!("MONAGED_CMD bun run test\nMONAGED_EXIT 1\n{line}\n"));
        assert!(matches!(&c, Checks::Failed { tail, .. } if !tail.contains('\u{1b}')), "{}", c.describe());
    }

    #[test]
    fn a_verdict_needs_a_serious_finding_behind_it() {
        let issue = |severity: &str| Issue { severity: severity.into(), file: "a.ts".into(), problem: "x".into() };
        // api-client-worker: "broken", three minor findings, so 2/5 every round.
        let (verdict, said) = settle_verdict("broken", &[issue("low"), issue("low")]);
        assert_eq!((verdict.as_str(), said.as_deref()), ("unclear", Some("broken")));
        let checks = Checks::FailedBefore { command: "bun run test".into(), tail: String::new() };
        assert_eq!(score(&checks, &verdict, &[issue("low")]).0, 4);
        // With something serious it stands, and so do "works" and "unclear".
        assert_eq!(settle_verdict("partly", &[issue("medium")]), ("partly".to_string(), None));
        assert_eq!(settle_verdict("broken", &[issue("high")]), ("broken".to_string(), None));
        assert_eq!(settle_verdict("works", &[]), ("works".to_string(), None));
    }

    #[test]
    fn a_failed_install_says_what_failed() {
        let c = read("MONAGED_CMD bun run test\nMONAGED_INSTALL bun install --frozen-lockfile\nMONAGED_INSTALL_FAILED 1\nerror: lockfile had changes, but lockfile is frozen\n");
        assert!(matches!(&c, Checks::Unavailable(why) if why.contains("bun install --frozen-lockfile") && why.contains("lockfile is frozen")), "{}", c.describe());
    }

    #[test]
    fn findings_that_arent_defects_dont_cost_score() {
        let issues = read_issues(&json!([
            { "severity": "high", "file": "a.ts", "problem": "Imports X, which doesn't exist.", "confirmed": true },
            { "severity": "high", "file": "b.ts", "problem": "Depends on ensureIndex, not visible here.", "confirmed": false },
            { "severity": "medium", "file": "q.ts", "problem": "Uses IN (?) with an array; this is fine. No concrete defect confirmed.", "confirmed": true },
            { "severity": "low", "file": "c.ts", "problem": "Naming.", "confirmed": false },
        ]));
        assert_eq!(issues.len(), 3);
        assert_eq!(issues[0].severity, "high");
        assert_eq!(issues[1].severity, "low");
        assert!(issues[1].problem.starts_with("(Not confirmed"));
        assert_eq!(issues[2].problem, "Naming.");
        // Older reviews without the field read as confirmed.
        assert_eq!(read_issues(&json!([{ "severity": "medium", "file": "", "problem": "x" }]))[0].severity, "medium");
    }
}

#[cfg(test)]
mod wsl_checks {
    use super::*;

    /// Runs the real check script in WSL on throwaway projects (needs WSL:
    /// `cargo test -- --ignored checks_run_in_wsl`).
    #[test]
    #[ignore]
    fn checks_run_in_wsl() {
        let distro = std::env::var("MONAGED_TEST_DISTRO").unwrap_or_else(|_| "Ubuntu".into());
        let dir = run(&distro, "mktemp -d").unwrap().trim().lines().last().unwrap().to_string();
        let setup = format!(
            "cd {d} && mkdir bunp npmp && cd bunp && printf '%s' '{{\"name\":\"t\",\"packageManager\": \"bun@1.2.0\",\"scripts\":{{\"test\":\"bun test\"}}}}' > package.json \
             && printf 'import {{ test, expect }} from \"bun:test\";\ntest(\"adds\", () => expect(1 + 1).toBe(2));\n' > a.test.ts && bun install >/dev/null 2>&1; \
             cd ../npmp && printf '%s' '{{\"name\":\"t\",\"scripts\":{{\"test\":\"node -e 1\"}}}}' > package.json",
            d = quote(&dir)
        );
        run(&distro, &setup).unwrap();
        let bun = run_checks(&distro, &format!("{dir}/bunp"), None, None, false);
        let npm = run_checks(&distro, &format!("{dir}/npmp"), None, None, false);
        let _ = run(&distro, &format!("rm -rf {}", quote(&dir)));
        assert!(matches!(&bun, Checks::Passed(c) if c == "bun run test"), "bun project: {}", bun.describe());
        println!("npm project: {}", npm.describe());
        assert!(!matches!(npm, Checks::Failed { .. }), "an npm project without a Linux npm must not count as failing: {}", npm.describe());
    }
}

#[cfg(test)]
mod real_worktree {
    use super::*;

    /// Runs the checks on a real worktree, with the base comparison
    /// (`MONAGED_TEST_WORKTREE`, `MONAGED_TEST_BASE`; needs WSL):
    /// `cargo test -- --ignored checks_on_a_real_worktree --nocapture`.
    #[test]
    #[ignore]
    fn checks_on_a_real_worktree() {
        let (Ok(worktree), Ok(base)) = (std::env::var("MONAGED_TEST_WORKTREE"), std::env::var("MONAGED_TEST_BASE")) else { return };
        let distro = std::env::var("MONAGED_TEST_DISTRO").unwrap_or_else(|_| "Ubuntu".into());
        let checks = run_checks(&distro, &worktree, None, Some(&base), false);
        println!("CHECKS: {}", checks.describe().lines().take(3).collect::<Vec<_>>().join(" | "));
    }
}
