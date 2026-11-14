//! An agent's changes: `git diff` of its worktree against the point where its
//! branch left the subproject branch, including new (untracked) files.

use anyhow::Result;

use crate::wsl::{quote, run};

#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    Hunk,
    Context,
    Add,
    Remove,
}

#[derive(Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Clone)]
pub struct FileDiff {
    pub path: String,
    /// "modified", "added", "deleted" or "renamed".
    pub status: &'static str,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
}

pub struct Changes {
    pub files: Vec<FileDiff>,
    /// Secrets whose values are in this diff (they're masked in `files` and `raw`).
    pub secrets: Vec<String>,
    /// The unified diff text (for reviews), possibly truncated.
    pub raw: String,
    pub truncated: bool,
}

const MAX_RAW: usize = 1_500_000;

pub fn collect(distro: &str, worktree: &str, base_branch: &str) -> Result<Changes> {
    let script = format!(
        "cd {wt} || exit 1\n\
         base=$(git merge-base HEAD {base} 2>/dev/null || git rev-parse HEAD)\n\
         git -c core.quotepath=off diff --no-color --no-ext-diff -M \"$base\"\n\
         git ls-files --others --exclude-standard -z | while IFS= read -r -d '' f; do \
           git -c core.quotepath=off diff --no-color --no-index -- /dev/null \"$f\"; done\n\
         true",
        wt = quote(worktree),
        base = quote(base_branch),
    );
    let mut raw = run(distro, &script)?;
    let truncated = raw.len() > MAX_RAW;
    if truncated {
        let mut cut = MAX_RAW;
        while !raw.is_char_boundary(cut) {
            cut -= 1;
        }
        raw.truncate(cut);
    }
    // Secret values never reach the screen or the reviewer (Haiku).
    let (raw, secrets) = crate::env::mask_names(&raw);
    Ok(Changes { files: parse(&raw), raw, truncated, secrets })
}

pub fn parse(raw: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut in_hunk = false;
    let (mut old_no, mut new_no) = (0u32, 0u32);

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take());
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, p)| p).to_string();
            current = Some(FileDiff { path, status: "modified", added: 0, removed: 0, binary: false, lines: Vec::new() });
            in_hunk = false;
            continue;
        }
        let Some(file) = current.as_mut() else { continue };

        if let Some(header) = line.strip_prefix("@@") {
            in_hunk = true;
            (old_no, new_no) = hunk_start(header);
            file.lines.push(DiffLine { kind: LineKind::Hunk, old_no: None, new_no: None, text: line.to_string() });
            continue;
        }
        if !in_hunk {
            if line.starts_with("new file mode") {
                file.status = "added";
            } else if line.starts_with("deleted file mode") {
                file.status = "deleted";
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.status = "renamed";
                file.path = to.to_string();
            } else if line.starts_with("Binary files") {
                file.binary = true;
            }
            continue;
        }
        if let Some(text) = line.strip_prefix('+') {
            file.added += 1;
            file.lines.push(DiffLine { kind: LineKind::Add, old_no: None, new_no: Some(new_no), text: text.to_string() });
            new_no += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            file.removed += 1;
            file.lines.push(DiffLine { kind: LineKind::Remove, old_no: Some(old_no), new_no: None, text: text.to_string() });
            old_no += 1;
        } else if let Some(text) = line.strip_prefix(' ') {
            file.lines.push(DiffLine { kind: LineKind::Context, old_no: Some(old_no), new_no: Some(new_no), text: text.to_string() });
            old_no += 1;
            new_no += 1;
        }
        // "\ No newline at end of file" and anything else: ignored.
    }
    files.extend(current);
    files
}

/// " -12,3 +12,4 @@ fn x" -> (12, 12)
fn hunk_start(header: &str) -> (u32, u32) {
    let mut parts = header.split_whitespace();
    let number = |token: Option<&str>| {
        token
            .and_then(|t| t.get(1..))
            .and_then(|t| t.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    };
    (number(parts.next()), number(parts.next()))
}

/// One line per file, e.g. "frontend/README.md (added, +6 -0)".
pub fn diffstat(files: &[FileDiff]) -> String {
    files
        .iter()
        .map(|f| format!("{} ({}, +{} -{}{})", f.path, f.status, f.added, f.removed, if f.binary { ", binary" } else { "" }))
        .collect::<Vec<_>>()
        .join("\n")
}
