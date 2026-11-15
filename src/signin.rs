//! Signing in without a terminal window.
//!
//! - **Claude**: `claude auth login` runs hidden in WSL. Monaged shows the
//!   sign-in link it prints and a box for the code Claude's page shows
//!   afterwards, and types that code in for you. The CLI reads the code from
//!   plain input, so no terminal is needed.
//! - **GitHub**: `gh auth login --web` without a terminal prints a one-time
//!   code and github.com/login/device; Monaged shows both and waits.
//! - **GitLab**: `glab auth login --stdin` with a token you paste.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};

/// Which Claude account to sign in with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Account {
    /// A Claude Pro, Max, Team or Enterprise subscription.
    Subscription,
    /// An Anthropic Console account (API billing).
    Console,
}

/// What a sign-in reports, in order.
#[derive(Debug)]
pub enum Event {
    /// The page to sign in on.
    Url(String),
    /// The one-time code to enter on that page (GitHub).
    Code(String),
    /// Something it said, to show (a wrong code, a warning).
    Note(String),
    /// It ended: signed in, or why not.
    Finished(Result<(), String>),
}

/// A sign-in that's running: its input (for the code) and a way to stop it.
#[derive(Clone)]
pub struct Live {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
}

impl Live {
    /// Types a line into it.
    pub fn send_line(&self, line: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().map_err(|_| anyhow::anyhow!("the sign-in stopped"))?;
        let pipe = stdin.as_mut().context("the sign-in stopped")?;
        pipe.write_all(format!("{line}\n").as_bytes()).context("the sign-in stopped")?;
        pipe.flush().context("the sign-in stopped")?;
        Ok(())
    }

    /// Closes its input (for programs that read to the end).
    pub fn close_input(&self) {
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
    }

    pub fn stop(&self) {
        self.close_input();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// Drops terminal escapes: colours (`ESC [ … letter`) and links (`ESC ] … BEL`
/// or `ESC ] … ESC \`), keeping the text.
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if c != '\u{7}' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The first https link in a line.
pub fn find_url(line: &str) -> Option<String> {
    let at = line.find("https://")?;
    let url: String = line[at..].chars().take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')')).collect();
    (url.len() > "https://".len()).then_some(url)
}

/// GitHub's one-time code ("! First copy your one-time code: ABCD-1234").
pub fn find_device_code(line: &str) -> Option<String> {
    let at = line.find("one-time code:")?;
    let code = line[at + "one-time code:".len()..].split_whitespace().next()?.to_string();
    (code.len() >= 4 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(code)
}

/// What the sign-in runs inside WSL (as `bash -lc`, so no double quotes:
/// wsl.exe drops them). BROWSER=true keeps the CLI from opening a Linux
/// browser itself; Monaged opens the page in Windows.
pub fn claude_script(cli: &str, account: Account) -> String {
    let cli = match cli.strip_prefix("~/") {
        Some(rest) => format!("$HOME/{}", crate::wsl::quote(rest)),
        None => crate::wsl::quote(cli),
    };
    let flag = match account {
        Account::Subscription => "--claudeai",
        Account::Console => "--console",
    };
    format!("export BROWSER=true NO_COLOR=1; exec timeout 1800 {cli} auth login {flag}")
}

/// Checks a pasted Claude code: the page shows `code#state`, all of it needed.
pub fn check_claude_code(code: &str) -> Result<String, &'static str> {
    let code = code.trim().trim_matches('`').trim();
    if code.is_empty() {
        return Err("Paste the code the page shows after you sign in.");
    }
    if code.chars().any(char::is_whitespace) {
        return Err("That doesn't look like the code; copy it with the page's copy button.");
    }
    match code.split_once('#') {
        Some((a, b)) if !a.is_empty() && !b.is_empty() => Ok(code.to_string()),
        _ => Err("That's only part of the code; copy all of it with the page's copy button."),
    }
}

/// Reads a program's output line by line (at `\n` and `\r`) on its own thread.
fn read_lines(pipe: impl Read + Send + 'static, mut on_line: impl FnMut(String) + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = crate::wsl::decode(&buf).replace('\0', "");
                    for part in text.split('\r') {
                        let line = strip_escapes(part).trim().to_string();
                        if !line.is_empty() {
                            on_line(line);
                        }
                    }
                }
            }
        }
    })
}

fn spawn(mut command: Command) -> Result<(Live, std::process::ChildStdout, std::process::ChildStderr)> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("couldn't start the sign-in")?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().context("the sign-in's output is missing")?;
    let stderr = child.stderr.take().context("the sign-in's output is missing")?;
    Ok((Live { child: Arc::new(Mutex::new(child)), stdin: Arc::new(Mutex::new(stdin)) }, stdout, stderr))
}

/// Waits for the program on its own thread, then reports how it ended.
fn finish(live: &Live, readers: Vec<std::thread::JoinHandle<()>>, ok_seen: Arc<Mutex<bool>>, last_error: Arc<Mutex<String>>, what: &'static str, send: Arc<dyn Fn(Event) + Send + Sync>) {
    let child = live.child.clone();
    std::thread::spawn(move || {
        for reader in readers {
            let _ = reader.join();
        }
        // Output is closed: the program is done or about to be.
        let status = loop {
            let done = child.lock().ok().and_then(|mut c| c.try_wait().ok().flatten());
            if let Some(status) = done {
                break Some(status);
            }
            if child.lock().is_err() {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let ok = status.is_some_and(|s| s.success()) || ok_seen.lock().is_ok_and(|v| *v);
        let result = if ok {
            Ok(())
        } else {
            let why = last_error.lock().map(|e| e.clone()).unwrap_or_default();
            Err(if why.is_empty() { format!("{what} stopped before it was done.") } else { why })
        };
        send(Event::Finished(result));
    });
}

/// Starts `claude auth login` in the distro, hidden. Reports the link, then
/// whether it worked once you've given it the code (`Live::send_line`).
pub fn start_claude(distro: &str, cli: &str, account: Account, send: impl Fn(Event) + Send + Sync + 'static) -> Result<Live> {
    if !crate::wsl::installed() {
        bail!("WSL isn't installed yet.");
    }
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "--cd", "~", "--exec", "bash", "-lc", &claude_script(cli, account)]);
    let (live, stdout, stderr) = spawn(command)?;
    let send: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(send);
    let ok_seen = Arc::new(Mutex::new(false));
    let last_error = Arc::new(Mutex::new(String::new()));
    let url_sent = Arc::new(Mutex::new(false));
    let on_line = {
        let (send, ok_seen, last_error, url_sent) = (send.clone(), ok_seen.clone(), last_error.clone(), url_sent.clone());
        move |line: String| {
            if line.starts_with("Login successful") {
                if let Ok(mut ok) = ok_seen.lock() {
                    *ok = true;
                }
                return;
            }
            if let Some(url) = find_url(&line) {
                let mut sent = url_sent.lock().unwrap_or_else(|e| e.into_inner());
                if !*sent && (url.contains("oauth") || url.contains("authorize") || line.contains("visit") || line.contains("Sign in at")) {
                    *sent = true;
                    send(Event::Url(url));
                }
                return;
            }
            if line.starts_with("Paste code here") || line.starts_with("Opening browser") || line.starts_with("The browser can't return") {
                return;
            }
            if line.starts_with("Invalid code") {
                send(Event::Note("That's not the whole code; copy all of it with the page's copy button.".into()));
                return;
            }
            if (line.starts_with("Login failed") || line.contains("rror"))
                && let Ok(mut e) = last_error.lock()
            {
                *e = line.clone();
            }
            send(Event::Note(line));
        }
    };
    let on_err = on_line.clone();
    let readers = vec![read_lines(stdout, on_line), read_lines(stderr, on_err)];
    finish(&live, readers, ok_seen, last_error, "The sign-in", send);
    Ok(live)
}

/// `gh auth login` for github.com in the browser, hidden: reports the
/// one-time code and the page to enter it on.
pub fn start_github(send: impl Fn(Event) + Send + Sync + 'static) -> Result<Live> {
    let mut command = Command::new("gh");
    command.args(["auth", "login", "--hostname", "github.com", "--git-protocol", "https", "--web"]).env("GH_NO_UPDATE_NOTIFIER", "1").env("NO_COLOR", "1");
    let (live, stdout, stderr) = spawn(command)?;
    let send: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(send);
    let ok_seen = Arc::new(Mutex::new(false));
    let last_error = Arc::new(Mutex::new(String::new()));
    let on_line = {
        let (send, ok_seen, last_error) = (send.clone(), ok_seen.clone(), last_error.clone());
        move |line: String| {
            if let Some(code) = find_device_code(&line) {
                send(Event::Code(code));
            } else if let Some(url) = find_url(&line) {
                send(Event::Url(url));
            } else if line.contains("Logged in as") || line.contains("Authentication complete") {
                if let Ok(mut ok) = ok_seen.lock() {
                    *ok = true;
                }
            } else {
                if let Ok(mut e) = last_error.lock() {
                    *e = line.clone();
                }
                send(Event::Note(line));
            }
        }
    };
    let on_err = on_line.clone();
    let readers = vec![read_lines(stdout, on_line), read_lines(stderr, on_err)];
    finish(&live, readers, ok_seen, last_error, "GitHub's sign-in", send);
    Ok(live)
}

/// `glab auth login` with a personal access token. Blocking.
pub fn glab_token(host: &str, token: &str) -> Result<()> {
    let mut command = Command::new("glab");
    command.args(["auth", "login", "--hostname", host, "--stdin"]).env("NO_COLOR", "1");
    let (live, mut stdout, mut stderr) = spawn(command)?;
    live.send_line(token.trim())?;
    live.close_input();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let _ = stdout.read_to_end(&mut out);
    let _ = stderr.read_to_end(&mut err);
    let status = live.child.lock().map_err(|_| anyhow::anyhow!("glab stopped"))?.wait()?;
    if !status.success() {
        let text = strip_escapes(&format!("{}\n{}", crate::wsl::decode(&err), crate::wsl::decode(&out)));
        let why: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(2).collect();
        bail!("GitLab didn't accept it: {}", if why.is_empty() { "glab failed".to_string() } else { why.join(" ") });
    }
    Ok(())
}

/// Where to make a GitLab token with the scopes glab needs.
pub fn gitlab_token_page(host: &str) -> String {
    format!("https://{host}/-/user_settings/personal_access_tokens?name=Monaged&scopes=api,write_repository")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colours_and_links_but_keeps_the_text() {
        let line = "If the browser didn't open, visit: \u{1b}]8;;https://claude.ai/oauth/authorize?a=1\u{7}https://claude.ai/oauth/authorize?a=1\u{1b}]8;;\u{7}";
        assert_eq!(strip_escapes(line), "If the browser didn't open, visit: https://claude.ai/oauth/authorize?a=1");
        assert_eq!(strip_escapes("\u{1b}[1m\u{1b}[33m!\u{1b}[0m code"), "! code");
        assert_eq!(strip_escapes("a\u{1b}]8;;x\u{1b}\\b"), "ab");
    }

    #[test]
    fn finds_the_link_and_githubs_code() {
        assert_eq!(find_url("visit: https://claude.ai/oauth/authorize?x=1&y=2 now").as_deref(), Some("https://claude.ai/oauth/authorize?x=1&y=2"));
        assert_eq!(find_url("no link here"), None);
        assert_eq!(find_device_code("! First copy your one-time code: AB12-CD34").as_deref(), Some("AB12-CD34"));
        assert_eq!(find_device_code("Open this URL to continue in your web browser: https://github.com/login/device"), None);
    }

    #[test]
    fn the_claude_script_has_no_double_quotes_and_picks_the_account() {
        let script = claude_script("~/.local/share/claude-agent-acp/current/claude", Account::Subscription);
        assert!(!script.contains('"'));
        assert!(script.contains("$HOME/'.local/share/claude-agent-acp/current/claude' auth login --claudeai"));
        assert!(script.contains("BROWSER=true"));
        assert!(claude_script("claude", Account::Console).ends_with("auth login --console"));
    }

    #[test]
    fn checks_pasted_codes() {
        assert_eq!(check_claude_code("  abc#def \n").as_deref(), Ok("abc#def"));
        assert!(check_claude_code("abc").is_err());
        assert!(check_claude_code("#def").is_err());
        assert!(check_claude_code("").is_err());
        assert!(check_claude_code("ab c#d").is_err());
    }
}
