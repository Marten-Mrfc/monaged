//! GitHub and GitLab: merge/pull requests and CI status.
//!
//! With an account from Settings → Git accounts, Monaged talks to the host's
//! REST API itself, so neither `gh` nor `glab` is needed. Without one it uses
//! those CLIs on Windows and the logins they hold (and hands them the account's
//! token when there is one, so they work even when they're not logged in).

use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    pub fn request_name(self) -> &'static str {
        match self {
            Provider::GitHub => "pull request",
            Provider::GitLab => "merge request",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub provider: Option<Provider>,
    pub host: String,
    /// "owner/repo" or "group/subgroup/repo".
    pub slug: String,
    /// The account this project signs in as, when it isn't the host's default
    /// (Settings → Git accounts; the Git tab switches it).
    pub login: Option<String>,
}

/// Understands `git@host:owner/repo.git`, `https://host/owner/repo(.git)` and
/// `ssh://git@host:port/group/repo.git`.
pub fn parse_remote(url: &str) -> Option<Remote> {
    let url = url.trim();
    let (host, path) = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?.to_string(), path.to_string())
    } else {
        let rest = url.split_once('@').map_or(url, |(_, r)| r);
        let (host, path) = rest.split_once(':')?;
        (host.to_string(), path.to_string())
    };
    let slug = path.trim_matches('/').trim_end_matches(".git").to_string();
    if host.is_empty() || !slug.contains('/') {
        return None;
    }
    let lower = host.to_lowercase();
    let provider = if lower.contains("github") {
        Some(Provider::GitHub)
    } else if lower.contains("gitlab") {
        Some(Provider::GitLab)
    } else {
        None
    };
    Some(Remote { provider, host, slug, login: None })
}

fn cli(provider: Provider, remote: &Remote, args: &[&str]) -> Result<String> {
    let program = match provider {
        Provider::GitHub => "gh",
        Provider::GitLab => "glab",
    };
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null());
    if provider == Provider::GitLab && remote.host != "gitlab.com" {
        command.env("GITLAB_HOST", format!("https://{}", remote.host));
    }
    if let Some(stored) = crate::git_auth::token_for(&remote.host, remote.login.as_deref()) {
        match provider {
            Provider::GitHub if remote.host == "github.com" => drop(command.env("GH_TOKEN", &stored.token)),
            Provider::GitHub => drop(command.env("GH_ENTERPRISE_TOKEN", &stored.token).env("GH_HOST", &remote.host)),
            Provider::GitLab => drop(command.env("GITLAB_TOKEN", &stored.token)),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let output = command.output().with_context(|| format!("couldn't run {program} — is it installed?"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.lines().chain(stdout.lines()).find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        bail!("{program}: {message}");
    }
    Ok(stdout)
}

fn repo_arg(remote: &Remote) -> String {
    match remote.provider {
        Some(Provider::GitHub) if remote.host != "github.com" => format!("{}/{}", remote.host, remote.slug),
        _ => remote.slug.clone(),
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub number: u64,
    pub state: String,
    pub url: String,
    pub draft: bool,
    /// GitHub's GraphQL id (to mark a draft ready).
    pub node_id: String,
}

impl Request {
    pub fn is_open(&self) -> bool {
        matches!(self.state.as_str(), "open" | "opened")
    }
}

/// Whether the host would merge a request now.
#[derive(Clone, Debug, PartialEq)]
pub enum Mergeable {
    Ready,
    Draft,
    /// Required checks are running or failing.
    Checks,
    /// It needs approvals (or resolved discussions) first.
    Reviews,
    Conflicts,
    /// The target moved on and the host wants the branch up to date first.
    Behind,
    /// The host is still working it out, or said something else.
    Unknown(String),
}

impl Mergeable {
    /// In words, after "The pull request …".
    pub fn describe(&self) -> String {
        match self {
            Mergeable::Ready => "can be merged".into(),
            Mergeable::Draft => "is a draft".into(),
            Mergeable::Checks => "waits for its required checks".into(),
            Mergeable::Reviews => "needs approval (or resolved discussions) first".into(),
            Mergeable::Conflicts => "has conflicts with its target".into(),
            Mergeable::Behind => "needs the latest of its target first".into(),
            Mergeable::Unknown(why) => why.clone(),
        }
    }
}

/// GitHub's `mergeable_state` (REST) or `mergeStateStatus` (gh), any case.
pub fn github_mergeable(state: &str) -> Mergeable {
    match state.to_lowercase().as_str() {
        "clean" | "has_hooks" | "unstable" => Mergeable::Ready,
        "draft" => Mergeable::Draft,
        "blocked" => Mergeable::Reviews,
        "dirty" => Mergeable::Conflicts,
        "behind" => Mergeable::Behind,
        "unknown" | "" => Mergeable::Unknown("is still being checked by GitHub".into()),
        other => Mergeable::Unknown(format!("is {other} on GitHub")),
    }
}

/// GitLab's `detailed_merge_status` (or the older `merge_status`).
pub fn gitlab_mergeable(state: &str) -> Mergeable {
    match state {
        "mergeable" | "can_be_merged" => Mergeable::Ready,
        "draft_status" => Mergeable::Draft,
        "ci_must_pass" | "ci_still_running" | "external_status_checks" => Mergeable::Checks,
        "not_approved" | "discussions_not_resolved" | "requested_changes" | "jira_association_missing" => Mergeable::Reviews,
        "conflict" | "broken_status" | "cannot_be_merged" => Mergeable::Conflicts,
        "need_rebase" => Mergeable::Behind,
        "checking" | "unchecked" | "preparing" | "approvals_syncing" | "" => Mergeable::Unknown("is still being checked by GitLab".into()),
        other => Mergeable::Unknown(format!("is {} on GitLab", other.replace('_', " "))),
    }
}

/// The signed-in account's token for the remote's host, if there is one.
fn api_token(remote: &Remote) -> Option<String> {
    remote.provider?;
    crate::git_auth::token_for(&remote.host, remote.login.as_deref()).map(|s| s.token)
}

fn encoded(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn create_request_api(provider: Provider, remote: &Remote, token: &str, source: &str, target: &str, title: &str, body: &str, draft: bool) -> Result<String> {
    use crate::git_auth::{api, api_error};
    match provider {
        Provider::GitHub => {
            let payload = serde_json::json!({ "title": title, "head": source, "base": target, "body": body, "draft": draft });
            let (status, reply, _) = api("POST", provider, &remote.host, token, &format!("/repos/{}/pulls", remote.slug), Some(&payload))?;
            if status == 422
                && reply.to_string().contains("already exists")
                && let Some(existing) = request_status(remote, source)?.filter(|r| r.state == "open")
            {
                return Ok(existing.url);
            }
            if status != 201 {
                bail!("GitHub: {}", api_error(status, &reply));
            }
            Ok(reply["html_url"].as_str().unwrap_or_default().to_string())
        }
        Provider::GitLab => {
            let title = if draft { format!("Draft: {title}") } else { title.to_string() };
            let payload = serde_json::json!({ "title": title, "source_branch": source, "target_branch": target, "description": body, "remove_source_branch": false });
            let (status, reply, _) = api("POST", provider, &remote.host, token, &format!("/projects/{}/merge_requests", encoded(&remote.slug)), Some(&payload))?;
            if status == 409
                && let Some(existing) = request_status(remote, source)?.filter(|r| r.state == "opened" || r.state == "open")
            {
                return Ok(existing.url);
            }
            if status != 201 {
                bail!("GitLab: {}", api_error(status, &reply));
            }
            Ok(reply["web_url"].as_str().unwrap_or_default().to_string())
        }
    }
}

fn request_status_api(provider: Provider, remote: &Remote, token: &str, source: &str) -> Result<Option<Request>> {
    use crate::git_auth::{api, api_error};
    let (status, list, _) = match provider {
        Provider::GitHub => {
            let owner = remote.slug.split('/').next().unwrap_or_default();
            api("GET", provider, &remote.host, token, &format!("/repos/{}/pulls?state=all&per_page=1&head={}", remote.slug, encoded(&format!("{owner}:{source}"))), None)?
        }
        Provider::GitLab => api(
            "GET",
            provider,
            &remote.host,
            token,
            &format!("/projects/{}/merge_requests?state=all&per_page=1&source_branch={}", encoded(&remote.slug), encoded(source)),
            None,
        )?,
    };
    if status != 200 {
        bail!("{}: {}", remote.host, api_error(status, &list));
    }
    let Some(first) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
    let state = if first["merged_at"].is_string() { "merged".to_string() } else { first["state"].as_str().unwrap_or("unknown").to_lowercase() };
    Ok(Some(Request {
        number: first["number"].as_u64().or_else(|| first["iid"].as_u64()).unwrap_or(0),
        state,
        url: first["html_url"].as_str().or_else(|| first["web_url"].as_str()).unwrap_or_default().to_string(),
        draft: first["draft"].as_bool().or_else(|| first["work_in_progress"].as_bool()).unwrap_or(false),
        node_id: first["node_id"].as_str().unwrap_or_default().to_string(),
    }))
}

fn ci_status_api(provider: Provider, remote: &Remote, token: &str, branch: &str) -> Result<Option<Ci>> {
    use crate::git_auth::{api, api_error};
    match provider {
        Provider::GitHub => {
            let (status, reply, _) = api("GET", provider, &remote.host, token, &format!("/repos/{}/actions/runs?per_page=1&branch={}", remote.slug, encoded(branch)), None)?;
            if status != 200 {
                bail!("GitHub: {}", api_error(status, &reply));
            }
            let Some(run) = reply["workflow_runs"].as_array().and_then(|a| a.first()) else { return Ok(None) };
            let state = match (run["status"].as_str().unwrap_or_default(), run["conclusion"].as_str().unwrap_or_default()) {
                ("completed", "success" | "skipped" | "neutral") => "success",
                ("completed", _) => "failure",
                _ => "running",
            };
            Ok(Some(Ci {
                state: state.into(),
                name: run["display_title"].as_str().or_else(|| run["name"].as_str()).unwrap_or("workflow").to_string(),
                url: run["html_url"].as_str().unwrap_or_default().to_string(),
            }))
        }
        Provider::GitLab => {
            let (status, reply, _) = api("GET", provider, &remote.host, token, &format!("/projects/{}/pipelines?per_page=1&ref={}", encoded(&remote.slug), encoded(branch)), None)?;
            if status != 200 {
                bail!("GitLab: {}", api_error(status, &reply));
            }
            let Some(run) = reply.as_array().and_then(|a| a.first()) else { return Ok(None) };
            Ok(Some(Ci { state: gitlab_state(run["status"].as_str().unwrap_or_default()).into(), name: format!("pipeline #{}", run["id"]), url: run["web_url"].as_str().unwrap_or_default().to_string() }))
        }
    }
}

fn gitlab_state(status: &str) -> &'static str {
    match status {
        "success" | "skipped" | "manual" => "success",
        "failed" | "canceled" => "failure",
        _ => "running",
    }
}

/// Opens a merge/pull request from `source` into `target` (as a draft when
/// asked); returns its URL. An open one for `source` is reused.
pub fn create_request(remote: &Remote, source: &str, target: &str, title: &str, body: &str, draft: bool) -> Result<String> {
    let Some(provider) = remote.provider else { bail!("{} isn't GitHub or GitLab", remote.host) };
    if let Some(token) = api_token(remote) {
        return create_request_api(provider, remote, &token, source, target, title, body, draft);
    }
    let repo = repo_arg(remote);
    let mut args: Vec<&str> = match provider {
        Provider::GitHub => vec!["pr", "create", "--repo", &repo, "--head", source, "--base", target, "--title", title, "--body", body],
        Provider::GitLab => vec![
            "mr", "create", "-R", &repo, "--source-branch", source, "--target-branch", target, "--title", title, "--description", body, "--yes",
        ],
    };
    if draft {
        args.push("--draft");
    }
    let out = cli(provider, remote, &args)?;
    Ok(out.split_whitespace().find(|w| w.starts_with("https://")).unwrap_or(&out).to_string())
}

/// The newest merge/pull request for `source`, if there is one.
pub fn request_status(remote: &Remote, source: &str) -> Result<Option<Request>> {
    let Some(provider) = remote.provider else { return Ok(None) };
    if let Some(token) = api_token(remote) {
        return request_status_api(provider, remote, &token, source);
    }
    let repo = repo_arg(remote);
    let out = match provider {
        Provider::GitHub => cli(
            provider,
            remote,
            &["pr", "list", "--repo", &repo, "--head", source, "--state", "all", "--json", "number,state,url,isDraft,id", "--limit", "1"],
        )?,
        Provider::GitLab => cli(provider, remote, &["mr", "list", "-R", &repo, "--source-branch", source, "--all", "--output", "json"])?,
    };
    let list: Value = serde_json::from_str(&out).with_context(|| format!("unexpected reply: {}", out.chars().take(120).collect::<String>()))?;
    let Some(first) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
    Ok(Some(Request {
        number: first["number"].as_u64().or_else(|| first["iid"].as_u64()).unwrap_or(0),
        state: first["state"].as_str().unwrap_or("unknown").to_lowercase(),
        url: first["url"].as_str().or_else(|| first["web_url"].as_str()).unwrap_or_default().to_string(),
        draft: first["isDraft"].as_bool().or_else(|| first["draft"].as_bool()).unwrap_or(false),
        node_id: first["id"].as_str().unwrap_or_default().to_string(),
    }))
}

/// Whether the host would merge the request now.
pub fn request_mergeable(remote: &Remote, number: u64) -> Result<Mergeable> {
    use crate::git_auth::{api, api_error};
    let Some(provider) = remote.provider else { bail!("{} isn't GitHub or GitLab", remote.host) };
    if let Some(token) = api_token(remote) {
        return match provider {
            Provider::GitHub => {
                let (status, pr, _) = api("GET", provider, &remote.host, &token, &format!("/repos/{}/pulls/{number}", remote.slug), None)?;
                if status != 200 {
                    bail!("GitHub: {}", api_error(status, &pr));
                }
                if pr["draft"].as_bool() == Some(true) {
                    return Ok(Mergeable::Draft);
                }
                Ok(github_mergeable(pr["mergeable_state"].as_str().unwrap_or_default()))
            }
            Provider::GitLab => {
                let path = format!("/projects/{}/merge_requests/{number}", encoded(&remote.slug));
                let (status, mr, _) = api("GET", provider, &remote.host, &token, &path, None)?;
                if status != 200 {
                    bail!("GitLab: {}", api_error(status, &mr));
                }
                let detailed = mr["detailed_merge_status"].as_str().or_else(|| mr["merge_status"].as_str()).unwrap_or_default();
                Ok(gitlab_mergeable(detailed))
            }
        };
    }
    let repo = repo_arg(remote);
    let n = number.to_string();
    match provider {
        Provider::GitHub => {
            let out = cli(provider, remote, &["pr", "view", &n, "--repo", &repo, "--json", "mergeStateStatus,isDraft"])?;
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            if v["isDraft"].as_bool() == Some(true) {
                return Ok(Mergeable::Draft);
            }
            Ok(github_mergeable(v["mergeStateStatus"].as_str().unwrap_or_default()))
        }
        Provider::GitLab => {
            let out = cli(provider, remote, &["mr", "view", &n, "-R", &repo, "--output", "json"])?;
            let v: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            Ok(gitlab_mergeable(v["detailed_merge_status"].as_str().or_else(|| v["merge_status"].as_str()).unwrap_or_default()))
        }
    }
}

/// Takes a request out of draft.
pub fn mark_ready(remote: &Remote, request: &Request) -> Result<()> {
    use crate::git_auth::{api, api_error, api_url, graphql_url};
    let Some(provider) = remote.provider else { bail!("{} isn't GitHub or GitLab", remote.host) };
    let n = request.number.to_string();
    if let Some(token) = api_token(remote) {
        match provider {
            Provider::GitHub => {
                // The REST API can't do this; GraphQL can.
                let query = serde_json::json!({ "query": "mutation($id: ID!) { markPullRequestReadyForReview(input: {pullRequestId: $id}) { clientMutationId } }", "variables": { "id": request.node_id } });
                let (status, reply, _) = api_url("POST", provider, &remote.host, &token, &graphql_url(&remote.host), Some(&query))?;
                if status != 200 || reply["errors"].is_array() {
                    bail!("GitHub: {}", reply["errors"][0]["message"].as_str().map(str::to_owned).unwrap_or_else(|| api_error(status, &reply)));
                }
            }
            Provider::GitLab => {
                let path = format!("/projects/{}/merge_requests/{n}", encoded(&remote.slug));
                let (_, mr, _) = api("GET", provider, &remote.host, &token, &path, None)?;
                let title = undraft_title(mr["title"].as_str().unwrap_or_default());
                let (status, reply, _) = api("PUT", provider, &remote.host, &token, &path, Some(&serde_json::json!({ "title": title })))?;
                if status != 200 {
                    bail!("GitLab: {}", api_error(status, &reply));
                }
            }
        }
        return Ok(());
    }
    let repo = repo_arg(remote);
    match provider {
        Provider::GitHub => cli(provider, remote, &["pr", "ready", &n, "--repo", &repo])?,
        Provider::GitLab => cli(provider, remote, &["mr", "update", &n, "-R", &repo, "--ready"])?,
    };
    Ok(())
}

/// "Draft: Order history" → "Order history" (GitLab marks drafts in the title).
pub fn undraft_title(title: &str) -> String {
    let mut t = title.trim();
    loop {
        let before = t;
        for prefix in ["Draft:", "[Draft]", "(Draft)", "WIP:", "[WIP]"] {
            if t.len() >= prefix.len() && t[..prefix.len()].eq_ignore_ascii_case(prefix) {
                t = t[prefix.len()..].trim_start();
            }
        }
        if t == before {
            return t.to_string();
        }
    }
}

/// Merges the request on the host (a merge commit, the branch kept).
pub fn merge_request(remote: &Remote, number: u64, message: &str) -> Result<()> {
    use crate::git_auth::{api, api_error};
    let Some(provider) = remote.provider else { bail!("{} isn't GitHub or GitLab", remote.host) };
    if let Some(token) = api_token(remote) {
        let (status, reply, _) = match provider {
            Provider::GitHub => api(
                "PUT",
                provider,
                &remote.host,
                &token,
                &format!("/repos/{}/pulls/{number}/merge", remote.slug),
                Some(&serde_json::json!({ "merge_method": "merge", "commit_title": message })),
            )?,
            Provider::GitLab => api(
                "PUT",
                provider,
                &remote.host,
                &token,
                &format!("/projects/{}/merge_requests/{number}/merge", encoded(&remote.slug)),
                Some(&serde_json::json!({ "merge_commit_message": message, "should_remove_source_branch": false })),
            )?,
        };
        if status != 200 {
            bail!("{}: {}", remote.host, api_error(status, &reply));
        }
        return Ok(());
    }
    let repo = repo_arg(remote);
    let n = number.to_string();
    match provider {
        Provider::GitHub => cli(provider, remote, &["pr", "merge", &n, "--repo", &repo, "--merge"])?,
        Provider::GitLab => cli(provider, remote, &["mr", "merge", &n, "-R", &repo, "--yes"])?,
    };
    Ok(())
}

/// The latest CI run on a branch.
#[derive(Clone, Debug, PartialEq)]
pub struct Ci {
    /// "success", "failure", "running" or "none".
    pub state: String,
    pub name: String,
    pub url: String,
}

/// The newest CI run (GitHub Actions or a GitLab pipeline) for `branch`.
pub fn ci_status(remote: &Remote, branch: &str) -> Result<Option<Ci>> {
    let Some(provider) = remote.provider else { return Ok(None) };
    if let Some(token) = api_token(remote) {
        return ci_status_api(provider, remote, &token, branch);
    }
    let repo = repo_arg(remote);
    match provider {
        Provider::GitHub => {
            let out = cli(provider, remote, &["run", "list", "--repo", &repo, "--branch", branch, "--limit", "1", "--json", "status,conclusion,displayTitle,url"])?;
            let list: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let Some(run) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
            let state = match (run["status"].as_str().unwrap_or_default(), run["conclusion"].as_str().unwrap_or_default()) {
                ("completed", "success" | "skipped" | "neutral") => "success",
                ("completed", _) => "failure",
                _ => "running",
            };
            Ok(Some(Ci {
                state: state.into(),
                name: run["displayTitle"].as_str().unwrap_or("workflow").to_string(),
                url: run["url"].as_str().unwrap_or_default().to_string(),
            }))
        }
        Provider::GitLab => {
            let path = format!("projects/{}/pipelines?ref={}&per_page=1", remote.slug.replace('/', "%2F"), branch.replace('/', "%2F"));
            let out = cli(provider, remote, &["api", &path])?;
            let list: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
            let Some(run) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
            let state = gitlab_state(run["status"].as_str().unwrap_or_default());
            Ok(Some(Ci { state: state.into(), name: format!("pipeline #{}", run["id"]), url: run["web_url"].as_str().unwrap_or_default().to_string() }))
        }
    }
}
