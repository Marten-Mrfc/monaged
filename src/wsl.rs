//! Helpers for running commands inside a WSL distro from Windows.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Keeps `wsl.exe` from flashing a console window when launched from a GUI app.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `wsl.exe -d <distro> -- bash -lc <script>`, without a console window.
pub fn bash(distro: &str, script: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "--", "bash", "-lc", script]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Runs a bash script in the distro and returns its trimmed stdout.
///
/// The script goes to `bash -s` over stdin rather than as an argument:
/// `wsl.exe` drops embedded double quotes from its arguments, which silently
/// changes scripts. Git never prompts for credentials here (it would hang
/// without a terminal).
pub fn run(distro: &str, script: &str) -> Result<String> {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "--", "bash", "-l", "-s"]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run wsl.exe — is WSL installed?")?;
    {
        let mut stdin = child.stdin.take().context("wsl stdin missing")?;
        stdin.write_all(format!("export GIT_TERMINAL_PROMPT=0\n{script}\n").as_bytes())?;
    }
    let output = child.wait_with_output().context("waiting for wsl.exe failed")?;
    if !output.status.success() {
        let stderr = decode(&output.stderr);
        let stdout = decode(&output.stdout);
        let message = if stderr.trim().is_empty() { stdout } else { stderr };
        let short: Vec<&str> = message.trim().lines().take(4).collect();
        bail!("{}", short.join(" · "));
    }
    Ok(decode(&output.stdout).trim().to_string())
}

/// Single-quotes a string for bash.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Quotes a path for bash but keeps a leading `~` working.
pub fn shell_path(path: &str) -> String {
    let path = path.trim();
    if path == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", quote(rest))
    } else {
        quote(path)
    }
}

/// Linux programs print UTF-8, but `wsl.exe`'s own errors are UTF-16LE.
pub fn decode(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2 && bytes.iter().skip(1).step_by(2).take(16).all(|b| *b == 0);
    if looks_utf16 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}
