//! Git operations inside WSL: preparing projects, subproject branches and
//! one worktree per agent. All functions block; call them from a background task.

use anyhow::Result;

use crate::wsl::{quote, run, shell_path};

pub struct Prepared {
    /// Absolute Linux path of the repository.
    pub path: String,
    /// `develop` when it exists (the user's branch model), else the current branch.
    pub base_branch: String,
    /// Git has no user.name/user.email for this repo, so agents can't commit yet.
    pub identity_missing: bool,
}

pub fn linux_user(distro: &str) -> Result<String> {
    run(distro, "whoami")
}

/// Makes a folder that's already in WSL ready for Monaged: a git repo with at
/// least one commit (worktrees need one), its base branch and git identity.
/// Cloning and creating folders happen before this (see `add_project`).
pub fn prepare_project(distro: &str, folder: &str) -> Result<Prepared> {
    let dir = shell_path(folder);
    let path = run(distro, &format!("cd {dir} && pwd"))?;
    let repo = quote(&path);

    // Worktrees need a repository with at least one commit.
    run(
        distro,
        &format!(
            "cd {repo} && (git rev-parse --is-inside-work-tree >/dev/null 2>&1 || git init -q -b main) && \
             if ! git rev-parse --verify -q HEAD >/dev/null; then \
               n=\"$(git config user.name || whoami)\"; e=\"$(git config user.email || echo \"$n@localhost\")\"; \
               git -c user.name=\"$n\" -c user.email=\"$e\" commit -q --allow-empty -m 'Initial commit'; \
             fi"
        ),
    )?;
    let base_branch = run(
        distro,
        &format!("cd {repo} && (git show-ref --verify -q refs/heads/develop && echo develop || git symbolic-ref --short HEAD)"),
    )?;
    let identity = run(
        distro,
        &format!("cd {repo} && [ -n \"$(git config user.name)\" ] && [ -n \"$(git config user.email)\" ] && echo ok || echo missing"),
    )?;
    Ok(Prepared { path, base_branch, identity_missing: identity != "ok" })
}

/// Creates `branch` from `from` unless it already exists.
pub fn ensure_branch(distro: &str, repo: &str, branch: &str, from: &str) -> Result<()> {
    let (repo, branch, from) = (quote(repo), quote(branch), quote(from));
    run(distro, &format!("cd {repo} && (git show-ref --verify -q refs/heads/{branch} || git branch -q {branch} {from})"))?;
    Ok(())
}

/// Adds a worktree at `worktree` on `branch` (created from `from` if needed).
/// Does nothing when the worktree folder already exists.
pub fn add_worktree(distro: &str, repo: &str, worktree: &str, branch: &str, from: &str) -> Result<()> {
    let (repo, worktree, branch, from) = (quote(repo), quote(worktree), quote(branch), quote(from));
    run(
        distro,
        &format!(
            "cd {repo} && if [ -d {worktree} ]; then exit 0; fi && mkdir -p \"$(dirname {worktree})\" && \
             if git show-ref --verify -q refs/heads/{branch}; then git worktree add -q {worktree} {branch}; \
             else git worktree add -q -b {branch} {worktree} {from}; fi"
        ),
    )?;
    Ok(())
}

/// Git has a name and email for commits in this repo.
pub fn identity_ok(distro: &str, repo: &str) -> Result<bool> {
    let repo = quote(repo);
    let out = run(
        distro,
        &format!("cd {repo} && [ -n \"$(git config user.name)\" ] && [ -n \"$(git config user.email)\" ] && echo ok || echo missing"),
    )?;
    Ok(out == "ok")
}

/// Copies the Windows git identity (global user.name/email) into WSL's global git config.
pub fn copy_windows_identity(distro: &str) -> Result<()> {
    let read = |key: &str| -> Result<String> {
        let mut command = std::process::Command::new("git");
        command.args(["config", "--global", key]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
        }
        let output = command.output().map_err(|e| anyhow::anyhow!("couldn't run git on Windows: {e}"))?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            anyhow::bail!("Windows git has no {key} either; set it in WSL by hand");
        }
        Ok(value)
    };
    let (name, email) = (read("user.name")?, read("user.email")?);
    run(distro, &format!("git config --global user.name {} && git config --global user.email {}", quote(&name), quote(&email)))?;
    Ok(())
}

// ---- Branches and merging (Phase 5) ---------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct BranchState {
    pub branches: Vec<String>,
    /// branch -> path of the worktree it is checked out in.
    pub checked_out: Vec<(String, String)>,
    pub remote_url: Option<String>,
    /// Branches origin has (as of the last fetch), without the `origin/`.
    pub on_origin: Vec<String>,
}

impl BranchState {
    pub fn has(&self, branch: &str) -> bool {
        self.branches.iter().any(|b| b == branch)
    }

    pub fn on_origin(&self, branch: &str) -> bool {
        self.on_origin.iter().any(|b| b == branch)
    }
}

pub fn branch_state(distro: &str, repo: &str) -> Result<BranchState> {
    let out = run(
        distro,
        &format!(
            "cd {repo} || exit 1\n\
             echo MONAGED_BRANCHES; git for-each-ref --format='%(refname:short)' refs/heads\n\
             echo MONAGED_WORKTREES; git worktree list --porcelain | awk '/^worktree /{{p=substr($0,10)}} $1==\"branch\"{{sub(\"refs/heads/\",\"\",$2); print $2\"\\t\"p}}'\n\
             echo MONAGED_REMOTE; git remote get-url origin 2>/dev/null || true\n\
             echo MONAGED_ORIGIN; git for-each-ref --format='%(refname:lstrip=3)' refs/remotes/origin",
            repo = quote(repo)
        ),
    )?;
    let mut state = BranchState::default();
    let mut section = "";
    for line in out.lines() {
        match line.trim() {
            "MONAGED_BRANCHES" | "MONAGED_WORKTREES" | "MONAGED_REMOTE" | "MONAGED_ORIGIN" => section = line.trim(),
            "" => {}
            value => match section {
                "MONAGED_BRANCHES" => state.branches.push(value.to_string()),
                "MONAGED_WORKTREES" => {
                    if let Some((branch, path)) = value.split_once('\t') {
                        state.checked_out.push((branch.to_string(), path.to_string()));
                    }
                }
                "MONAGED_REMOTE" => state.remote_url = Some(value.to_string()),
                "MONAGED_ORIGIN" if value != "HEAD" => state.on_origin.push(value.to_string()),
                _ => {}
            },
        }
    }
    Ok(state)
}

/// Commits in `source` that are not in `target`, for each pair.
pub fn ahead_counts(distro: &str, repo: &str, pairs: &[(String, String)]) -> Result<Vec<usize>> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let mut script = format!("cd {} || exit 1\n", quote(repo));
    for (source, target) in pairs {
        script.push_str(&format!("git rev-list --count {}..{} 2>/dev/null || echo 0\n", quote(target), quote(source)));
    }
    let out = run(distro, &script)?;
    Ok(out.lines().map(|l| l.trim().parse().unwrap_or(0)).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub enum MergeOutcome {
    Merged(String),
    AlreadyMerged,
    Conflict(Vec<String>),
    Blocked(String),
}

/// A merge and what happened around it on origin.
#[derive(Clone, Debug, PartialEq)]
pub struct MergeReport {
    pub outcome: MergeOutcome,
    /// The target was behind origin and was caught up first (commits taken).
    pub caught_up: usize,
    /// After merging: None when not asked to push (or no origin), else how it went.
    pub pushed: Option<Result<(), String>>,
}

/// Merges `source` into `target` with a merge commit, without disturbing
/// anyone's work: in the worktree where `target` is checked out (only if it
/// has no uncommitted changes), or else in Monaged's own merge worktree.
/// With an origin, it fetches first and catches `target` up with origin
/// (refusing when they diverged), and pushes `target` afterwards when asked.
/// Conflicts are aborted and reported.
pub fn merge_branch(distro: &str, repo: &str, source: &str, target: &str, message: &str, push: bool) -> Result<MergeReport> {
    let found = secrets_in(distro, repo, &format!("{}..{}", quote(target), quote(source)))?;
    if !found.is_empty() {
        let why = format!("{source} has commits with the value of {}; remove it before merging", found.join(", "));
        return Ok(MergeReport { outcome: MergeOutcome::Blocked(why), caught_up: 0, pushed: None });
    }
    let merge_worktree = worktree_path(repo, "_merge", "worktree");
    let out = run(distro, &merge_script(repo, source, target, message, &merge_worktree, push))?;
    Ok(parse_merge(&out))
}

/// The script behind `merge_branch` (kept apart so it can be tested in bash).
pub fn merge_script(repo: &str, source: &str, target: &str, message: &str, merge_worktree: &str, push: bool) -> String {
    format!(
        "cd {repo} || exit 1\n\
         src={src}; dst={dst}; msg={msg}; mwt={mwt}; push={push}\n\
         origin=$(git remote | grep -qx origin && echo 1)\n\
         # What origin has now (quietly: offline still merges here).\n\
         # One at a time: a branch origin doesn't have yet would fail both.\n\
         if [ -n \"$origin\" ]; then timeout 90 git fetch -q origin \"$dst\" >/dev/null 2>&1; timeout 90 git fetch -q origin \"$src\" >/dev/null 2>&1; fi\n\
         wt=$(git worktree list --porcelain | awk -v b=\"refs/heads/$dst\" '/^worktree /{{p=substr($0,10)}} $1==\"branch\" && $2==b {{print p}}')\n\
         release=0\n\
         if [ -n \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain)\" ]; then echo \"MONAGED_BLOCKED $dst is checked out in $wt, which has uncommitted changes\"; exit 0; fi\n\
         else\n\
           wt=\"$mwt\"\n\
           if [ ! -d \"$wt\" ]; then mkdir -p \"$(dirname \"$wt\")\"; git worktree add -q --detach \"$wt\" >/dev/null 2>&1 || {{ echo \"MONAGED_BLOCKED couldn't create the merge worktree\"; exit 0; }}; fi\n\
           git -C \"$wt\" checkout -q \"$dst\" 2>/dev/null || {{ echo \"MONAGED_BLOCKED couldn't check out $dst\"; exit 0; }}\n\
           release=1\n\
         fi\n\
         done_here() {{ [ \"$release\" = 1 ] && git -C \"$wt\" checkout -q --detach; }}\n\
         # Catch up with origin first; never merge on top of a branch that went its own way.\n\
         if [ -n \"$origin\" ] && git rev-parse -q --verify \"refs/remotes/origin/$dst\" >/dev/null; then\n\
           if ! git merge-base --is-ancestor \"origin/$dst\" \"$dst\"; then\n\
             if git merge-base --is-ancestor \"$dst\" \"origin/$dst\"; then\n\
               echo \"MONAGED_CAUGHT_UP $(git rev-list --count \"$dst..origin/$dst\")\"\n\
               git -C \"$wt\" merge -q --ff-only \"origin/$dst\" >/dev/null 2>&1 || {{ echo \"MONAGED_BLOCKED couldn't catch $dst up with origin\"; done_here; exit 0; }}\n\
             else\n\
               echo \"MONAGED_BLOCKED $dst has commits here and on origin that the other doesn't: pull it first (Git tab → Pull), then merge again\"; done_here; exit 0\n\
             fi\n\
           fi\n\
         fi\n\
         if git merge-base --is-ancestor \"$src\" \"$dst\"; then echo MONAGED_UPTODATE; done_here; exit 0; fi\n\
         if out=$(git -C \"$wt\" merge --no-ff -m \"$msg\" \"$src\" 2>&1); then\n\
           echo \"MONAGED_MERGED $(git -C \"$wt\" rev-parse --short HEAD)\"\n\
           if [ \"$push\" = 1 ] && [ -n \"$origin\" ]; then\n\
             if pout=$(timeout 120 git -C \"$wt\" push -q origin \"$dst\" 2>&1); then echo MONAGED_PUSHED; else echo \"MONAGED_PUSH_FAILED $(echo \"$pout\" | grep -v '^\\s*$' | tail -n 2 | tr '\\n' ' ')\"; fi\n\
           fi\n\
         else\n\
           files=$(git -C \"$wt\" diff --name-only --diff-filter=U)\n\
           git -C \"$wt\" merge --abort >/dev/null 2>&1\n\
           if [ -n \"$files\" ]; then echo MONAGED_CONFLICT; echo \"$files\"; else echo \"MONAGED_BLOCKED $(echo \"$out\" | tail -n 1)\"; fi\n\
         fi\n\
         done_here\n\
         true",
        repo = quote(repo),
        src = quote(source),
        dst = quote(target),
        msg = quote(message),
        mwt = quote(merge_worktree),
        push = if push { 1 } else { 0 },
    )
}

/// Reads what `merge_script` printed.
pub fn parse_merge(out: &str) -> MergeReport {
    let mut report = MergeReport { outcome: MergeOutcome::Blocked("git said nothing".into()), caught_up: 0, pushed: None };
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty()).peekable();
    let mut outcome = None;
    while let Some(line) = lines.next() {
        if let Some(n) = line.strip_prefix("MONAGED_CAUGHT_UP ") {
            report.caught_up = n.trim().parse().unwrap_or(0);
        } else if line == "MONAGED_UPTODATE" {
            outcome = Some(MergeOutcome::AlreadyMerged);
        } else if let Some(sha) = line.strip_prefix("MONAGED_MERGED ") {
            outcome = Some(MergeOutcome::Merged(sha.trim().to_string()));
        } else if line == "MONAGED_PUSHED" {
            report.pushed = Some(Ok(()));
        } else if let Some(why) = line.strip_prefix("MONAGED_PUSH_FAILED") {
            report.pushed = Some(Err(why.trim().to_string()));
        } else if line == "MONAGED_CONFLICT" {
            let files: Vec<String> = lines.by_ref().map(str::to_owned).collect();
            outcome = Some(MergeOutcome::Conflict(files));
        } else if let Some(why) = line.strip_prefix("MONAGED_BLOCKED ") {
            outcome = Some(MergeOutcome::Blocked(why.to_string()));
        } else if outcome.is_none() && !line.starts_with("MONAGED_") {
            outcome = Some(MergeOutcome::Blocked(format!("unexpected git output: {line}")));
        }
    }
    if let Some(outcome) = outcome {
        report.outcome = outcome;
    }
    report
}

/// Catches a local branch up with origin when it's only behind (after a
/// merge on GitHub or GitLab): in its worktree if it's checked out and clean,
/// else by moving the branch. Returns how many commits it took.
pub fn catch_up(distro: &str, repo: &str, branch: &str) -> Result<usize> {
    let out = run(distro, &catch_up_script(repo, branch))?;
    Ok(out.lines().find_map(|l| l.strip_prefix("MONAGED_N ")).and_then(|n| n.trim().parse().ok()).unwrap_or(0))
}

pub fn catch_up_script(repo: &str, branch: &str) -> String {
    format!(
        "cd {repo} || exit 1\nb={b}\n\
         timeout 90 git fetch -q origin \"$b\" >/dev/null 2>&1 || {{ echo MONAGED_N 0; exit 0; }}\n\
         git rev-parse -q --verify \"refs/remotes/origin/$b\" >/dev/null || {{ echo MONAGED_N 0; exit 0; }}\n\
         git merge-base --is-ancestor \"$b\" \"origin/$b\" || {{ echo MONAGED_N 0; exit 0; }}\n\
         n=$(git rev-list --count \"$b..origin/$b\")\n\
         wt=$(git worktree list --porcelain | awk -v r=\"refs/heads/$b\" '/^worktree /{{p=substr($0,10)}} $1==\"branch\" && $2==r {{print p}}')\n\
         if [ -n \"$wt\" ]; then [ -z \"$(git -C \"$wt\" status --porcelain)\" ] && git -C \"$wt\" merge -q --ff-only \"origin/$b\" >/dev/null 2>&1 || n=0\n\
         else git branch -f \"$b\" \"origin/$b\" >/dev/null 2>&1 || n=0; fi\n\
         echo \"MONAGED_N $n\"",
        repo = quote(repo),
        b = quote(branch),
    )
}

/// `git push -u origin <branch>` from WSL.
/// Names of secrets whose values are in a git range's patches (empty when
/// Monaged has no secrets to look for).
fn secrets_in(distro: &str, repo: &str, revs: &str) -> Result<Vec<String>> {
    if !crate::env::has_masks() {
        return Ok(Vec::new());
    }
    let patches = run(distro, &format!("cd {} && git log -p --no-color --no-ext-diff {revs} 2>/dev/null; true", quote(repo)))?;
    Ok(crate::env::mask_names(&patches).1)
}

pub fn push_branch(distro: &str, repo: &str, branch: &str) -> Result<String> {
    // Commits that aren't on any remote yet must not carry a secret's value.
    let found = secrets_in(distro, repo, &format!("{} --not --remotes", quote(branch)))?;
    if !found.is_empty() {
        anyhow::bail!(
            "Not pushed: {branch} has commits with the value of {}. Remove it from the code and the commits (or rotate it) first.",
            found.join(", ")
        );
    }
    run(distro, &format!("cd {} && git push -u origin {} 2>&1", quote(repo), quote(branch)))
}

#[derive(Debug, PartialEq)]
pub enum Cleanup {
    /// Worktree removed; `true` when its branch was also deleted.
    Removed(bool),
    /// Kept: it has uncommitted changes.
    Dirty,
    Failed(String),
}

/// Removes an agent's worktree if it has no uncommitted changes, and deletes
/// its branch only when every commit on it is already in `keep_into`.
pub fn remove_worktree(distro: &str, repo: &str, worktree: &str, branch: &str, keep_into: &str) -> Result<Cleanup> {
    let script = format!(
        "cd {repo} || exit 1\n\
         wt={wt}; br={br}; into={into}\n\
         if [ -d \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain)\" ]; then echo MONAGED_DIRTY; exit 0; fi\n\
           git worktree remove \"$wt\" 2>&1 | tail -n 1 | sed 's/^/MONAGED_FAILED /' | grep . && exit 0\n\
         fi\n\
         git worktree prune\n\
         if [ \"$br\" != \"$into\" ] && git show-ref -q --verify \"refs/heads/$br\" && git merge-base --is-ancestor \"$br\" \"$into\"; then\n\
           git branch -D \"$br\" >/dev/null && echo MONAGED_REMOVED_BRANCH && exit 0\n\
         fi\n\
         echo MONAGED_REMOVED",
        repo = quote(repo),
        wt = quote(worktree),
        br = quote(branch),
        into = quote(keep_into),
    );
    let out = run(distro, &script)?;
    let first = out.lines().next().unwrap_or_default();
    Ok(match first {
        "MONAGED_DIRTY" => Cleanup::Dirty,
        "MONAGED_REMOVED_BRANCH" => Cleanup::Removed(true),
        "MONAGED_REMOVED" => Cleanup::Removed(false),
        other => Cleanup::Failed(other.trim_start_matches("MONAGED_FAILED ").to_string()),
    })
}

/// Worktrees live next to the repo (`~/code/.monaged/<repo>/…`), not inside it,
/// so editors and language servers on the main checkout don't index them.
pub fn worktree_path(repo: &str, subproject_slug: &str, agent_slug: &str) -> String {
    let repo = repo.trim_end_matches('/');
    let (parent, name) = repo.rsplit_once('/').unwrap_or(("", repo));
    format!("{parent}/.monaged/{name}/{subproject_slug}/{agent_slug}")
}

/// "Company car reservation!" -> "company-car-reservation"
pub fn slug(text: &str) -> String {
    let mut slug = String::new();
    for ch in text.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() { "untitled".into() } else { slug }
}
