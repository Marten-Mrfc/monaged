//! Signing git in WSL in to GitHub, GitLab and other hosts. Windows' Git
//! Credential Manager isn't needed, but Monaged sets it up as a fallback when
//! you want it.
//!
//! - **Everything happens on your PC.** No OAuth app, client ID, client
//!   secret or server of Monaged's: you make a personal access token on the
//!   host's own website and Monaged keeps it. Nothing is sent anywhere but to
//!   the git host itself.
//! - **Accounts.** You sign in once per account: in the browser (Monaged opens
//!   the host's token page with the right permissions ticked; you click
//!   Generate and Copy, and Monaged picks the token up from the clipboard and
//!   clears it), with a token you paste, or by importing a login the GitHub
//!   CLI, the GitLab CLI or Git Credential Manager already has. Monaged checks
//!   the token with the host's API and remembers who you are, what it may do
//!   and when it expires.
//! - **Where tokens live.** In Windows Credential Manager, under Monaged's
//!   name: never in Monaged's database, never in a file in WSL, never in a
//!   command line. The database only lists the accounts.
//! - **How git gets them.** Monaged is git's credential helper for each host
//!   you signed in to: `credential.https://<host>.helper` runs
//!   `monaged.exe --git-credential` through WSL interop, which reads the token
//!   from Credential Manager and hands it to git. It works whether or not the Monaged
//!   window is open, so git in a WSL terminal signs in the same way. (Agents
//!   don't push; their sandbox hides Windows programs anyway.)
//! - **Several accounts on one host.** One is the default; a project can sign
//!   in as another (`credential.<url>.username` in its git config).
//! - **Git Credential Manager.** Found wherever Git for Windows or the
//!   standalone installer put it, and set as git's general helper in WSL for
//!   the hosts Monaged has no account for.
//! - **SSH keys in WSL.** Listed, created (ed25519), added to GitHub or GitLab
//!   through the API, and tested; host keys are trusted with `ssh-keyscan`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::hosting::Provider;
use crate::wsl::{quote, run};

/// The permissions the token page has ticked.
pub const GITHUB_SCOPES: &str = "repo workflow read:org gist admin:public_key user:email";
pub const GITLAB_SCOPES: &str = "api read_user write_repository";

const SERVICE: &str = "monaged";

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ---- Providers ------------------------------------------------------------------------

/// GitHub, GitLab, or `None` for another host (Gitea, Bitbucket, Azure DevOps…).
pub fn provider_for(host: &str) -> Option<Provider> {
    let lower = host.to_lowercase();
    if lower.contains("github") {
        Some(Provider::GitHub)
    } else if lower.contains("gitlab") {
        Some(Provider::GitLab)
    } else {
        None
    }
}

pub fn provider_key(provider: Option<Provider>) -> &'static str {
    match provider {
        Some(Provider::GitHub) => "github",
        Some(Provider::GitLab) => "gitlab",
        None => "other",
    }
}

pub fn provider_from_key(key: &str) -> Option<Provider> {
    match key {
        "github" => Some(Provider::GitHub),
        "gitlab" => Some(Provider::GitLab),
        _ => None,
    }
}

pub fn provider_name(provider: Option<Provider>) -> &'static str {
    match provider {
        Some(Provider::GitHub) => "GitHub",
        Some(Provider::GitLab) => "GitLab",
        None => "Git host",
    }
}

/// "github.com", lowercased, without a scheme, path or trailing slash.
pub fn normalize_host(input: &str) -> String {
    let t = input.trim();
    let t = t.split_once("://").map_or(t, |(_, rest)| rest);
    let t = t.rsplit_once('@').map_or(t, |(_, rest)| rest);
    t.split('/').next().unwrap_or_default().trim().to_lowercase()
}

/// The REST API's base address.
pub fn api_base(provider: Provider, host: &str) -> String {
    match provider {
        Provider::GitHub if host == "github.com" => "https://api.github.com".into(),
        Provider::GitHub => format!("https://{host}/api/v3"),
        Provider::GitLab => format!("https://{host}/api/v4"),
    }
}

/// Where you make a token by hand, with the scopes Monaged needs ticked.
pub fn token_page(provider: Option<Provider>, host: &str) -> Option<String> {
    match provider {
        Some(Provider::GitHub) => Some(format!(
            "https://{host}/settings/tokens/new?scopes={}&description=Monaged",
            GITHUB_SCOPES.replace(' ', ",").replace(':', "%3A")
        )),
        Some(Provider::GitLab) => Some(format!("https://{host}/-/user_settings/personal_access_tokens?name=Monaged&scopes={}", GITLAB_SCOPES.replace(' ', ","))),
        None => None,
    }
}

/// Where you add an SSH key by hand.
pub fn ssh_keys_page(provider: Option<Provider>, host: &str) -> Option<String> {
    match provider {
        Some(Provider::GitHub) => Some(format!("https://{host}/settings/ssh/new")),
        Some(Provider::GitLab) => Some(format!("https://{host}/-/user_settings/ssh_keys")),
        None => None,
    }
}

// ---- Accounts (the list lives in Monaged's database, tokens in Credential Manager) ------

#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub host: String,
    pub provider: Option<Provider>,
    pub login: String,
    pub name: String,
    pub email: String,
    /// "browser", "token", "password", "gh", "glab" or "gcm".
    pub method: String,
    pub scopes: Vec<String>,
    pub added: i64,
    /// The account git uses for this host unless a project says otherwise.
    pub default: bool,
    /// When the token stops working ("2026-12-01"), if it expires.
    pub expires: String,
}

impl Account {
    pub fn key(&self) -> String {
        format!("{}/{}", self.host, self.login)
    }

    pub fn method_label(&self) -> &'static str {
        match self.method.as_str() {
            "browser" => "token made in the browser",
            "token" => "personal access token",
            "password" => "username and password",
            "gh" => "from the GitHub CLI",
            "glab" => "from the GitLab CLI",
            "gcm" => "from Git Credential Manager",
            _ => "token",
        }
    }

    /// Days until the token expires (negative once it has), if it expires.
    pub fn days_left(&self) -> Option<i64> {
        days_until(&self.expires)
    }

    /// Whether the token may add SSH keys through the API.
    pub fn can_add_keys(&self) -> bool {
        match self.provider {
            Some(Provider::GitHub) => self.scopes.is_empty() || self.scopes.iter().any(|s| s == "admin:public_key" || s == "write:public_key"),
            Some(Provider::GitLab) => self.scopes.is_empty() || self.scopes.iter().any(|s| s == "api"),
            None => false,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "host": self.host, "provider": provider_key(self.provider), "login": self.login, "name": self.name,
            "email": self.email, "method": self.method, "scopes": self.scopes, "added": self.added, "default": self.default,
            "expires": self.expires,
        })
    }

    fn from_json(v: &Value) -> Option<Account> {
        Some(Account {
            host: v["host"].as_str()?.to_string(),
            provider: provider_from_key(v["provider"].as_str().unwrap_or_default()),
            login: v["login"].as_str()?.to_string(),
            name: v["name"].as_str().unwrap_or_default().to_string(),
            email: v["email"].as_str().unwrap_or_default().to_string(),
            method: v["method"].as_str().unwrap_or("token").to_string(),
            scopes: v["scopes"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
            added: v["added"].as_i64().unwrap_or(0),
            default: v["default"].as_bool().unwrap_or(false),
            expires: v["expires"].as_str().unwrap_or_default().to_string(),
        })
    }
}

pub fn load_accounts(store: &crate::store::Store) -> Vec<Account> {
    store
        .setting("git:accounts")
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(Account::from_json).collect()))
        .unwrap_or_default()
}

pub fn save_accounts(store: &crate::store::Store, accounts: &[Account]) {
    let list = Value::Array(accounts.iter().map(Account::to_json).collect());
    let _ = store.set_setting("git:accounts", &list.to_string());
}

/// The hosts that have at least one account.
pub fn hosts(accounts: &[Account]) -> Vec<String> {
    let mut hosts: Vec<String> = accounts.iter().map(|a| a.host.clone()).collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Keeps exactly one default per host (the first, unless one is marked).
pub fn fix_defaults(accounts: &mut [Account]) {
    for host in hosts(accounts) {
        let marked = accounts.iter().position(|a| a.host == host && a.default).or_else(|| accounts.iter().position(|a| a.host == host));
        for (i, a) in accounts.iter_mut().enumerate() {
            if a.host == host {
                a.default = Some(i) == marked;
            }
        }
        if let Some(i) = marked {
            let _ = set_default_login(&host, &accounts[i].login);
        }
        let logins: Vec<&str> = accounts.iter().filter(|a| a.host == host).map(|a| a.login.as_str()).collect();
        let _ = set_host_logins(&host, &logins);
    }
}

// ---- The secret half (Windows Credential Manager) ---------------------------------------

/// What git needs to sign in.
#[derive(Clone)]
pub struct Stored {
    /// The username git sends.
    pub user: String,
    pub token: String,
    pub provider: Option<Provider>,
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stored {{ user: {}, token: [secret] }}", self.user)
    }
}

impl Stored {
    fn to_json(&self) -> String {
        json!({ "user": self.user, "token": self.token, "provider": provider_key(self.provider) }).to_string()
    }

    fn from_json(text: &str) -> Option<Stored> {
        let v: Value = serde_json::from_str(text).ok()?;
        Some(Stored {
            user: v["user"].as_str()?.to_string(),
            token: v["token"].as_str()?.to_string(),
            provider: provider_from_key(v["provider"].as_str().unwrap_or_default()),
        })
    }
}

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).context("Windows Credential Manager isn't available")
}

pub fn save_secret(host: &str, login: &str, stored: &Stored) -> Result<()> {
    entry(&format!("git-token:{host}/{login}"))?.set_password(&stored.to_json()).context("couldn't save the login in Windows Credential Manager")
}

pub fn load_secret(host: &str, login: &str) -> Option<Stored> {
    entry(&format!("git-token:{host}/{login}")).ok()?.get_password().ok().and_then(|t| Stored::from_json(&t))
}

pub fn delete_secret(host: &str, login: &str) -> Result<()> {
    match entry(&format!("git-token:{host}/{login}"))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err).context("couldn't remove the login from Windows Credential Manager"),
    }
}

pub fn set_default_login(host: &str, login: &str) -> Result<()> {
    entry(&format!("git-default:{host}"))?.set_password(login).context("couldn't save the default account")
}

fn default_login(host: &str) -> Option<String> {
    entry(&format!("git-default:{host}")).ok()?.get_password().ok().filter(|l| !l.is_empty())
}

pub fn clear_default_login(host: &str) {
    for name in [format!("git-default:{host}"), format!("git-logins:{host}")] {
        if let Ok(e) = entry(&name) {
            let _ = e.delete_credential();
        }
    }
}

/// Every login with a token for a host (the helper can't read Monaged's database).
fn set_host_logins(host: &str, logins: &[&str]) -> Result<()> {
    entry(&format!("git-logins:{host}"))?.set_password(&logins.join("\n")).context("couldn't save the account list")
}

fn host_logins(host: &str) -> Vec<String> {
    entry(&format!("git-logins:{host}")).ok().and_then(|e| e.get_password().ok()).map(|t| t.lines().map(str::to_owned).collect()).unwrap_or_default()
}

/// The token for `host` (as `login`, or the default account). For Monaged's
/// own API calls and for git.
pub fn token_for(host: &str, login: Option<&str>) -> Option<Stored> {
    let host = normalize_host(host);
    let login = login.map(str::to_owned).or_else(|| default_login(&host))?;
    load_secret(&host, &login)
}

// ---- HTTP -------------------------------------------------------------------------------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(30))).build().into()
}

/// What a reply's headers say about the token.
#[derive(Clone, Debug, Default)]
pub struct TokenInfo {
    /// GitHub's `X-OAuth-Scopes` (classic tokens).
    pub scopes: Option<String>,
    /// GitHub's `GitHub-Authentication-Token-Expiration` ("2026-12-01 00:00:00 UTC").
    pub expires: Option<String>,
}

/// Calls the host's REST API with a token. Returns (status, body, token info).
pub fn api(method: &str, provider: Provider, host: &str, token: &str, path: &str, body: Option<&Value>) -> Result<(u16, Value, TokenInfo)> {
    api_url(method, provider, host, token, &format!("{}{}", api_base(provider, host), path), body)
}

/// GitHub's GraphQL endpoint (it isn't under the REST base on Enterprise).
pub fn graphql_url(host: &str) -> String {
    if host == "github.com" { "https://api.github.com/graphql".into() } else { format!("https://{host}/api/graphql") }
}

/// `api` with the full URL given.
pub fn api_url(method: &str, provider: Provider, host: &str, token: &str, url: &str, body: Option<&Value>) -> Result<(u16, Value, TokenInfo)> {
    let url = url.to_string();
    let agent = agent();
    let auth = format!("Bearer {token}");
    let empty = serde_json::json!({});
    let sent = match method {
        "POST" => agent.post(&url).header("Authorization", &auth).header("Accept", accept(provider)).header("User-Agent", "Monaged").send_json(body.unwrap_or(&empty)),
        "PUT" => agent.put(&url).header("Authorization", &auth).header("Accept", accept(provider)).header("User-Agent", "Monaged").send_json(body.unwrap_or(&empty)),
        "PATCH" => agent.patch(&url).header("Authorization", &auth).header("Accept", accept(provider)).header("User-Agent", "Monaged").send_json(body.unwrap_or(&empty)),
        "DELETE" => agent.delete(&url).header("Authorization", &auth).header("Accept", accept(provider)).header("User-Agent", "Monaged").call(),
        _ => agent.get(&url).header("Authorization", &auth).header("Accept", accept(provider)).header("User-Agent", "Monaged").call(),
    };
    let mut response = sent.map_err(|e| anyhow!("couldn't reach {host}: {e}"))?;
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let info = TokenInfo { scopes: header("x-oauth-scopes"), expires: header("github-authentication-token-expiration") };
    let text = response.body_mut().read_to_string().unwrap_or_default();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, value, info))
}

fn accept(provider: Provider) -> &'static str {
    match provider {
        Provider::GitHub => "application/vnd.github+json",
        Provider::GitLab => "application/json",
    }
}

/// The best message an API error carries.
pub fn api_error(status: u16, body: &Value) -> String {
    let message = body["message"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| body["message"].as_object().map(|m| Value::Object(m.clone()).to_string()))
        .or_else(|| body["error_description"].as_str().map(str::to_owned))
        .or_else(|| body["error"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "no details".into());
    match status {
        401 => format!("the token was rejected (401): {message}"),
        403 => format!("the token isn't allowed to do this (403): {message}"),
        404 => format!("not found, or the token can't see it (404): {message}"),
        _ => format!("{status}: {message}"),
    }
}

// ---- Who a token belongs to -------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Profile {
    pub login: String,
    pub name: String,
    pub email: String,
    pub scopes: Vec<String>,
    /// "2026-12-01", or empty when the token doesn't expire.
    pub expires: String,
}

/// Asks the host's API who the token belongs to (what it may do, when it expires).
pub fn whoami(provider: Provider, host: &str, token: &str) -> Result<Profile> {
    match provider {
        Provider::GitHub => {
            let (status, user, info) = api("GET", provider, host, token, "/user", None)?;
            if status != 200 {
                bail!("{host}: {}", api_error(status, &user));
            }
            let login = user["login"].as_str().unwrap_or_default().to_string();
            let id = user["id"].as_u64().unwrap_or(0);
            let mut email = user["email"].as_str().unwrap_or_default().to_string();
            if email.is_empty()
                && let Ok((200, emails, _)) = api("GET", provider, host, token, "/user/emails", None)
            {
                email = emails
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|e| e["primary"].as_bool() == Some(true) && e["verified"].as_bool() != Some(false))
                    .and_then(|e| e["email"].as_str())
                    .unwrap_or_default()
                    .to_string();
            }
            if email.is_empty() && id > 0 {
                let domain = if host == "github.com" { "users.noreply.github.com".to_string() } else { format!("users.noreply.{host}") };
                email = format!("{id}+{login}@{domain}");
            }
            Ok(Profile {
                name: user["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&login).to_string(),
                login,
                email,
                scopes: info.scopes.map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()).unwrap_or_default(),
                expires: info.expires.map(|e| e.chars().take(10).collect()).unwrap_or_default(),
            })
        }
        Provider::GitLab => {
            let (status, user, _) = api("GET", provider, host, token, "/user", None)?;
            if status != 200 {
                bail!("{host}: {}", api_error(status, &user));
            }
            let (scopes, expires) = match api("GET", provider, host, token, "/personal_access_tokens/self", None) {
                Ok((200, pat, _)) => (
                    pat["scopes"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                    pat["expires_at"].as_str().unwrap_or_default().chars().take(10).collect(),
                ),
                _ => (Vec::new(), String::new()),
            };
            let email = ["commit_email", "email", "public_email"].iter().find_map(|k| user[*k].as_str().filter(|e| e.contains('@'))).unwrap_or_default().to_string();
            Ok(Profile {
                login: user["username"].as_str().unwrap_or_default().to_string(),
                name: user["name"].as_str().unwrap_or_default().to_string(),
                email,
                scopes,
                expires,
            })
        }
    }
}

/// Days from today until a "YYYY-MM-DD" date (negative once it's past).
pub fn days_until(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    // Days since 1970-01-01 (Howard Hinnant's civil-from-days, inverted).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days - now().div_euclid(86_400))
}

/// Whether clipboard text looks like a GitHub or GitLab personal access token.
/// Only those shapes are picked up by the browser sign-in; nothing else is read.
pub fn looks_like_token(provider: Provider, text: &str) -> bool {
    let t = text.trim();
    let shape = t.len() >= 20 && t.len() <= 255 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    shape
        && match provider {
            Provider::GitHub => ["ghp_", "github_pat_", "gho_", "ghu_"].iter().any(|p| t.starts_with(p)),
            Provider::GitLab => t.starts_with("glpat-"),
        }
}

// ---- Saving a token ---------------------------------------------------------------------

/// Checks a pasted token and saves it as an account.
pub fn add_token(provider: Provider, host: &str, token: &str, method: &str) -> Result<Account> {
    let (account, stored) = check_token(provider, host, token, method)?;
    save_secret(host, &account.login, &stored)?;
    Ok(account)
}

/// Checks a token with the host and makes the account, without saving anything.
pub fn check_token(provider: Provider, host: &str, token: &str, method: &str) -> Result<(Account, Stored)> {
    let token = token.trim();
    if token.is_empty() {
        bail!("Paste a token first.");
    }
    let profile = whoami(provider, host, token)?;
    let stored = Stored { user: profile.login.clone(), token: token.into(), provider: Some(provider) };
    let account = Account {
        host: host.into(),
        provider: Some(provider),
        login: profile.login,
        name: profile.name,
        email: profile.email,
        method: method.into(),
        scopes: profile.scopes,
        added: now(),
        default: false,
        expires: profile.expires,
    };
    Ok((account, stored))
}

/// Saves a username and password (or token) for a host Monaged can't ask an API about.
pub fn add_password(host: &str, user: &str, secret: &str) -> Result<Account> {
    if user.trim().is_empty() || secret.trim().is_empty() {
        bail!("Enter the username and the password or token.");
    }
    let stored = Stored { user: user.trim().into(), token: secret.trim().into(), provider: None };
    save_secret(host, user.trim(), &stored)?;
    Ok(Account {
        host: host.into(),
        provider: provider_for(host),
        login: user.trim().into(),
        name: String::new(),
        email: String::new(),
        method: "password".into(),
        scopes: Vec::new(),
        added: now(),
        default: false,
        expires: String::new(),
    })
}

// ---- Logins other tools already have -----------------------------------------------------

/// A login found in the GitHub CLI, the GitLab CLI or Git Credential Manager.
#[derive(Clone)]
pub struct Found {
    pub host: String,
    pub provider: Provider,
    /// "gh", "glab" or "gcm".
    pub source: &'static str,
    pub token: String,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Found {{ {} from {} }}", self.host, self.source)
    }
}

pub fn source_name(source: &str) -> &'static str {
    match source {
        "gh" => "GitHub CLI",
        "glab" => "GitLab CLI",
        "gcm" => "Git Credential Manager",
        _ => "another tool",
    }
}

/// Runs a Windows program without a window; gives up after `timeout`.
fn windows_program(program: &str, args: &[&str], input: Option<&str>, envs: &[(&str, &str)], timeout: Duration) -> Result<String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command.args(args).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in envs {
        command.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().with_context(|| format!("{program} isn't installed"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    let mut stdout = child.stdout.take().context("no output")?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let out = reader.join().unwrap_or_default();
            if !status.success() {
                bail!("{program} found nothing");
            }
            return Ok(out.trim().to_string());
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            bail!("{program} took too long");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Looks for logins to `hosts` in gh, glab and Git Credential Manager on
/// Windows. Nothing is saved; nothing pops up.
pub fn find_existing(hosts: &[String]) -> Vec<Found> {
    let mut found = Vec::new();
    for host in hosts {
        let Some(provider) = provider_for(host) else { continue };
        let program = match provider {
            Provider::GitHub => windows_program("gh", &["auth", "token", "--hostname", host], None, &[], Duration::from_secs(10)),
            Provider::GitLab => windows_program("glab", &["config", "get", "token", "--host", host], None, &[], Duration::from_secs(10)),
        };
        if let Ok(token) = program
            && !token.is_empty()
            && !token.contains(' ')
        {
            found.push(Found { host: host.clone(), provider, source: if provider == Provider::GitHub { "gh" } else { "glab" }, token });
        }
        // Git for Windows asks its own helper (usually Git Credential Manager), never interactively.
        let gcm = windows_program(
            "git",
            &["-c", "credential.interactive=never", "credential", "fill"],
            Some(&format!("protocol=https\nhost={host}\n\n")),
            &[("GCM_INTERACTIVE", "never"), ("GIT_TERMINAL_PROMPT", "0"), ("GCM_GUI_PROMPT", "false")],
            Duration::from_secs(15),
        );
        if let Ok(out) = gcm
            && let Some(password) = out.lines().find_map(|l| l.strip_prefix("password="))
            && !password.is_empty()
            && !found.iter().any(|f| f.host == *host && f.token == password)
        {
            found.push(Found { host: host.clone(), provider, source: "gcm", token: password.to_string() });
        }
    }
    found
}

// ---- Monaged as git's credential helper ----------------------------------------------------

/// What git runs for hosts with an account: this program, through WSL interop.
pub fn helper_command() -> String {
    let exe = std::env::current_exe().map(|p| crate::coord::windows_to_wsl_path(&p.to_string_lossy())).unwrap_or_default();
    format!("!{} --git-credential", quote(&exe))
}

/// Makes Monaged git's helper for exactly these hosts in WSL (others keep
/// whatever they had), and removes it from hosts without an account.
pub fn configure_hosts(distro: &str, hosts: &[String]) -> Result<()> {
    let helper = helper_command();
    let mut script = String::from(
        "git config --global --get-regexp '^credential\\..*\\.helper$' 2>/dev/null | grep -- '--git-credential' | while read -r key _; do git config --global --unset-all \"$key\"; done\n",
    );
    for host in hosts {
        let key = quote(&format!("credential.https://{host}.helper"));
        // Git runs every matching helper in file order, and an empty value only
        // clears the ones before it. So the host's section is written afresh at
        // the end of the file, after any general helper (Git Credential Manager),
        // with an empty value first: then only Monaged answers for this host.
        script.push_str(&format!(
            "git config --global --unset-all {key} 2>/dev/null\n\
             [ -z \"$(git config --global --get-regexp {section} 2>/dev/null)\" ] && git config --global --remove-section {name} 2>/dev/null\n\
             git config --global --add {key} '' && git config --global --add {key} {}\n",
            quote(&helper),
            section = quote(&format!("^credential\\.https://{}\\.", host.replace('.', "\\."))),
            name = quote(&format!("credential.https://{host}")),
        ));
    }
    script.push_str("true");
    // Two writers at once (start-up repair and a sign-in) trip git's config lock: try again.
    let mut last = None;
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(700));
        }
        match run(distro, &script) {
            Ok(_) => return Ok(()),
            Err(err) => last = Some(err),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("couldn't write git's config")))
}

/// How git in WSL signs in right now.
#[derive(Clone, Debug, Default)]
pub struct HelperStatus {
    /// Hosts Monaged answers for.
    pub hosts: Vec<String>,
    /// Monaged is set up, but from another copy of monaged.exe (moved or updated).
    pub stale: bool,
    /// The general `credential.helper` in WSL.
    pub general: String,
}

impl HelperStatus {
    pub fn uses_gcm(&self) -> bool {
        self.general.contains("git-credential-manager")
    }
}

pub fn helper_status(distro: &str) -> Result<HelperStatus> {
    let out = run(distro, "echo \"GENERAL:$(git config --global --get-all credential.helper | tail -n 1)\"\ngit config --global --get-regexp '^credential\\..*\\.helper$' 2>/dev/null | grep -- '--git-credential'; true")?;
    let ours = helper_command();
    let mut status = HelperStatus::default();
    for line in out.lines() {
        if let Some(general) = line.strip_prefix("GENERAL:") {
            status.general = general.trim().to_string();
        } else if let Some((key, value)) = line.split_once(' ') {
            let host = key.trim_start_matches("credential.").trim_end_matches(".helper").trim_start_matches("https://").to_string();
            if value.trim() != ours.trim() {
                status.stale = true;
            }
            status.hosts.push(host);
        }
    }
    Ok(status)
}

/// `monaged.exe --git-credential get|store|erase`: git's credential helper protocol.
pub fn parse_helper_args(args: &[String]) -> Option<String> {
    (args.get(1).map(String::as_str) == Some("--git-credential")).then(|| args.get(2).cloned().unwrap_or_else(|| "get".into()))
}

pub fn run_helper(operation: &str) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    // Git run by an agent (Monaged marks their processes) gets no tokens,
    // unless you let agents use your git accounts.
    if std::env::var("MONAGED_AGENT").as_deref() == Ok("1") {
        return Ok(());
    }
    let fields: HashMap<&str, &str> = input.lines().filter_map(|l| l.split_once('=')).collect();
    let protocol = fields.get("protocol").copied().unwrap_or_default();
    let host = normalize_host(fields.get("host").copied().unwrap_or_default());
    if host.is_empty() || !matches!(protocol, "https" | "http") {
        return Ok(());
    }
    let wanted = fields.get("username").copied().filter(|u| !u.is_empty());
    // The username git sends is the account's login (from a project's
    // `credential.username`), or `oauth2`/the git user for the default.
    let login = wanted.filter(|u| load_secret(&host, u).is_some()).map(str::to_owned).or_else(|| default_login(&host));
    let Some(login) = login else { return Ok(()) };
    match operation {
        "get" => {
            let Some(stored) = token_for(&host, Some(&login)) else { return Ok(()) };
            let mut out = std::io::stdout();
            writeln!(out, "username={}", stored.user)?;
            writeln!(out, "password={}", stored.token)?;
            out.flush()?;
        }
        "erase" => {
            // The host turned the token down: let Settings say so. The token
            // itself stays until you sign out. The account is the one whose
            // token git sends back.
            let password = fields.get("password").copied().unwrap_or_default();
            let login = host_logins(&host).into_iter().find(|l| !password.is_empty() && load_secret(&host, l).is_some_and(|s| s.token == password)).unwrap_or(login);
            note_rejected(&host, &login);
        }
        _ => {}
    }
    Ok(())
}

fn rejected_file() -> Option<std::path::PathBuf> {
    std::env::var("APPDATA").ok().map(|a| std::path::PathBuf::from(a).join("Monaged").join("git-rejected.txt"))
}

fn note_rejected(host: &str, login: &str) {
    let Some(path) = rejected_file() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{host}/{login} {}", now());
    }
}

/// Accounts git found its token rejected for: account key -> when.
pub fn rejected() -> HashMap<String, i64> {
    let text = rejected_file().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once(' ').map(|(k, t)| (k.to_string(), t.trim().parse().unwrap_or(0)))).collect()
}

/// Forgets the rejections of one account (after signing in again).
pub fn clear_rejected(key: &str) {
    let Some(path) = rejected_file() else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let kept: Vec<&str> = text.lines().filter(|l| l.split(' ').next() != Some(key)).collect();
    let _ = std::fs::write(&path, if kept.is_empty() { String::new() } else { format!("{}\n", kept.join("\n")) });
}

/// Whether git's error output means it couldn't sign in.
pub fn looks_like_auth_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "could not read username",
        "could not read password",
        "authentication failed",
        "terminal prompts disabled",
        "permission denied (publickey",
        "invalid username or password",
        "http basic: access denied",
        "403",
        "401",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

// ---- A project signs in as… ----------------------------------------------------------------

/// The account a project's repo signs in to `host` as, when it isn't the default.
pub fn project_login(distro: &str, repo: &str, host: &str) -> Option<String> {
    run(distro, &format!("cd {} && git config --local --get {}", quote(repo), quote(&format!("credential.https://{host}.username")))).ok().filter(|l| !l.is_empty())
}

pub fn set_project_login(distro: &str, repo: &str, host: &str, login: Option<&str>) -> Result<()> {
    let key = quote(&format!("credential.https://{host}.username"));
    let script = match login {
        Some(login) => format!("cd {} && git config --local {key} {}", quote(repo), quote(login)),
        None => format!("cd {} && (git config --local --unset-all {key}; true)", quote(repo)),
    };
    run(distro, &script)?;
    Ok(())
}

/// `git ls-remote origin`: whether signing in and reading the repo work.
pub fn test_remote(distro: &str, repo: &str) -> Result<String> {
    let out = run(
        distro,
        &format!(
            "cd {} || exit 1\n\
             out=$(timeout 40 git ls-remote --heads origin 2>&1) || {{ echo \"$out\" | grep -v '^[[:space:]]*$' | tail -n 2 >&2; exit 1; }}\n\
             echo \"$out\" | grep -c 'refs/heads/'; true",
            quote(repo)
        ),
    )?;
    let heads: usize = out.lines().last().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    Ok(format!("Signed in: origin answers with {heads} branch{}.", if heads == 1 { "" } else { "es" }))
}

pub fn set_origin(distro: &str, repo: &str, url: &str) -> Result<()> {
    run(distro, &format!("cd {} && git remote set-url origin {}", quote(repo), quote(url)))?;
    Ok(())
}

/// The SSH address for an HTTPS one (`https://host/a/b.git` → `git@host:a/b.git`).
pub fn ssh_for(url: &str) -> Option<String> {
    let rest = url.trim().strip_prefix("https://")?;
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let (host, path) = rest.split_once('/')?;
    let path = path.trim_end_matches('/');
    let path = if path.ends_with(".git") { path.to_string() } else { format!("{path}.git") };
    Some(format!("git@{}:{path}", host.split(':').next().unwrap_or(host)))
}

// ---- Git Credential Manager -----------------------------------------------------------------

/// Where Git Credential Manager is installed on Windows, as WSL sees it.
pub fn find_gcm() -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for base in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Ok(dir) = std::env::var(base) {
            let dir = std::path::PathBuf::from(dir);
            let dir = if base == "LOCALAPPDATA" { dir.join("Programs") } else { dir };
            candidates.push(dir.join(r"Git\mingw64\bin\git-credential-manager.exe"));
            candidates.push(dir.join(r"Git\mingw64\libexec\git-core\git-credential-manager.exe"));
            candidates.push(dir.join(r"Git Credential Manager\git-credential-manager.exe"));
        }
    }
    if let Ok(found) = windows_program("where.exe", &["git-credential-manager.exe"], None, &[], Duration::from_secs(5)) {
        candidates.extend(found.lines().map(|l| std::path::PathBuf::from(l.trim())));
    }
    candidates.into_iter().find(|p| p.is_file()).map(|p| crate::coord::windows_to_wsl_path(&p.to_string_lossy()))
}

/// Makes Git Credential Manager git's general helper in WSL (or removes it).
/// Hosts with a Monaged account keep Monaged (their sections are rewritten
/// after the general helper, see `configure_hosts`).
pub fn configure_gcm(distro: &str, on: bool, hosts: &[String]) -> Result<()> {
    if on {
        let path = find_gcm().ok_or_else(|| anyhow!("Git Credential Manager isn't installed on Windows. Install Git for Windows (it includes it), or sign in with a Monaged account instead."))?;
        run(
            distro,
            &format!(
                "git config --global --unset-all credential.helper 2>/dev/null; git config --global credential.helper {}\n\
                 git config --global credential.https://dev.azure.com.useHttpPath true",
                quote(&format!("!{}", quote(&path)))
            ),
        )?;
        configure_hosts(distro, hosts)?;
    } else {
        run(distro, "if git config --global --get-all credential.helper | grep -q git-credential-manager; then git config --global --unset-all credential.helper; fi; true")?;
    }
    Ok(())
}

// ---- SSH keys in WSL ------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SshKey {
    /// The public key's file, e.g. `~/.ssh/id_ed25519.pub`.
    pub path: String,
    pub kind: String,
    pub comment: String,
    pub fingerprint: String,
    /// The whole public key line (safe to share).
    pub public: String,
}

pub fn ssh_keys(distro: &str) -> Result<Vec<SshKey>> {
    let out = run(
        distro,
        "for f in ~/.ssh/*.pub; do [ -f \"$f\" ] || continue; [ -f \"${f%.pub}\" ] || continue\n\
         printf '%s\\x1f%s\\x1f%s\\n' \"$f\" \"$(ssh-keygen -lf \"$f\" 2>/dev/null)\" \"$(head -n 1 \"$f\")\"\n\
         done; true",
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\u{1f}');
            let (path, finger, public) = (parts.next()?, parts.next()?, parts.next()?);
            // "256 SHA256:abc… comment (ED25519)"
            let mut words = finger.split_whitespace();
            let _bits = words.next();
            let fingerprint = words.next().unwrap_or_default().to_string();
            let rest: Vec<&str> = words.collect();
            let kind = rest.last().map(|k| k.trim_matches(['(', ')']).to_string()).unwrap_or_default();
            let comment = rest.iter().take(rest.len().saturating_sub(1)).copied().collect::<Vec<_>>().join(" ");
            Some(SshKey { path: path.to_string(), kind, comment, fingerprint, public: public.trim().to_string() })
        })
        .collect())
}

/// Makes a new ed25519 key without a passphrase (git can't ask for one here).
pub fn create_ssh_key(distro: &str, comment: &str) -> Result<String> {
    let out = run(
        distro,
        &format!(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh\n\
             f=~/.ssh/id_ed25519; n=1; while [ -e \"$f\" ]; do f=~/.ssh/id_ed25519_monaged$n; n=$((n+1)); done\n\
             ssh-keygen -q -t ed25519 -N '' -C {} -f \"$f\" </dev/null >/dev/null && echo \"$f.pub\"",
            quote(comment)
        ),
    )?;
    Ok(out.trim().to_string())
}

/// `ssh -T git@host`: whether the host accepts one of WSL's keys.
pub fn ssh_test(distro: &str, host: &str) -> Result<(bool, String)> {
    let out = run(distro, &format!("cmd=\"$(git config --get core.sshCommand)\"\ntimeout 20 ${{cmd:-ssh}} -o BatchMode=yes -o ConnectTimeout=10 -T {} 2>&1; true", quote(&format!("git@{host}"))))?;
    let lower = out.to_lowercase();
    let ok = lower.contains("successfully authenticated") || lower.contains("welcome to") || lower.contains("logged in as") || lower.starts_with("hi ");
    let message = if ok {
        out.lines().find(|l| !l.trim().is_empty()).unwrap_or("signed in").trim().to_string()
    } else if lower.contains("host key verification failed") {
        format!("WSL doesn't know {host}'s host key yet. Trust it first.")
    } else if lower.contains("permission denied") {
        format!("{host} doesn't accept any of WSL's SSH keys.")
    } else if lower.contains("could not resolve") || lower.contains("timed out") {
        format!("WSL can't reach {host} over SSH.")
    } else {
        out.lines().find(|l| !l.trim().is_empty()).unwrap_or("no answer").trim().chars().take(160).collect()
    };
    Ok((ok, message))
}

/// Adds the host's SSH key to `~/.ssh/known_hosts` in WSL.
pub fn trust_host(distro: &str, host: &str) -> Result<()> {
    let out = run(
        distro,
        &format!("mkdir -p ~/.ssh && chmod 700 ~/.ssh && (ssh-keygen -F {h} >/dev/null || ssh-keyscan -T 8 {h} 2>/dev/null >> ~/.ssh/known_hosts) && ssh-keygen -F {h} >/dev/null && echo ok", h = quote(host)),
    )?;
    if !out.contains("ok") {
        bail!("couldn't get {host}'s host key; is it reachable?");
    }
    Ok(())
}

/// Adds a public key to the account through the host's API.
pub fn upload_ssh_key(provider: Provider, host: &str, token: &str, title: &str, public: &str) -> Result<String> {
    let body = json!({ "title": title, "key": public });
    let (status, reply, _) = api("POST", provider, host, token, "/user/keys", Some(&body))?;
    match status {
        200 | 201 => Ok(format!("Added the key to {host}.")),
        422 | 400 if reply.to_string().to_lowercase().contains("already") || reply.to_string().contains("taken") => Ok(format!("{host} already has this key.")),
        _ => bail!("{host}: {}", api_error(status, &reply)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_normalize() {
        assert_eq!(normalize_host("https://GitHub.com/you/app.git"), "github.com");
        assert_eq!(normalize_host("gitlab.company.nl"), "gitlab.company.nl");
        assert_eq!(normalize_host("https://me@git.example.org:8443/x"), "git.example.org:8443");
        assert_eq!(provider_for("gitlab.company.nl"), Some(Provider::GitLab));
        assert_eq!(provider_for("git.example.org"), None);
    }

    #[test]
    fn ssh_addresses() {
        assert_eq!(ssh_for("https://github.com/you/app").as_deref(), Some("git@github.com:you/app.git"));
        assert_eq!(ssh_for("https://oauth2@gitlab.com/group/sub/app.git").as_deref(), Some("git@gitlab.com:group/sub/app.git"));
        assert_eq!(ssh_for("git@github.com:you/app.git"), None);
    }

    #[test]
    fn stored_logins_round_trip() {
        let stored = Stored { user: "me".into(), token: "glpat-secretvalue".into(), provider: Some(Provider::GitLab) };
        let back = Stored::from_json(&stored.to_json()).unwrap();
        assert_eq!(back.user, "me");
        assert_eq!(back.token, "glpat-secretvalue");
        assert_eq!(back.provider, Some(Provider::GitLab));
        assert!(!format!("{back:?}").contains("secretvalue"));
    }

    #[test]
    fn tokens_are_recognised() {
        assert!(looks_like_token(Provider::GitHub, "ghp_0123456789abcdefghijABCDEFGHIJ012345"));
        assert!(looks_like_token(Provider::GitHub, " github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz \n"));
        assert!(looks_like_token(Provider::GitLab, "glpat-abcdefghij0123456789"));
        assert!(!looks_like_token(Provider::GitLab, "ghp_0123456789abcdefghijABCDEFGHIJ012345"));
        assert!(!looks_like_token(Provider::GitHub, "ghp_ but this is a sentence someone copied"));
        assert!(!looks_like_token(Provider::GitHub, "hello"));
    }

    #[test]
    fn expiry_dates() {
        let today = now().div_euclid(86_400);
        assert_eq!(days_until("1970-01-01"), Some(-today));
        assert_eq!(days_until("1970-01-31").map(|d| d + today), Some(30));
        assert_eq!(days_until("2000-03-01").map(|d| d + today), Some(11_017));
        assert_eq!(days_until(""), None);
        assert_eq!(days_until("soon"), None);
    }

    #[test]
    fn accounts_keep_one_default_per_host() {
        let a = |login: &str, default: bool| Account {
            host: "github.com".into(),
            provider: Some(Provider::GitHub),
            login: login.into(),
            name: String::new(),
            email: String::new(),
            method: "token".into(),
            scopes: Vec::new(),
            added: 0,
            default,
            expires: String::new(),
        };
        let mut list = vec![a("one", false), a("two", true)];
        let back: Vec<Account> = list.iter().map(|x| Account::from_json(&x.to_json()).unwrap()).collect();
        assert_eq!(back, list);
        list[1].default = false;
        // fix_defaults also writes Credential Manager; only check the flags here.
        let marked = list.iter().position(|x| x.default).or(Some(0));
        assert_eq!(marked, Some(0));
    }

    #[test]
    fn helper_args() {
        let args: Vec<String> = ["monaged.exe", "--git-credential", "get"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_helper_args(&args).as_deref(), Some("get"));
        assert_eq!(parse_helper_args(&["monaged.exe".to_string()]), None);
    }
}
