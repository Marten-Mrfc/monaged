//! Opening folders and files in Zed, connected to WSL.

use std::process::{Command, Stdio};

use anyhow::{Context as _, Result};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `zed --wsl <user>@<distro> <path>`: Zed opens the Linux path through its WSL remote.
pub fn open(distro: &str, user: &str, path: &str) -> Result<()> {
    let mut command = Command::new("zed");
    command.args(["--wsl", &format!("{user}@{distro}"), path]);
    #[cfg(windows)]
    command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start Zed — is the `zed` command on your PATH?")?;
    Ok(())
}
