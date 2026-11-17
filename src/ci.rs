//! CI pipelines on GitLab (pipelines) and GitHub (Actions runs), per branch:
//! the latest run with its jobs by stage, and what you can do with it (open
//! it, retry what failed, cancel it, run a new one, read a failed job's log).
//!
//! With an account from Settings → Git accounts, Monaged calls the host's REST
//! API itself; without one it goes through `glab api` / `gh api`, so the
//! logins those CLIs hold work too.

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::hosting::{Provider, Remote};

/// Where a run or a job stands, the same words for both hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Running,
    Passed,
    Failed,
    Canceled,
    Skipped,
    /// Waits for someone to start it (GitLab manual jobs).
    Manual,
}

impl Status {
    pub fn from_gitlab(status: &str) -> Self {
        match status {
            "running" => Status::Running,
            "success" => Status::Passed,
            "failed" => Status::Failed,
            "canceled" | "canceling" => Status::Canceled,
            "skipped" => Status::Skipped,
            "manual" => Status::Manual,
            _ => Status::Waiting,
        }
    }

    pub fn from_github(status: &str, conclusion: &str) -> Self {
        match (status, conclusion) {
            ("completed", "success") => Status::Passed,
            ("completed", "cancelled") => Status::Canceled,
            ("completed", "skipped" | "neutral") => Status::Skipped,
            ("completed", _) => Status::Failed,
            ("in_progress", _) => Status::Running,
            _ => Status::Waiting,
        }
    }

    /// Still going (or about to).
    pub fn active(self) -> bool {
        matches!(self, Status::Waiting | Status::Running)
    }

    pub fn word(self) -> &'static str {
        match self {
            Status::Waiting => "waiting",
            Status::Running => "running",
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Canceled => "canceled",
            Status::Skipped => "skipped",
            Status::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: u64,
    pub name: String,
    /// GitLab's stage; GitHub has none (empty).
    pub stage: String,
    pub status: Status,
    /// Seconds, once it ran.
    pub duration: Option<f64>,
    pub url: String,
    /// A failure here doesn't fail the run.
    pub allow_failure: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub id: u64,
    pub provider: Provider,
    pub status: Status,
    pub branch: String,
    /// The commit it ran on (short).
    pub sha: String,
    pub url: String,
    /// What started it: a push, a merge request, a schedule, by hand…
    pub source: String,
    /// When it was made and last changed (RFC 3339, from the host).
    pub created: String,
    pub updated: String,
    pub jobs: Vec<Job>,
}

impl Run {
    /// Jobs whose failure fails the run.
    pub fn failed_jobs(&self) -> Vec<&Job> {
        self.jobs.iter().filter(|j| j.status == Status::Failed && !j.allow_failure).collect()
    }

    /// (finished, counted): jobs that ran or will run, not manual or skipped ones.
    pub fn progress(&self) -> (usize, usize) {
        let counted: Vec<&Job> = self.jobs.iter().filter(|j| !matches!(j.status, Status::Manual | Status::Skipped)).collect();
        (counted.iter().filter(|j| !j.status.active()).count(), counted.len())
    }

    /// "passed", "running (3 of 7 jobs)", "failed in test and build".
    pub fn summary(&self) -> String {
        match self.status {
            Status::Running | Status::Waiting => {
                let (done, total) = self.progress();
                if total == 0 { self.status.word().to_string() } else { format!("{} ({done} of {total} jobs)", self.status.word()) }
            }
            Status::Failed => {
                let failed: Vec<&str> = self.failed_jobs().iter().map(|j| j.name.as_str()).collect();
                match failed.len() {
                    0 => "failed".into(),
                    1 => format!("failed in {}", failed[0]),
                    n if n <= 3 => format!("failed in {} and {}", failed[..n - 1].join(", "), failed[n - 1]),
                    n => format!("failed in {} and {} more", failed[..2].join(", "), n - 2),
                }
            }
            other => other.word().to_string(),
        }
    }

    /// The jobs in their stages' order (stages as they first appear).
    pub fn stages(&self) -> Vec<(String, Vec<&Job>)> {
        let mut out: Vec<(String, Vec<&Job>)> = Vec::new();
        for job in &self.jobs {
            match out.iter_mut().find(|(s, _)| *s == job.stage) {
                Some((_, jobs)) => jobs.push(job),
                None => out.push((job.stage.clone(), vec![job])),
            }
        }
        out
    }
}

/// `%2F`-style escaping for path segments and query values.
fn enc(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn provider_of(remote: &Remote) -> Result<Provider> {
    remote.provider.with_context(|| format!("{} isn't GitHub or GitLab", remote.host))
}

/// A JSON call: the account's token when there is one, else `glab api` / `gh api`.
fn call(remote: &Remote, method: &str, path: &str) -> Result<Value> {
    let provider = provider_of(remote)?;
    if let Some(token) = crate::git_auth::token_for(&remote.host, remote.login.as_deref()).map(|s| s.token) {
        let (status, reply, _) = crate::git_auth::api(method, provider, &remote.host, &token, path, None)?;
        if !(200..300).contains(&status) {
            bail!("{}: {}", remote.host, crate::git_auth::api_error(status, &reply));
        }
        return Ok(reply);
    }
    let out = cli_api(remote, provider, method, path)?;
    if out.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&out).with_context(|| format!("unexpected reply from {}: {}", remote.host, out.chars().take(120).collect::<String>()))
}

fn cli_api(remote: &Remote, provider: Provider, method: &str, path: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let program = match provider {
        Provider::GitHub => "gh",
        Provider::GitLab => "glab",
    };
    let mut command = Command::new(program);
    command.arg("api").arg(path.trim_start_matches('/')).stdin(Stdio::null());
    if method != "GET" {
        command.args(["-X", method]);
    }
    match provider {
        Provider::GitLab if remote.host != "gitlab.com" => drop(command.env("GITLAB_HOST", format!("https://{}", remote.host))),
        Provider::GitHub if remote.host != "github.com" => drop(command.args(["--hostname", &remote.host])),
        _ => {}
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let output = command.output().with_context(|| format!("couldn't run {program}; sign in under Settings → Git accounts, or install {program}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.lines().chain(stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        bail!("{program}: {message}");
    }
    Ok(stdout)
}

fn text(remote: &Remote, path: &str) -> Result<String> {
    let provider = provider_of(remote)?;
    if let Some(token) = crate::git_auth::token_for(&remote.host, remote.login.as_deref()).map(|s| s.token) {
        let (status, body) = crate::git_auth::api_text(provider, &remote.host, &token, path)?;
        if !(200..300).contains(&status) {
            bail!("{}: HTTP {status}", remote.host);
        }
        return Ok(body);
    }
    cli_api(remote, provider, "GET", path)
}

fn project(remote: &Remote) -> String {
    enc(&remote.slug)
}

/// The newest run on `branch` with its jobs, if the host has one.
pub fn latest_run(remote: &Remote, branch: &str) -> Result<Option<Run>> {
    match provider_of(remote)? {
        Provider::GitLab => {
            let list = call(remote, "GET", &format!("/projects/{}/pipelines?ref={}&per_page=1&order_by=id&sort=desc", project(remote), enc(branch)))?;
            let Some(p) = list.as_array().and_then(|a| a.first()).cloned() else { return Ok(None) };
            let id = p["id"].as_u64().unwrap_or(0);
            let jobs = call(remote, "GET", &format!("/projects/{}/pipelines/{id}/jobs?per_page=100", project(remote)))?;
            Ok(Some(gitlab_run(&p, &jobs, branch)))
        }
        Provider::GitHub => {
            // One push starts a run per workflow, and some skip themselves: the
            // newest that didn't skip is the one that says something.
            let list = call(remote, "GET", &format!("/repos/{}/actions/runs?branch={}&per_page=10", remote.slug, enc(branch)))?;
            let runs = list["workflow_runs"].as_array().cloned().unwrap_or_default();
            let skipped = |r: &Value| r["conclusion"].as_str().is_some_and(|c| c == "skipped" || c == "neutral");
            let Some(r) = runs.iter().find(|r| !skipped(r)).or_else(|| runs.first()).cloned() else { return Ok(None) };
            let id = r["id"].as_u64().unwrap_or(0);
            let jobs = call(remote, "GET", &format!("/repos/{}/actions/runs/{id}/jobs?per_page=100", remote.slug))?;
            Ok(Some(github_run(&r, &jobs, branch)))
        }
    }
}

pub fn gitlab_run(p: &Value, jobs: &Value, branch: &str) -> Run {
    let mut jobs: Vec<Job> = jobs
        .as_array()
        .into_iter()
        .flatten()
        .map(|j| Job {
            id: j["id"].as_u64().unwrap_or(0),
            name: j["name"].as_str().unwrap_or("job").to_string(),
            stage: j["stage"].as_str().unwrap_or_default().to_string(),
            status: Status::from_gitlab(j["status"].as_str().unwrap_or_default()),
            duration: j["duration"].as_f64(),
            url: j["web_url"].as_str().unwrap_or_default().to_string(),
            allow_failure: j["allow_failure"].as_bool().unwrap_or(false),
        })
        .collect();
    // The API lists the newest jobs first; the pipeline reads oldest first.
    jobs.reverse();
    Run {
        id: p["id"].as_u64().unwrap_or(0),
        provider: Provider::GitLab,
        status: Status::from_gitlab(p["status"].as_str().unwrap_or_default()),
        branch: branch.to_string(),
        sha: p["sha"].as_str().unwrap_or_default().chars().take(8).collect(),
        url: p["web_url"].as_str().unwrap_or_default().to_string(),
        source: p["source"].as_str().unwrap_or_default().replace('_', " "),
        created: p["created_at"].as_str().unwrap_or_default().to_string(),
        updated: p["updated_at"].as_str().unwrap_or_default().to_string(),
        jobs,
    }
}

pub fn github_run(r: &Value, jobs: &Value, branch: &str) -> Run {
    let seconds = |start: &Value, end: &Value| -> Option<f64> {
        let (a, b) = (start.as_str()?, end.as_str()?);
        Some((parse_time(b)? - parse_time(a)?) as f64)
    };
    let jobs: Vec<Job> = jobs["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|j| Job {
            id: j["id"].as_u64().unwrap_or(0),
            name: j["name"].as_str().unwrap_or("job").to_string(),
            stage: String::new(),
            status: Status::from_github(j["status"].as_str().unwrap_or_default(), j["conclusion"].as_str().unwrap_or_default()),
            duration: seconds(&j["started_at"], &j["completed_at"]),
            url: j["html_url"].as_str().unwrap_or_default().to_string(),
            allow_failure: false,
        })
        .collect();
    Run {
        id: r["id"].as_u64().unwrap_or(0),
        provider: Provider::GitHub,
        status: Status::from_github(r["status"].as_str().unwrap_or_default(), r["conclusion"].as_str().unwrap_or_default()),
        branch: branch.to_string(),
        sha: r["head_sha"].as_str().unwrap_or_default().chars().take(8).collect(),
        url: r["html_url"].as_str().unwrap_or_default().to_string(),
        source: r["event"].as_str().unwrap_or_default().replace('_', " "),
        created: r["created_at"].as_str().unwrap_or_default().to_string(),
        updated: r["updated_at"].as_str().unwrap_or_default().to_string(),
        jobs,
    }
}

/// Seconds since 1970 from an RFC 3339 time ("2026-11-17T19:59:14.123Z", "+01:00").
pub fn parse_time(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let time: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == ':').collect();
    let mut t = time.split(':').map(|p| p.parse::<i64>());
    let (h, min, s) = (t.next()?.ok()?, t.next()?.ok()?, t.next().and_then(|v| v.ok()).unwrap_or(0));
    // The offset after the time (Z, +hh:mm, -hh:mm).
    let tail = &rest[rest.find(['Z', '+', '-']).unwrap_or(rest.len())..];
    let offset = match tail.chars().next() {
        Some(sign @ ('+' | '-')) => {
            let mut o = tail[1..].split(':').map(|p| p.parse::<i64>().unwrap_or(0));
            let secs = o.next().unwrap_or(0) * 3600 + o.next().unwrap_or(0) * 60;
            if sign == '+' { secs } else { -secs }
        }
        _ => 0,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + min * 60 + s - offset)
}

/// Runs the failed jobs again.
pub fn retry(remote: &Remote, run: &Run) -> Result<()> {
    match run.provider {
        Provider::GitLab => call(remote, "POST", &format!("/projects/{}/pipelines/{}/retry", project(remote), run.id))?,
        Provider::GitHub => call(remote, "POST", &format!("/repos/{}/actions/runs/{}/rerun-failed-jobs", remote.slug, run.id))?,
    };
    Ok(())
}

pub fn cancel(remote: &Remote, run: &Run) -> Result<()> {
    match run.provider {
        Provider::GitLab => call(remote, "POST", &format!("/projects/{}/pipelines/{}/cancel", project(remote), run.id))?,
        Provider::GitHub => call(remote, "POST", &format!("/repos/{}/actions/runs/{}/cancel", remote.slug, run.id))?,
    };
    Ok(())
}

/// A new run of the branch's pipeline (GitLab; GitHub runs come from its workflows).
pub fn start(remote: &Remote, branch: &str) -> Result<()> {
    match provider_of(remote)? {
        Provider::GitLab => {
            call(remote, "POST", &format!("/projects/{}/pipeline?ref={}", project(remote), enc(branch)))?;
            Ok(())
        }
        Provider::GitHub => bail!("GitHub starts runs from its workflows' own triggers; retry the latest run instead"),
    }
}

/// The last `lines` lines of a job's log, without colours or GitLab's section markers.
pub fn job_log_tail(remote: &Remote, provider: Provider, job: u64, lines: usize) -> Result<String> {
    let raw = match provider {
        Provider::GitLab => text(remote, &format!("/projects/{}/jobs/{job}/trace", project(remote)))?,
        Provider::GitHub => text(remote, &format!("/repos/{}/actions/jobs/{job}/logs", remote.slug))?,
    };
    Ok(clean_log(&raw, lines))
}

/// A log as a person reads it: the end, colours and section markers taken out.
pub fn clean_log(raw: &str, lines: usize) -> String {
    let plain = crate::review::strip_colors(raw);
    let kept: Vec<&str> = plain
        .lines()
        // GitLab writes "section_start:…\r" markers and carriage-return redraws.
        .map(|l| l.rsplit('\r').next().unwrap_or(l))
        .filter(|l| !l.starts_with("section_start:") && !l.starts_with("section_end:"))
        .collect();
    let start = kept.len().saturating_sub(lines);
    kept[start..].join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_gitlab_pipeline_reads_as_a_run_with_stages() {
        let p = json!({ "id": 42, "status": "failed", "sha": "53f0ccfb0123", "web_url": "https://gitlab.com/x/-/pipelines/42", "source": "push", "created_at": "2026-11-17T20:11:50Z", "updated_at": "2026-11-17T20:19:00Z" });
        // Newest first, as the API lists them.
        let jobs = json!([
            { "id": 4, "name": "build", "stage": "build", "status": "skipped" },
            { "id": 3, "name": "test", "stage": "test", "status": "failed", "duration": 61.5 },
            { "id": 2, "name": "audit", "stage": "check", "status": "failed", "allow_failure": true },
            { "id": 1, "name": "lint", "stage": "check", "status": "success", "duration": 12.0 }
        ]);
        let run = gitlab_run(&p, &jobs, "develop");
        assert_eq!((run.id, run.status, run.sha.as_str(), run.source.as_str()), (42, Status::Failed, "53f0ccfb", "push"));
        let stages: Vec<(String, usize)> = run.stages().into_iter().map(|(s, j)| (s, j.len())).collect();
        assert_eq!(stages, [("check".to_string(), 2), ("test".to_string(), 1), ("build".to_string(), 1)]);
        // A failure that's allowed doesn't count.
        assert_eq!(run.summary(), "failed in test");
        assert_eq!(run.progress(), (3, 3));
    }

    #[test]
    fn a_running_github_run_counts_its_jobs() {
        let r = json!({ "id": 7, "status": "in_progress", "conclusion": null, "head_sha": "abcdef1234", "html_url": "u", "event": "push", "created_at": "2026-11-17T20:00:00Z", "updated_at": "2026-11-17T20:01:00Z" });
        let jobs = json!({ "jobs": [
            { "id": 1, "name": "Build and package", "status": "completed", "conclusion": "success", "started_at": "2026-11-17T20:00:05Z", "completed_at": "2026-11-17T20:02:59Z" },
            { "id": 2, "name": "Publish", "status": "queued", "conclusion": null }
        ]});
        let run = github_run(&r, &jobs, "main");
        assert_eq!(run.status, Status::Running);
        assert_eq!(run.summary(), "running (1 of 2 jobs)");
        assert_eq!(run.jobs[0].duration, Some(174.0));
    }

    #[test]
    fn statuses_and_times() {
        assert_eq!(Status::from_gitlab("pending"), Status::Waiting);
        assert_eq!(Status::from_gitlab("canceling"), Status::Canceled);
        assert_eq!(Status::from_github("completed", "timed_out"), Status::Failed);
        assert!(Status::Waiting.active() && !Status::Manual.active());
        assert_eq!(parse_time("1970-01-01T00:01:00Z"), Some(60));
        assert_eq!(parse_time("2026-11-17T20:11:50.123Z"), parse_time("2026-11-17T21:11:50+01:00"));
        assert_eq!(parse_time("nope"), None);
    }

    #[test]
    fn logs_lose_colours_and_section_markers() {
        let raw = "section_start:1:prepare\r\u{1b}[0KPreparing\nline 1\nprogress 10%\rprogress 100%\n\u{1b}[31mFAIL src/a.test.ts\u{1b}[0m\nsection_end:2:prepare\r\u{1b}[0K\nERROR: Job failed: exit code 1";
        assert_eq!(clean_log(raw, 3), "progress 100%\nFAIL src/a.test.ts\nERROR: Job failed: exit code 1");
    }
}

#[cfg(test)]
mod real_host {
    use super::*;

    /// Reads a real repository's latest runs, changing nothing
    /// (`MONAGED_TEST_REMOTE` = its origin URL, `MONAGED_TEST_BRANCHES`):
    /// `cargo test -- --ignored runs_on_a_real_host --nocapture`.
    #[test]
    #[ignore]
    fn runs_on_a_real_host() {
        let Ok(url) = std::env::var("MONAGED_TEST_REMOTE") else { return };
        let remote = crate::hosting::parse_remote(&url).expect("a GitHub or GitLab URL");
        let branches = std::env::var("MONAGED_TEST_BRANCHES").unwrap_or_else(|_| "main".into());
        for branch in branches.split(',') {
            match latest_run(&remote, branch) {
                Ok(Some(run)) => {
                    println!("RUN {branch}: #{} {} · {} · {} jobs · {}", run.id, run.summary(), run.source, run.jobs.len(), run.sha);
                    for (stage, jobs) in run.stages() {
                        println!("  [{stage}] {}", jobs.iter().map(|j| format!("{}={}", j.name, j.status.word())).collect::<Vec<_>>().join(", "));
                    }
                    if let Some(job) = run.failed_jobs().first() {
                        let tail = job_log_tail(&remote, run.provider, job.id, 5).unwrap_or_else(|e| format!("log error: {e:#}"));
                        println!("  LOG {}: {}", job.name, tail.replace('\n', " | "));
                    }
                }
                Ok(None) => println!("RUN {branch}: none"),
                Err(e) => println!("RUN {branch}: error {e:#}"),
            }
        }
    }
}
