//! Everyday git for the Git tab, run inside WSL: a worktree's status, commit,
//! discard and stash; fetching and how far branches are from origin; a
//! fast-forward pull; the commit graph; one commit's diff; and finishing a
//! conflicted merge with a choice per file.

use anyhow::{Result, bail};

use crate::git::worktree_path;
use crate::wsl::{quote, run};

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub branch: String,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub files: Vec<StatusFile>,
}

#[derive(Clone, Debug)]
pub struct StatusFile {
    /// Two-letter porcelain code, e.g. " M", "A ", "??".
    pub code: String,
    pub path: String,
}

impl StatusFile {
    pub fn label(&self) -> &'static str {
        let c = self.code.trim();
        match c {
            "??" => "new",
            _ if c.contains('D') => "deleted",
            _ if c.contains('A') => "added",
            _ if c.contains('R') => "renamed",
            _ if c.contains('U') => "conflict",
            _ => "modified",
        }
    }

    pub fn untracked(&self) -> bool {
        self.code == "??"
    }
}

/// `git status` of a worktree.
pub fn status(distro: &str, worktree: &str) -> Result<Status> {
    let out = run(distro, &format!("cd {} || exit 1\ngit -c core.quotepath=off status --porcelain=v1 --branch", quote(worktree)))?;
    let mut status = Status::default();
    for line in out.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            // "main...origin/main [ahead 1, behind 2]" or "No commits yet on main" or "HEAD (no branch)"
            let (names, counts) = head.split_once(" [").map_or((head, ""), |(n, c)| (n, c.trim_end_matches(']')));
            let (branch, upstream) = names.split_once("...").map_or((names.to_string(), None), |(b, u)| (b.to_string(), Some(u.to_string())));
            status.branch = branch;
            status.upstream = upstream;
            for part in counts.split(", ") {
                if let Some(n) = part.strip_prefix("ahead ") {
                    status.ahead = n.parse().unwrap_or(0);
                }
                if let Some(n) = part.strip_prefix("behind ") {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
            continue;
        }
        if line.len() > 3 {
            let path = line[3..].rsplit(" -> ").next().unwrap_or(&line[3..]).trim_matches('"').to_string();
            status.files.push(StatusFile { code: line[..2].to_string(), path });
        }
    }
    Ok(status)
}

/// Commits everything (or the given paths) with `message`; returns the short sha.
pub fn commit(distro: &str, worktree: &str, message: &str, paths: &[String]) -> Result<String> {
    if message.trim().is_empty() {
        bail!("Write a commit message first.");
    }
    let add = if paths.is_empty() {
        "git add -A".to_string()
    } else {
        format!("git add -A -- {}", paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" "))
    };
    let out = run(
        distro,
        &format!("cd {} || exit 1\n{add} && git commit -q -m {} && git rev-parse --short HEAD", quote(worktree), quote(message)),
    )?;
    Ok(out.lines().last().unwrap_or_default().trim().to_string())
}

/// The newest commit of a worktree: (short sha, subject, already on a remote).
pub fn last_commit(distro: &str, worktree: &str) -> Result<Option<(String, String, bool)>> {
    let out = run(
        distro,
        &format!(
            "cd {} || exit 1\ngit rev-parse -q --verify HEAD >/dev/null || exit 0\n\
             git log -1 --format='%h%x1f%s'\n\
             if [ -n \"$(git branch -r --contains HEAD 2>/dev/null | head -n 1)\" ]; then echo pushed; else echo local; fi",
            quote(worktree)
        ),
    )?;
    let mut lines = out.lines();
    let Some((sha, subject)) = lines.next().and_then(|l| l.split_once('\u{1f}')) else { return Ok(None) };
    Ok(Some((sha.to_string(), subject.to_string(), lines.next() == Some("pushed"))))
}

/// Adds the uncommitted changes (all, or the given paths) to the last commit,
/// with a new message when one is given. Refuses once the commit is pushed.
pub fn amend(distro: &str, worktree: &str, message: &str, paths: &[String]) -> Result<String> {
    let add = if paths.is_empty() { "git add -A".to_string() } else { format!("git add -A -- {}", paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ")) };
    let msg = if message.trim().is_empty() { "--no-edit".to_string() } else { format!("-m {}", quote(message)) };
    let out = run(
        distro,
        &format!(
            "cd {} || exit 1\n\
             if [ -n \"$(git branch -r --contains HEAD 2>/dev/null | head -n 1)\" ]; then echo 'The last commit is already pushed; make a new commit instead.' >&2; exit 1; fi\n\
             {add} && git commit -q --amend {msg} && git rev-parse --short HEAD",
            quote(worktree)
        ),
    )?;
    Ok(out.lines().last().unwrap_or_default().trim().to_string())
}

/// Takes back the last commit and keeps its changes as uncommitted work.
/// Refuses once the commit is pushed.
pub fn undo_commit(distro: &str, worktree: &str) -> Result<String> {
    let out = run(
        distro,
        &format!(
            "cd {} || exit 1\n\
             if [ -n \"$(git branch -r --contains HEAD 2>/dev/null | head -n 1)\" ]; then echo 'The last commit is already pushed, so it stays.' >&2; exit 1; fi\n\
             git rev-parse -q --verify HEAD~1 >/dev/null || {{ echo 'This is the first commit; there is nothing before it.' >&2; exit 1; }}\n\
             s=$(git log -1 --format=%s); git reset -q --soft HEAD~1 && echo \"$s\"",
            quote(worktree)
        ),
    )?;
    Ok(out.trim().to_string())
}

/// Merges origin's version of a branch into it when both have moved
/// (no rebase: nothing that's pushed gets rewritten). A conflict leaves
/// everything as it was and lists the files.
pub fn pull_merge(distro: &str, repo: &str, branch: &str) -> Result<String> {
    let merge_worktree = worktree_path(repo, "_merge", "worktree");
    let script = format!(
        "cd {repo} || exit 1\nb={b}; mwt={mwt}\n\
         wt=$(git worktree list --porcelain | awk -v r=\"refs/heads/$b\" '/^worktree /{{p=substr($0,10)}} $1==\"branch\" && $2==r {{print p}}')\n\
         release=0\n\
         if [ -n \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain)\" ]; then echo \"$b has uncommitted changes in $wt; commit or stash them first\" >&2; exit 1; fi\n\
         else\n\
           wt=\"$mwt\"\n\
           if [ ! -d \"$wt\" ]; then mkdir -p \"$(dirname \"$wt\")\"; git worktree add -q --detach \"$wt\" >/dev/null 2>&1 || {{ echo \"couldn't create the merge worktree\" >&2; exit 1; }}; fi\n\
           git -C \"$wt\" checkout -q \"$b\" || exit 1\n\
           release=1\n\
         fi\n\
         if git -C \"$wt\" merge --no-edit -q \"origin/$b\" >/dev/null 2>&1; then sha=$(git -C \"$wt\" rev-parse --short HEAD); ok=1\n\
         else files=$(git -C \"$wt\" diff --name-only --diff-filter=U | tr '\\n' ' '); git -C \"$wt\" merge --abort >/dev/null 2>&1; ok=0; fi\n\
         [ \"$release\" = 1 ] && git -C \"$wt\" checkout -q --detach\n\
         if [ \"$ok\" = 1 ]; then echo \"$sha\"; else echo \"$b and origin/$b conflict in: $files\" >&2; exit 1; fi",
        repo = quote(repo),
        b = quote(branch),
        mwt = quote(&merge_worktree),
    );
    Ok(run(distro, &script)?.trim().to_string())
}

/// Throws away a file's uncommitted changes (a new file is deleted).
pub fn discard(distro: &str, worktree: &str, file: &StatusFile) -> Result<()> {
    let script = if file.untracked() {
        format!("cd {} && git clean -fq -- {}", quote(worktree), quote(&file.path))
    } else {
        format!("cd {} && git restore --staged --worktree --source=HEAD -- {}", quote(worktree), quote(&file.path))
    };
    run(distro, &script)?;
    Ok(())
}

pub fn stash(distro: &str, worktree: &str) -> Result<()> {
    run(distro, &format!("cd {} && git stash push -u -q -m 'Stashed from Monaged'", quote(worktree)))?;
    Ok(())
}

pub fn stash_pop(distro: &str, worktree: &str) -> Result<()> {
    run(distro, &format!("cd {} && git stash pop -q", quote(worktree)))?;
    Ok(())
}

pub fn stash_count(distro: &str, worktree: &str) -> Result<usize> {
    Ok(run(distro, &format!("cd {} && git stash list | wc -l", quote(worktree)))?.trim().parse().unwrap_or(0))
}

/// `git fetch --all --prune` in the project.
pub fn fetch(distro: &str, repo: &str) -> Result<()> {
    run(distro, &format!("cd {} && git fetch --all --prune --quiet 2>&1", quote(repo)))?;
    Ok(())
}

/// For each branch: (ahead of origin, behind origin), or None without an origin branch.
pub fn remote_counts(distro: &str, repo: &str, branches: &[String]) -> Result<Vec<Option<(usize, usize)>>> {
    if branches.is_empty() {
        return Ok(Vec::new());
    }
    let mut script = format!("cd {} || exit 1\n", quote(repo));
    for b in branches {
        script.push_str(&format!(
            "if git rev-parse -q --verify refs/remotes/origin/{b} >/dev/null; then git rev-list --left-right --count {b}...origin/{b}; else echo none; fi\n",
            b = quote(b)
        ));
    }
    let out = run(distro, &script)?;
    Ok(out
        .lines()
        .map(|l| {
            let mut parts = l.split_whitespace();
            match (parts.next().and_then(|a| a.parse().ok()), parts.next().and_then(|b| b.parse().ok())) {
                (Some(ahead), Some(behind)) => Some((ahead, behind)),
                _ => None,
            }
        })
        .collect())
}

/// Brings a branch up to origin when that's a fast-forward, without touching
/// anyone's uncommitted work.
pub fn pull_ff(distro: &str, repo: &str, branch: &str) -> Result<String> {
    let script = format!(
        "cd {repo} || exit 1\nb={b}\n\
         wt=$(git worktree list --porcelain | awk -v r=\"refs/heads/$b\" '/^worktree /{{p=substr($0,10)}} $1==\"branch\" && $2==r {{print p}}')\n\
         if [ -n \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain)\" ]; then echo \"$b has uncommitted changes in $wt\" >&2; exit 1; fi\n\
           git -C \"$wt\" merge --ff-only -q \"origin/$b\" 2>&1 || {{ echo \"$b and origin/$b have both moved; merge them instead\" >&2; exit 1; }}\n\
         else\n\
           git fetch -q origin \"$b:$b\" 2>&1 || {{ echo \"$b and origin/$b have both moved; merge them instead\" >&2; exit 1; }}\n\
         fi\n\
         git rev-parse --short \"$b\"",
        repo = quote(repo),
        b = quote(branch),
    );
    Ok(run(distro, &script)?.trim().to_string())
}

/// One line of `git log --graph`.
#[derive(Clone, Debug)]
pub struct GraphLine {
    /// The graph's drawing for this line (`* |`, `|\`, …).
    pub graph: String,
    pub commit: Option<CommitInfo>,
}

#[derive(Clone, Debug)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub when: String,
    /// Branch and tag names pointing here.
    pub refs: Vec<String>,
}

/// The graph of the given branches, newest first.
pub fn graph(distro: &str, repo: &str, branches: &[String], limit: usize) -> Result<Vec<GraphLine>> {
    let refs = branches.iter().map(|b| quote(b)).collect::<Vec<_>>().join(" ");
    let out = run(
        distro,
        &format!(
            "cd {} || exit 1\ngit log --graph --date-order -n {limit} --format='%x1f%h%x1f%s%x1f%an%x1f%ar%x1f%D' {refs} -- 2>/dev/null",
            quote(repo)
        ),
    )?;
    Ok(out
        .lines()
        .map(|line| match line.split_once('\u{1f}') {
            Some((graph, rest)) => {
                let f: Vec<&str> = rest.split('\u{1f}').collect();
                let refs = f.get(4).map(|r| r.split(", ").filter(|s| !s.is_empty()).map(|s| s.trim_start_matches("HEAD -> ").to_string()).collect()).unwrap_or_default();
                GraphLine {
                    graph: graph.trim_end().to_string(),
                    commit: Some(CommitInfo {
                        sha: f.first().unwrap_or(&"").to_string(),
                        subject: f.get(1).unwrap_or(&"").to_string(),
                        author: f.get(2).unwrap_or(&"").to_string(),
                        when: f.get(3).unwrap_or(&"").to_string(),
                        refs,
                    }),
                }
            }
            None => GraphLine { graph: line.trim_end().to_string(), commit: None },
        })
        .collect())
}

/// One commit: its message and its diff.
pub fn show(distro: &str, repo: &str, sha: &str) -> Result<(String, Vec<crate::diff::FileDiff>)> {
    let out = run(
        distro,
        &format!("cd {} || exit 1\ngit -c core.quotepath=off show --no-color -M --format='%an · %ar%n%n%B%x1e' {}", quote(repo), quote(sha)),
    )?;
    let (head, diff) = out.split_once('\u{1e}').unwrap_or((&out, ""));
    Ok((head.trim().to_string(), crate::diff::parse(diff)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    /// Keep the target branch's version.
    Ours,
    /// Take the source branch's version.
    Theirs,
}

/// Merges `source` into `target` like `git::merge_branch`, resolving the
/// conflicted files as chosen. Refuses (and leaves everything as it was)
/// when a conflicted file has no choice.
pub fn merge_with_choices(distro: &str, repo: &str, source: &str, target: &str, message: &str, choices: &[(String, Side)]) -> Result<String> {
    let merge_worktree = worktree_path(repo, "_merge", "worktree");
    let mut resolve = String::new();
    for (path, side) in choices {
        let flag = if *side == Side::Ours { "--ours" } else { "--theirs" };
        resolve.push_str(&format!("git -C \"$wt\" checkout {flag} -- {p} 2>/dev/null && git -C \"$wt\" add -- {p}\n", p = quote(path)));
    }
    let script = format!(
        "cd {repo} || exit 1\n\
         src={src}; dst={dst}; msg={msg}; mwt={mwt}\n\
         wt=$(git worktree list --porcelain | awk -v b=\"refs/heads/$dst\" '/^worktree /{{p=substr($0,10)}} $1==\"branch\" && $2==b {{print p}}')\n\
         release=0\n\
         if [ -n \"$wt\" ]; then\n\
           if [ -n \"$(git -C \"$wt\" status --porcelain)\" ]; then echo \"$dst is checked out in $wt, which has uncommitted changes\" >&2; exit 1; fi\n\
         else\n\
           wt=\"$mwt\"\n\
           if [ ! -d \"$wt\" ]; then mkdir -p \"$(dirname \"$wt\")\"; git worktree add -q --detach \"$wt\" >/dev/null 2>&1 || {{ echo \"couldn't create the merge worktree\" >&2; exit 1; }}; fi\n\
           git -C \"$wt\" checkout -q \"$dst\" || exit 1\n\
           release=1\n\
         fi\n\
         git -C \"$wt\" merge --no-ff --no-commit \"$src\" >/dev/null 2>&1\n\
         {resolve}\
         left=$(git -C \"$wt\" diff --name-only --diff-filter=U)\n\
         if [ -n \"$left\" ]; then git -C \"$wt\" merge --abort >/dev/null 2>&1; [ \"$release\" = 1 ] && git -C \"$wt\" checkout -q --detach; echo \"still conflicted: $left\" >&2; exit 1; fi\n\
         git -C \"$wt\" commit -q -m \"$msg\" || {{ git -C \"$wt\" merge --abort >/dev/null 2>&1; exit 1; }}\n\
         sha=$(git -C \"$wt\" rev-parse --short HEAD)\n\
         [ \"$release\" = 1 ] && git -C \"$wt\" checkout -q --detach\n\
         echo \"$sha\"",
        repo = quote(repo),
        src = quote(source),
        dst = quote(target),
        msg = quote(message),
        mwt = quote(&merge_worktree),
    );
    Ok(run(distro, &script)?.trim().to_string())
}

/// The global git identity inside WSL: (name, email).
pub fn identity(distro: &str) -> Result<(String, String)> {
    let out = run(distro, "echo \"$(git config --global user.name)\"; echo \"$(git config --global user.email)\"")?;
    let mut lines = out.lines();
    Ok((lines.next().unwrap_or_default().to_string(), lines.next().unwrap_or_default().to_string()))
}

pub fn set_identity(distro: &str, name: &str, email: &str) -> Result<()> {
    if name.trim().is_empty() || !email.contains('@') {
        bail!("Enter a name and an email address.");
    }
    run(distro, &format!("git config --global user.name {} && git config --global user.email {}", quote(name.trim()), quote(email.trim())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_lines_parse() {
        let parsed = super::StatusFile { code: "??".into(), path: "new.txt".into() };
        assert_eq!(parsed.label(), "new");
        assert!(parsed.untracked());
        let m = super::StatusFile { code: " M".into(), path: "a.rs".into() };
        assert_eq!(m.label(), "modified");
    }
}
