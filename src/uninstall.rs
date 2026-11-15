//! Uninstalling Monaged (`monaged.exe --uninstall`, from Settings → Apps or
//! the Start menu): everything Monaged put on this PC, a choice for each
//! part, and never WSL itself.
//!
//! - **Look first.** Before anything goes, the uninstaller reads Monaged's
//!   database (read-only), the registry, Credential Manager and its folders,
//!   then asks WSL what's there, and offers only what it found.
//! - **What goes.** The program always, and with it git's sign-in setting in
//!   WSL: git would otherwise call a monaged.exe that's gone. The Claude agent
//!   Monaged downloaded is ticked; projects and settings, saved sign-ins,
//!   agents' working folders, the playground and Claude's sign-in in WSL are
//!   choices. The Linux distro the setup guide installed (that one only) can
//!   go too, but is never ticked by itself.
//! - **In order.** Everything that needs WSL while the distro is still there;
//!   then the distro (`wsl --terminate`, `wsl --unregister`); then saved
//!   sign-ins; then the data folders (an imported distro's folder only once WSL
//!   has let go of it); then the program, whose folder goes once the window
//!   closes. A part that fails doesn't stop the rest, except that the program
//!   stays while git in WSL still points at it.
//! - **Never.** WSL itself, other distros, apps installed with winget (Zed,
//!   gh), Linux packages, SSH keys, your repositories and their branches.
//!
//! `--uninstall --all` ticks everything, the distro included (after a
//! confirmation). `--uninstall --quiet` removes without a window (the default
//! choice, or everything with `--all`) and exits with 0 when all went well.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{AnyElement, Context, FontWeight, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::icons::{Icon, icon_sized};
use crate::onboarding::{AGENT_HOME, INSTALLED_DISTRO_SETTING};
use crate::ui::{self, Kind, StepState, StepView};
use crate::{install, instance, theme, wsl};

/// Monaged's service name in Windows Credential Manager.
const SERVICE: &str = "monaged";
const API_KEY: &str = "anthropic-api-key";
/// Decrypts the environment values saved in the database: goes only with it.
const VAULT_KEY: &str = "env-vault-key";
const GIT_ENTRIES: [&str; 3] = ["git-token:", "git-default:", "git-logins:"];
/// Where Claude Code keeps its sign-in on Linux.
const CLAUDE_LOGIN: &str = "~/.claude/.credentials.json";
/// The playground's local remote (see `playground::script`).
const PLAYGROUND_REMOTE: &str = "~/code/.remotes/monaged-playground.git";

// ---- The parts ------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Program,
    GitHelper,
    Agent,
    Credentials,
    Data,
    Worktrees,
    Playground,
    ClaudeLogin,
    Distro,
}

impl Part {
    /// The checklist's order.
    pub const LISTED: [Part; 9] =
        [Part::Program, Part::GitHelper, Part::Agent, Part::Credentials, Part::Data, Part::Worktrees, Part::Playground, Part::ClaudeLogin, Part::Distro];
    /// The order of removing: WSL while the distro is there, then the distro,
    /// sign-ins, data, and the program last.
    pub const ORDER: [Part; 9] =
        [Part::GitHelper, Part::ClaudeLogin, Part::Worktrees, Part::Playground, Part::Agent, Part::Distro, Part::Credentials, Part::Data, Part::Program];

    /// Always goes: the program, and git's setting that points at it.
    pub fn locked(self) -> bool {
        matches!(self, Part::Program | Part::GitHelper)
    }

    pub fn ticked_by_default(self) -> bool {
        matches!(self, Part::Program | Part::GitHelper | Part::Agent)
    }

    pub fn in_wsl(self) -> bool {
        matches!(self, Part::GitHelper | Part::Agent | Part::Worktrees | Part::Playground | Part::ClaudeLogin)
    }

    /// Takes something of yours with it, so the uninstaller asks once more.
    fn asks_first(self) -> bool {
        matches!(self, Part::Distro | Part::Data | Part::Worktrees | Part::Playground)
    }
}

pub fn default_choice() -> HashSet<Part> {
    Part::LISTED.into_iter().filter(|p| p.ticked_by_default()).collect()
}

pub fn everything() -> HashSet<Part> {
    Part::LISTED.into_iter().collect()
}

// ---- What's on this PC -------------------------------------------------------------------------

/// A distro WSL has for this user (from the registry).
#[derive(Clone, Debug, PartialEq)]
pub struct Registered {
    pub name: String,
    /// The folder with its disk.
    pub base: Option<PathBuf>,
    pub disk_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectRef {
    pub name: String,
    pub distro: String,
    pub path: String,
}

/// What Monaged's database says.
#[derive(Clone, Debug, Default)]
pub struct Db {
    pub found: bool,
    /// `wsl:distro`: the distro Monaged runs in.
    pub distro: Option<String>,
    /// The distro the setup guide installed (`setup:installed_distro`).
    pub installed_distro: Option<String>,
    pub projects: Vec<ProjectRef>,
    /// Git accounts as (host, login).
    pub accounts: Vec<(String, String)>,
    pub theme: Option<String>,
}

/// What one distro holds of Monaged's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    /// Git settings that call monaged.exe.
    pub helper: usize,
    pub agent_kb: Option<u64>,
    pub playground_kb: Option<u64>,
    pub claude_login: bool,
    /// Worktree folders that are there, by root, with their size.
    pub worktrees: HashMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub path: PathBuf,
    pub bytes: u64,
}

/// One project's agent folders: `<parent>/.monaged/<repo>` next to the repo.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeRoot {
    pub distro: String,
    pub repo: String,
    pub root: String,
}

/// Everything read before anything is removed.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// %LOCALAPPDATA%\Programs\Monaged
    pub program: Option<Folder>,
    /// %APPDATA%\Monaged
    pub data: Option<Folder>,
    /// %LOCALAPPDATA%\Monaged (its size leaves out disks WSL still uses).
    pub local: Option<Folder>,
    pub db: Db,
    /// Monaged's entries in Credential Manager.
    pub credentials: Vec<String>,
    pub wsl: bool,
    pub registered: Vec<Registered>,
    /// What each distro holds, once asked (missing: couldn't tell).
    pub probes: HashMap<String, Probe>,
    pub probed: bool,
    /// A Monaged window is open.
    pub running: bool,
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

impl Facts {
    fn registered(&self, name: &str) -> Option<&Registered> {
        self.registered.iter().find(|r| same_name(&r.name, name))
    }

    /// Distros Monaged used that WSL still has, as WSL spells them.
    pub fn used_distros(&self) -> Vec<String> {
        if !self.wsl {
            return Vec::new();
        }
        let mut wanted: Vec<&str> = Vec::new();
        match &self.db.distro {
            Some(distro) => wanted.push(distro),
            // Before the setting existed, Monaged used "Ubuntu".
            None if self.db.found => wanted.push("Ubuntu"),
            None => {}
        }
        wanted.extend(self.db.installed_distro.as_deref());
        wanted.extend(self.db.projects.iter().map(|p| p.distro.as_str()));
        let mut out: Vec<String> = Vec::new();
        for name in wanted {
            if let Some(r) = self.registered(name)
                && !out.iter().any(|o| same_name(o, &r.name))
            {
                out.push(r.name.clone());
            }
        }
        out
    }

    /// The other distros WSL has (container tools' own left out). Monaged may
    /// have been git's helper in one before you switched distros, so they're
    /// asked about that, and only that.
    pub fn other_distros(&self) -> Vec<String> {
        if !self.wsl {
            return Vec::new();
        }
        let used = self.used_distros();
        self.registered
            .iter()
            .map(|r| r.name.clone())
            .filter(|name| !used.iter().any(|u| same_name(u, name)))
            .filter(|name| {
                let lower = name.to_lowercase();
                !["docker-desktop", "rancher-desktop", "podman-machine"].iter().any(|tool| lower.starts_with(tool))
            })
            .collect()
    }

    /// The distro the setup guide installed, while WSL still has it: the only
    /// one the uninstaller ever offers to remove.
    pub fn removable_distro(&self) -> Option<&Registered> {
        if !self.wsl {
            return None;
        }
        self.registered(self.db.installed_distro.as_deref()?)
    }

    /// The distro that goes with this choice.
    fn distro_going(&self, chosen: &HashSet<Part>) -> Option<String> {
        chosen.contains(&Part::Distro).then(|| self.removable_distro().map(|r| r.name.clone())).flatten()
    }

    /// The probe found it in `distro`; not asked (or no answer) counts as there.
    fn has(&self, distro: &str, test: impl Fn(&Probe) -> bool) -> bool {
        self.probes.get(distro).is_none_or(test)
    }

    /// Every project's agent folder root, where WSL still has its distro.
    fn all_worktree_roots(&self) -> Vec<WorktreeRoot> {
        let mut out: Vec<WorktreeRoot> = Vec::new();
        if !self.wsl {
            return out;
        }
        for p in &self.db.projects {
            let (Some(r), Some(root)) = (self.registered(&p.distro), worktree_root(&p.path)) else { continue };
            if !out.iter().any(|w| w.distro == r.name && w.root == root) {
                out.push(WorktreeRoot { distro: r.name.clone(), repo: p.path.trim().trim_end_matches('/').to_string(), root });
            }
        }
        out
    }

    /// Agent folder roots that are there (or might be).
    pub fn worktree_roots(&self) -> Vec<WorktreeRoot> {
        self.all_worktree_roots().into_iter().filter(|w| self.has(&w.distro, |p| p.worktrees.contains_key(&w.root))).collect()
    }

    fn playground_project_in(&self, distro: &str) -> bool {
        self.db.projects.iter().any(|p| same_name(&p.distro, distro) && p.path.trim_end_matches('/').ends_with("/monaged-playground"))
    }

    /// The distros a WSL part has something to remove in.
    pub fn targets(&self, part: Part) -> Vec<String> {
        let used = self.used_distros();
        match part {
            Part::GitHelper => {
                let mut out: Vec<String> = used.into_iter().filter(|d| self.has(d, |p| p.helper > 0)).collect();
                // Other distros only where the probe saw git calling Monaged.
                out.extend(self.other_distros().into_iter().filter(|d| self.probes.get(d).is_some_and(|p| p.helper > 0)));
                out
            }
            Part::Agent => used.into_iter().filter(|d| self.has(d, |p| p.agent_kb.is_some())).collect(),
            Part::ClaudeLogin => used.into_iter().filter(|d| self.has(d, |p| p.claude_login)).collect(),
            Part::Playground => used
                .into_iter()
                .filter(|d| match self.probes.get(d) {
                    Some(p) => p.playground_kb.is_some(),
                    None => self.playground_project_in(d),
                })
                .collect(),
            Part::Worktrees => {
                let mut out: Vec<String> = Vec::new();
                for w in self.worktree_roots() {
                    if !out.contains(&w.distro) {
                        out.push(w.distro);
                    }
                }
                out
            }
            Part::Distro => self.removable_distro().map(|r| vec![r.name.clone()]).unwrap_or_default(),
            Part::Program | Part::Credentials | Part::Data => Vec::new(),
        }
    }

    /// `targets`, without the distro that's being removed anyway. Git's
    /// setting goes from that one too: should removing the distro fail, git
    /// there mustn't be left calling a monaged.exe that's gone.
    pub fn staying(&self, part: Part, chosen: &HashSet<Part>) -> Vec<String> {
        let going = self.distro_going(chosen).filter(|_| part != Part::GitHelper);
        self.targets(part).into_iter().filter(|d| going.as_deref().is_none_or(|g| !same_name(d, g))).collect()
    }

    fn staying_roots(&self, chosen: &HashSet<Part>) -> Vec<WorktreeRoot> {
        let going = self.distro_going(chosen);
        self.worktree_roots().into_iter().filter(|w| going.as_deref().is_none_or(|g| !same_name(&w.distro, g))).collect()
    }

    /// A WSL part with nothing left to do once the distro goes.
    pub fn goes_with_distro(&self, part: Part, chosen: &HashSet<Part>) -> Option<String> {
        let going = self.distro_going(chosen)?;
        (part.in_wsl() && !self.targets(part).is_empty() && self.staying(part, chosen).is_empty()).then_some(going)
    }

    /// Saved sign-ins (the vault key belongs to the data).
    fn sign_ins(&self) -> Vec<String> {
        self.credentials.iter().filter(|c| *c != VAULT_KEY).cloned().collect()
    }
}

// ---- The plan --------------------------------------------------------------------------------

/// One line of the checklist.
#[derive(Clone, Debug)]
pub struct Item {
    pub part: Part,
    pub title: String,
    pub detail: String,
    pub bytes: Option<u64>,
    /// Still asking WSL what's there.
    pub checking: bool,
}

/// What can be removed, in the checklist's order.
pub fn items(f: &Facts) -> Vec<Item> {
    Part::LISTED.into_iter().filter_map(|part| item(f, part)).collect()
}

fn item(f: &Facts, part: Part) -> Option<Item> {
    let targets = f.targets(part);
    if part.in_wsl() && targets.is_empty() {
        return None;
    }
    let kb = |get: fn(&Probe) -> Option<u64>| -> Option<u64> {
        let sizes: Vec<u64> = targets.iter().filter_map(|d| f.probes.get(d).and_then(get)).collect();
        (!sizes.is_empty()).then(|| sizes.iter().sum::<u64>() * 1024)
    };
    let (title, detail, bytes) = match part {
        Part::Program => (
            "The program".to_string(),
            "monaged.exe, its shortcuts, its entry in Settings → Apps and leftover downloads.".to_string(),
            f.program.as_ref().map(|p| p.bytes),
        ),
        Part::GitHelper => (
            "Git's sign-in setting in WSL".to_string(),
            format!("Git in {} asks Monaged for your git sign-ins. That fails once Monaged is gone, so this goes with the program.", and_list(&targets)),
            None,
        ),
        Part::Agent => (
            "The Claude agent".to_string(),
            format!("claude-agent-acp and the Claude CLI that Monaged downloaded into ~/.local/share in {}. Monaged downloads it again if you reinstall.", and_list(&targets)),
            kb(|p| p.agent_kb),
        ),
        Part::Credentials => {
            let names = f.sign_ins();
            if names.is_empty() {
                return None;
            }
            (
                "Saved sign-ins".to_string(),
                format!("{} in Windows Credential Manager. Git tokens keep working on the website until you revoke them there.", capitalize(&sign_ins_text(&names))),
                None,
            )
        }
        Part::Data => {
            if f.data.is_none() && f.local.as_ref().is_none_or(|l| l.bytes == 0) {
                return None;
            }
            let bytes = f.data.as_ref().map_or(0, |d| d.bytes) + f.local.as_ref().map_or(0, |d| d.bytes);
            (
                "Projects and settings".to_string(),
                "Your projects list, agent conversations, settings, saved environment values and logs (%APPDATA%\\Monaged). Your code isn't in there.".to_string(),
                Some(bytes),
            )
        }
        Part::Worktrees => {
            let roots = f.worktree_roots();
            let sizes: Vec<u64> = roots.iter().filter_map(|w| f.probes.get(&w.distro).and_then(|p| p.worktrees.get(&w.root).copied())).collect();
            (
                "Agents' working folders".to_string(),
                format!(
                    "The folders agents work in, next to {} (in .monaged). Changes agents didn't commit are lost; their branches stay in your repos.",
                    count(roots.len(), "project")
                ),
                (!sizes.is_empty()).then(|| sizes.iter().sum::<u64>() * 1024),
            )
        }
        Part::Playground => (
            "The playground".to_string(),
            "The practice project Monaged made: ~/code/monaged-playground and its local remote.".to_string(),
            kb(|p| p.playground_kb),
        ),
        Part::ClaudeLogin => (
            "Claude's sign-in in WSL".to_string(),
            "Signs the Claude CLI out. Claude Code you run yourself uses the same sign-in and asks again; your Claude settings and history stay.".to_string(),
            None,
        ),
        Part::Distro => {
            let r = f.removable_distro()?;
            (
                format!("{}, the Linux Monaged installed", r.name),
                format!("Everything in {} goes: your Linux files, code you haven't pushed anywhere, programs you installed. WSL itself stays.", r.name),
                r.disk_bytes,
            )
        }
    };
    Some(Item { part, title, detail, bytes, checking: part.in_wsl() && !f.probed })
}

/// What a step does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Task {
    /// Close the open Monaged window first.
    CloseMonaged,
    Remove(Part),
}

#[derive(Clone, Debug)]
pub struct Step {
    pub task: Task,
    pub label: String,
    /// Shown instead of running it (it goes with the distro).
    pub skip: Option<String>,
}

fn step_label(f: &Facts, part: Part) -> String {
    match part {
        Part::Program => "Remove the program".into(),
        Part::GitHelper => "Remove git's sign-in setting in WSL".into(),
        Part::Agent => "Remove the Claude agent".into(),
        Part::Credentials => "Remove saved sign-ins".into(),
        Part::Data => "Remove projects and settings".into(),
        Part::Worktrees => "Remove agents' working folders".into(),
        Part::Playground => "Remove the playground".into(),
        Part::ClaudeLogin => "Sign Claude out in WSL".into(),
        Part::Distro => format!("Remove {}", f.removable_distro().map_or("the distro", |r| r.name.as_str())),
    }
}

/// The steps for this choice, in the order they run.
pub fn steps(f: &Facts, chosen: &HashSet<Part>) -> Vec<Step> {
    let present: Vec<Part> = items(f).into_iter().map(|i| i.part).collect();
    let mut out = Vec::new();
    if f.running {
        out.push(Step { task: Task::CloseMonaged, label: "Close Monaged".into(), skip: None });
    }
    for part in Part::ORDER {
        if !present.contains(&part) || !(part.locked() || chosen.contains(&part)) {
            continue;
        }
        let skip = f.goes_with_distro(part, chosen).map(|name| format!("Goes with {name}"));
        out.push(Step { task: Task::Remove(part), label: step_label(f, part), skip });
    }
    out
}

/// Whether this choice takes something of yours (asks once more first).
pub fn asks_first(f: &Facts, chosen: &HashSet<Part>) -> bool {
    steps(f, chosen).iter().any(|s| s.skip.is_none() && matches!(s.task, Task::Remove(p) if p.asks_first()))
}

/// What the confirmation lists, the worst first.
pub fn warnings(f: &Facts, chosen: &HashSet<Part>) -> Vec<String> {
    let running: Vec<Part> = steps(f, chosen)
        .iter()
        .filter(|s| s.skip.is_none())
        .filter_map(|s| match s.task {
            Task::Remove(p) => Some(p),
            Task::CloseMonaged => None,
        })
        .collect();
    let worst_first = [Part::Distro, Part::Data, Part::Worktrees, Part::Playground, Part::Credentials, Part::ClaudeLogin];
    worst_first
        .into_iter()
        .filter(|p| running.contains(p))
        .filter_map(|p| {
            Some(match p {
                Part::Distro => {
                    let r = f.removable_distro()?;
                    let size = r.disk_bytes.map(|b| format!(" ({})", size_text(b))).unwrap_or_default();
                    format!("{}{size} and everything in it: your Linux files, code you haven't pushed anywhere and the programs you installed.", r.name)
                }
                Part::Data => "Monaged's projects list, agent conversations, settings and saved environment values. Reinstalling starts from scratch.".into(),
                Part::Worktrees => "Agents' working folders: changes they didn't commit are lost. Their branches stay in your repos.".into(),
                Part::Playground => "The playground project, with anything you changed in it.".into(),
                Part::Credentials => format!("{} saved by Monaged.", capitalize(&sign_ins_text(&f.sign_ins()))),
                Part::ClaudeLogin => "Claude's sign-in in WSL: Claude Code asks you to sign in again.".into(),
                _ => return None,
            })
        })
        .collect()
}

// ---- Path safety -----------------------------------------------------------------------------

/// `<parent>/.monaged/<repo>` for a repo in WSL, where agents' worktrees go
/// (see `git::worktree_path`). None for a path that isn't plainly a folder
/// in a folder: relative, `..`, line breaks, or straight under `/`.
pub fn worktree_root(repo: &str) -> Option<String> {
    let repo = repo.trim().trim_end_matches('/');
    if repo.contains(['\n', '\r', '\0']) || !(repo.starts_with('/') || repo.starts_with("~/")) {
        return None;
    }
    if repo.split('/').any(|part| part == "..") {
        return None;
    }
    let (parent, name) = repo.rsplit_once('/')?;
    if parent.is_empty() || name.is_empty() || name == "." || name == ".monaged" {
        return None;
    }
    Some(format!("{parent}/.monaged/{name}"))
}

/// An absolute folder named Monaged right inside `parent`: the only shape of
/// folder the uninstaller deletes whole.
pub fn safe_folder(path: &Path, parent: &Path) -> bool {
    path.is_absolute()
        && parent.is_absolute()
        && parent.components().count() >= 3
        && path.parent() == Some(parent)
        && path.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("Monaged"))
}

/// `…\Programs\Monaged`, absolute.
pub fn program_folder_ok(dir: &Path) -> bool {
    dir.parent().is_some_and(|parent| safe_folder(dir, parent) && parent.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("Programs")))
}

fn env_folder(var: &str) -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os(var)?);
    let path = base.join("Monaged");
    safe_folder(&path, &base).then_some(path)
}

/// %APPDATA%\Monaged: the database, backups and logs.
pub fn data_root() -> Option<PathBuf> {
    env_folder("APPDATA")
}

/// %LOCALAPPDATA%\Monaged: downloads, the WSL install log, imported distros.
pub fn local_root() -> Option<PathBuf> {
    env_folder("LOCALAPPDATA")
}

fn components(path: &Path) -> Vec<String> {
    path.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect()
}

/// `inner` is `outer` or inside it (Windows paths: case doesn't count).
fn is_within(inner: &Path, outer: &Path) -> bool {
    let (inner, outer) = (components(inner), components(outer));
    !outer.is_empty() && inner.len() >= outer.len() && inner[..outer.len()] == outer[..]
}

/// Removing `path` would take the disk of a distro WSL still has.
pub fn protected(path: &Path, registered: &[Registered]) -> bool {
    registered.iter().filter_map(|r| r.base.as_deref()).any(|base| is_within(base, path))
}

/// What may go from %LOCALAPPDATA%\Monaged (`children`, and `wsl_children`
/// in its wsl folder): everything but the folder of a distro WSL still has,
/// by name or by where its disk is. (to remove, to keep)
pub fn local_plan(root: &Path, children: &[String], wsl_children: &[String], registered: &[Registered]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut remove, mut keep) = (Vec::new(), Vec::new());
    for child in children {
        if child.eq_ignore_ascii_case("wsl") {
            for name in wsl_children {
                let path = root.join(child).join(name);
                if registered.iter().any(|r| same_name(&r.name, name)) || protected(&path, registered) {
                    keep.push(path);
                } else {
                    remove.push(path);
                }
            }
            continue;
        }
        let path = root.join(child);
        if protected(&path, registered) {
            keep.push(path);
        } else {
            remove.push(path);
        }
    }
    (remove, keep)
}

fn names_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
}

/// A registry BasePath as a plain path (`\\?\` and %VARIABLES% resolved).
pub fn clean_base(raw: &str) -> PathBuf {
    let mut text = raw.trim().trim_start_matches(r"\\?\").to_string();
    for _ in 0..4 {
        let Some(start) = text.find('%') else { break };
        let Some(len) = text[start + 1..].find('%') else { break };
        let Ok(value) = std::env::var(&text[start + 1..start + 1 + len]) else { break };
        text.replace_range(start..start + len + 2, &value);
    }
    PathBuf::from(text)
}

// ---- WSL scripts (they go over stdin; see `wsl::run`) ----------------------------------------

/// Takes Monaged out of git's config: every credential helper that calls
/// `monaged.exe … --git-credential` (with the empty value Monaged writes
/// before it), and a host's section once nothing else is left in it. The
/// user's own helpers (Git Credential Manager) stay.
pub fn helper_script() -> String {
    r#"n=0
for key in $(git config --global --get-regexp '^credential\..*helper$' 2>/dev/null | grep -e ' --git-credential$' | grep -i monaged | cut -d' ' -f1 | sort -u); do
  if [ "$key" = credential.helper ]; then
    git config --global --unset-all credential.helper ' --git-credential$' 2>/dev/null && n=$((n+1))
    continue
  fi
  git config --global --unset-all "$key" 2>/dev/null && n=$((n+1))
  section=${key%.helper}
  left=0
  while read -r name; do
    case "$name" in "$section".*) left=1 ;; esac
  done <<MONAGED_KEYS
$(git config --global --name-only --list 2>/dev/null)
MONAGED_KEYS
  [ "$left" = 0 ] && git config --global --remove-section "$section" 2>/dev/null
done
echo "MONAGED_HELPER_REMOVED $n"
"#
    .to_string()
}

/// Removes these paths (`~` allowed), then `tidy` folders if they're empty.
/// Says MONAGED_REMOVED, MONAGED_ABSENT or MONAGED_LEFT (something stayed).
fn remove_script(paths: &[&str], tidy: &[&str]) -> String {
    let list = |items: &[&str]| items.iter().map(|p| wsl::shell_path(p)).collect::<Vec<_>>().join(" ");
    let mut s = format!(
        "left=0; removed=0\n\
         for d in {}; do\n\
         \x20 if [ -e \"$d\" ] || [ -L \"$d\" ]; then rm -rf -- \"$d\"; if [ -e \"$d\" ]; then left=1; else removed=1; fi; fi\n\
         done\n",
        list(paths)
    );
    if !tidy.is_empty() {
        s.push_str(&format!("rmdir -- {} 2>/dev/null\n", list(tidy)));
    }
    s.push_str("if [ $left = 1 ]; then echo MONAGED_LEFT; elif [ $removed = 1 ]; then echo MONAGED_REMOVED; else echo MONAGED_ABSENT; fi\n");
    s
}

/// The Claude agent Monaged downloaded (AGENT_HOME, all versions).
pub fn agent_script() -> String {
    remove_script(&[AGENT_HOME], &[])
}

/// The playground, its agents' folders and its local remote.
pub fn playground_script() -> String {
    let worktrees = worktree_root(crate::playground::DIR).unwrap_or_default();
    remove_script(&[&worktrees, crate::playground::DIR, PLAYGROUND_REMOTE], &["~/code/.remotes", "~/code/.monaged"])
}

/// Signs the Claude CLI out (its saved sign-in only).
pub fn claude_login_script() -> String {
    remove_script(&[CLAUDE_LOGIN], &[])
}

/// Removes agents' folders next to each repo, then lets git forget them
/// (`git worktree prune`). A root that doesn't end in `/.monaged/<name>` is
/// never touched. Says MONAGED_WORKTREES <removed> <failed>.
pub fn worktrees_script(roots: &[WorktreeRoot]) -> String {
    let mut s = String::from("removed=0; failed=0\n");
    for r in roots {
        s.push_str(&format!(
            "w={w}; r={repo}\n\
             case \"$w\" in\n\
             \x20 */.monaged/?*)\n\
             \x20   if [ -e \"$w\" ] || [ -L \"$w\" ]; then rm -rf -- \"$w\"; if [ -e \"$w\" ]; then failed=$((failed+1)); else removed=$((removed+1)); fi; fi\n\
             \x20   [ -d \"$r\" ] && git -C \"$r\" worktree prune 2>/dev/null\n\
             \x20   rmdir -- \"${{w%/*}}\" 2>/dev/null\n\
             \x20   ;;\n\
             esac\n",
            w = wsl::shell_path(&r.root),
            repo = wsl::shell_path(&r.repo),
        ));
    }
    s.push_str("echo \"MONAGED_WORKTREES $removed $failed\"\n");
    s
}

/// What a distro holds of Monaged's, with sizes. `roots` are its projects'
/// agent folder roots, reported by index.
pub fn probe_script(roots: &[String]) -> String {
    let mut s = format!(
        r#"kb() {{ du -sk -- "$1" 2>/dev/null | cut -f1; }}
echo "MONAGED_HELPER $(git config --global --get-regexp '^credential\..*helper$' 2>/dev/null | grep -e ' --git-credential$' | grep -ci monaged)"
a={agent}; [ -d "$a" ] && echo "MONAGED_AGENT $(kb "$a")"
p={playground}; [ -d "$p" ] && echo "MONAGED_PLAYGROUND $(kb "$p")"
[ -f {login} ] && echo MONAGED_CLAUDE
"#,
        agent = wsl::shell_path(AGENT_HOME),
        playground = wsl::shell_path(crate::playground::DIR),
        login = wsl::shell_path(CLAUDE_LOGIN),
    );
    for (i, root) in roots.iter().enumerate() {
        s.push_str(&format!("w={}; [ -d \"$w\" ] && echo \"MONAGED_WT {i} $(kb \"$w\")\"\n", wsl::shell_path(root)));
    }
    s.push_str("echo MONAGED_PROBED\n");
    s
}

/// The probe's answer; None when it didn't get to the end.
pub fn parse_probe(out: &str, roots: &[String]) -> Option<Probe> {
    let mut probe = Probe::default();
    let mut finished = false;
    for line in out.lines() {
        let mut words = line.split_whitespace();
        let number = |w: Option<&str>| w.and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        match words.next() {
            Some("MONAGED_HELPER") => probe.helper = number(words.next()) as usize,
            Some("MONAGED_AGENT") => probe.agent_kb = Some(number(words.next())),
            Some("MONAGED_PLAYGROUND") => probe.playground_kb = Some(number(words.next())),
            Some("MONAGED_CLAUDE") => probe.claude_login = true,
            Some("MONAGED_WT") => {
                let index = words.next().and_then(|i| i.parse::<usize>().ok());
                if let Some(root) = index.and_then(|i| roots.get(i)) {
                    probe.worktrees.insert(root.clone(), number(words.next()));
                }
            }
            Some("MONAGED_PROBED") => finished = true,
            _ => {}
        }
    }
    finished.then_some(probe)
}

/// `reg query …\Lxss /s`: each distro's name and BasePath.
pub fn parse_lxss(text: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let (mut name, mut base): (Option<String>, Option<String>) = (None, None);
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            if let Some(n) = name.take() {
                found.push((n, base.take()));
            }
            base = None;
            continue;
        }
        let mut parts = line.trim().splitn(3, "    ");
        let (Some(key), Some(kind), Some(value)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if !kind.starts_with("REG_") {
            continue;
        }
        match key {
            "DistributionName" => name = Some(value.trim().to_string()),
            "BasePath" => base = Some(value.trim().to_string()),
            _ => {}
        }
    }
    if let Some(n) = name {
        found.push((n, base));
    }
    found
}

/// A Credential Manager target that is one of Monaged's entries, as the
/// entry's name (keyring stores `<name>.<service>`).
pub fn monaged_entry(target: &str) -> Option<String> {
    let name = target.strip_suffix(".monaged")?;
    let ours = name == API_KEY || name == VAULT_KEY || GIT_ENTRIES.iter().any(|p| name.len() > p.len() && name.starts_with(p));
    ours.then(|| name.to_string())
}

// ---- Words -----------------------------------------------------------------------------------

/// "a", "a and b", "a, b and c".
pub fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "your Anthropic API key and 2 git sign-ins".
fn sign_ins_text(names: &[String]) -> String {
    let mut parts = Vec::new();
    if names.iter().any(|n| n == API_KEY) {
        parts.push("your Anthropic API key".to_string());
    }
    let tokens = names.iter().filter(|n| n.starts_with("git-token:")).count();
    if tokens > 0 {
        parts.push(count(tokens, "git sign-in"));
    }
    if parts.is_empty() { "Monaged's git account details".into() } else { and_list(&parts) }
}

/// "34 KB", "220 MB", "1.4 GB".
pub fn size_text(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    let b = bytes as f64;
    if b < MB {
        format!("{} KB", (bytes / 1000).max(1))
    } else if b < 1000.0 * MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{:.1} GB", b / (1000.0 * MB))
    }
}

// ---- Looking (reads only) --------------------------------------------------------------------

fn folder_bytes(path: &Path, skip: &dyn Fn(&Path) -> bool) -> u64 {
    let (mut total, mut seen) = (0u64, 0usize);
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            seen += 1;
            if seen > 100_000 {
                return total;
            }
            let p = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() || skip(&p) {
                continue;
            }
            if kind.is_dir() {
                stack.push(p);
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

fn folder(path: Option<PathBuf>, skip: &dyn Fn(&Path) -> bool) -> Option<Folder> {
    let path = path.filter(|p| p.is_dir())?;
    Some(Folder { bytes: folder_bytes(&path, skip), path })
}

/// Distros WSL has for this user, from the registry (no wsl.exe).
pub fn registered_distros() -> Vec<Registered> {
    let mut command = std::process::Command::new("reg.exe");
    command.args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss", "/s"]).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(wsl::CREATE_NO_WINDOW);
    }
    let Ok(out) = command.output() else { return Vec::new() };
    parse_lxss(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .map(|(name, base)| {
            let base = base.map(|b| clean_base(&b));
            let disk_bytes = base.as_ref().and_then(|b| std::fs::metadata(b.join("ext4.vhdx")).ok()).map(|m| m.len());
            Registered { name, base, disk_bytes }
        })
        .collect()
}

fn read_db() -> Db {
    let Some(path) = data_root().map(|d| d.join("monaged.db")).filter(|p| p.is_file()) else { return Db::default() };
    let mut db = Db { found: true, ..Db::default() };
    let Ok(conn) = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return db };
    let setting = |key: &str| {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get::<_, String>(0))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    db.distro = setting("wsl:distro");
    db.installed_distro = setting(INSTALLED_DISTRO_SETTING);
    db.theme = setting("theme");
    db.accounts = setting("git:accounts").map(|text| parse_accounts(&text)).unwrap_or_default();
    if let Ok(mut stmt) = conn.prepare("SELECT name, distro, path FROM projects")
        && let Ok(rows) = stmt.query_map([], |r| Ok(ProjectRef { name: r.get(0)?, distro: r.get(1)?, path: r.get(2)? }))
    {
        db.projects = rows.flatten().collect();
    }
    db
}

/// `git:accounts` as (host, login) pairs.
fn parse_accounts(text: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    list.iter().filter_map(|a| Some((a["host"].as_str()?.to_string(), a["login"].as_str()?.to_string()))).collect()
}

/// Every generic credential's target name in Credential Manager.
#[cfg(windows)]
fn credential_targets() -> Vec<String> {
    use std::ffi::c_void;
    // The start of CREDENTIALW; only these fields are read, through Windows' own buffer.
    #[repr(C)]
    #[allow(dead_code)]
    struct CredentialStart {
        flags: u32,
        kind: u32,
        target_name: *const u16,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredEnumerateW(filter: *const u16, flags: u32, count: *mut u32, credentials: *mut *mut *mut CredentialStart) -> i32;
        fn CredFree(buffer: *mut c_void);
    }
    const CRED_TYPE_GENERIC: u32 = 1;
    let mut count = 0u32;
    let mut list: *mut *mut CredentialStart = std::ptr::null_mut();
    // SAFETY: Windows allocates `list` (count pointers to credentials, in one
    // block) and CredFree releases it; nothing is kept past that. The records
    // in that block needn't be aligned (Wine packs them on 4 bytes), so every
    // read is unaligned.
    unsafe {
        if CredEnumerateW(std::ptr::null(), 0, &mut count, &mut list) == 0 || list.is_null() {
            return Vec::new();
        }
        let mut names = Vec::new();
        for i in 0..count as usize {
            let cred = list.add(i).read_unaligned();
            if cred.is_null() {
                continue;
            }
            let start = cred.read_unaligned();
            if start.kind != CRED_TYPE_GENERIC || start.target_name.is_null() {
                continue;
            }
            let (mut units, mut at) = (Vec::new(), start.target_name);
            while units.len() < 32_768 {
                let unit = at.read_unaligned();
                if unit == 0 {
                    break;
                }
                units.push(unit);
                at = at.add(1);
            }
            names.push(String::from_utf16_lossy(&units));
        }
        CredFree(list.cast());
        names
    }
}

#[cfg(not(windows))]
fn credential_targets() -> Vec<String> {
    Vec::new()
}

/// Monaged's entries in Credential Manager. Looks through all of them; if
/// that finds none, checks the names Monaged would have used.
fn credential_names(db: &Db) -> Vec<String> {
    let mut names: Vec<String> = credential_targets().iter().filter_map(|t| monaged_entry(t)).collect();
    if names.is_empty() {
        let mut known = vec![API_KEY.to_string(), VAULT_KEY.to_string()];
        for (host, login) in &db.accounts {
            known.push(format!("git-token:{host}/{login}"));
            known.push(format!("git-default:{host}"));
            known.push(format!("git-logins:{host}"));
        }
        names = known.into_iter().filter(|n| keyring::Entry::new(SERVICE, n).is_ok_and(|e| e.get_password().is_ok())).collect();
    }
    names.sort();
    names.dedup();
    names
}

/// Reads everything the plan needs, without changing anything. Doesn't run
/// wsl.exe (see `probe_all`).
pub fn gather() -> Facts {
    let registered = registered_distros();
    let db = read_db();
    let program = folder(Some(install::install_dir()), &|_| false);
    let data = folder(data_root(), &|_| false);
    // Its size leaves out what WSL still uses, and the setup leftovers that go with the program.
    let local = local_root().and_then(|root| {
        let wsl_dir = root.join("wsl");
        let (downloads, log) = (root.join("downloads"), root.join("wsl-install.log"));
        let skip = |p: &Path| {
            p == downloads
                || p == log
                || protected(p, &registered)
                || (p.parent() == Some(wsl_dir.as_path()) && p.file_name().is_some_and(|n| registered.iter().any(|r| same_name(&r.name, &n.to_string_lossy()))))
        };
        folder(Some(root.clone()), &skip)
    });
    let credentials = credential_names(&db);
    Facts {
        program,
        data,
        local,
        credentials,
        wsl: wsl::installed(),
        registered,
        db,
        probes: HashMap::new(),
        probed: false,
        running: instance::running(),
    }
}

/// Runs `f` on its own thread and gives up after `secs` (a hung WSL mustn't
/// hang the uninstaller; the thread ends on its own).
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(Duration::from_secs(secs)).unwrap_or_else(|_| Err(anyhow!("WSL didn't answer within {secs} seconds")))
}

/// A script in a distro, with a time limit.
fn in_wsl(distro: &str, script: String, secs: u64) -> Result<String> {
    if !wsl::installed() {
        bail!("WSL isn't installed on this PC any more");
    }
    let distro = distro.to_string();
    within(secs, move || wsl::run(&distro, &script))
}

/// Asks each distro Monaged used what it holds of Monaged's.
pub fn probe_all(f: &Facts) -> HashMap<String, Probe> {
    let mut out = HashMap::new();
    if !f.wsl {
        return out;
    }
    let roots = f.all_worktree_roots();
    for distro in f.used_distros().into_iter().chain(f.other_distros()) {
        let mine: Vec<String> = roots.iter().filter(|w| w.distro == distro).map(|w| w.root.clone()).collect();
        if let Ok(text) = in_wsl(&distro, probe_script(&mine), 60)
            && let Some(probe) = parse_probe(&text, &mine)
        {
            out.insert(distro, probe);
        }
    }
    out
}

// ---- Removing ----------------------------------------------------------------------------------

/// The text after `marker` on the line that starts with it.
fn marker<'a>(out: &'a str, name: &str) -> Option<&'a str> {
    out.lines().find_map(|l| l.trim().strip_prefix(name)).map(str::trim)
}

/// Runs a script in each distro and says what happened in each; fails
/// naming the distros where it didn't work (the others are still done).
fn in_each(distros: &[String], secs: u64, script: &str, says: impl Fn(&str, &str) -> Result<String>) -> Result<String> {
    let (mut done, mut failed) = (Vec::new(), Vec::new());
    for distro in distros {
        match in_wsl(distro, script.to_string(), secs).and_then(|out| says(distro, &out)) {
            Ok(text) => done.push(text),
            Err(err) => failed.push(format!("{distro}: {err:#}")),
        }
    }
    if failed.is_empty() {
        Ok(done.join(" · "))
    } else if done.is_empty() {
        bail!("{}", failed.join(" · "))
    } else {
        bail!("{} (done: {})", failed.join(" · "), done.join(" · "))
    }
}

/// For `remove_script`'s answer.
fn removed_or_absent(out: &str, distro: &str) -> Result<String> {
    if out.lines().any(|l| l.trim() == "MONAGED_LEFT") {
        bail!("some files couldn't be removed");
    }
    if out.lines().any(|l| l.trim() == "MONAGED_REMOVED") {
        Ok(format!("Removed from {distro}"))
    } else if out.lines().any(|l| l.trim() == "MONAGED_ABSENT") {
        Ok(format!("Already gone from {distro}"))
    } else {
        bail!("WSL stopped before it was done")
    }
}

fn delete_credential(name: &str) -> Result<()> {
    match keyring::Entry::new(SERVICE, name).context("Windows Credential Manager isn't available")?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err).context("Credential Manager didn't remove it"),
    }
}

/// Closes the open Monaged window (agents stop with it) and waits for it.
fn close_monaged() -> Result<String> {
    if !instance::running() {
        return Ok("It was already closed".into());
    }
    instance::close_running();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(300));
        if !instance::running() {
            // A moment for its last writes.
            std::thread::sleep(Duration::from_millis(500));
            return Ok("Closed".into());
        }
    }
    bail!("Monaged is still open. Close it yourself, then try again.")
}

fn remove_distro(f: &Facts) -> Result<String> {
    let r = f.removable_distro().context("WSL doesn't have it any more")?;
    if !wsl::installed() {
        bail!("WSL isn't installed on this PC any more");
    }
    let name = r.name.clone();
    let _ = crate::distro::wsl_run(&["--terminate", &name], Duration::from_secs(60));
    let (_, said) = crate::distro::wsl_run(&["--unregister", &name], Duration::from_secs(10 * 60))?;
    let now = registered_distros();
    if now.iter().any(|x| same_name(&x.name, &name)) {
        let said = said.lines().map(str::trim).filter(|l| !l.is_empty()).take(2).collect::<Vec<_>>().join(" ");
        bail!("WSL didn't remove {name}{}", if said.is_empty() { String::new() } else { format!(": {said}") });
    }
    // A distro the older way imported leaves its (now empty) folder in
    // Monaged's; it can go now that WSL has let go of it.
    if let Some(local) = local_root() {
        let folder = local.join("wsl").join(&name);
        if folder.exists() && !protected(&folder, &now) {
            std::fs::remove_dir_all(&folder).with_context(|| format!("{name} is gone from WSL, but {} couldn't be removed", folder.display()))?;
            let _ = std::fs::remove_dir(local.join("wsl"));
            return Ok(format!("{name} and its folder are gone from WSL"));
        }
    }
    Ok(format!("{name} is gone from WSL"))
}

fn remove_sign_ins(f: &Facts) -> Result<String> {
    let names = f.sign_ins();
    let failed: Vec<String> = names.iter().filter_map(|name| delete_credential(name).err().map(|e| format!("{name}: {e:#}"))).collect();
    if !failed.is_empty() {
        bail!("{}", failed.join(" · "));
    }
    Ok(format!("Removed {} from Windows Credential Manager", sign_ins_text(&names)))
}

fn remove_data(f: &Facts) -> Result<String> {
    if instance::running() {
        bail!("Monaged is open again. Close it, then try again.");
    }
    let (mut removed, mut failed, mut kept) = (Vec::<String>::new(), Vec::<String>::new(), Vec::<PathBuf>::new());
    let mut data_gone = true;
    if let Some(dir) = data_root()
        && dir.exists()
    {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => removed.push("%APPDATA%\\Monaged".into()),
            Err(err) => {
                // Whatever is left (the database, its backups) still needs the key below.
                data_gone = !dir.exists();
                failed.push(format!("{}: {err}", dir.display()));
            }
        }
    }
    // The key that reads saved environment values goes only with them.
    if data_gone && f.credentials.iter().any(|c| c == VAULT_KEY) {
        match delete_credential(VAULT_KEY) {
            Ok(()) => removed.push("the key for saved environment values".into()),
            Err(err) => failed.push(format!("{VAULT_KEY}: {err:#}")),
        }
    }
    if let Some(root) = local_root()
        && root.is_dir()
    {
        let registered = registered_distros();
        let (remove, keep) = local_plan(&root, &names_in(&root), &names_in(&root.join("wsl")), &registered);
        let mut all = true;
        for path in remove {
            let result = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            if let Err(err) = result {
                all = false;
                failed.push(format!("{}: {err}", path.display()));
            }
        }
        let _ = std::fs::remove_dir(root.join("wsl"));
        let _ = std::fs::remove_dir(&root);
        if all {
            removed.push("%LOCALAPPDATA%\\Monaged".into());
        }
        kept = keep;
    }
    if !failed.is_empty() {
        bail!("{}", failed.join(" · "));
    }
    let mut text = if removed.is_empty() { "Nothing was left to remove".to_string() } else { format!("Removed {}", and_list(&removed)) };
    if !kept.is_empty() {
        let kept: Vec<String> = kept.iter().map(|p| p.display().to_string()).collect();
        text.push_str(&format!(". Kept {}: WSL still uses it", and_list(&kept)));
    }
    Ok(text)
}

/// Does one step; says what it removed.
pub fn run_step(task: Task, f: &Facts, chosen: &HashSet<Part>) -> Result<String> {
    let part = match task {
        Task::CloseMonaged => return close_monaged(),
        Task::Remove(part) => part,
    };
    let here = f.staying(part, chosen);
    match part {
        Part::GitHelper => in_each(&here, 90, &helper_script(), |distro, out| {
            let n: usize = marker(out, "MONAGED_HELPER_REMOVED").and_then(|n| n.parse().ok()).context("WSL stopped before it was done")?;
            Ok(if n > 0 { format!("Git in {distro} no longer asks Monaged") } else { format!("Git in {distro} didn't use Monaged") })
        }),
        Part::ClaudeLogin => in_each(&here, 60, &claude_login_script(), |distro, out| {
            removed_or_absent(out, distro).map(|t| if t.starts_with("Removed") { format!("Signed out in {distro}") } else { format!("{distro} wasn't signed in") })
        }),
        Part::Agent => in_each(&here, 120, &agent_script(), |distro, out| removed_or_absent(out, distro)),
        Part::Playground => in_each(&here, 120, &playground_script(), |distro, out| removed_or_absent(out, distro)),
        Part::Worktrees => {
            let roots = f.staying_roots(chosen);
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for distro in &here {
                let mine: Vec<WorktreeRoot> = roots.iter().filter(|w| &w.distro == distro).cloned().collect();
                let result = in_wsl(distro, worktrees_script(&mine), 300).and_then(|out| {
                    let numbers: Vec<usize> = marker(&out, "MONAGED_WORKTREES").context("WSL stopped before it was done")?.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                    let (removed, broken) = (numbers.first().copied().unwrap_or(0), numbers.get(1).copied().unwrap_or(0));
                    if broken > 0 {
                        bail!("the agents' folders of {} couldn't be removed completely", count(broken, "project"));
                    }
                    Ok(if removed > 0 { format!("Removed the agents' folders of {} in {distro}", count(removed, "project")) } else { format!("Already gone from {distro}") })
                });
                match result {
                    Ok(text) => done.push(text),
                    Err(err) => failed.push(format!("{distro}: {err:#}")),
                }
            }
            if failed.is_empty() { Ok(done.join(" · ")) } else { bail!("{}", failed.join(" · ")) }
        }
        Part::Distro => remove_distro(f),
        Part::Credentials => remove_sign_ins(f),
        Part::Data => remove_data(f),
        Part::Program => install::remove_program(),
    }
}

/// What the work reports, in order.
#[derive(Clone, Debug)]
pub enum Progress {
    Running(usize),
    Done(usize, String),
    Failed(usize, String),
    Skipped(usize, String),
    Finished,
}

/// Runs the steps in order, past failures, except: nothing runs while
/// Monaged stays open, and the program stays when git in WSL still points
/// at it.
pub fn run_all(steps: &[Step], f: &Facts, chosen: &HashSet<Part>, send: &dyn Fn(Progress)) {
    let mut stop: Option<&str> = None;
    let mut helper_failed = false;
    for (i, step) in steps.iter().enumerate() {
        if let Some(why) = stop {
            send(Progress::Skipped(i, why.into()));
            continue;
        }
        if let Some(why) = &step.skip {
            send(Progress::Skipped(i, why.clone()));
            continue;
        }
        if step.task == Task::Remove(Part::Program) && helper_failed {
            send(Progress::Failed(
                i,
                "Kept: git in WSL still asks Monaged for sign-ins, and that setting couldn't be removed. Run the uninstaller again once WSL works.".into(),
            ));
            continue;
        }
        send(Progress::Running(i));
        match run_step(step.task, f, chosen) {
            Ok(text) => send(Progress::Done(i, text)),
            Err(err) => {
                send(Progress::Failed(i, format!("{err:#}")));
                match step.task {
                    Task::CloseMonaged => stop = Some("Not started: Monaged is still open"),
                    Task::Remove(Part::GitHelper) => helper_failed = true,
                    Task::Remove(_) => {}
                }
            }
        }
    }
    send(Progress::Finished);
}

/// `--uninstall --quiet [--all]`: no window. Prints each step (when there's
/// somewhere to print to) and returns the exit code: 0 when all went well.
pub fn run_quiet(all: bool) -> i32 {
    let mut facts = gather();
    facts.probes = probe_all(&facts);
    facts.probed = true;
    let chosen = if all { everything() } else { default_choice() };
    let steps = steps(&facts, &chosen);
    let failures = std::cell::Cell::new(0);
    run_all(&steps, &facts, &chosen, &|progress| match progress {
        Progress::Done(i, text) => println!("{}: {text}", steps[i].label),
        Progress::Skipped(i, text) => println!("{}: skipped ({text})", steps[i].label),
        Progress::Failed(i, text) => {
            failures.set(failures.get() + 1);
            eprintln!("{}: failed: {text}", steps[i].label);
        }
        Progress::Running(_) | Progress::Finished => {}
    });
    if failures.get() == 0 { 0 } else { 1 }
}

// ---- The window --------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Looking,
    Choose,
    Confirm,
    Working,
    Finished,
}

pub struct UninstallView {
    screen: Screen,
    facts: Facts,
    chosen: HashSet<Part>,
    steps: Vec<Step>,
    views: Vec<StepView>,
    /// Bumped on every look, so an old answer doesn't land on new facts.
    generation: u64,
}

impl UninstallView {
    /// `all`: everything ticked (`--all`).
    pub fn new(all: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let weak = cx.weak_entity();
        // Closing half-way would leave things half removed.
        window.on_window_should_close(cx, move |_, cx| weak.upgrade().is_none_or(|view| view.read(cx).screen != Screen::Working));
        let mut view = Self { screen: Screen::Looking, facts: Facts::default(), chosen: if all { everything() } else { default_choice() }, steps: Vec::new(), views: Vec::new(), generation: 0 };
        view.look(cx);
        view.watch_monaged(cx);
        view
    }

    /// Reads what's there, then asks WSL in the background.
    fn look(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Looking;
        self.generation += 1;
        let generation = self.generation;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let facts = cx.background_spawn(async { gather() }).await;
            let for_probe = facts.clone();
            let current = view
                .update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    this.facts = facts;
                    this.screen = Screen::Choose;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let probes = cx.background_spawn(async move { probe_all(&for_probe) }).await;
            view.update(cx, |this, cx| {
                // Once removing has started, its plan stays as it was.
                if this.generation == generation && matches!(this.screen, Screen::Choose | Screen::Confirm) {
                    this.facts.probes = probes;
                    this.facts.probed = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Notices Monaged opening or closing while you choose.
    fn watch_monaged(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let running = cx.background_spawn(async { instance::running() }).await;
                let open = view
                    .update(cx, |this, cx| {
                        if matches!(this.screen, Screen::Choose | Screen::Confirm) && this.facts.running != running {
                            this.facts.running = running;
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !open {
                    break;
                }
            }
        })
        .detach();
    }

    fn toggle(&mut self, part: Part, cx: &mut Context<Self>) {
        if part.locked() || self.facts.goes_with_distro(part, &self.chosen).is_some() {
            return;
        }
        if !self.chosen.remove(&part) {
            self.chosen.insert(part);
        }
        cx.notify();
    }

    /// The parts you can tick or untick: Some(all of them are ticked), None
    /// when there's nothing to choose.
    fn all_ticked(&self) -> Option<bool> {
        let choosable: Vec<Part> = items(&self.facts).into_iter().map(|i| i.part).filter(|p| !p.locked()).collect();
        (!choosable.is_empty()).then(|| choosable.iter().all(|p| self.chosen.contains(p) || self.facts.goes_with_distro(*p, &self.chosen).is_some()))
    }

    fn go(&mut self, cx: &mut Context<Self>) {
        if asks_first(&self.facts, &self.chosen) {
            self.screen = Screen::Confirm;
            cx.notify();
        } else {
            self.start(cx);
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.facts.running = instance::running();
        self.steps = steps(&self.facts, &self.chosen);
        self.views = self
            .steps
            .iter()
            .map(|s| StepView {
                label: SharedString::from(s.label.clone()),
                detail: s.skip.clone().map(SharedString::from),
                state: if s.skip.is_some() { StepState::Skipped } else { StepState::Waiting },
                progress: None,
            })
            .collect();
        self.screen = Screen::Working;
        cx.notify();
        let (tx, mut rx) = unbounded::<Progress>();
        let (steps, facts, chosen) = (self.steps.clone(), self.facts.clone(), self.chosen.clone());
        cx.background_spawn(async move {
            run_all(&steps, &facts, &chosen, &|p| {
                let _ = tx.unbounded_send(p);
            });
        })
        .detach();
        cx.spawn(async move |view, cx| {
            while let Some(progress) = rx.next().await {
                if view.update(cx, |this, cx| this.on_progress(progress, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_progress(&mut self, progress: Progress, cx: &mut Context<Self>) {
        let (i, state, detail) = match progress {
            Progress::Running(i) => (i, StepState::Running, None),
            Progress::Done(i, text) => (i, StepState::Done, Some(text)),
            Progress::Failed(i, text) => (i, StepState::Failed, Some(text)),
            Progress::Skipped(i, text) => (i, StepState::Skipped, Some(text)),
            Progress::Finished => return self.finish(cx),
        };
        if let Some(view) = self.views.get_mut(i) {
            view.state = state;
            view.progress = (state == StepState::Running).then_some(None);
            if let Some(detail) = detail {
                view.detail = Some(SharedString::from(detail));
            }
        }
        cx.notify();
    }

    /// The result screen. When everything went, the window closes by itself
    /// after a minute: the program folder can only go once it has.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Finished;
        cx.notify();
        if self.failures() > 0 {
            return;
        }
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(60)).await;
            if view.update(cx, |this, _| this.screen == Screen::Finished).unwrap_or(false) {
                cx.update(|cx| cx.quit()).ok();
            }
        })
        .detach();
    }

    fn failures(&self) -> usize {
        self.views.iter().filter(|v| v.state == StepState::Failed).count()
    }

    /// Nothing was done because Monaged stayed open.
    fn stopped(&self) -> bool {
        self.steps.first().is_some_and(|s| s.task == Task::CloseMonaged) && self.views.first().is_some_and(|v| v.state == StepState::Failed)
    }

    fn apply_theme(&self, window: &Window) {
        let Some(key) = self.facts.db.theme.as_deref() else { return };
        let key = if key == "system" {
            match window.appearance() {
                gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight => "paper",
                _ => "carbon",
            }
        } else {
            key
        };
        theme::set(key);
    }

    fn render_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = items(&self.facts);
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .when(self.facts.running, |d| {
                d.child(
                    ui::callout(Tone::Warn, "Monaged is open. It closes when you click Uninstall, and agents that are working stop.").child(
                        div().flex_none().flex().child(ui::button("uninstall-close-now", Kind::Secondary, None, "Close it now").on_click(cx.listener(|_, _, _, cx| {
                            cx.background_spawn(async {
                                instance::close_running();
                            })
                            .detach();
                        }))),
                    ),
                )
            })
            .children(list.iter().enumerate().map(|(i, item)| self.render_item(i, item, cx)))
            .into_any_element()
    }

    fn render_item(&self, i: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let part = item.part;
        let with_distro = self.facts.goes_with_distro(part, &self.chosen);
        let fixed = part.locked() || with_distro.is_some();
        let on = fixed || self.chosen.contains(&part);
        let note: Option<String> = if part.locked() { Some("Always removed".into()) } else { with_distro.map(|name| format!("Goes with {name}")) };
        let size: Option<String> = if item.checking { Some("Checking…".into()) } else { item.bytes.filter(|b| *b > 0).map(size_text) };
        let danger = part == Part::Distro && on;
        div()
            .id(("uninstall-part", i))
            .flex()
            .items_start()
            .gap(px(12.))
            .when(!fixed, |d| d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle(part, cx))))
            .child(check_box(on, fixed))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(if danger { theme::red_text() } else { theme::text() }))
                                    .child(item.title.clone()),
                            )
                            .children(note.map(|n| div().flex_none().text_size(px(11.5)).text_color(rgb(theme::text_3())).child(n)))
                            .children(size.map(|s| div().flex_none().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(s))),
                    )
                    .child(div().text_size(px(12.)).line_height(px(18.)).text_color(rgb(theme::text_3())).child(item.detail.clone())),
            )
            .into_any_element()
    }

    fn render_confirm(&self) -> AnyElement {
        let lines = warnings(&self.facts, &self.chosen);
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .children(lines.into_iter().map(|line| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .child(div().flex_none().pt(px(2.)).child(icon_sized(Icon::Alert, 14.).text_color(rgb(theme::red_text()))))
                    .child(div().flex_1().min_w(px(0.)).line_height(px(19.)).child(line))
            }))
            .child(div().text_size(px(12.)).line_height(px(18.)).text_color(rgb(theme::text_3())).child("WSL itself, other Linux distributions and apps like Zed stay."))
            .into_any_element()
    }

    fn render_result(&self) -> AnyElement {
        let kept: Vec<String> = items(&self.facts)
            .into_iter()
            .filter(|i| !i.part.locked() && !self.chosen.contains(&i.part) && self.facts.goes_with_distro(i.part, &self.chosen).is_none())
            .map(|i| kept_name(&self.facts, i.part))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(ui::steps("uninstall-steps", &self.views))
            .when(self.screen == Screen::Finished && !kept.is_empty() && !self.stopped(), |d| {
                d.child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(rgb(theme::text_3()))
                        .child(format!("Kept on this PC: {}. WSL itself and your other Linux distributions weren't touched.", and_list(&kept))),
                )
            })
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let row = div().flex().items_center().gap(px(8.));
        match self.screen {
            Screen::Looking => row.child(div().flex_1()).child(ui::button("uninstall-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit()))),
            Screen::Choose => row
                .when_some(self.all_ticked(), |d, all| {
                    d.child(ui::button("uninstall-everything", Kind::Secondary, None, if all { "Back to the defaults" } else { "Remove everything" }).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.chosen = if all { default_choice() } else { everything() };
                            cx.notify();
                        }),
                    ))
                })
                .child(div().flex_1())
                .child(ui::button("uninstall-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|_, _, _, cx| cx.quit())))
                .child(ui::button("uninstall-go", Kind::Danger, None, "Uninstall").on_click(cx.listener(|this, _, _, cx| this.go(cx)))),
            Screen::Confirm => row
                .child(ui::button("uninstall-back", Kind::Ghost, None, "Back").on_click(cx.listener(|this, _, _, cx| {
                    this.screen = Screen::Choose;
                    cx.notify();
                })))
                .child(div().flex_1())
                .child(ui::button("uninstall-sure", Kind::Danger, Some(Icon::Trash), "Remove for good").on_click(cx.listener(|this, _, _, cx| this.start(cx)))),
            Screen::Working => row.child(div().flex_1()).child(ui::busy_button("uninstall-busy", Kind::Primary, "Removing…")),
            Screen::Finished => row
                .when(self.failures() > 0, |d| d.child(ui::button("uninstall-again", Kind::Secondary, Some(Icon::Refresh), "Try again").on_click(cx.listener(|this, _, _, cx| this.look(cx)))))
                .child(div().flex_1())
                .child(ui::button("uninstall-close", Kind::Primary, None, "Close").on_click(cx.listener(|_, _, _, cx| cx.quit()))),
        }
        .into_any_element()
    }
}

/// A part as the result screen's "Kept on this PC" names it.
fn kept_name(f: &Facts, part: Part) -> String {
    match part {
        Part::Program => "the program".into(),
        Part::GitHelper => "git's sign-in setting".into(),
        Part::Agent => "the Claude agent".into(),
        Part::Credentials => "your saved sign-ins".into(),
        Part::Data => "your projects and settings".into(),
        Part::Worktrees => "agents' working folders".into(),
        Part::Playground => "the playground".into(),
        Part::ClaudeLogin => "Claude's sign-in in WSL".into(),
        Part::Distro => f.removable_distro().map_or_else(|| "the distro".into(), |r| r.name.clone()),
    }
}

/// A tick box; `fixed` ones can't be changed.
fn check_box(on: bool, fixed: bool) -> impl IntoElement {
    div()
        .flex_none()
        .mt(px(1.))
        .size(px(16.))
        .rounded(px(4.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .border_color(rgb(if on { theme::text() } else { theme::line_strong() }))
        .when(on, |d| d.bg(rgb(theme::text())).child(icon_sized(Icon::Check, 12.).text_color(rgb(theme::bg()))))
        .when(fixed, |d| d.opacity(0.5))
}

impl Render for UninstallView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_theme(window);
        let failures = self.failures();
        let (title, intro): (String, String) = match self.screen {
            Screen::Looking => ("Uninstall Monaged".into(), "Looking at what Monaged put on this PC…".into()),
            Screen::Choose => ("Uninstall Monaged".into(), "Pick what to remove. WSL itself stays, and so do your other Linux distributions and apps like Zed.".into()),
            Screen::Confirm => ("Remove these for good?".into(), "This can't be undone.".into()),
            Screen::Working => ("Removing Monaged…".into(), "Keep this window open until it's done.".into()),
            Screen::Finished if self.stopped() => ("Monaged is still open".into(), "Nothing was removed. Close Monaged, then try again.".into()),
            Screen::Finished if failures > 0 => ("Not everything could be removed".into(), "The red lines say why. Everything else is done.".into()),
            Screen::Finished => ("Monaged is removed".into(), "Thanks for trying it. This window closes by itself in a minute.".into()),
        };
        let body: AnyElement = match self.screen {
            Screen::Looking => ui::skeleton_text("uninstall-looking", 5).into_any_element(),
            Screen::Choose => self.render_choices(cx),
            Screen::Confirm => self.render_confirm(),
            Screen::Working | Screen::Finished => self.render_result(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::bg()))
            .text_color(rgb(theme::text()))
            .font_family(theme::SANS)
            .text_size(px(13.))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(28.))
                    .pt(px(24.))
                    .pb(px(16.))
                    .child(div().flex().items_center().gap(px(14.)).child(crate::onboarding::logo_mark(36.)).child(ui::title(title)))
                    .child(div().line_height(px(20.)).text_color(rgb(theme::text_2())).child(intro)),
            )
            // The list scrolls; the buttons below always stay in reach.
            .child(
                div()
                    .id("uninstall-body")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .px(px(28.))
                    .child(div().flex_none().flex().flex_col().child(body))
                    .child(div().flex_none().h(px(16.))),
            )
            .child(div().flex_none().flex().flex_col().px(px(28.)).py(px(14.)).border_t_1().border_color(rgb(theme::line())).child(self.render_footer(cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered(name: &str, base: Option<&str>) -> Registered {
        Registered { name: name.into(), base: base.map(PathBuf::from), disk_bytes: None }
    }

    /// Monaged in "Ubuntu" (installed by the setup guide) with one project,
    /// and the user's own "Debian" with another.
    fn facts() -> Facts {
        Facts {
            program: Some(Folder { path: PathBuf::from(r"C:\Users\sam\AppData\Local\Programs\Monaged"), bytes: 30_000_000 }),
            data: Some(Folder { path: PathBuf::from(r"C:\Users\sam\AppData\Roaming\Monaged"), bytes: 2_000_000 }),
            local: None,
            db: Db {
                found: true,
                distro: Some("Ubuntu".into()),
                installed_distro: Some("ubuntu".into()),
                projects: vec![
                    ProjectRef { name: "hr-app".into(), distro: "Ubuntu".into(), path: "/home/sam/code/hr-app".into() },
                    ProjectRef { name: "tools".into(), distro: "Debian".into(), path: "/home/sam/src/tools/".into() },
                    ProjectRef { name: "Monaged playground".into(), distro: "Ubuntu".into(), path: "/home/sam/code/monaged-playground".into() },
                ],
                accounts: vec![("github.com".into(), "sam".into())],
                theme: None,
            },
            credentials: vec![API_KEY.into(), VAULT_KEY.into(), "git-token:github.com/sam".into(), "git-default:github.com".into()],
            wsl: true,
            registered: vec![
                registered("Ubuntu", Some(r"C:\Users\sam\AppData\Local\wsl\{1234}")),
                registered("Debian", Some(r"C:\Users\sam\AppData\Local\Packages\Debian\LocalState")),
                registered("Other", None),
            ],
            probes: HashMap::new(),
            probed: false,
            running: false,
        }
    }

    fn parts(items: &[Item]) -> Vec<Part> {
        items.iter().map(|i| i.part).collect()
    }

    #[test]
    fn the_plan_offers_what_is_there() {
        let f = facts();
        assert_eq!(f.used_distros(), vec!["Ubuntu".to_string(), "Debian".to_string()]);
        let list = items(&f);
        assert_eq!(parts(&list), Part::LISTED.to_vec());
        let distro = list.iter().find(|i| i.part == Part::Distro).unwrap();
        assert!(distro.title.starts_with("Ubuntu,"), "{}", distro.title);
        // Before WSL answered, its parts say they're being checked.
        assert!(list.iter().filter(|i| i.part.in_wsl()).all(|i| i.checking));
        // The sign-ins don't count the vault key.
        let sign_ins = list.iter().find(|i| i.part == Part::Credentials).unwrap();
        assert!(sign_ins.detail.starts_with("Your Anthropic API key and 1 git sign-in in"), "{}", sign_ins.detail);
    }

    #[test]
    fn only_the_distro_monaged_installed_is_ever_offered() {
        let mut f = facts();
        f.db.installed_distro = None;
        assert!(f.removable_distro().is_none());
        assert!(!parts(&items(&f)).contains(&Part::Distro));
        // Named, but WSL doesn't have it any more.
        f.db.installed_distro = Some("Ubuntu-24.04".into());
        assert!(f.removable_distro().is_none());
        // Without WSL nothing in it is offered at all.
        let mut f = facts();
        f.wsl = false;
        let list = parts(&items(&f));
        assert!(list.iter().all(|p| !p.in_wsl() && *p != Part::Distro), "{list:?}");
    }

    #[test]
    fn defaults_keep_your_things() {
        let chosen = default_choice();
        assert!(chosen.contains(&Part::Program) && chosen.contains(&Part::GitHelper) && chosen.contains(&Part::Agent));
        for part in [Part::Credentials, Part::Data, Part::Worktrees, Part::Playground, Part::ClaudeLogin, Part::Distro] {
            assert!(!chosen.contains(&part), "{part:?} is ticked by default");
        }
        assert!(Part::Program.locked() && Part::GitHelper.locked());
        assert_eq!(everything().len(), Part::LISTED.len());
        // The defaults don't ask again; the distro or the data does.
        let f = facts();
        assert!(!asks_first(&f, &default_choice()));
        assert!(asks_first(&f, &everything()));
        let mut data = default_choice();
        data.insert(Part::Data);
        assert!(asks_first(&f, &data));
    }

    #[test]
    fn steps_run_wsl_first_then_the_distro_then_sign_ins_data_and_the_program() {
        let mut f = facts();
        f.running = true;
        let order: Vec<Task> = steps(&f, &everything()).iter().map(|s| s.task).collect();
        let at = |t: Task| order.iter().position(|o| *o == t).unwrap();
        assert_eq!(order[0], Task::CloseMonaged);
        for wsl_part in [Part::GitHelper, Part::ClaudeLogin, Part::Worktrees, Part::Playground, Part::Agent] {
            assert!(at(Task::Remove(wsl_part)) < at(Task::Remove(Part::Distro)), "{wsl_part:?}");
        }
        assert!(at(Task::Remove(Part::Distro)) < at(Task::Remove(Part::Credentials)));
        assert!(at(Task::Remove(Part::Credentials)) < at(Task::Remove(Part::Data)));
        assert_eq!(*order.last().unwrap(), Task::Remove(Part::Program));
        // The program's git setting goes even when only the defaults are ticked.
        let defaults: Vec<Task> = steps(&f, &HashSet::new()).iter().map(|s| s.task).collect();
        assert!(defaults.contains(&Task::Remove(Part::GitHelper)) && defaults.contains(&Task::Remove(Part::Program)));
        assert!(!defaults.contains(&Task::Remove(Part::Data)));
    }

    #[test]
    fn wsl_work_skips_the_distro_that_goes_but_not_the_others() {
        let mut f = facts();
        let all = everything();
        assert_eq!(f.staying(Part::Agent, &all), vec!["Debian".to_string()]);
        // Git's setting goes from the distro that's going too, in case removing it fails.
        assert_eq!(f.staying(Part::GitHelper, &all), vec!["Ubuntu".to_string(), "Debian".to_string()]);
        f.db.projects.retain(|p| p.distro == "Ubuntu");
        assert_eq!(f.goes_with_distro(Part::GitHelper, &all), None);
        assert_eq!(f.goes_with_distro(Part::Agent, &all), Some("Ubuntu".into()));
        let f = facts();
        let roots = f.staying_roots(&all);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].root, "/home/sam/src/.monaged/tools");
        // The playground only lives in Ubuntu: it goes with it.
        assert_eq!(f.goes_with_distro(Part::Playground, &all), Some("Ubuntu".into()));
        let playground = steps(&f, &all).into_iter().find(|s| s.task == Task::Remove(Part::Playground)).unwrap();
        assert_eq!(playground.skip.as_deref(), Some("Goes with Ubuntu"));
        // Keeping the distro, everything runs where it is.
        let mut keep = everything();
        keep.remove(&Part::Distro);
        assert!(steps(&f, &keep).iter().all(|s| s.skip.is_none()));
        assert_eq!(f.staying(Part::Playground, &keep), vec!["Ubuntu".to_string()]);
    }

    #[test]
    fn what_wsl_says_narrows_the_plan() {
        let mut f = facts();
        let roots: Vec<String> = vec!["/home/sam/code/.monaged/hr-app".into(), "/home/sam/code/.monaged/monaged-playground".into()];
        let ubuntu = parse_probe("noise\nMONAGED_HELPER 2\nMONAGED_AGENT 215000\nMONAGED_WT 0 1200\nMONAGED_PROBED\n", &roots).unwrap();
        assert_eq!(ubuntu.helper, 2);
        assert_eq!(ubuntu.agent_kb, Some(215_000));
        assert_eq!(ubuntu.playground_kb, None);
        assert_eq!(ubuntu.worktrees.get("/home/sam/code/.monaged/hr-app"), Some(&1200));
        assert!(!ubuntu.claude_login);
        let debian = parse_probe("MONAGED_HELPER 0\nMONAGED_PROBED", &[]).unwrap();
        assert!(parse_probe("MONAGED_HELPER 1\n", &[]).is_none(), "an unfinished answer isn't trusted");
        f.probes.insert("Ubuntu".into(), ubuntu);
        f.probes.insert("Debian".into(), debian);
        f.probed = true;
        assert_eq!(f.targets(Part::GitHelper), vec!["Ubuntu".to_string()]);
        assert_eq!(f.targets(Part::Agent), vec!["Ubuntu".to_string()]);
        assert!(f.targets(Part::Playground).is_empty());
        assert!(f.targets(Part::ClaudeLogin).is_empty());
        let list = items(&f);
        assert!(!parts(&list).contains(&Part::Playground) && !parts(&list).contains(&Part::ClaudeLogin));
        let agent = list.iter().find(|i| i.part == Part::Agent).unwrap();
        assert_eq!(agent.bytes, Some(215_000 * 1024));
        assert!(!agent.checking);
        let worktrees = list.iter().find(|i| i.part == Part::Worktrees).unwrap();
        assert!(worktrees.detail.contains("1 project "), "{}", worktrees.detail);
        // A distro Monaged didn't use is only cleaned of git's setting, and only when it has it.
        assert_eq!(f.other_distros(), vec!["Other".to_string()]);
        assert_eq!(f.targets(Part::GitHelper), vec!["Ubuntu".to_string()]);
        f.probes.insert("Other".into(), Probe { helper: 1, agent_kb: Some(10), claude_login: true, ..Probe::default() });
        assert_eq!(f.targets(Part::GitHelper), vec!["Ubuntu".to_string(), "Other".to_string()]);
        assert_eq!(f.targets(Part::Agent), vec!["Ubuntu".to_string()]);
        assert!(f.targets(Part::ClaudeLogin).is_empty());
        // Container tools' distros aren't asked at all.
        f.registered.push(registered("docker-desktop", None));
        assert_eq!(f.other_distros(), vec!["Other".to_string()]);
    }

    #[test]
    fn the_confirmation_names_what_is_lost() {
        let mut f = facts();
        f.registered[0].disk_bytes = Some(4_200_000_000);
        let lines = warnings(&f, &everything());
        assert!(lines[0].starts_with("Ubuntu (4.2 GB) and everything in it"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("projects list")));
        assert!(lines.iter().any(|l| l.contains("didn't commit")));
        assert!(warnings(&f, &default_choice()).is_empty());
    }

    #[test]
    fn worktree_roots_are_only_ever_next_to_a_repo() {
        assert_eq!(worktree_root("/home/sam/code/hr-app"), Some("/home/sam/code/.monaged/hr-app".into()));
        assert_eq!(worktree_root("/home/sam/code/hr-app/"), Some("/home/sam/code/.monaged/hr-app".into()));
        assert_eq!(worktree_root("~/code/monaged-playground"), Some("~/code/.monaged/monaged-playground".into()));
        for bad in ["", "/", "/hr-app", "code/hr-app", "/home/sam/../etc", "/home/sam/code/..", "/home/sam/code/.", "/home/sam/x\nrm", "~", "~/", "/home/sam/.monaged"] {
            assert_eq!(worktree_root(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_monaged_folders_in_the_right_place_are_deleted() {
        let appdata = Path::new(r"C:\Users\sam\AppData\Roaming");
        assert!(safe_folder(&appdata.join("Monaged"), appdata));
        assert!(!safe_folder(&appdata.join("Other"), appdata));
        assert!(!safe_folder(Path::new(r"C:\Monaged"), Path::new(r"C:\")));
        assert!(!safe_folder(Path::new(r".\Monaged"), Path::new(".")));
        assert!(!safe_folder(Path::new(r"C:\Users\sam\Monaged"), appdata));
        assert!(program_folder_ok(Path::new(r"C:\Users\sam\AppData\Local\Programs\Monaged")));
        assert!(!program_folder_ok(Path::new(r"C:\Users\sam\AppData\Local\Monaged")));
        assert!(!program_folder_ok(Path::new(r"Programs\Monaged")));
    }

    #[test]
    fn a_registered_distros_folder_is_never_deleted() {
        let root = Path::new(r"C:\Users\sam\AppData\Local\Monaged");
        let children: Vec<String> = ["downloads", "wsl-install.log", "WSL"].iter().map(|s| s.to_string()).collect();
        let wsl_children: Vec<String> = ["Ubuntu", "Old", "Moved"].iter().map(|s| s.to_string()).collect();
        let registered = vec![
            registered("ubuntu", None),
            // Registered under another name, with its disk in our folder.
            registered("Work", Some(r"\\?\C:\Users\sam\AppData\Local\Monaged\wsl\moved")),
        ];
        let registered: Vec<Registered> = registered.into_iter().map(|r| Registered { base: r.base.map(|b| clean_base(&b.to_string_lossy())), ..r }).collect();
        let (remove, keep) = local_plan(root, &children, &wsl_children, &registered);
        assert!(remove.contains(&root.join("downloads")) && remove.contains(&root.join("wsl-install.log")));
        assert!(remove.contains(&root.join("WSL").join("Old")));
        assert!(keep.contains(&root.join("WSL").join("Ubuntu")), "{keep:?}");
        assert!(keep.contains(&root.join("WSL").join("Moved")), "{keep:?}");
        assert!(!remove.iter().any(|p| p == &root.join("WSL") || p == root));
        // Once Ubuntu is unregistered, its folder may go.
        let (remove, _) = local_plan(root, &children, &wsl_children, &registered[1..]);
        assert!(remove.contains(&root.join("WSL").join("Ubuntu")));
        // A disk somewhere inside a folder protects the whole folder.
        assert!(protected(Path::new(r"C:\Users\sam\AppData\Local\Monaged"), &registered));
        assert!(!protected(Path::new(r"C:\Users\sam\AppData\Local\Monaged\downloads"), &registered));
    }

    #[test]
    fn reads_wsls_registry_entries() {
        let text = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\r\n    DefaultDistribution    REG_SZ    {aaa}\r\n\r\n\
                    HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{aaa}\r\n    State    REG_DWORD    0x1\r\n    DistributionName    REG_SZ    Ubuntu\r\n    BasePath    REG_EXPAND_SZ    \\\\?\\C:\\Users\\sam\\AppData\\Local\\wsl\\{aaa}\r\n\r\n\
                    HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{bbb}\r\n    BasePath    REG_SZ    C:\\Users\\sam\\My  Distros\\Debian\r\n    DistributionName    REG_SZ    Debian\r\n";
        let found = parse_lxss(text);
        assert_eq!(
            found,
            vec![
                ("Ubuntu".to_string(), Some(r"\\?\C:\Users\sam\AppData\Local\wsl\{aaa}".to_string())),
                ("Debian".to_string(), Some(r"C:\Users\sam\My  Distros\Debian".to_string())),
            ]
        );
        assert_eq!(clean_base(r"\\?\C:\Users\sam\AppData\Local\wsl\{aaa}"), PathBuf::from(r"C:\Users\sam\AppData\Local\wsl\{aaa}"));
    }

    #[test]
    fn only_monaged_credentials_are_picked_up() {
        assert_eq!(monaged_entry("anthropic-api-key.monaged").as_deref(), Some(API_KEY));
        assert_eq!(monaged_entry("env-vault-key.monaged").as_deref(), Some(VAULT_KEY));
        assert_eq!(monaged_entry("git-token:github.com/sam.monaged").as_deref(), Some("git-token:github.com/sam"));
        assert_eq!(monaged_entry("git-logins:gitlab.com.monaged").as_deref(), Some("git-logins:gitlab.com"));
        for other in ["git-token:.monaged", "anthropic-api-key", "anthropic-api-key.other", "MicrosoftAccount:user=sam", "something.monaged"] {
            assert_eq!(monaged_entry(other), None, "{other}");
        }
    }

    #[test]
    fn the_git_script_only_takes_out_monaged() {
        let s = helper_script();
        // Only helpers that run monaged.exe --git-credential are touched...
        assert!(s.contains("grep -e ' --git-credential$' | grep -i monaged"));
        // ...a general helper only loses Monaged's value, not Git Credential Manager...
        assert!(s.contains("--unset-all credential.helper ' --git-credential$'"));
        // ...and a host's section goes only when nothing else is left in it.
        assert!(s.contains("--remove-section \"$section\""));
        assert!(s.contains("[ \"$left\" = 0 ]"));
        assert!(s.trim_end().ends_with("echo \"MONAGED_HELPER_REMOVED $n\""));
    }

    #[test]
    fn the_removal_scripts_touch_only_monageds_paths() {
        let agent = agent_script();
        assert!(agent.contains("for d in \"$HOME\"/'.local/share/claude-agent-acp'; do"), "{agent}");
        assert!(agent.contains("rm -rf -- \"$d\""));
        let playground = playground_script();
        assert!(playground.contains("\"$HOME\"/'code/monaged-playground'"));
        assert!(playground.contains("\"$HOME\"/'code/.remotes/monaged-playground.git'"));
        assert!(playground.contains("\"$HOME\"/'code/.monaged/monaged-playground'"));
        assert!(playground.contains("rmdir -- \"$HOME\"/'code/.remotes' \"$HOME\"/'code/.monaged'"));
        let login = claude_login_script();
        assert!(login.contains("\"$HOME\"/'.claude/.credentials.json'"));
        assert!(!login.contains("'.claude'") && !login.contains(".claude.json"), "only the sign-in goes: {login}");
        let roots = vec![WorktreeRoot { distro: "Ubuntu".into(), repo: "/home/sam/it's/hr-app".into(), root: "/home/sam/it's/.monaged/hr-app".into() }];
        let wt = worktrees_script(&roots);
        assert!(wt.contains(r"w='/home/sam/it'\''s/.monaged/hr-app'; r='/home/sam/it'\''s/hr-app'"), "{wt}");
        assert!(wt.contains("*/.monaged/?*)"));
        assert!(wt.contains("git -C \"$r\" worktree prune"));
        assert!(wt.contains("rmdir -- \"${w%/*}\""));
        assert!(wt.trim_end().ends_with("echo \"MONAGED_WORKTREES $removed $failed\""));
        let probe = probe_script(&["/home/sam/code/.monaged/hr-app".into()]);
        assert!(probe.contains("w='/home/sam/code/.monaged/hr-app'; [ -d \"$w\" ] && echo \"MONAGED_WT 0 $(kb \"$w\")\""), "{probe}");
        assert!(probe.trim_end().ends_with("echo MONAGED_PROBED"));
        assert!(!probe.contains("rm "), "the probe only looks");
    }

    #[test]
    fn words_read_well() {
        assert_eq!(and_list(&[]), "");
        assert_eq!(and_list(&["a".into()]), "a");
        assert_eq!(and_list(&["a".into(), "b".into(), "c".into()]), "a, b and c");
        assert_eq!(size_text(34_000), "34 KB");
        assert_eq!(size_text(220_400_000), "220 MB");
        assert_eq!(size_text(1_430_000_000), "1.4 GB");
        assert_eq!(sign_ins_text(&["git-token:a/b".into(), "git-token:a/c".into()]), "2 git sign-ins");
        assert_eq!(parse_accounts(r#"[{"host":"github.com","login":"sam"},{"host":"x"}]"#), vec![("github.com".to_string(), "sam".to_string())]);
    }
}
