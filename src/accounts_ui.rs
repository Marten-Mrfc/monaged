//! Settings → Git accounts: signing git in WSL in to GitHub, GitLab and other
//! hosts (see `git_auth` for how it works underneath):
//! - your accounts, with who they are, how they signed in and what they may do;
//! - signing in: in the browser, with a token, with a username and password
//!   for other hosts, or by importing a login gh, glab or Git Credential
//!   Manager already has;
//! - Git Credential Manager as the fallback for other hosts (found and set up
//!   for you, or turned off);
//! - SSH keys in WSL: create, copy, add to an account, test, trust a host.
//!
//! And in the Git tab: which account a project signs in as, a connection
//! test, and switching origin between HTTPS and SSH.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{AnyElement, ClipboardItem, Context, Entity, FontWeight, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::git_auth::{self, Account, Found, HelperStatus, SshKey};
use crate::hosting::Provider;
use crate::icons::{Icon, icon_sized};
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

#[derive(Default)]
pub(crate) struct GitAuthUi {
    pub accounts: Vec<Account>,
    pub helper: Option<Result<HelperStatus, String>>,
    /// `Some(None)`: checked, not installed.
    pub gcm_path: Option<Option<String>>,
    pub keys: Option<Result<Vec<SshKey>, String>>,
    /// Accounts git found rejected: key -> when.
    pub rejected: HashMap<String, i64>,
    pub busy: Option<String>,
    pub sign_in: Option<SignIn>,
    /// Logins found in other tools, not saved yet.
    pub found: Option<Vec<Found>>,
    /// SSH test results per host.
    pub ssh_results: HashMap<String, (bool, String)>,
    /// Bumped to stop a browser sign-in that's waiting for the clipboard.
    pub watch_gen: u64,
    /// Projects that sign in as an account other than the host's default.
    pub project_logins: HashMap<i64, String>,
}

/// The open sign-in form.
pub(crate) struct SignIn {
    pub provider: Option<Provider>,
    pub host: Entity<TextInput>,
    pub user: Entity<TextInput>,
    pub token: Entity<TextInput>,
    /// The browser sign-in is watching the clipboard for the new token.
    pub watching: bool,
    pub working: Option<String>,
    pub error: Option<String>,
}

impl Monaged {
    // ---- Loading -----------------------------------------------------------------------------

    /// Reads the accounts and how git in WSL signs in. At start it also
    /// repairs the helper setup when monaged.exe moved or accounts changed.
    pub(crate) fn refresh_git_auth(&mut self, repair: bool, cx: &mut Context<Self>) {
        self.git_auth.accounts = git_auth::load_accounts(&self.store);
        let hosts = git_auth::hosts(&self.git_auth.accounts);
        let accounts = self.git_auth.accounts.clone();
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let mut helper = git_auth::helper_status(&distro);
                    if repair
                        && let Ok(status) = &helper
                        && (status.stale || {
                            let mut configured = status.hosts.clone();
                            configured.sort();
                            configured.dedup();
                            configured != hosts
                        })
                        && git_auth::configure_hosts(&distro, &hosts).is_ok()
                    {
                        helper = git_auth::helper_status(&distro);
                    }
                    let gcm = git_auth::find_gcm();
                    let keys = git_auth::ssh_keys(&distro).map_err(|e| format!("{e:#}"));
                    // Tokens are masked wherever Monaged shows text, and block pushes.
                    let masks = accounts
                        .iter()
                        .filter_map(|a| git_auth::load_secret(&a.host, &a.login).map(|s| (crate::env::Secret::new(s.token.clone()), format!("git token {}", a.key()))))
                        .collect();
                    crate::env::set_git_token_masks(masks);
                    (helper.map_err(|e| format!("{e:#}")), gcm, keys, git_auth::rejected())
                })
                .await;
            view.update(cx, |this, cx| {
                let (helper, gcm, keys, rejected) = result;
                this.creds_ok = Some(helper.as_ref().is_ok_and(|h| !h.hosts.is_empty() || h.uses_gcm()));
                this.git_auth.helper = Some(helper);
                this.git_auth.gcm_path = Some(gcm);
                this.git_auth.keys = Some(keys);
                this.git_auth.rejected = rejected;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Writes git's helper setup for the current accounts.
    fn sync_git_helper(&mut self, cx: &mut Context<Self>) {
        let hosts = git_auth::hosts(&self.git_auth.accounts);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { git_auth::configure_hosts(&distro, &hosts) }).await;
            view.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.toast_error(format!("Couldn't set up git in WSL to use your accounts: {err:#}"), cx);
                }
                this.refresh_git_auth(false, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Runs a job in the background with a busy label, then reloads.
    fn auth_job(&mut self, label: &str, job: impl FnOnce(String) -> anyhow::Result<String> + Send + 'static, cx: &mut Context<Self>) {
        if self.git_auth.busy.is_some() {
            return;
        }
        self.git_auth.busy = Some(label.to_string());
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { job(distro) }).await;
            view.update(cx, |this, cx| {
                this.git_auth.busy = None;
                match result {
                    Ok(done) if !done.is_empty() => this.toast(Tone::Info, done, cx),
                    Ok(_) => {}
                    Err(err) => this.toast_error(format!("{err:#}"), cx),
                }
                this.refresh_git_auth(false, cx);
            })
            .ok();
        })
        .detach();
    }

    // ---- Accounts ----------------------------------------------------------------------------

    fn add_git_account(&mut self, account: Account, cx: &mut Context<Self>) {
        let key = account.key();
        let mut accounts = git_auth::load_accounts(&self.store);
        let first_for_host = !accounts.iter().any(|a| a.host == account.host && a.key() != key);
        let was_default = accounts.iter().any(|a| a.key() == key && a.default);
        accounts.retain(|a| a.key() != key);
        let mut account = account;
        account.default = first_for_host || was_default;
        if account.default {
            for a in accounts.iter_mut().filter(|a| a.host == account.host) {
                a.default = false;
            }
        }
        let text = format!("Signed in to {} as {}. Git in WSL uses this account from now on.", account.host, account.login);
        accounts.push(account);
        git_auth::fix_defaults(&mut accounts);
        git_auth::save_accounts(&self.store, &accounts);
        git_auth::clear_rejected(&key);
        self.git_auth.accounts = accounts;
        self.git_auth.sign_in = None;
        self.toast(Tone::Info, text, cx);
        self.sync_git_helper(cx);
        cx.notify();
    }

    pub(crate) fn sign_out_git(&mut self, key: String, cx: &mut Context<Self>) {
        let mut accounts = git_auth::load_accounts(&self.store);
        let Some(account) = accounts.iter().find(|a| a.key() == key).cloned() else { return };
        if let Err(err) = git_auth::delete_secret(&account.host, &account.login) {
            self.toast_error(format!("{err:#}"), cx);
            return;
        }
        accounts.retain(|a| a.key() != key);
        if !accounts.iter().any(|a| a.host == account.host) {
            git_auth::clear_default_login(&account.host);
        }
        git_auth::fix_defaults(&mut accounts);
        git_auth::save_accounts(&self.store, &accounts);
        git_auth::clear_rejected(&key);
        self.git_auth.accounts = accounts;
        self.toast(Tone::Info, format!("Signed out of {} ({}). The token is deleted from Windows Credential Manager; revoke it on the website too if it might have leaked.", account.host, account.login), cx);
        self.sync_git_helper(cx);
    }

    fn make_default_account(&mut self, key: &str, cx: &mut Context<Self>) {
        let mut accounts = git_auth::load_accounts(&self.store);
        let Some(host) = accounts.iter().find(|a| a.key() == key).map(|a| a.host.clone()) else { return };
        for a in accounts.iter_mut().filter(|a| a.host == host) {
            a.default = a.key() == key;
        }
        git_auth::fix_defaults(&mut accounts);
        git_auth::save_accounts(&self.store, &accounts);
        self.git_auth.accounts = accounts;
        cx.notify();
    }

    fn use_account_identity(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(account) = self.git_auth.accounts.iter().find(|a| a.key() == key).cloned() else { return };
        if account.email.is_empty() {
            self.toast(Tone::Warn, format!("{} didn't share an email address for {}. Type it under Settings → Git.", account.host, account.login), cx);
            return;
        }
        let name = if account.name.is_empty() { account.login.clone() } else { account.name.clone() };
        let (n, e) = (name.clone(), account.email.clone());
        if let Some((name_input, email_input)) = &self.git_identity {
            name_input.update(cx, |i, cx| i.set_text(n.clone(), cx));
            email_input.update(cx, |i, cx| i.set_text(e.clone(), cx));
        }
        self.auth_job(
            "Saving your identity…",
            move |distro| crate::git_ops::set_identity(&distro, &n, &e).map(|_| format!("Git in WSL now commits as {n} <{e}>.")),
            cx,
        );
        self.identity_missing = false;
    }

    /// Asks the host who the token belongs to, and refreshes what it may do.
    fn test_account(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(account) = self.git_auth.accounts.iter().find(|a| a.key() == key).cloned() else { return };
        let Some(provider) = account.provider else {
            self.toast(Tone::Info, "Monaged can't ask this host who you are; test it with a repository instead (Git tab → Test).", cx);
            return;
        };
        self.git_auth.busy = Some(format!("Checking {}…", account.login));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let a = account.clone();
            let result = cx
                .background_spawn(async move {
                    let stored = git_auth::token_for(&a.host, Some(&a.login)).ok_or_else(|| anyhow::anyhow!("the token is missing from Windows Credential Manager; sign in again"))?;
                    git_auth::whoami(provider, &a.host, &stored.token)
                })
                .await;
            view.update(cx, |this, cx| {
                this.git_auth.busy = None;
                match result {
                    Ok(profile) => {
                        let mut accounts = git_auth::load_accounts(&this.store);
                        if let Some(a) = accounts.iter_mut().find(|a| a.key() == account.key()) {
                            if !profile.scopes.is_empty() {
                                a.scopes = profile.scopes.clone();
                            }
                            if !profile.email.is_empty() {
                                a.email = profile.email.clone();
                            }
                            if !profile.name.is_empty() {
                                a.name = profile.name.clone();
                            }
                        }
                        git_auth::save_accounts(&this.store, &accounts);
                        this.git_auth.accounts = accounts;
                        git_auth::clear_rejected(&account.key());
                        this.git_auth.rejected.remove(&account.key());
                        this.toast(Tone::Info, format!("{} accepts the login: you're {}.", account.host, profile.login), cx);
                    }
                    Err(err) => this.toast_error(format!("{err:#}. Sign in again to fix it."), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- Signing in --------------------------------------------------------------------------

    fn open_sign_in(&mut self, provider: Option<Provider>, cx: &mut Context<Self>) {
        self.git_auth.watch_gen += 1;
        let host_text = match provider {
            Some(Provider::GitHub) => "github.com",
            Some(Provider::GitLab) => "gitlab.com",
            None => "",
        };
        let host = cx.new(|cx| TextInput::new("Host, e.g. gitlab.company.nl", cx));
        host.update(cx, |i, cx| i.set_text(host_text, cx));
        let user = cx.new(|cx| TextInput::new("Username", cx));
        let token = cx.new(|cx| TextInput::new(if provider.is_some() { "Paste the token" } else { "Password or token" }, cx).masked());
        self.git_auth.sign_in = Some(SignIn { provider, host, user, token, watching: false, working: None, error: None });
        cx.notify();
    }

    fn close_sign_in(&mut self, cx: &mut Context<Self>) {
        self.git_auth.watch_gen += 1;
        self.git_auth.sign_in = None;
        cx.notify();
    }

    fn sign_in_host(&self, cx: &Context<Self>) -> Option<(Option<Provider>, String)> {
        let form = self.git_auth.sign_in.as_ref()?;
        let host = git_auth::normalize_host(form.host.read(cx).text());
        // A custom host keeps the kind you chose; github.com and friends are recognised.
        Some((form.provider.or_else(|| git_auth::provider_for(&host)), host))
    }

    fn set_sign_in_error(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(form) = self.git_auth.sign_in.as_mut() {
            form.error = error;
            form.working = None;
        }
        cx.notify();
    }

    /// Browser sign-in without any OAuth app: open the host's token page with
    /// the permissions ticked, then watch the clipboard for a token of that
    /// host's shape. When you click Copy on the page, Monaged checks the token
    /// with the host, saves it and empties the clipboard. Only text that looks
    /// like a token is ever kept; anything else on the clipboard is ignored.
    fn start_browser_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some((provider, host)) = self.sign_in_host(cx) else { return };
        let Some(provider) = provider else {
            self.set_sign_in_error(Some("Browser sign-in works with GitHub and GitLab. Use a username and token for this host.".into()), cx);
            return;
        };
        if host.is_empty() {
            self.set_sign_in_error(Some("Enter the host first.".into()), cx);
            return;
        }
        let Some(page) = git_auth::token_page(Some(provider), &host) else { return };
        self.git_auth.watch_gen += 1;
        let generation = self.git_auth.watch_gen;
        // What's on the clipboard now doesn't count: only a token copied from now on.
        let before = cx.read_from_clipboard().and_then(|item| item.text()).map(|t| t.trim().to_string());
        if let Some(form) = self.git_auth.sign_in.as_mut() {
            form.error = None;
            form.watching = true;
            form.working = Some(format!("Waiting for you to copy the new token on {host}…"));
        }
        cx.open_url(&page);
        cx.notify();
        cx.spawn(async move |view, cx| {
            let started = std::time::Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let picked = view
                    .update(cx, |this, cx| {
                        if this.git_auth.watch_gen != generation {
                            return Err(());
                        }
                        if started.elapsed() > Duration::from_secs(15 * 60) {
                            if let Some(form) = this.git_auth.sign_in.as_mut() {
                                form.watching = false;
                            }
                            this.set_sign_in_error(Some("Stopped waiting for a token after 15 minutes. Start again, or paste it below.".into()), cx);
                            return Err(());
                        }
                        let text = cx.read_from_clipboard().and_then(|item| item.text()).map(|t| t.trim().to_string());
                        Ok(text.filter(|t| Some(t) != before.as_ref() && git_auth::looks_like_token(provider, t)))
                    })
                    .unwrap_or(Err(()));
                let token = match picked {
                    Err(()) => return,
                    Ok(None) => continue,
                    Ok(Some(token)) => token,
                };
                // Got it: stop watching, empty the clipboard, check it with the host.
                view.update(cx, |this, cx| {
                    let still = cx.read_from_clipboard().and_then(|item| item.text()).is_some_and(|t| t.trim() == token);
                    if still {
                        crate::env::clear_clipboard();
                    }
                    if let Some(form) = this.git_auth.sign_in.as_mut() {
                        form.watching = false;
                        form.working = Some(format!("Got the token. Checking it with {host}…"));
                    }
                    cx.notify();
                })
                .ok();
                let h = host.clone();
                let result = cx.background_spawn(async move { git_auth::check_token(provider, &h, &token, "browser") }).await;
                view.update(cx, |this, cx| {
                    // Cancelled meanwhile: nothing is saved.
                    if this.git_auth.watch_gen != generation {
                        return;
                    }
                    match result.and_then(|(account, stored)| git_auth::save_secret(&account.host, &account.login, &stored).map(|_| account)) {
                        Ok(account) => this.add_git_account(account, cx),
                        Err(err) => this.set_sign_in_error(Some(format!("{err:#}")), cx),
                    }
                })
                .ok();
                return;
            }
        })
        .detach();
    }

    fn cancel_browser_sign_in(&mut self, cx: &mut Context<Self>) {
        self.git_auth.watch_gen += 1;
        if let Some(form) = self.git_auth.sign_in.as_mut() {
            form.watching = false;
            form.working = None;
        }
        cx.notify();
    }

    fn sign_in_with_token(&mut self, cx: &mut Context<Self>) {
        let Some((provider, host)) = self.sign_in_host(cx) else { return };
        let Some(form) = &self.git_auth.sign_in else { return };
        let token = form.token.read(cx).text().trim().to_string();
        let user = form.user.read(cx).text().trim().to_string();
        let input = form.token.clone();
        if host.is_empty() {
            self.set_sign_in_error(Some("Enter the host first.".into()), cx);
            return;
        }
        if token.is_empty() {
            self.set_sign_in_error(Some(if provider.is_some() { "Paste the token first." } else { "Enter the password or token first." }.into()), cx);
            return;
        }
        if provider.is_none() && user.is_empty() {
            self.set_sign_in_error(Some("Enter your username on this host.".into()), cx);
            return;
        }
        if let Some(form) = self.git_auth.sign_in.as_mut() {
            form.error = None;
            form.working = Some(match provider {
                Some(_) => format!("Checking the token with {host}…"),
                None => "Saving…".into(),
            });
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    match provider {
                        Some(provider) => git_auth::add_token(provider, &host, &token, "token"),
                        None => git_auth::add_password(&host, &user, &token),
                    }
                })
                .await;
            view.update(cx, |this, cx| {
                input.update(cx, |i, cx| i.clear(cx));
                match result {
                    Ok(account) => this.add_git_account(account, cx),
                    Err(err) => this.set_sign_in_error(Some(format!("{err:#}")), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_token_page(&mut self, cx: &mut Context<Self>) {
        let Some((provider, host)) = self.sign_in_host(cx) else { return };
        match git_auth::token_page(provider, &host) {
            Some(url) if !host.is_empty() => cx.open_url(&url),
            _ => self.set_sign_in_error(Some("Make a token (or app password) in your account settings on that host, then paste it here.".into()), cx),
        }
    }

    // ---- Logins other tools have ---------------------------------------------------------------

    fn find_existing_logins(&mut self, cx: &mut Context<Self>) {
        let mut hosts = vec!["github.com".to_string(), "gitlab.com".to_string()];
        for project in &self.projects {
            if let Some(url) = self.branch_info.get(&project.id).and_then(|i| i.state.remote_url.clone())
                && let Some(remote) = crate::hosting::parse_remote(&url)
            {
                hosts.push(git_auth::normalize_host(&remote.host));
            }
        }
        hosts.extend(self.git_auth.accounts.iter().map(|a| a.host.clone()));
        hosts.sort();
        hosts.dedup();
        self.git_auth.busy = Some("Looking in the GitHub CLI, the GitLab CLI and Git Credential Manager…".into());
        cx.notify();
        cx.spawn(async move |view, cx| {
            let found = cx.background_spawn(async move { git_auth::find_existing(&hosts) }).await;
            view.update(cx, |this, cx| {
                this.git_auth.busy = None;
                if found.is_empty() {
                    this.toast(Tone::Info, "No existing logins found in the GitHub CLI, the GitLab CLI or Git Credential Manager on Windows.", cx);
                }
                this.git_auth.found = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn import_found(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self.git_auth.found.as_ref().and_then(|f| f.get(index)).cloned() else { return };
        self.git_auth.busy = Some(format!("Checking the login from {}…", git_auth::source_name(found.source)));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let f = found.clone();
            let result = cx.background_spawn(async move { git_auth::add_token(f.provider, &f.host, &f.token, f.source) }).await;
            view.update(cx, |this, cx| {
                this.git_auth.busy = None;
                if let Some(list) = this.git_auth.found.as_mut() {
                    list.retain(|f| !(f.host == found.host && f.source == found.source));
                }
                match result {
                    Ok(account) => this.add_git_account(account, cx),
                    Err(err) => this.toast_error(format!("That login from {} doesn't work: {err:#}", git_auth::source_name(found.source)), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    // ---- Git Credential Manager ------------------------------------------------------------------

    pub(crate) fn set_gcm(&mut self, on: bool, cx: &mut Context<Self>) {
        let hosts = git_auth::hosts(&self.git_auth.accounts);
        self.auth_job(
            if on { "Setting up Git Credential Manager…" } else { "Turning Git Credential Manager off…" },
            move |distro| {
                git_auth::configure_gcm(&distro, on, &hosts)?;
                Ok(if on {
                    "Git in WSL uses Git Credential Manager for hosts without a Monaged account.".into()
                } else {
                    "Git Credential Manager is off in WSL; only your Monaged accounts sign in.".into()
                })
            },
            cx,
        );
    }

    // ---- SSH keys ----------------------------------------------------------------------------------

    fn create_ssh_key(&mut self, cx: &mut Context<Self>) {
        let comment = format!("monaged@{}", std::env::var("COMPUTERNAME").unwrap_or_else(|_| "wsl".into()).to_lowercase());
        self.auth_job("Creating an SSH key…", move |distro| git_auth::create_ssh_key(&distro, &comment).map(|path| format!("Created {path}. Add it to your accounts below.")), cx);
    }

    fn upload_ssh_key(&mut self, public: String, account_key: String, cx: &mut Context<Self>) {
        let Some(account) = self.git_auth.accounts.iter().find(|a| a.key() == account_key).cloned() else { return };
        let Some(provider) = account.provider else { return };
        let title = format!("Monaged on {}", std::env::var("COMPUTERNAME").unwrap_or_else(|_| "WSL".into()));
        let page = git_auth::ssh_keys_page(account.provider, &account.host);
        self.auth_job(
            &format!("Adding the key to {}…", account.host),
            move |distro| {
                let stored = git_auth::token_for(&account.host, Some(&account.login)).ok_or_else(|| anyhow::anyhow!("the token is missing; sign in again"))?;
                let done = git_auth::upload_ssh_key(provider, &account.host, &stored.token, &title, &public).map_err(|e| {
                    anyhow::anyhow!("{e:#}{}", page.as_ref().map(|p| format!(". Add it by hand at {p} (the key is on your clipboard)")).unwrap_or_default())
                })?;
                let _ = git_auth::trust_host(&distro, &account.host);
                Ok(done)
            },
            cx,
        );
    }

    fn test_ssh(&mut self, host: String, cx: &mut Context<Self>) {
        self.git_auth.ssh_results.insert(host.clone(), (false, "Testing…".into()));
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let h = host.clone();
            let result = cx.background_spawn(async move { git_auth::ssh_test(&distro, &h) }).await;
            view.update(cx, |this, cx| {
                let entry = result.unwrap_or_else(|e| (false, format!("{e:#}")));
                this.git_auth.ssh_results.insert(host, entry);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn trust_ssh_host(&mut self, host: String, cx: &mut Context<Self>) {
        let h = host.clone();
        self.auth_job(&format!("Getting {host}'s host key…"), move |distro| git_auth::trust_host(&distro, &h).map(|_| format!("WSL now knows {h}'s SSH host key.")), cx);
        self.git_auth.ssh_results.remove(&host);
    }

    // ---- Rendering: Settings → Git accounts --------------------------------------------------

    pub(crate) fn render_git_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let auth = &self.git_auth;
        let mut col = div().flex().flex_col().gap(px(14.));

        // How git signs in, in one or two lines.
        let summary: (Tone, String) = match &auth.helper {
            None => (Tone::Info, "Checking how git in WSL signs in…".into()),
            Some(Err(err)) => (Tone::Error, format!("Couldn't read git's setup in WSL: {err}")),
            Some(Ok(status)) => {
                let mut parts = Vec::new();
                if !status.hosts.is_empty() {
                    parts.push(format!("Monaged signs git in to {}", status.hosts.join(", ")));
                }
                if status.uses_gcm() {
                    parts.push(if status.hosts.is_empty() { "Git Credential Manager signs git in to every host".into() } else { "Git Credential Manager to other hosts".to_string() });
                } else if !status.general.is_empty() {
                    parts.push(format!("other hosts use {}", status.general));
                }
                if parts.is_empty() {
                    (Tone::Warn, "Git in WSL has no logins yet, so pushing to GitHub or GitLab and cloning private repos will fail. Sign in below.".into())
                } else {
                    let mut text = parts.join("; ");
                    text.push('.');
                    if status.stale {
                        text.push_str(" (Monaged moved since; it's being set up again.)");
                    }
                    (Tone::Info, text)
                }
            }
        };
        col = col.child(ui::callout(summary.0, summary.1));

        // The accounts.
        if !auth.accounts.is_empty() {
            col = col.child(div().flex().flex_col().gap(px(6.)).children(auth.accounts.iter().enumerate().map(|(i, a)| self.render_account_row(i, a, cx))));
        }

        // Signing in.
        let busy = auth.busy.clone();
        col = col.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(ui::button("signin-github", Kind::Primary, Some(Icon::User), "Sign in to GitHub").on_click(cx.listener(|this, _, _, cx| this.open_sign_in(Some(Provider::GitHub), cx))))
                .child(ui::button("signin-gitlab", Kind::Secondary, Some(Icon::User), "Sign in to GitLab").on_click(cx.listener(|this, _, _, cx| this.open_sign_in(Some(Provider::GitLab), cx))))
                .child(ui::button("signin-other", Kind::Secondary, Some(Icon::Plus), "Other host").on_click(cx.listener(|this, _, _, cx| this.open_sign_in(None, cx))))
                .child(match &busy {
                    Some(label) => ui::busy_button("signin-find", Kind::Ghost, label.clone()).into_any_element(),
                    None => ui::button("signin-find", Kind::Ghost, Some(Icon::Search), "Find my existing logins").on_click(cx.listener(|this, _, _, cx| this.find_existing_logins(cx))).into_any_element(),
                }),
        );
        if let Some(form) = &auth.sign_in {
            col = col.child(self.render_sign_in(form, cx));
        }
        if let Some(found) = auth.found.as_ref().filter(|f| !f.is_empty()) {
            col = col.child(
                ui::card()
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Logins found on this PC"))
                    .child(ui::meta("Monaged checks each with the host before it uses it, then keeps its own copy in Windows Credential Manager."))
                    .children(found.iter().enumerate().map(|(i, f)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .h(px(30.))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(13.)).child(format!("{} · from {}", f.host, git_auth::source_name(f.source))))
                            .child(ui::button(("found-add", i), Kind::Secondary, Some(Icon::Plus), "Use this login").on_click(cx.listener(move |this, _, _, cx| this.import_found(i, cx))))
                    })),
            );
        }

        // Git Credential Manager.
        let gcm_on = matches!(&auth.helper, Some(Ok(s)) if s.uses_gcm());
        let gcm_line = match &auth.gcm_path {
            None => "Looking for Git Credential Manager…".to_string(),
            Some(Some(path)) => format!("Installed: {path}"),
            Some(None) => "Not installed on Windows. You don't need it: your Monaged accounts sign git in by themselves.".to_string(),
        };
        let gcm_found = matches!(&auth.gcm_path, Some(Some(_)));
        col = col.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .pt(px(6.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Git Credential Manager (optional)"))
                .child(ui::meta(gcm_line))
                .when(gcm_found || gcm_on, |d| {
                    d.child(ui::switch("gcm-switch", "Let Windows' Git Credential Manager sign in to hosts that have no Monaged account", gcm_on).on_click(cx.listener(move |this, _, _, cx| this.set_gcm(!gcm_on, cx))))
                })
                .when(!gcm_found && !gcm_on, |d| {
                    d.child(div().flex().child(ui::button("gcm-get", Kind::Ghost, Some(Icon::External), "Get Git Credential Manager").on_click(cx.listener(|_, _, _, cx| {
                        cx.open_url("https://github.com/git-ecosystem/git-credential-manager/releases/latest")
                    }))))
                }),
        );

        // SSH keys.
        col = col.child(self.render_ssh_keys(cx));
        col.into_any_element()
    }

    fn render_account_row(&self, i: usize, a: &Account, cx: &mut Context<Self>) -> AnyElement {
        let key = a.key();
        let several = self.git_auth.accounts.iter().filter(|x| x.host == a.host).count() > 1;
        let rejected = self.git_auth.rejected.get(&key).copied();
        let mut details = vec![format!("{} · {}", git_auth::provider_name(a.provider), a.method_label())];
        if !a.email.is_empty() {
            details.push(a.email.clone());
        }
        if !a.scopes.is_empty() {
            details.push(format!("may: {}", a.scopes.join(", ")));
        }
        let days = a.days_left();
        match days {
            Some(d) if d < 0 => details.push(format!("expired {}", a.expires)),
            Some(_) => details.push(format!("expires {}", a.expires)),
            None if a.provider.is_some() => details.push("doesn't expire".into()),
            None => {}
        }
        let (k1, k2, k3, k5) = (key.clone(), key.clone(), key.clone(), key.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(if rejected.is_some() || days.is_some_and(|d| d < 14) { theme::red_line() } else { theme::line() }))
            .bg(rgb(theme::panel()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon_sized(Icon::User, 15.).text_color(rgb(theme::text_2())))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).child(if a.name.is_empty() || a.name == a.login { a.login.clone() } else { format!("{} ({})", a.login, a.name) }))
                    .child(ui::mono(a.host.clone()))
                    .when(a.default && several, |d| d.child(div().px(px(5.)).rounded(px(3.)).border_1().border_color(rgb(theme::line_strong())).text_size(px(10.5)).text_color(rgb(theme::text_2())).child("default")))
                    .child(div().flex_1()),
            )
            .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(details.join(" · ")))
            .when_some(rejected, |d, _| {
                d.child(div().text_size(px(12.5)).text_color(rgb(theme::red_text())).child(format!("{} turned this login down the last time git used it. Test it, or sign in again.", a.host)))
            })
            .when_some(days.filter(|d| *d < 14), |d, left| {
                d.child(div().text_size(px(12.5)).text_color(rgb(theme::red_text())).child(match left {
                    l if l < 0 => format!("This token expired on {}. Sign in again to make a new one.", a.expires),
                    0 => "This token expires today. Sign in again to make a new one.".to_string(),
                    l => format!("This token expires in {l} day{}. Sign in again to make a new one.", if l == 1 { "" } else { "s" }),
                }))
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .when(a.provider.is_some(), |d| d.child(ui::button(("acct-test", i), Kind::Ghost, Some(Icon::Check), "Test").on_click(cx.listener(move |this, _, _, cx| this.test_account(&k1, cx)))))
                    .when(!a.email.is_empty(), |d| {
                        d.child(ui::button(("acct-identity", i), Kind::Ghost, Some(Icon::Commit), "Commit as this account").on_click(cx.listener(move |this, _, _, cx| this.use_account_identity(&k2, cx))))
                    })
                    .when(several && !a.default, |d| d.child(ui::button(("acct-default", i), Kind::Ghost, None, "Make default").on_click(cx.listener(move |this, _, _, cx| this.make_default_account(&k3, cx)))))
                    .when(rejected.is_some() || days.is_some_and(|d| d < 14), |d| {
                        let provider = a.provider;
                        let host = a.host.clone();
                        d.child(ui::button(("acct-again", i), Kind::Secondary, Some(Icon::Refresh), "Sign in again").on_click(cx.listener(move |this, _, _, cx| {
                            this.open_sign_in(provider, cx);
                            if let Some(form) = &this.git_auth.sign_in {
                                form.host.update(cx, |i, cx| i.set_text(host.clone(), cx));
                            }
                        })))
                    })
                    .child(ui::button(("acct-signout", i), Kind::Ghost, Some(Icon::Trash), "Sign out").on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_confirm(crate::feedback::ConfirmAction::SignOutGit(k5.clone()), window, cx)
                    }))),
            )
            .into_any_element()
    }

    fn render_sign_in(&self, form: &SignIn, cx: &mut Context<Self>) -> AnyElement {
        let host = git_auth::normalize_host(form.host.read(cx).text());
        let provider = form.provider.or_else(|| git_auth::provider_for(&host));
        let name = git_auth::provider_name(provider);
        let mut card = ui::card().child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().flex_1().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(match form.provider {
                    Some(p) => format!("Sign in to {}", git_auth::provider_name(Some(p))),
                    None => "Sign in to another host".into(),
                }))
                .child(ui::icon_button("signin-close", Icon::X).on_click(cx.listener(|this, _, _, cx| this.close_sign_in(cx)))),
        );
        card = card.child(ui::field("Host", Some("Change it for GitHub Enterprise or a self-hosted GitLab."), form.host.clone()));
        if form.provider.is_none() {
            card = card.child(div().flex().gap(px(6.)).children([(None, "Other"), (Some(Provider::GitHub), "GitHub Enterprise"), (Some(Provider::GitLab), "GitLab")].into_iter().enumerate().map(|(i, (p, label))| {
                let on = provider == p;
                ui::choice(("signin-kind", i), label, on).on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this.git_auth.sign_in.as_mut() {
                        form.provider = p;
                    }
                    cx.notify();
                }))
            })));
        }

        if let Some(provider) = provider {
            // Browser sign-in: the host's own token page, picked up from the clipboard.
            let page = git_auth::token_page(Some(provider), if host.is_empty() { "github.com" } else { &host });
            let what = match provider {
                Provider::GitHub => "Generate token, then the copy button next to it",
                Provider::GitLab => "Create token, then the copy button next to it",
            };
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("In the browser (recommended)"))
                    .child(ui::meta(format!(
                        "Opens {name}'s token page with the right permissions ticked. Click {what}: Monaged picks the token up from the clipboard, checks it with {name} and empties the clipboard. No app to register, nothing goes through anyone but {name}."
                    )))
                    .child(if form.watching {
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(ui::button("signin-reopen", Kind::Secondary, Some(Icon::External), "Open the page again").on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(page) = &page {
                                    cx.open_url(page)
                                }
                            })))
                            .child(ui::button("signin-cancel", Kind::Ghost, None, "Stop waiting").on_click(cx.listener(|this, _, _, cx| this.cancel_browser_sign_in(cx))))
                    } else {
                        div().flex().child(ui::button("signin-browser", Kind::Primary, Some(Icon::External), format!("Sign in with {name}")).on_click(cx.listener(|this, _, _, cx| this.start_browser_sign_in(cx))))
                    }),
            );
        }

        // Token / password.
        let has_page = git_auth::token_page(provider, &host).is_some();
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .pt(px(4.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(if provider.is_some() { "With a token" } else { "With a username and password or token" }))
                .child(ui::meta(match provider {
                    Some(_) => "Already have a personal access token? Paste it here. Monaged checks it and keeps it in Windows Credential Manager.",
                    None => "For Gitea, Bitbucket, Azure DevOps and others: an app password or access token works best. Kept in Windows Credential Manager.",
                }))
                .when(provider.is_none(), |d| d.child(ui::field("Username", None, form.user.clone())))
                .child(div().flex().items_end().gap(px(8.)).child(div().flex_1().child(form.token.clone())).when(has_page, |d| {
                    d.child(ui::button("token-page", Kind::Ghost, Some(Icon::External), "Make a token").on_click(cx.listener(|this, _, _, cx| this.open_token_page(cx))))
                }))
                .child(div().flex().child(ui::button("signin-token", if provider.is_some() { Kind::Secondary } else { Kind::Primary }, Some(Icon::Key), if provider.is_some() { "Sign in with this token" } else { "Save" }).on_click(cx.listener(|this, _, _, cx| this.sign_in_with_token(cx))))),
        );
        if let Some(working) = &form.working {
            card = card.child(div().flex().items_center().gap(px(8.)).child(ui::spinner("signin-spin", 13., theme::text_3())).child(ui::meta(working.clone())));
        }
        if let Some(error) = &form.error {
            card = card.child(ui::callout(Tone::Error, error.clone()));
        }
        card.into_any_element()
    }

    fn render_ssh_keys(&self, cx: &mut Context<Self>) -> AnyElement {
        let auth = &self.git_auth;
        let mut col = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pt(px(6.))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("SSH keys in WSL"))
            .child(ui::meta("For git@… addresses. HTTPS addresses use the accounts above and need no key."));
        let uploaders: Vec<(String, String)> = auth.accounts.iter().filter(|a| a.can_add_keys()).map(|a| (a.key(), format!("Add to {} ({})", a.host, a.login))).collect();
        match &auth.keys {
            None => col = col.child(ui::skeleton_text("ssh-sk", 2)),
            Some(Err(err)) => col = col.child(ui::callout(Tone::Error, err.clone())),
            Some(Ok(keys)) if keys.is_empty() => col = col.child(ui::meta("WSL has no SSH keys yet.")),
            Some(Ok(keys)) => {
                col = col.children(keys.iter().enumerate().map(|(i, k)| {
                    let public = k.public.clone();
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(icon_sized(Icon::Key, 14.).text_color(rgb(theme::text_2())))
                                .child(div().font_family(theme::MONO).text_size(px(12.)).child(format!("~/.ssh/{}", k.path.rsplit('/').next().unwrap_or(&k.path))))
                                .child(ui::mono(k.kind.clone()))
                                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_3())).child(k.comment.clone())),
                        )
                        .child(div().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_4())).child(k.fingerprint.clone()))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .child(ui::button(("ssh-copy", i), Kind::Ghost, Some(Icon::Copy), "Copy public key").on_click(cx.listener({
                                    let public = public.clone();
                                    move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(public.clone()));
                                        this.toast(Tone::Info, "Copied the public key (it's safe to share).", cx);
                                    }
                                })))
                                .children(uploaders.iter().enumerate().map(|(j, (account_key, label))| {
                                    let (public, account_key) = (public.clone(), account_key.clone());
                                    ui::button(SharedString::from(format!("ssh-up-{i}-{j}")), Kind::Ghost, Some(Icon::Push), label.clone()).on_click(cx.listener(move |this, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(public.clone()));
                                        this.upload_ssh_key(public.clone(), account_key.clone(), cx);
                                    }))
                                })),
                        )
                }));
            }
        }
        col = col.child(div().flex().child(ui::button("ssh-create", Kind::Secondary, Some(Icon::Plus), "Create a key").on_click(cx.listener(|this, _, _, cx| this.create_ssh_key(cx)))));

        // Test the hosts you use.
        let mut hosts: Vec<String> = auth.accounts.iter().filter(|a| a.provider.is_some()).map(|a| a.host.clone()).collect();
        for project in &self.projects {
            if let Some(remote) = self.branch_info.get(&project.id).and_then(|i| i.state.remote_url.as_deref()).filter(|u| !u.starts_with("http")).and_then(crate::hosting::parse_remote) {
                hosts.push(remote.host);
            }
        }
        if hosts.is_empty() {
            hosts.push("github.com".into());
        }
        hosts.sort();
        hosts.dedup();
        col = col.children(hosts.into_iter().enumerate().map(|(i, host)| {
            let result = auth.ssh_results.get(&host).cloned();
            let (h1, h2) = (host.clone(), host.clone());
            let unknown = result.as_ref().is_some_and(|(_, m)| m.contains("host key"));
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(28.))
                .child(div().w(px(160.)).flex_none().font_family(theme::MONO).text_size(px(12.)).child(format!("git@{host}")))
                .child(div().flex_1().min_w(px(0.)).text_size(px(12.)).text_color(rgb(match &result {
                    Some((true, _)) => theme::text_2(),
                    Some((false, m)) if m != "Testing…" => theme::red_text(),
                    _ => theme::text_3(),
                })).child(result.map(|(_, m)| m).unwrap_or_else(|| "Not tested".into())))
                .when(unknown, |d| d.child(ui::button(("ssh-trust", i), Kind::Secondary, Some(Icon::Shield), "Trust host").on_click(cx.listener(move |this, _, _, cx| this.trust_ssh_host(h1.clone(), cx)))))
                .child(ui::button(("ssh-test", i), Kind::Ghost, None, "Test").on_click(cx.listener(move |this, _, _, cx| this.test_ssh(h2.clone(), cx))))
        }));
        col.into_any_element()
    }

    // ---- The Git tab: how a project signs in ---------------------------------------------------

    /// The host of a project's origin when it's an HTTPS address.
    pub(crate) fn origin_https_host(&self, project_id: i64) -> Option<String> {
        let url = self.branch_info.get(&project_id)?.state.remote_url.clone()?;
        url.starts_with("http").then(|| git_auth::normalize_host(&url))
    }

    /// Switches the project between its host's accounts (the default first).
    pub(crate) fn cycle_project_login(&mut self, sub_id: i64, project_id: i64, host: String, current: Option<String>, cx: &mut Context<Self>) {
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(self.git_auth.accounts.iter().filter(|a| a.host == host && !a.default).map(|a| Some(a.login.clone())));
        let at = options.iter().position(|o| *o == current).unwrap_or(0);
        let next = options[(at + 1) % options.len()].clone();
        let Some(repo) = self.projects.iter().find(|p| p.id == project_id).map(|p| p.path.clone()) else { return };
        if let Some(panel) = self.git_panels.get_mut(&sub_id) {
            panel.login = Some(next.clone());
        }
        match &next {
            Some(login) => drop(self.git_auth.project_logins.insert(project_id, login.clone())),
            None => drop(self.git_auth.project_logins.remove(&project_id)),
        }
        let label = next.clone().unwrap_or_else(|| "the default account".into());
        let distro = self.config.distro.clone();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { git_auth::set_project_login(&distro, &repo, &host, next.as_deref()) }).await;
            view.update(cx, |this, cx| match result {
                Ok(()) => this.toast(Tone::Info, format!("This project now signs in as {label}."), cx),
                Err(err) => this.toast_error(format!("{err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn test_origin(&mut self, sub_id: i64, repo: String, cx: &mut Context<Self>) {
        self.git_job(sub_id, "Testing the connection…", move |distro| git_auth::test_remote(&distro, &repo), cx);
    }

    pub(crate) fn switch_origin(&mut self, sub_id: i64, repo: String, url: String, cx: &mut Context<Self>) {
        let u = url;
        self.git_job(sub_id, "Changing origin…", move |distro| git_auth::set_origin(&distro, &repo, &u).map(|_| format!("origin is now {u}")), cx);
    }

    /// The Origin section's first rows: the address, who it signs in as, a
    /// connection test, and switching between HTTPS and SSH.
    pub(crate) fn render_origin_signin(&self, sub_id: i64, project_id: i64, repo: &str, panel: &crate::git_ui::GitPanel, cx: &mut Context<Self>) -> AnyElement {
        let Some(url) = self.branch_info.get(&project_id).and_then(|i| i.state.remote_url.clone()) else { return div().into_any_element() };
        let https = url.starts_with("http");
        let host = git_auth::normalize_host(&crate::hosting::parse_remote(&url).map(|r| r.host).unwrap_or_else(|| url.clone()));
        let accounts: Vec<&Account> = self.git_auth.accounts.iter().filter(|a| a.host == host).collect();
        let gcm = matches!(&self.git_auth.helper, Some(Ok(s)) if s.uses_gcm());
        let current = panel.login.clone().flatten();
        let (line, needs_sign_in) = if !https {
            ("SSH key in WSL".to_string(), false)
        } else if let Some(user) = url.split_once("://").and_then(|(_, rest)| rest.split('/').next()).and_then(|auth| auth.rsplit_once('@')).map(|(user, _)| user.to_string()) {
            // Git uses a username in the address before anything else.
            (format!("{user} (named in origin's address; use the account switch after removing it)"), false)
        } else if let Some(login) = &current {
            (format!("{login} (chosen for this project)"), false)
        } else if let Some(default) = accounts.iter().find(|a| a.default) {
            (format!("{} (Monaged account)", default.login), false)
        } else if gcm {
            ("Git Credential Manager".to_string(), false)
        } else {
            ("no login: pushes will fail".to_string(), true)
        };
        let switch_to = if https { git_auth::ssh_for(&url) } else { crate::add_project::https_for(&url) };
        let (r1, r2, u2, h1) = (repo.to_string(), repo.to_string(), switch_to.clone(), host.clone());
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(url.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon_sized(if https { Icon::User } else { Icon::Key }, 12.).text_color(rgb(theme::text_3())))
                    .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(if needs_sign_in { theme::red_text() } else { theme::text_2() })).child(format!("Signs in with: {line}")))
                    .when(https && accounts.len() > 1, |d| {
                        d.child(ui::button("origin-account", Kind::Ghost, None, "Switch account").tooltip(crate::shell::tip("Cycle through your accounts on this host")).on_click(cx.listener(move |this, _, _, cx| {
                            this.cycle_project_login(sub_id, project_id, h1.clone(), current.clone(), cx)
                        })))
                    })
                    .when(needs_sign_in, |d| {
                        d.child(ui::button("origin-signin", Kind::Secondary, Some(Icon::User), "Sign in").on_click(cx.listener(|this, _, window, cx| this.open_settings_at("Git accounts", window, cx))))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(ui::button("origin-test", Kind::Ghost, Some(Icon::Check), "Test connection").on_click(cx.listener(move |this, _, _, cx| this.test_origin(sub_id, r1.clone(), cx))))
                    .when_some(u2, |d, to| {
                        d.child(
                            ui::button("origin-switch", Kind::Ghost, None, if https { "Use SSH instead" } else { "Use HTTPS instead" })
                                .tooltip(crate::session_ui::tip_static(format!("origin becomes {to}")))
                                .on_click(cx.listener(move |this, _, _, cx| this.switch_origin(sub_id, r2.clone(), to.clone(), cx))),
                        )
                    }),
            )
            .into_any_element()
    }
}
