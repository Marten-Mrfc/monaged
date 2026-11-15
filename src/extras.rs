//! Installing the setup guide's extras from inside Monaged, without a
//! terminal: Windows programs through winget (hidden; Windows still asks for
//! permission when an installer needs it), Linux packages through apt as root
//! in WSL (no Linux password needed: WSL lets the Windows user do that).

use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Extra {
    Zed,
    Gh,
    Sandbox,
    Pytest,
    /// curl and tar, which downloading the Claude agent needs.
    Curl,
}

impl Extra {
    pub fn title(self) -> &'static str {
        match self {
            Extra::Zed => "Zed",
            Extra::Gh => "the GitHub CLI",
            Extra::Sandbox => "bubblewrap and socat",
            Extra::Pytest => "pytest",
            Extra::Curl => "curl and tar",
        }
    }

    /// The winget package, for Windows programs.
    pub fn winget_id(self) -> Option<&'static str> {
        match self {
            Extra::Zed => Some("ZedIndustries.Zed"),
            Extra::Gh => Some("GitHub.cli"),
            _ => None,
        }
    }

    /// The apt packages, for Linux tools in WSL.
    pub fn apt_packages(self) -> &'static [&'static str] {
        match self {
            Extra::Sandbox => &["bubblewrap", "socat"],
            Extra::Pytest => &["python3-pytest"],
            Extra::Curl => &["curl", "tar", "ca-certificates"],
            _ => &[],
        }
    }

    /// Where to get it by hand when winget isn't there.
    pub fn website(self) -> Option<&'static str> {
        match self {
            Extra::Zed => Some("https://zed.dev/download"),
            Extra::Gh => Some("https://cli.github.com"),
            _ => None,
        }
    }
}

/// Progress from one line of output: what to show (None: keep the last) and
/// how far along (None: unknown).
pub fn read_progress(line: &str) -> (Option<String>, Option<f32>) {
    let line = line.trim();
    // apt's machine-readable status (APT::Status-Fd): "pmstatus:pkg:42.5:Unpacking pkg".
    for kind in ["dlstatus:", "pmstatus:"] {
        if let Some(rest) = line.strip_prefix(kind) {
            let mut parts = rest.splitn(3, ':');
            let (_, percent, message) = (parts.next(), parts.next(), parts.next());
            let fraction = percent.and_then(|p| p.trim().parse::<f32>().ok()).map(|p| (p / 100.0).clamp(0.0, 1.0));
            return (message.map(|m| m.trim().to_string()).filter(|m| !m.is_empty()), fraction);
        }
    }
    // winget's spinner (- \ | /) and empty progress bars say nothing.
    let meaningful = line.chars().any(|c| c.is_alphanumeric());
    if !meaningful {
        return (None, None);
    }
    // winget: "███▒▒▒  10.5 MB / 12.3 MB"
    if let Some(fraction) = sizes_fraction(line) {
        let text: String = line.chars().filter(|c| !matches!(c, '█' | '▒' | '▓' | '░')).collect();
        return (Some(text.trim().to_string()), Some(fraction));
    }
    let percent = crate::distro::percent_in(line);
    let text: String = line.chars().filter(|c| !matches!(c, '█' | '▒' | '▓' | '░')).collect();
    (Some(text.trim().to_string()), percent)
}

/// "10.5 MB / 12.3 MB" → 0.85.
fn sizes_fraction(line: &str) -> Option<f32> {
    let (left, right) = line.split_once(" / ")?;
    let size = |words: Vec<&str>| -> Option<f64> {
        let (number, unit) = (words.first()?, words.get(1)?);
        let value: f64 = number.replace(',', ".").parse().ok()?;
        let scale = match unit.to_uppercase().as_str() {
            "B" => 1.0,
            "KB" => 1e3,
            "MB" => 1e6,
            "GB" => 1e9,
            _ => return None,
        };
        Some(value * scale)
    };
    let mut before: Vec<&str> = left.split_whitespace().rev().take(2).collect();
    before.reverse();
    let after: Vec<&str> = right.split_whitespace().take(2).collect();
    let (done, total) = (size(before)?, size(after)?);
    (total > 0.0).then(|| (done / total).clamp(0.0, 1.0) as f32)
}

/// Whether winget is on this PC.
pub fn has_winget() -> bool {
    quiet("winget").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn quiet(program: &str) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    command
}

/// `winget install` without a window, line by line.
pub fn winget_install(id: &str, mut on_line: impl FnMut(&str)) -> Result<()> {
    use std::io::Read;
    if !has_winget() {
        bail!("winget isn't on this PC (it comes with \"App Installer\" from the Microsoft Store), so Monaged can't install this by itself.");
    }
    let mut child = quiet("winget")
        .args([
            "install",
            "--exact",
            "--id",
            id,
            "--source",
            "winget",
            "--accept-package-agreements",
            "--accept-source-agreements",
            "--disable-interactivity",
            "--silent",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run winget")?;
    let mut stdout = child.stdout.take().context("winget's output is missing")?;
    let mut all = String::new();
    let mut line = Vec::new();
    let mut buf = [0u8; 4096];
    let mut flush = |line: &mut Vec<u8>, all: &mut String| {
        if !line.is_empty() {
            let text = String::from_utf8_lossy(line).into_owned();
            on_line(&text);
            all.push_str(&text);
            all.push('\n');
            line.clear();
        }
    };
    loop {
        let n = stdout.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                flush(&mut line, &mut all);
            } else {
                line.push(b);
            }
        }
    }
    flush(&mut line, &mut all);
    let output = child.wait_with_output().context("winget stopped")?;
    // Already installed counts as installed.
    let code = output.status.code().unwrap_or(-1) as u32;
    if output.status.success() || code == 0x8A15_002B || all.contains("already installed") {
        refresh_path();
        return Ok(());
    }
    if code == 0x8A15_0014 {
        bail!("winget couldn't find {id}; its package list may be out of date.");
    }
    let said: Vec<String> = all.lines().map(str::trim).filter(|l| l.chars().any(char::is_alphanumeric)).map(String::from).collect();
    let tail = said[said.len().saturating_sub(2)..].join(" ");
    bail!("winget couldn't install it (code 0x{code:08X}){}", if tail.is_empty() { String::new() } else { format!(": {tail}") });
}

/// The script that installs Linux packages as root, with apt's progress.
pub fn apt_script(packages: &[&str]) -> String {
    format!(
        "set -e\nexport DEBIAN_FRONTEND=noninteractive\n\
         echo 'Updating the package list…'\n\
         apt-get -o DPkg::Lock::Timeout=300 -o APT::Status-Fd=1 update -q\n\
         apt-get -o DPkg::Lock::Timeout=300 -o APT::Status-Fd=1 -o Dpkg::Use-Pty=0 install -y -q --no-install-recommends {}\n",
        packages.join(" ")
    )
}

/// `apt-get install` in the distro as root, line by line.
pub fn apt_install(distro: &str, packages: &[&str], on_line: impl FnMut(&str)) -> Result<()> {
    crate::wsl::stream_root(distro, &apt_script(packages), on_line).map(|_| ())
}

/// Picks up a PATH that an installer just changed: Monaged started with the
/// old one, so a program winget just installed wouldn't be found otherwise.
pub fn refresh_path() {
    #[cfg(windows)]
    {
        let read = |key: &str| -> Option<String> {
            let out = quiet("reg.exe").args(["query", key, "/v", "Path"]).output().ok()?;
            let text = String::from_utf8_lossy(&out.stdout).to_string();
            let line = text.lines().find(|l| l.trim_start().starts_with("Path"))?;
            let at = line.find("REG_")?;
            let rest = &line[at..];
            let value = rest.split_once(char::is_whitespace)?.1.trim().to_string();
            Some(expand(&value))
        };
        let machine = read(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment").unwrap_or_default();
        let user = read(r"HKCU\Environment").unwrap_or_default();
        let current = std::env::var("PATH").unwrap_or_default();
        let links = std::env::var("LOCALAPPDATA").map(|l| format!(r"{l}\Microsoft\WinGet\Links")).unwrap_or_default();
        let joined = merge_paths(&[&machine, &user, &current, &links]);
        // SAFETY: Windows' environment block is locked by the system; Monaged
        // doesn't hand environment pointers to C code that could race this.
        unsafe { std::env::set_var("PATH", joined) };
    }
}

/// Expands %VARIABLES% in a registry path value.
#[cfg_attr(not(windows), allow(dead_code))]
fn expand(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// PATH lists joined in order, each folder once.
pub fn merge_paths(lists: &[&str]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for list in lists {
        for dir in list.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            if seen.insert(dir.trim_end_matches('\\').to_lowercase()) {
                out.push(dir.to_string());
            }
        }
    }
    out.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_apt_and_winget_progress() {
        let (text, fraction) = read_progress("pmstatus:socat:42.8571:Unpacking socat (amd64)");
        assert_eq!(text.as_deref(), Some("Unpacking socat (amd64)"));
        assert!((fraction.unwrap() - 0.428571).abs() < 1e-4);
        assert_eq!(read_progress("dlstatus:1:12.5:Retrieving file 1 of 8").1, Some(0.125));
        let (text, fraction) = read_progress("  ██████████▒▒▒▒▒▒▒▒▒▒  5.00 MB / 10.0 MB");
        assert_eq!(text.as_deref(), Some("5.00 MB / 10.0 MB"));
        assert_eq!(fraction, Some(0.5));
        assert_eq!(read_progress("  -  "), (None, None));
        assert_eq!(read_progress("Successfully installed").0.as_deref(), Some("Successfully installed"));
    }

    #[test]
    fn the_apt_script_installs_what_was_asked() {
        let script = apt_script(&["bubblewrap", "socat"]);
        assert!(script.contains("install -y -q --no-install-recommends bubblewrap socat"));
        assert!(script.contains("APT::Status-Fd=1"));
        assert!(script.contains("DEBIAN_FRONTEND=noninteractive"));
    }

    #[test]
    fn merges_paths_once_each() {
        assert_eq!(merge_paths(&[r"C:\A;C:\B\", r"c:\b;C:\C", ""]), r"C:\A;C:\B\;C:\C");
    }

    #[test]
    fn expands_variables_it_knows() {
        // SAFETY: tests in this module don't read this variable concurrently.
        unsafe { std::env::set_var("MONAGED_TEST_DIR", r"C:\Tools") };
        assert_eq!(expand(r"%MONAGED_TEST_DIR%\bin;%NOPE_NOT_SET%\x;50%"), r"C:\Tools\bin;%NOPE_NOT_SET%\x;50%");
    }
}
