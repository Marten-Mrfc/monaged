//! Adding a project: clone a repo, use a folder that's already in WSL, or
//! start an empty one.
//!
//! - What you type is checked a moment after you stop: whether WSL can sign
//!   in to the repo's host, where it will land and whether that folder is
//!   free, or what's in the folder you picked. Windows paths are understood.
//! - Each problem comes with its fix as a button: use the HTTPS address, trust
//!   a host, use the folder that's already there, clone a folder that sits on
//!   a Windows drive into WSL.
//! - Adding shows its steps, with git's real download progress. Esc or Stop
//!   ends a clone and removes the folder it started.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, ClipboardItem, Context, Entity, Focusable, FontWeight, PathPromptOptions, SharedString, Subscription, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon_sized};
use crate::text_input::{InputEvent, TextInput};
use crate::ui::{self, Kind, StepState, StepView};
use crate::wsl::{quote, shell_path};
use crate::{Form, Monaged, SourceKind, git, theme};

pub(crate) struct AddProject {
    pub source: SourceKind,
    pub url: Entity<TextInput>,
    pub name: Entity<TextInput>,
    pub folder: Entity<TextInput>,
    /// The field to fix before adding (its label), and why.
    pub invalid: Option<(&'static str, String)>,
    pub check: Check,
    check_gen: u64,
    pub run: Option<Run>,
    pub show_output: bool,
    _subs: Vec<Subscription>,
}

/// What Monaged found out about the address or folder so far.
#[derive(Clone, Default)]
pub(crate) struct Check {
    pub busy: bool,
    pub notes: Vec<Note>,
}

#[derive(Clone)]
pub(crate) struct Note {
    pub tone: NoteTone,
    pub text: String,
    pub fixes: Vec<Fix>,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NoteTone {
    Good,
    Info,
    Warn,
    Bad,
}

/// A one-click fix for a problem.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Fix {
    UseHttps(String),
    TrustHost(String),
    UseExisting(String),
    RemoveEmpty(String),
    CloneIntoWsl(String),
    GitSettings,
    Retry,
}

impl Fix {
    fn label(&self) -> String {
        match self {
            Fix::UseHttps(_) => "Use the HTTPS address".into(),
            Fix::TrustHost(host) => format!("Trust {host}"),
            Fix::UseExisting(_) => "Use that folder".into(),
            Fix::RemoveEmpty(_) => "Remove the empty folder".into(),
            Fix::CloneIntoWsl(_) => "Clone it into ~/code instead".into(),
            Fix::GitSettings => "Open Settings → Git".into(),
            Fix::Retry => "Try again".into(),
        }
    }
}

/// Adding, step by step.
pub(crate) struct Run {
    id: u64,
    pub name: String,
    pub steps: Vec<StepView>,
    pub started: Instant,
    cancel: Arc<AtomicBool>,
    pgid: Arc<Mutex<Option<String>>>,
    pub failure: Option<Failure>,
    pub done: bool,
}

impl Run {
    fn active(&self) -> bool {
        !self.done && self.failure.is_none()
    }
}

#[derive(Clone)]
pub(crate) struct Failure {
    pub title: String,
    pub detail: String,
    pub fixes: Vec<Fix>,
    pub output: String,
}

/// What the form adds, worked out from the fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plan {
    pub source: SourceKind,
    pub url: String,
    pub name: String,
    /// The folder in WSL (may start with `~`).
    pub dest: String,
}

/// A git remote address, as far as the form cares.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Remote {
    /// `git@host:path` or `ssh://user@host/path`
    Ssh { user: String, host: String, path: String },
    Https { host: String },
    /// A path to another repository on this machine.
    Local(String),
    Unknown,
}

pub(crate) fn parse_remote(url: &str) -> Remote {
    let url = url.trim();
    if url.is_empty() {
        return Remote::Unknown;
    }
    if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        let authority = rest.split('/').next().unwrap_or_default();
        let host = authority.rsplit('@').next().unwrap_or(authority);
        return if host.is_empty() { Remote::Unknown } else { Remote::Https { host: host.to_string() } };
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (user, host) = auth.split_once('@').unwrap_or(("git", auth));
        let host = host.split(':').next().unwrap_or(host);
        return Remote::Ssh { user: user.into(), host: host.into(), path: path.into() };
    }
    if url.starts_with('/') || url.starts_with('~') || to_wsl_path(url) != url {
        return Remote::Local(to_wsl_path(url));
    }
    // scp style: user@host:path
    if let Some((auth, path)) = url.split_once(':')
        && let Some((user, host)) = auth.split_once('@')
        && !host.is_empty()
        && !path.is_empty()
    {
        return Remote::Ssh { user: user.into(), host: host.into(), path: path.trim_start_matches('/').into() };
    }
    Remote::Unknown
}

/// The HTTPS address for an SSH one (GitLab, GitHub and friends use the same path).
pub(crate) fn https_for(url: &str) -> Option<String> {
    match parse_remote(url) {
        Remote::Ssh { host, path, .. } => {
            let path = path.trim_end_matches('/');
            let path = if path.ends_with(".git") { path.to_string() } else { format!("{path}.git") };
            Some(format!("https://{host}/{path}"))
        }
        _ => None,
    }
}

/// Windows paths as WSL sees them: `O:\Projects\x` is `/mnt/o/Projects/x`,
/// `\\wsl.localhost\Ubuntu\home\me\x` is `/home/me/x`. Linux paths stay.
pub(crate) fn to_wsl_path(input: &str) -> String {
    let t = input.trim().trim_matches('"');
    let lower = t.to_lowercase();
    for prefix in [r"\\wsl.localhost\", r"\\wsl$\", "//wsl.localhost/", "//wsl$/"] {
        if lower.starts_with(prefix) {
            let rest = &t[prefix.len()..];
            let after_distro = rest.split_once(['\\', '/']).map(|(_, r)| r).unwrap_or("");
            return format!("/{}", after_distro.replace('\\', "/"));
        }
    }
    let bytes = t.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes.len() == 2 || bytes[2] == b'\\' || bytes[2] == b'/') {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = t[2..].replace('\\', "/");
        let rest = rest.trim_start_matches('/').trim_end_matches('/');
        return if rest.is_empty() { format!("/mnt/{drive}") } else { format!("/mnt/{drive}/{rest}") };
    }
    t.to_string()
}

fn windows_drive(path: &str) -> Option<char> {
    path.strip_prefix("/mnt/").and_then(|r| r.chars().next()).filter(|c| c.is_ascii_alphabetic() && path.chars().nth(6).is_none_or(|c| c == '/'))
}

fn last_segment(text: &str) -> String {
    let trimmed = text.trim().trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', ':', '\\']).next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

impl Plan {
    fn from_fields(source: SourceKind, url: &str, name: &str, folder: &str) -> Plan {
        let url = url.trim().to_string();
        let folder = to_wsl_path(folder);
        let name = name.trim().to_string();
        let name = if !name.is_empty() {
            name
        } else {
            match source {
                SourceKind::Clone => last_segment(&url),
                SourceKind::Existing => last_segment(&folder),
                SourceKind::Empty => String::new(),
            }
        };
        let dest = if !folder.is_empty() {
            folder
        } else if source == SourceKind::Existing || name.is_empty() {
            String::new()
        } else {
            format!("~/code/{}", git::slug(&name))
        };
        Plan { source, url, name, dest }
    }
}

// ---- Checking as you type ----------------------------------------------------------------

/// What WSL says about the plan. Runs in the background (one wsl.exe call).
fn check(distro: &str, plan: &Plan) -> Vec<Note> {
    let mut notes = Vec::new();
    let dest_script = format!(
        "dest={d}\n\
         if [ -e \"$dest\" ]; then\n\
           if [ ! -d \"$dest\" ]; then echo DEST:file\n\
           elif [ -z \"$(ls -A \"$dest\" 2>/dev/null)\" ]; then echo DEST:empty\n\
           elif [ \"$(ls -A \"$dest\")\" = .git ] && ! git -C \"$dest\" rev-parse -q --verify HEAD >/dev/null 2>&1; then echo DEST:emptyrepo\n\
           else echo DEST:taken; fi\n\
         else echo DEST:free; fi\n",
        d = shell_path(&plan.dest)
    );
    match plan.source {
        SourceKind::Clone => {
            let remote = parse_remote(&plan.url);
            let mut script = String::new();
            if !plan.dest.is_empty() && windows_drive(&plan.dest).is_none() {
                script.push_str(&dest_script);
            }
            match &remote {
                Remote::Unknown => {
                    notes.push(Note {
                        tone: NoteTone::Bad,
                        text: "That doesn't look like a git address. Copy it from the repo's Clone button: git@host:group/repo.git or https://host/group/repo.git.".into(),
                        fixes: Vec::new(),
                    });
                    return notes;
                }
                Remote::Ssh { user, host, .. } => script.push_str(&format!(
                    "cmd=\"$(git config --get core.sshCommand)\"\n\
                     echo \"SSH:$(timeout 12 ${{cmd:-ssh}} -o BatchMode=yes -o ConnectTimeout=8 -T {target} 2>&1 | tr '\\n' ' ')\"\n\
                     ls ~/.ssh/id_* >/dev/null 2>&1 && echo KEY:yes\n\
                     echo \"HELPER:$(git config --get credential.helper)\"\n",
                    target = quote(&format!("{user}@{host}"))
                )),
                Remote::Https { .. } => script.push_str("echo \"HELPER:$(git config --get credential.helper)\"\n"),
                Remote::Local(path) => script.push_str(&format!("p={}\n[ -d \"$p/.git\" ] || [ -f \"$p/HEAD\" ] && echo LOCAL:repo || echo LOCAL:missing\n", shell_path(path))),
            }
            let out = crate::wsl::run(distro, &script).unwrap_or_default();
            let get = |key: &str| out.lines().find_map(|l| l.strip_prefix(key)).map(str::trim).map(str::to_owned);
            match &remote {
                Remote::Ssh { host, .. } => {
                    let ssh = get("SSH:").unwrap_or_default();
                    let lower = ssh.to_lowercase();
                    let has_key = get("KEY:").is_some();
                    let helper = get("HELPER:").unwrap_or_default();
                    let https = https_for(&plan.url);
                    let via = if helper.contains("manager") { "Git Credential Manager" } else { "your saved git login" };
                    if lower.contains("successfully authenticated") || lower.contains("welcome to") || lower.contains("logged in as") || lower.starts_with("hi ") {
                        notes.push(Note { tone: NoteTone::Good, text: format!("WSL can sign in to {host} over SSH."), fixes: Vec::new() });
                    } else if lower.contains("host key verification failed") {
                        let mut fixes = vec![Fix::TrustHost(host.clone())];
                        let mut text = format!("WSL hasn't connected to {host} over SSH before, so it can't confirm the server is really {host}.");
                        if !has_key && let Some(https) = https.clone() {
                            text.push_str(&format!(" WSL has no SSH key either; the HTTPS address signs in with {via}."));
                            fixes.insert(0, Fix::UseHttps(https));
                        }
                        notes.push(Note { tone: NoteTone::Warn, text, fixes });
                    } else if lower.contains("permission denied") {
                        let text = if has_key {
                            format!("{host} doesn't accept WSL's SSH key. The HTTPS address signs in with {via} instead.")
                        } else {
                            format!("WSL has no SSH key that {host} accepts. The HTTPS address signs in with {via} instead.")
                        };
                        notes.push(Note { tone: NoteTone::Warn, text, fixes: https.map(Fix::UseHttps).into_iter().collect() });
                    } else if lower.contains("could not resolve") || lower.contains("timed out") || lower.contains("unreachable") || lower.contains("refused") {
                        notes.push(Note { tone: NoteTone::Bad, text: format!("WSL can't reach {host}. Check the address and your connection."), fixes: Vec::new() });
                    } else if !ssh.is_empty() {
                        notes.push(Note { tone: NoteTone::Info, text: format!("SSH to {host} said: {}", ssh.chars().take(160).collect::<String>()), fixes: Vec::new() });
                    }
                }
                Remote::Https { host } => {
                    let helper = get("HELPER:").unwrap_or_default();
                    if helper.contains("manager") {
                        notes.push(Note {
                            tone: NoteTone::Good,
                            text: format!("Signs in to {host} with Git Credential Manager; a Windows sign-in window may appear the first time."),
                            fixes: Vec::new(),
                        });
                    } else if !helper.is_empty() {
                        notes.push(Note { tone: NoteTone::Good, text: format!("Signs in to {host} with git's saved login."), fixes: Vec::new() });
                    } else {
                        notes.push(Note {
                            tone: NoteTone::Warn,
                            text: "Git in WSL has no saved logins, so a private repo won't clone. Settings → Git connects your Windows logins.".into(),
                            fixes: vec![Fix::GitSettings],
                        });
                    }
                }
                Remote::Local(path) => {
                    if get("LOCAL:").as_deref() == Some("missing") {
                        notes.push(Note { tone: NoteTone::Bad, text: format!("There's no git repo at {path}."), fixes: Vec::new() });
                    } else {
                        notes.push(Note { tone: NoteTone::Good, text: format!("Copies the repo at {path}, with its history."), fixes: Vec::new() });
                    }
                }
                Remote::Unknown => {}
            }
            if let Some(drive) = windows_drive(&plan.dest) {
                notes.push(Note {
                    tone: NoteTone::Warn,
                    text: format!("{} is on the {}: drive. Git and the agents are much slower there; leave the folder empty for ~/code.", plan.dest, drive.to_ascii_uppercase()),
                    fixes: Vec::new(),
                });
            } else {
                dest_notes(&mut notes, &out, &plan.dest, "Clones into");
            }
        }
        SourceKind::Existing => {
            if plan.dest.is_empty() {
                return notes;
            }
            let script = format!(
                "p={p}\n\
                 if [ ! -e \"$p\" ]; then echo MISSING; exit 0; fi\n\
                 if [ ! -d \"$p\" ]; then echo NOTDIR; exit 0; fi\n\
                 echo \"ENTRIES:$(ls -A \"$p\" | wc -l)\"\n\
                 [ -e \"$p/.git\" ] && echo HASGIT\n\
                 case \"$(cd \"$p\" && pwd -P)\" in /mnt/*) exit 0;; esac\n\
                 if [ -e \"$p/.git\" ]; then cd \"$p\"\n\
                   echo \"BRANCH:$(git symbolic-ref --short HEAD 2>/dev/null)\"\n\
                   echo \"COMMITS:$(git rev-list --count HEAD 2>/dev/null || echo 0)\"\n\
                   echo \"REMOTE:$(git remote get-url origin 2>/dev/null)\"\n\
                   git show-ref --verify -q refs/heads/develop && echo DEVELOP\n\
                 fi\n",
                p = shell_path(&plan.dest)
            );
            let out = crate::wsl::run(distro, &script).unwrap_or_default();
            let get = |key: &str| out.lines().find_map(|l| l.strip_prefix(key)).map(str::trim).map(str::to_owned);
            if let Some(drive) = windows_drive(&plan.dest).filter(|_| !out.contains("MISSING")) {
                let drive = drive.to_ascii_uppercase();
                notes.push(if out.contains("HASGIT") {
                    Note {
                        tone: NoteTone::Warn,
                        text: format!("This folder is on the {drive}: drive. Git and the agents are much slower there, and WSL shows a \"performance tip\" whenever git runs in it. Cloning it into ~/code keeps its history and runs at full speed."),
                        fixes: vec![Fix::CloneIntoWsl(plan.dest.clone())],
                    }
                } else {
                    Note {
                        tone: NoteTone::Warn,
                        text: format!("This folder is on the {drive}: drive. Git and the agents are much slower there, and WSL shows a \"performance tip\" whenever git runs in it. Copy it into ~/code for full speed."),
                        fixes: Vec::new(),
                    }
                });
            }
            if out.contains("MISSING") {
                notes.push(Note { tone: NoteTone::Bad, text: format!("There's no folder at {}.", plan.dest), fixes: Vec::new() });
            } else if out.contains("NOTDIR") {
                notes.push(Note { tone: NoteTone::Bad, text: format!("{} is a file, not a folder.", plan.dest), fixes: Vec::new() });
            } else if out.contains("HASGIT") {
                let branch = get("BRANCH:").unwrap_or_default();
                let commits: usize = get("COMMITS:").and_then(|c| c.parse().ok()).unwrap_or(0);
                let remote = get("REMOTE:").filter(|r| !r.is_empty());
                let mut text = if branch.is_empty() { "A git repo".to_string() } else { format!("A git repo on {branch}, {commits} commit{}", if commits == 1 { "" } else { "s" }) };
                if let Some(remote) = remote {
                    text.push_str(&format!(", from {}", short_remote(&remote)));
                }
                text.push('.');
                if out.contains("DEVELOP") {
                    text.push_str(" Subprojects branch off develop.");
                }
                notes.push(Note { tone: NoteTone::Good, text, fixes: Vec::new() });
            } else {
                let entries: usize = get("ENTRIES:").and_then(|c| c.parse().ok()).unwrap_or(0);
                let text = if entries == 0 {
                    "An empty folder. Monaged makes it a git repo with a first commit.".to_string()
                } else {
                    format!("Not a git repo yet. Monaged runs git init and makes an empty first commit; your {entries} files stay as they are, uncommitted.")
                };
                notes.push(Note { tone: NoteTone::Info, text, fixes: Vec::new() });
            }
        }
        SourceKind::Empty => {
            if plan.dest.is_empty() {
                return notes;
            }
            if let Some(drive) = windows_drive(&plan.dest) {
                notes.push(Note {
                    tone: NoteTone::Warn,
                    text: format!("{} is on the {}: drive. Git and the agents are much slower there; leave the folder empty for ~/code.", plan.dest, drive.to_ascii_uppercase()),
                    fixes: Vec::new(),
                });
                return notes;
            }
            let out = crate::wsl::run(distro, &dest_script).unwrap_or_default();
            dest_notes(&mut notes, &out, &plan.dest, "Creates");
        }
    }
    notes
}

fn dest_notes(notes: &mut Vec<Note>, out: &str, dest: &str, verb: &str) {
    let state = out.lines().find_map(|l| l.strip_prefix("DEST:")).unwrap_or_default();
    match state {
        "free" => notes.push(Note { tone: NoteTone::Info, text: format!("{verb} {dest}."), fixes: Vec::new() }),
        "empty" => notes.push(Note { tone: NoteTone::Info, text: format!("{verb} {dest} (the folder is there and empty)."), fixes: Vec::new() }),
        "emptyrepo" => notes.push(Note {
            tone: NoteTone::Warn,
            text: format!("{dest} holds an empty git repo, left by a clone that didn't finish."),
            fixes: vec![Fix::RemoveEmpty(dest.to_string())],
        }),
        "taken" => notes.push(Note {
            tone: NoteTone::Warn,
            text: format!("{dest} already exists and has files in it. Use it as it is, or pick another folder."),
            fixes: vec![Fix::UseExisting(dest.to_string())],
        }),
        "file" => notes.push(Note { tone: NoteTone::Bad, text: format!("{dest} is a file. Pick another folder."), fixes: Vec::new() }),
        _ => {}
    }
}

fn short_remote(url: &str) -> String {
    match parse_remote(url) {
        Remote::Ssh { host, path, .. } => format!("{host}/{}", path.trim_end_matches(".git")),
        Remote::Https { .. } => url.trim_start_matches("https://").trim_start_matches("http://").trim_end_matches(".git").to_string(),
        _ => url.to_string(),
    }
}

/// Git's error output, in words, with the fixes that apply.
pub(crate) fn explain(output: &str, plan: &Plan) -> Failure {
    let lower = output.to_lowercase();
    let host = match parse_remote(&plan.url) {
        Remote::Ssh { host, .. } | Remote::Https { host } => host,
        _ => "the server".into(),
    };
    let https = https_for(&plan.url);
    let last = output.trim().lines().rev().find(|l| !l.trim().is_empty() && !l.starts_with("MONAGED_")).unwrap_or("").trim().to_string();
    let (title, detail, mut fixes): (String, String, Vec<Fix>) = if lower.contains("host key verification failed") {
        (
            format!("WSL doesn't trust {host} yet"),
            format!("WSL has never connected to {host} over SSH, so it can't confirm the server is really {host}."),
            vec![Fix::TrustHost(host.clone())],
        )
    } else if lower.contains("permission denied (publickey") {
        (format!("No SSH key that {host} accepts"), "WSL has no SSH key registered with this host. The HTTPS address signs in with Git Credential Manager instead.".into(), Vec::new())
    } else if lower.contains("could not read username") || lower.contains("authentication failed") || lower.contains("terminal prompts disabled") {
        (
            format!("Couldn't sign in to {host}"),
            "Git in WSL had no login for this host. Settings → Git connects your Windows logins (Git Credential Manager).".into(),
            vec![Fix::GitSettings],
        )
    } else if lower.contains("repository not found") || lower.contains("not found") || lower.contains("does not appear to be a git repository") || lower.contains("does not exist") {
        (format!("{host} has no such repo"), "Check the address, and that your account can see the repo.".into(), Vec::new())
    } else if lower.contains("could not resolve host") || lower.contains("could not resolve hostname") || lower.contains("timed out") || lower.contains("network is unreachable") {
        (format!("Can't reach {host}"), "Check your internet connection and the address.".into(), Vec::new())
    } else if lower.contains("already exists and is not an empty directory") || lower.contains("monaged_taken") {
        (format!("{} already exists", plan.dest), "That folder has files in it. Use it as it is, or pick another folder.".into(), vec![Fix::UseExisting(plan.dest.clone())])
    } else if lower.contains("cancelled") {
        ("Stopped".into(), "Nothing was added; the folder it started is gone.".into(), Vec::new())
    } else if lower.contains("no such file or directory") {
        ("That folder isn't there".into(), last.clone(), Vec::new())
    } else {
        ("That didn't work".into(), last.clone(), Vec::new())
    };
    if matches!(parse_remote(&plan.url), Remote::Ssh { .. })
        && (lower.contains("publickey") || lower.contains("host key"))
        && let Some(https) = https
    {
        fixes.insert(0, Fix::UseHttps(https));
    }
    Failure { title, detail, fixes, output: output.trim().to_string() }
}

/// Git's clone progress → (how far, what's happening).
fn clone_progress(line: &str, host: &str) -> Option<(Option<f32>, String)> {
    let percent = |l: &str| l.split('%').next().and_then(|a| a.rsplit(' ').next()).and_then(|n| n.trim().parse::<f32>().ok()).map(|p| p / 100.);
    let line = line.trim();
    if line.starts_with("Cloning into") {
        return Some((None, format!("Connecting to {host}")));
    }
    if line.starts_with("remote:") {
        return Some((None, format!("{host} is packing the files")));
    }
    if let Some(rest) = line.strip_prefix("Receiving objects:") {
        let speed = rest.split(", ").nth(1).map(|s| s.replace(" | ", " · ")).unwrap_or_default();
        let f = percent(rest).unwrap_or(0.);
        return Some((Some(f * 0.85), if speed.is_empty() { "Downloading".into() } else { format!("Downloading · {}", speed.trim()) }));
    }
    if let Some(rest) = line.strip_prefix("Resolving deltas:") {
        return Some((Some(0.85 + percent(rest).unwrap_or(0.) * 0.1), "Unpacking".into()));
    }
    if let Some(rest) = line.strip_prefix("Updating files:") {
        return Some((Some(0.95 + percent(rest).unwrap_or(0.) * 0.05), "Writing the files".into()));
    }
    None
}

enum Event {
    Step(usize, StepState, Option<String>),
    Progress(Option<f32>, String),
    Finished(Result<git::Prepared, Failure>),
}

/// Does the WSL side of adding, reporting each step.
fn work(distro: &str, plan: &Plan, cancel: &AtomicBool, pgid: &Mutex<Option<String>>, send: &dyn Fn(Event)) -> Result<git::Prepared, Failure> {
    let fail = |output: String| explain(&output, plan);
    let dest = shell_path(&plan.dest);
    match plan.source {
        SourceKind::Clone => {
            send(Event::Step(0, StepState::Running, None));
            let out = crate::wsl::run(
                distro,
                &format!("d={dest}\nif [ -e \"$d\" ] && [ -n \"$(ls -A \"$d\" 2>/dev/null)\" ]; then echo MONAGED_TAKEN; exit 0; fi\nmkdir -p \"$(dirname \"$d\")\" && ([ -e \"$d\" ] && echo MONAGED_EXISTED || echo MONAGED_NEW)"),
            )
            .map_err(|e| fail(format!("{e:#}")))?;
            if out.contains("MONAGED_TAKEN") {
                send(Event::Step(0, StepState::Failed, None));
                return Err(fail("MONAGED_TAKEN".into()));
            }
            let created = out.contains("MONAGED_NEW");
            send(Event::Step(0, StepState::Done, Some(plan.dest.clone())));
            send(Event::Step(1, StepState::Running, None));
            let host = match parse_remote(&plan.url) {
                Remote::Ssh { host, .. } | Remote::Https { host } => host,
                _ => "this machine".into(),
            };
            let url = match parse_remote(&plan.url) {
                Remote::Local(path) => path,
                _ => plan.url.clone(),
            };
            let script = format!(
                "exec setsid --wait bash -c {} < /dev/null",
                quote(&format!("echo \"MONAGED_PGID $$\"; exec git clone --progress {} {dest}", if url.starts_with('~') { shell_path(&url) } else { quote(&url) }))
            );
            let result = crate::wsl::stream(distro, &script, |line| {
                if let Some(id) = line.strip_prefix("MONAGED_PGID ") {
                    *pgid.lock().unwrap() = Some(id.trim().to_string());
                } else if let Some((fraction, text)) = clone_progress(line, &host) {
                    send(Event::Progress(fraction, text));
                }
            });
            *pgid.lock().unwrap() = None;
            if let Err(err) = result {
                let output = if cancel.load(Ordering::SeqCst) { "cancelled".to_string() } else { format!("{err:#}") };
                if created {
                    let _ = crate::wsl::run(distro, &format!("rm -rf {dest}"));
                }
                send(Event::Step(1, StepState::Failed, None));
                return Err(fail(output));
            }
            send(Event::Step(1, StepState::Done, None));
            send(Event::Step(2, StepState::Running, None));
            let prepared = git::prepare_project(distro, &plan.dest).map_err(|e| {
                send(Event::Step(2, StepState::Failed, None));
                fail(format!("{e:#}"))
            })?;
            send(Event::Step(2, StepState::Done, Some(format!("Base branch {}", prepared.base_branch))));
            Ok(prepared)
        }
        SourceKind::Existing | SourceKind::Empty => {
            send(Event::Step(0, StepState::Running, None));
            let locate = if plan.source == SourceKind::Empty { format!("mkdir -p {dest} && cd {dest} && pwd") } else { format!("cd {dest} && pwd") };
            let path = crate::wsl::run(distro, &locate).map_err(|e| {
                send(Event::Step(0, StepState::Failed, None));
                fail(format!("{e:#}"))
            })?;
            send(Event::Step(0, StepState::Done, Some(path.clone())));
            if cancel.load(Ordering::SeqCst) {
                return Err(fail("cancelled".into()));
            }
            send(Event::Step(1, StepState::Running, None));
            let prepared = git::prepare_project(distro, &path).map_err(|e| {
                send(Event::Step(1, StepState::Failed, None));
                fail(format!("{e:#}"))
            })?;
            send(Event::Step(1, StepState::Done, Some(format!("Base branch {}", prepared.base_branch))));
            Ok(prepared)
        }
    }
}

impl Monaged {
    fn add_page(&mut self) -> Option<&mut AddProject> {
        match &mut self.form {
            Some(Form::NewProject(page)) => Some(page),
            _ => None,
        }
    }

    pub(crate) fn open_project_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = cx.new(|cx| TextInput::new("git@gitlab.com:team/app.git  or  https://github.com/you/app.git", cx));
        let name = cx.new(|cx| TextInput::new("e.g. HR app", cx));
        let folder = cx.new(|cx| TextInput::new("~/code/…", cx));
        let subs = [&url, &name, &folder]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Changed) {
                        if let Some(page) = this.add_page() {
                            page.invalid = None;
                        }
                        this.schedule_project_check(cx);
                    }
                })
            })
            .collect();
        window.focus(&url.focus_handle(cx));
        self.form = Some(Form::NewProject(Box::new(AddProject {
            source: SourceKind::Clone,
            url,
            name,
            folder,
            invalid: None,
            check: Check::default(),
            check_gen: 0,
            run: None,
            show_output: false,
            _subs: subs,
        })));
        cx.notify();
    }

    fn project_plan(&self, cx: &gpui::App) -> Option<Plan> {
        let Some(Form::NewProject(page)) = &self.form else { return None };
        Some(Plan::from_fields(page.source, page.url.read(cx).text(), page.name.read(cx).text(), page.folder.read(cx).text()))
    }

    /// Checks the fields a moment after you stop typing.
    fn schedule_project_check(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.add_page() else { return };
        page.check_gen += 1;
        let generation = page.check_gen;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_millis(450)).await;
            let Ok(Some((plan, distro))) = view.update(cx, |this, cx| {
                let current = this.add_page().map(|p| p.check_gen) == Some(generation);
                let plan = this.project_plan(cx).filter(|_| current)?;
                if let Some(page) = this.add_page() {
                    page.check.busy = true;
                }
                cx.notify();
                Some((plan, this.config.distro.clone()))
            }) else {
                return;
            };
            let p = plan.clone();
            let notes = cx.background_spawn(async move { check(&distro, &p) }).await;
            view.update(cx, |this, cx| {
                if let Some(page) = this.add_page()
                    && page.check_gen == generation
                {
                    page.check = Check { busy: false, notes };
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_source(&mut self, source: SourceKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.add_page() else { return };
        page.source = source;
        page.invalid = None;
        page.run = None;
        page.check = Check::default();
        let target = if source == SourceKind::Clone { page.url.clone() } else if source == SourceKind::Existing { page.folder.clone() } else { page.name.clone() };
        window.focus(&target.focus_handle(cx));
        self.schedule_project_check(cx);
        cx.notify();
    }

    fn browse_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.first() else { return };
            let wsl_path = to_wsl_path(&path.to_string_lossy());
            view.update(cx, |this, cx| {
                if let Some(page) = this.add_page() {
                    let folder = page.folder.clone();
                    folder.update(cx, |input, cx| input.set_text(wsl_path, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_fix(&mut self, fix: Fix, window: &mut Window, cx: &mut Context<Self>) {
        let distro = self.config.distro.clone();
        match fix {
            Fix::UseHttps(url) => {
                if let Some(page) = self.add_page() {
                    page.run = None;
                    let input = page.url.clone();
                    input.update(cx, |i, cx| i.set_text(url, cx));
                }
            }
            Fix::UseExisting(dest) => {
                self.set_source(SourceKind::Existing, window, cx);
                if let Some(page) = self.add_page() {
                    let input = page.folder.clone();
                    input.update(cx, |i, cx| i.set_text(dest, cx));
                }
            }
            Fix::CloneIntoWsl(path) => {
                self.set_source(SourceKind::Clone, window, cx);
                if let Some(page) = self.add_page() {
                    let (url, folder) = (page.url.clone(), page.folder.clone());
                    url.update(cx, |i, cx| i.set_text(path, cx));
                    folder.update(cx, |i, cx| i.set_text("", cx));
                }
            }
            Fix::TrustHost(host) => {
                self.progress("trust-host", format!("Getting {host}'s SSH key…"), cx);
                let script = format!("mkdir -p ~/.ssh && chmod 700 ~/.ssh && ssh-keyscan -T 8 {h} 2>/dev/null >> ~/.ssh/known_hosts && ssh-keygen -F {h} >/dev/null && echo ok", h = quote(&host));
                cx.spawn(async move |view, cx| {
                    let result = cx.background_spawn(async move { crate::wsl::run(&distro, &script) }).await;
                    view.update(cx, |this, cx| {
                        match result {
                            Ok(out) if out.contains("ok") => this.settle("trust-host", Tone::Info, format!("WSL now knows {host}'s SSH key."), cx),
                            _ => this.settle("trust-host", Tone::Error, format!("Couldn't get {host}'s SSH key. Is {host} reachable?"), cx),
                        }
                        if let Some(page) = this.add_page() {
                            page.run = None;
                        }
                        this.schedule_project_check(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Fix::RemoveEmpty(dest) => {
                // Only an empty repo with no commits, checked again right before.
                let script = format!(
                    "d={d}\n[ \"$(ls -A \"$d\")\" = .git ] && ! git -C \"$d\" rev-parse -q --verify HEAD >/dev/null 2>&1 && rm -rf \"$d\" && echo removed",
                    d = shell_path(&dest)
                );
                cx.spawn(async move |view, cx| {
                    let result = cx.background_spawn(async move { crate::wsl::run(&distro, &script) }).await;
                    view.update(cx, |this, cx| {
                        match result {
                            Ok(out) if out.contains("removed") => this.toast(Tone::Info, format!("Removed the empty {dest}."), cx),
                            _ => this.toast(Tone::Warn, format!("Left {dest} alone: it isn't an empty repo anymore."), cx),
                        }
                        this.schedule_project_check(cx);
                    })
                    .ok();
                })
                .detach();
            }
            Fix::GitSettings => self.open_settings_at("Git", window, cx),
            Fix::Retry => self.submit_add_project(cx),
        }
        cx.notify();
    }

    /// Esc or Stop while adding: ends a clone. True when something was running.
    pub(crate) fn cancel_add_project(&mut self, cx: &mut Context<Self>) -> bool {
        let distro = self.config.distro.clone();
        let Some(run) = self.add_page().and_then(|p| p.run.as_mut()).filter(|r| r.active()) else { return false };
        run.cancel.store(true, Ordering::SeqCst);
        if let Some(pgid) = run.pgid.lock().unwrap().clone() {
            cx.background_spawn(async move {
                let _ = crate::wsl::run(&distro, &format!("kill -- -{pgid} 2>/dev/null; true"));
            })
            .detach();
        }
        cx.notify();
        true
    }

    pub(crate) fn submit_add_project(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.project_plan(cx) else { return };
        let Some(page) = self.add_page() else { return };
        if page.run.as_ref().is_some_and(|r| r.active()) {
            return;
        }
        let problem = match plan.source {
            SourceKind::Clone if plan.url.is_empty() => Some(("Git address", "Paste the address from the repo's Clone button.", page.url.clone())),
            SourceKind::Existing if plan.dest.is_empty() => Some(("Folder", "Type or browse to the folder.", page.folder.clone())),
            SourceKind::Empty if plan.name.is_empty() => Some(("Name", "Give the project a name.", page.name.clone())),
            _ if plan.name.is_empty() => Some(("Name", "Give the project a name.", page.name.clone())),
            _ => None,
        };
        if let Some((field, why, input)) = problem {
            page.invalid = Some((field, why.to_string()));
            input.update(cx, |i, cx| i.set_invalid(true, cx));
            cx.notify();
            return;
        }
        let repo = match plan.source {
            SourceKind::Clone => last_segment(&plan.url),
            _ => plan.name.clone(),
        };
        let labels: Vec<String> = match plan.source {
            SourceKind::Clone => vec!["Get the folder ready".into(), format!("Download {repo}"), "Set up the repo".into(), "Add it to Monaged".into()],
            SourceKind::Existing => vec![format!("Open {}", plan.dest), "Set up git".into(), "Add it to Monaged".into()],
            SourceKind::Empty => vec![format!("Make {}", plan.dest), "Make a git repo".into(), "Add it to Monaged".into()],
        };
        self.next_run += 1;
        let run_id = self.next_run;
        let cancel = Arc::new(AtomicBool::new(false));
        let pgid = Arc::new(Mutex::new(None));
        let Some(page) = self.add_page() else { return };
        page.show_output = false;
        page.run = Some(Run {
            id: run_id,
            name: plan.name.clone(),
            steps: labels.iter().map(|l| StepView { label: l.clone().into(), detail: None, state: StepState::Waiting, progress: None }).collect(),
            started: Instant::now(),
            cancel: cancel.clone(),
            pgid: pgid.clone(),
            failure: None,
            done: false,
        });
        cx.notify();

        let distro = self.config.distro.clone();
        let (tx, mut rx) = unbounded::<Event>();
        let worker_plan = plan.clone();
        let worker_distro = distro.clone();
        std::thread::spawn(move || {
            let send = |event: Event| {
                let _ = tx.unbounded_send(event);
            };
            let result = work(&worker_distro, &worker_plan, &cancel, &pgid, &send);
            send(Event::Finished(result));
        });
        // A clock for the running step, so a slow step still looks alive.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = view
                    .update(cx, |this, cx| {
                        let running = this.add_page().and_then(|p| p.run.as_ref()).is_some_and(|r| r.id == run_id && r.active());
                        if running {
                            cx.notify();
                        }
                        running
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |view, cx| {
            while let Some(event) = rx.next().await {
                let finished = matches!(event, Event::Finished(_));
                view.update(cx, |this, cx| this.on_add_event(run_id, &plan, &distro, event, cx)).ok();
                if finished {
                    break;
                }
            }
        })
        .detach();
    }

    fn run_mut(&mut self, run_id: u64) -> Option<&mut Run> {
        self.add_page().and_then(|p| p.run.as_mut()).filter(|r| r.id == run_id)
    }

    fn on_add_event(&mut self, run_id: u64, plan: &Plan, distro: &str, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Step(i, state, detail) => {
                if let Some(step) = self.run_mut(run_id).and_then(|r| r.steps.get_mut(i)) {
                    step.state = state;
                    if detail.is_some() || matches!(state, StepState::Running | StepState::Failed) {
                        step.detail = detail.map(Into::into);
                    }
                    step.progress = (state == StepState::Running && plan.source == SourceKind::Clone && i == 1).then_some(None);
                }
            }
            Event::Progress(fraction, text) => {
                if let Some(step) = self.run_mut(run_id).and_then(|r| r.steps.iter_mut().find(|s| s.state == StepState::Running)) {
                    step.progress = Some(fraction);
                    step.detail = Some(text.into());
                }
            }
            Event::Finished(Err(failure)) => {
                if let Some(run) = self.run_mut(run_id) {
                    for step in run.steps.iter_mut().filter(|s| s.state == StepState::Running) {
                        step.state = StepState::Failed;
                    }
                    run.failure = Some(failure);
                } else if failure.title != "Stopped" {
                    self.toast_error(format!("Couldn't add {}: {}", plan.name, failure.title), cx);
                }
            }
            Event::Finished(Ok(prepared)) => {
                let last = self.run_mut(run_id).map(|r| r.steps.len() - 1);
                if let (Some(run), Some(last)) = (self.run_mut(run_id), last) {
                    run.steps[last].state = StepState::Running;
                }
                match self.store.add_project(&plan.name, distro, &prepared.path, &prepared.base_branch) {
                    Ok(project) => {
                        let id = project.id;
                        self.projects.push(project);
                        self.projects.sort_by_key(|p| p.name.to_lowercase());
                        let still_open = self.run_mut(run_id).is_some();
                        if let (Some(run), Some(last)) = (self.run_mut(run_id), last) {
                            run.steps[last].state = StepState::Done;
                            run.done = true;
                        }
                        if prepared.identity_missing {
                            self.identity_missing = true;
                            self.toast_action(Tone::Warn, "Git inside WSL has no name or email yet, so agents can't commit.", "Fix in Settings", crate::feedback::ToastAction::OpenSettings, cx);
                        }
                        self.toast(Tone::Info, format!("Added {}", plan.name), cx);
                        if still_open {
                            // A moment on the finished steps, then the project.
                            cx.spawn(async move |view, cx| {
                                cx.background_executor().timer(Duration::from_millis(700)).await;
                                view.update(cx, |this, cx| {
                                    if this.add_page().and_then(|p| p.run.as_ref()).is_some_and(|r| r.id == run_id) {
                                        this.select_project(id, cx);
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        }
                    }
                    Err(err) => {
                        let failure = Failure { title: "Couldn't save the project".into(), detail: format!("{err:#}"), fixes: vec![Fix::Retry], output: format!("{err:#}") };
                        match self.run_mut(run_id) {
                            Some(run) => {
                                if let Some(last) = last {
                                    run.steps[last].state = StepState::Failed;
                                }
                                run.failure = Some(failure);
                            }
                            None => self.toast_error(format!("Couldn't add {}: {err:#}", plan.name), cx),
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    // ---- The screen --------------------------------------------------------------------------

    pub(crate) fn render_add_project(&self, page: &AddProject, main_w: f32, cx: &mut Context<Self>) -> AnyElement {
        let wide = main_w >= 1120.;
        let source = page.source;
        let plan = Plan::from_fields(source, page.url.read(cx).text(), page.name.read(cx).text(), page.folder.read(cx).text());
        let running = page.run.as_ref().is_some_and(|r| r.active());
        let problem = |label: &str| page.invalid.as_ref().filter(|(l, _)| *l == label).map(|(_, why)| why.clone());

        let source_card = |id: &'static str, glyph: Icon, title: &'static str, detail: &'static str, kind: SourceKind, cx: &mut Context<Self>| {
            let on = source == kind;
            div()
                .id(id)
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(14.))
                .py(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(if on { theme::text_2() } else { theme::line() }))
                .when(on, |d| d.bg(rgb(theme::selected())))
                .when(!on && !running, |d| d.cursor_pointer().hover(|s| s.bg(rgb(theme::hover())).border_color(rgb(theme::line_strong()))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon_sized(glyph, 14.).text_color(rgb(if on { theme::text() } else { theme::text_3() })))
                        .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(rgb(if on { theme::text() } else { theme::text_2() })).child(title)),
                )
                .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(detail))
                .when(!running, |d| d.on_click(cx.listener(move |this, _, window, cx| this.set_source(kind, window, cx))))
        };
        let sources = div()
            .flex()
            .when(!wide && main_w < 760., |d| d.flex_col())
            .gap(px(8.))
            .child(source_card("src-clone", Icon::ArrowDown, "Clone a repo", "From GitLab, GitHub or any git address.", SourceKind::Clone, cx))
            .child(source_card("src-existing", Icon::Folder, "A folder I have", "A folder in WSL or on a Windows drive.", SourceKind::Existing, cx))
            .child(source_card("src-empty", Icon::Plus, "Start empty", "A new folder with a fresh git repo.", SourceKind::Empty, cx));

        let browse = |cx: &mut Context<Self>| {
            ui::button("browse-folder", Kind::Secondary, Some(Icon::Folder), "Browse…").on_click(cx.listener(|this, _, _, cx| this.browse_folder(cx))).into_any_element()
        };
        let derived_name = match source {
            SourceKind::Clone => last_segment(&plan.url),
            SourceKind::Existing => last_segment(&plan.dest),
            SourceKind::Empty => String::new(),
        };
        let name_hint = if page.name.read(cx).text().trim().is_empty() && !derived_name.is_empty() {
            format!("Leave empty for “{derived_name}”.")
        } else {
            "What Monaged calls it.".into()
        };
        let folder_text = page.folder.read(cx).text().trim().to_string();
        let converted = to_wsl_path(&folder_text);
        let folder_hint = if !folder_text.is_empty() && converted != folder_text {
            format!("In WSL that's {converted}.")
        } else {
            match source {
                SourceKind::Existing => "A folder in WSL like ~/code/hr-app. Windows paths work too.".into(),
                _ if plan.dest.is_empty() => "Leave empty for ~/code/<name>.".into(),
                _ => format!("Leave empty for {}.", if folder_text.is_empty() { plan.dest.clone() } else { "~/code/<name>".into() }),
            }
        };
        let fields = match source {
            SourceKind::Clone => div()
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(field("Git address", "SSH (git@…) or HTTPS (https://…), from the repo's Clone button.".into(), page.url.clone(), problem("Git address"), None))
                .child(field("Name", name_hint, page.name.clone(), problem("Name"), None))
                .child(field("Folder in WSL", folder_hint, page.folder.clone(), problem("Folder"), Some(browse(cx)))),
            SourceKind::Existing => div()
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(field("Folder", folder_hint, page.folder.clone(), problem("Folder"), Some(browse(cx))))
                .child(field("Name", name_hint, page.name.clone(), problem("Name"), None)),
            SourceKind::Empty => div()
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(field("Name", "What Monaged calls it; also the folder's name.".into(), page.name.clone(), problem("Name"), None))
                .child(field("Folder in WSL", folder_hint, page.folder.clone(), problem("Folder"), Some(browse(cx)))),
        };

        let side = match &page.run {
            Some(run) => self.render_add_run(run, page.show_output, cx),
            None => self.render_add_check(page, &plan, cx),
        };

        let failed = page.run.as_ref().is_some_and(|r| r.failure.is_some());
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(if running {
                let stoppable = source == SourceKind::Clone;
                if stoppable {
                    ui::button("form-stop", Kind::Secondary, Some(Icon::Stop), "Stop").on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_add_project(cx);
                    }))
                } else {
                    ui::disabled_button("form-submit", Kind::Primary, None, "Adding…")
                }
            } else {
                ui::button("form-submit", Kind::Primary, None, if failed { "Try again" } else { "Add project" }).on_click(cx.listener(|this, _, _, cx| this.submit_add_project(cx)))
            })
            .child(ui::button("form-cancel", Kind::Ghost, None, if running { "Close" } else { "Cancel" }).on_click(cx.listener(|this, _, _, cx| {
                this.cancel_add_project(cx);
                this.form = None;
                cx.notify();
            })))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(11.5))
                    .text_color(rgb(theme::text_3()))
                    .child(ui::kbd("Enter"))
                    .child("add")
                    .child(ui::kbd("Esc"))
                    .child(if running { "stop" } else { "cancel" }),
            );

        let header = div().flex().flex_col().gap(px(4.)).child(ui::title("Add a project")).child(div().text_color(rgb(theme::text_3())).child("A git repo inside WSL that agents work on, each in its own worktree."));
        let form_col = div().flex().flex_col().gap(px(22.)).child(sources).child(fields).child(buttons);
        let content = if wide {
            div()
                .w_full()
                .max_w(px(1080.))
                .flex()
                .flex_col()
                .gap(px(26.))
                .child(header)
                .child(div().flex().gap(px(28.)).items_start().child(div().flex_1().min_w(px(0.)).child(form_col)).child(div().w(px(400.)).flex_none().child(side)))
        } else {
            div().w_full().max_w(px(640.)).flex().flex_col().gap(px(24.)).child(header).child(form_col).child(side)
        };
        div().id("form").flex_1().overflow_y_scroll().child(div().flex().justify_center().px(px(32.)).py(px(44.)).child(content)).into_any_element()
    }

    fn render_add_check(&self, page: &AddProject, plan: &Plan, cx: &mut Context<Self>) -> AnyElement {
        let empty = match plan.source {
            SourceKind::Clone if plan.url.is_empty() => Some("Paste a git address and Monaged checks it here: whether WSL can sign in, and where it lands."),
            SourceKind::Existing if plan.dest.is_empty() => Some("Pick a folder and Monaged looks inside: is it a git repo, which branch, where it came from."),
            SourceKind::Empty if plan.name.is_empty() => Some("Name it and Monaged shows where it goes."),
            _ => None,
        };
        let notes = page.check.notes.iter().enumerate().map(|(i, note)| {
            let (glyph, color) = match note.tone {
                NoteTone::Good => (Icon::Check, theme::text_2()),
                NoteTone::Info => (Icon::Info, theme::text_3()),
                NoteTone::Warn => (Icon::Alert, theme::text_2()),
                NoteTone::Bad => (Icon::Alert, theme::red_text()),
            };
            div()
                .flex()
                .gap(px(10.))
                .child(div().pt(px(2.)).child(icon_sized(glyph, 14.).text_color(rgb(color))))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().text_size(px(13.)).line_height(px(19.)).text_color(rgb(if note.tone == NoteTone::Bad { theme::red_text() } else { theme::text() })).child(note.text.clone()))
                        .when(!note.fixes.is_empty(), |d| d.child(fix_buttons(format!("note-fix-{i}"), &note.fixes, cx))),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(16.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(theme::line()))
            .bg(rgb(theme::panel()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::label("Before adding"))
                    .child(div().flex_1())
                    .when(page.check.busy, |d| d.child(ui::spinner("check-busy", 12., theme::text_3())).child(ui::meta("Checking"))),
            )
            .children(empty.filter(|_| page.check.notes.is_empty()).map(|t| ui::meta(t)))
            .children(notes)
            .into_any_element()
    }

    fn render_add_run(&self, run: &Run, show_output: bool, cx: &mut Context<Self>) -> AnyElement {
        let elapsed = run.started.elapsed().as_secs();
        let status = if run.done {
            "Added".to_string()
        } else if let Some(failure) = &run.failure {
            if failure.title == "Stopped" { "Stopped".to_string() } else { "Didn't work".to_string() }
        } else if elapsed >= 2 {
            format!("{elapsed} s")
        } else {
            String::new()
        };
        let failure = run.failure.clone().map(|f| {
            let output = f.output.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .pt(px(14.))
                .border_t_1()
                .border_color(rgb(theme::line()))
                .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(f.title.clone()))
                .child(div().text_size(px(13.)).line_height(px(19.)).text_color(rgb(theme::text_2())).child(f.detail.clone()))
                .when(!f.fixes.is_empty(), |d| d.child(fix_buttons("failure-fix", &f.fixes, cx)))
                .when(!f.output.is_empty() && f.title != "Stopped", |d| {
                    d.child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(ui::button("toggle-output", Kind::Ghost, None, if show_output { "Hide git's output" } else { "Show git's output" }).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(page) = this.add_page() {
                                    page.show_output = !page.show_output;
                                }
                                cx.notify();
                            })))
                            .child(ui::button("copy-output", Kind::Ghost, Some(Icon::Copy), "Copy").on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(output.clone()));
                                this.toast(Tone::Info, "Copied to the clipboard", cx);
                            }))),
                    )
                    .when(show_output, |d| {
                        d.child(
                            div()
                                .id("git-output")
                                .max_h(px(220.))
                                .overflow_y_scroll()
                                .p(px(10.))
                                .rounded(px(6.))
                                .bg(rgb(theme::code_bg()))
                                .font_family(theme::MONO)
                                .text_size(px(11.))
                                .line_height(px(16.))
                                .text_color(rgb(theme::text_2()))
                                .children(f.output.lines().map(|l| div().child(l.to_string()))),
                        )
                    })
                })
        });
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .p(px(16.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(if run.failure.is_some() { theme::line_strong() } else { theme::line() }))
            .bg(rgb(theme::panel()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(format!("Adding {}", run.name)))
                    .child(div().flex_1())
                    .child(ui::mono(status)),
            )
            .child(ui::steps(SharedString::from(format!("add-steps-{}", run.id)), &run.steps))
            .children(failure)
            .into_any_element()
    }
}

/// A labelled field; the hint under it turns into the problem when there is one.
fn field(label: &'static str, hint: String, input: Entity<TextInput>, problem: Option<String>, extra: Option<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text_2())).child(label))
        .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w(px(0.)).child(input)).children(extra))
        .child(match problem {
            Some(problem) => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(rgb(theme::red_text()))
                .child(icon_sized(Icon::Alert, 13.).text_color(rgb(theme::red_text())))
                .child(problem),
            None => div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(hint),
        })
}

fn fix_buttons(id: impl Into<SharedString>, fixes: &[Fix], cx: &mut Context<Monaged>) -> gpui::Div {
    let id: SharedString = id.into();
    div().flex().flex_wrap().gap(px(6.)).children(fixes.iter().enumerate().map(|(i, fix)| {
        let fix = fix.clone();
        let kind = if i == 0 && fix != Fix::Retry { Kind::Primary } else { Kind::Secondary };
        ui::button(SharedString::from(format!("{id}-{i}")), kind, None, fix.label()).on_click(cx.listener(move |this, _, window, cx| this.apply_fix(fix.clone(), window, cx)))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_become_wsl_paths() {
        assert_eq!(to_wsl_path(r"O:\Projects\app"), "/mnt/o/Projects/app");
        assert_eq!(to_wsl_path("C:/Users/me/"), "/mnt/c/Users/me");
        assert_eq!(to_wsl_path(r"\\wsl.localhost\Ubuntu\home\me\code\x"), "/home/me/code/x");
        assert_eq!(to_wsl_path(r"\\wsl$\Ubuntu\home\me"), "/home/me");
        assert_eq!(to_wsl_path("~/code/x"), "~/code/x");
        assert_eq!(windows_drive("/mnt/o/Projects"), Some('o'));
        assert_eq!(windows_drive("/mnt/wsl/x"), None);
    }

    #[test]
    fn remotes_parse() {
        assert_eq!(
            parse_remote("git@gitlab.com:webstores/projects/tools.git"),
            Remote::Ssh { user: "git".into(), host: "gitlab.com".into(), path: "webstores/projects/tools.git".into() }
        );
        assert_eq!(parse_remote("https://github.com/you/app.git"), Remote::Https { host: "github.com".into() });
        assert_eq!(https_for("git@gitlab.com:webstores/projects/tools.git").as_deref(), Some("https://gitlab.com/webstores/projects/tools.git"));
        assert_eq!(https_for("ssh://git@github.com/you/app").as_deref(), Some("https://github.com/you/app.git"));
        assert_eq!(parse_remote("not a url"), Remote::Unknown);
        assert_eq!(parse_remote(r"O:\Projects\app"), Remote::Local("/mnt/o/Projects/app".into()));
    }

    #[test]
    fn plans_fill_in_names_and_folders() {
        let p = Plan::from_fields(SourceKind::Clone, "git@gitlab.com:team/HR-App.git", "", "");
        assert_eq!((p.name.as_str(), p.dest.as_str()), ("HR-App", "~/code/hr-app"));
        let p = Plan::from_fields(SourceKind::Existing, "", "", r"O:\Projects\thing");
        assert_eq!((p.name.as_str(), p.dest.as_str()), ("thing", "/mnt/o/Projects/thing"));
    }

    #[test]
    fn git_errors_read_as_words() {
        let plan = Plan::from_fields(SourceKind::Clone, "git@gitlab.com:team/app.git", "", "");
        let f = explain("Host key verification failed.\nfatal: Could not read from remote repository.", &plan);
        assert!(f.title.contains("trust"));
        assert_eq!(f.fixes[0], Fix::UseHttps("https://gitlab.com/team/app.git".into()));
        assert!(f.fixes.contains(&Fix::TrustHost("gitlab.com".into())));
        let f = explain("git@gitlab.com: Permission denied (publickey).", &plan);
        assert_eq!(f.fixes[0], Fix::UseHttps("https://gitlab.com/team/app.git".into()));
        assert_eq!(clone_progress("Receiving objects:  50% (10/20), 1.00 MiB | 2.00 MiB/s", "gitlab.com").map(|p| p.0), Some(Some(0.425)));
    }
}
