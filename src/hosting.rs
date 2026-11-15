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

fn create_request_api(provider: Provider, remote: &Remote, token: &str, source: &str, target: &str, title: &str, body: &str) -> Result<String> {
    use crate::git_auth::{api, api_error};
    match provider {
        Provider::GitHub => {
            let payload = serde_json::json!({ "title": title, "head": source, "base": target, "body": body });
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

/// Opens a merge/pull request from `source` into `target`; returns its URL.
pub fn create_request(remote: &Remote, source: &str, target: &str, title: &str, body: &str) -> Result<String> {
    let Some(provider) = remote.provider else { bail!("{} isn't GitHub or GitLab", remote.host) };
    if let Some(token) = api_token(remote) {
        return create_request_api(provider, remote, &token, source, target, title, body);
    }
    let repo = repo_arg(remote);
    let out = match provider {
        Provider::GitHub => cli(
            provider,
            remote,
            &["pr", "create", "--repo", &repo, "--head", source, "--base", target, "--title", title, "--body", body],
        )?,
        Provider::GitLab => cli(
            provider,
            remote,
            &[
                "mr", "create", "-R", &repo, "--source-branch", source, "--target-branch", target, "--title", title,
                "--description", body, "--yes",
            ],
        )?,
    };
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
            &["pr", "list", "--repo", &repo, "--head", source, "--state", "all", "--json", "number,state,url", "--limit", "1"],
        )?,
        Provider::GitLab => cli(provider, remote, &["mr", "list", "-R", &repo, "--source-branch", source, "--all", "--output", "json"])?,
    };
    let list: Value = serde_json::from_str(&out).with_context(|| format!("unexpected reply: {}", out.chars().take(120).collect::<String>()))?;
    let Some(first) = list.as_array().and_then(|a| a.first()) else { return Ok(None) };
    Ok(Some(Request {
        number: first["number"].as_u64().or_else(|| first["iid"].as_u64()).unwrap_or(0),
        state: first["state"].as_str().unwrap_or("unknown").to_lowercase(),
        url: first["url"].as_str().or_else(|| first["web_url"].as_str()).unwrap_or_default().to_string(),
    }))
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
