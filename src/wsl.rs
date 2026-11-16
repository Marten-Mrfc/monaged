//! Helpers for running commands inside a WSL distro from Windows.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Keeps `wsl.exe` from flashing a console window when launched from a GUI app.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `wsl.exe -d <distro> --cd ~`, without a console window.
///
/// Every command starts in the Linux home. Without `--cd`, wsl.exe starts in
/// Monaged's own Windows folder (`/mnt/c/…`), which is slow, and WSL shows a
/// "Performance Tip" notification whenever git runs there.
pub fn command(distro: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "--cd", "~"]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Whether WSL is installed (see `onboarding::wsl_present`), remembered once
/// it is and checked again at most every few seconds while it isn't. Every
/// script goes through this first: on a PC without WSL, Windows' wsl.exe stub
/// may offer to install WSL by itself, and the script on its stdin would
/// answer that prompt.
pub fn installed() -> bool {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static LAST: Mutex<Option<(bool, Instant)>> = Mutex::new(None);
    let Ok(mut last) = LAST.lock() else { return crate::onboarding::wsl_present() };
    match *last {
        Some((true, _)) => true,
        Some((false, at)) if at.elapsed() < Duration::from_secs(5) => false,
        _ => {
            let present = crate::onboarding::wsl_present();
            *last = Some((present, Instant::now()));
            present
        }
    }
}

const NOT_INSTALLED: &str = "WSL isn't installed on this PC. Ctrl K → Setup guide installs it.";

/// What every script starts with: git never prompts (there's no terminal to
/// answer in), and neither does ssh, unless you set your own ssh command.
const PRELUDE: &str = "export GIT_TERMINAL_PROMPT=0\n\
[ -n \"$(git config --get core.sshCommand 2>/dev/null)\" ] || export GIT_SSH_COMMAND=\"ssh -o BatchMode=yes -o ConnectTimeout=20\"\n";

/// Puts the usual per-user tool folders (bun, nvm's newest node, cargo, go,
/// volta, deno, pnpm, ~/.local/bin) ahead of the rest of PATH when they
/// exist. Installers add them in ~/.bashrc, which stops early in a
/// non-interactive shell, so agents and checks otherwise found only Windows'
/// npm through interop (it can't run a project inside WSL) and no bun at all.
/// One line with no quotes in it, so it survives `wsl.exe … bash -lc`.
pub const TOOL_PATHS: &str = "for d in $HOME/.local/bin $HOME/go/bin $HOME/.cargo/bin $HOME/.volta/bin $HOME/.deno/bin \
$HOME/.local/share/pnpm $HOME/.bun/bin; do [ -d $d ] && case :$PATH: in *:$d:*) ;; *) PATH=$d:$PATH ;; esac; done; \
n=$(command -v node 2>/dev/null); case x$n in x|x/mnt/*) nb=; for v in $HOME/.nvm/versions/node/*/bin; do [ -x $v/node ] && nb=$v; done; \
[ -z $nb ] || PATH=$nb:$PATH ;; esac; export PATH; ";

/// `wsl.exe -d <distro> --cd ~ -- bash -lc <script>`, without a console window.
pub fn bash(distro: &str, script: &str) -> Command {
    let mut command = command(distro);
    command.args(["--", "bash", "-lc", script]);
    command
}

/// Runs a bash script in the distro and returns its trimmed stdout.
///
/// The script goes to `bash -s` over stdin rather than as an argument:
/// `wsl.exe` drops embedded double quotes from its arguments, which silently
/// changes scripts. Git never prompts for credentials here (it would hang
/// without a terminal).
pub fn run(distro: &str, script: &str) -> Result<String> {
    if !installed() {
        bail!(NOT_INSTALLED);
    }
    let mut command = command(distro);
    command.args(["--", "bash", "-l", "-s"]);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run wsl.exe — is WSL installed?")?;
    {
        let mut stdin = child.stdin.take().context("wsl stdin missing")?;
        stdin.write_all(format!("{PRELUDE}{TOOL_PATHS}\n{script}\n").as_bytes())?;
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

/// Runs a script and hands over its output as it comes, split at line ends
/// and at carriage returns (git redraws its progress with `\r`). stderr is
/// merged into stdout. Returns all of it; fails when the script does.
pub fn stream(distro: &str, script: &str, on_line: impl FnMut(&str)) -> Result<String> {
    let mut command = command(distro);
    command.args(["--", "bash", "-l", "-s"]);
    stream_with(command, script, on_line)
}

/// `stream` as root (`wsl.exe -u root`): installs Linux packages without
/// asking for the Linux password, as WSL lets the Windows user do.
pub fn stream_root(distro: &str, script: &str, on_line: impl FnMut(&str)) -> Result<String> {
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "-u", "root", "--cd", "/", "--exec", "bash", "-s"]);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    stream_with(command, script, on_line)
}

fn stream_with(mut command: Command, script: &str, mut on_line: impl FnMut(&str)) -> Result<String> {
    if !installed() {
        bail!(NOT_INSTALLED);
    }
    use std::io::Read;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run wsl.exe — is WSL installed?")?;
    {
        let mut stdin = child.stdin.take().context("wsl stdin missing")?;
        stdin.write_all(format!("{PRELUDE}exec 2>&1\n{script}\n").as_bytes())?;
    }
    let mut stdout = child.stdout.take().context("wsl stdout missing")?;
    let mut all = String::new();
    let mut line: Vec<u8> = Vec::new();
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
    let output = child.wait_with_output().context("waiting for wsl.exe failed")?;
    if !output.status.success() {
        let stderr = decode(&output.stderr);
        let message = if stderr.trim().is_empty() { all.clone() } else { stderr };
        let tail: Vec<&str> = message.trim().lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
        bail!("{}", tail.join("\n"));
    }
    Ok(all)
}

/// The PATH a login shell sets up (nvm, ~/.local/bin…), worked out once per
/// distro without any secrets in the environment.
pub fn login_path(distro: &str) -> String {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static PATHS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    if let Some(path) = PATHS.lock().ok().and_then(|p| p.get(distro).cloned()) {
        return path;
    }
    let path = run(distro, "printf '%s' \"$PATH\"").unwrap_or_else(|_| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
    if let Ok(mut paths) = PATHS.lock() {
        paths.insert(distro.to_string(), path.clone());
    }
    path
}

/// For scripts that carry secrets: bash without startup files (an agent can
/// write ~/.profile or ~/.bashrc, and they'd run with the secrets set), no
/// BASH_ENV, and the login shell's PATH.
pub fn clean_prelude(distro: &str) -> String {
    format!("unset BASH_ENV ENV PROMPT_COMMAND\nexport PATH={}\n", quote(&login_path(distro)))
}

/// `wsl.exe … bash --noprofile --norc -s`, for scripts that carry secrets.
pub fn clean_command(distro: &str) -> Command {
    let mut command = command(distro);
    command.args(["--", "bash", "--noprofile", "--norc", "-s"]);
    command
}

/// Like `run`, in a clean shell (see `clean_prelude`).
pub fn run_clean(distro: &str, script: &str) -> Result<String> {
    if !installed() {
        bail!(NOT_INSTALLED);
    }
    let prelude = clean_prelude(distro);
    let mut child = clean_command(distro)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run wsl.exe — is WSL installed?")?;
    {
        let mut stdin = child.stdin.take().context("wsl stdin missing")?;
        stdin.write_all(format!("{PRELUDE}{prelude}{script}\n").as_bytes())?;
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

/// Opens a terminal (your default one: Windows Terminal or the console)
/// with a shell in `path` inside the distro.
pub fn open_terminal(distro: &str, path: &str) -> Result<()> {
    let script = format!("cd {} 2>/dev/null || cd ~; exec \"${{SHELL:-bash}}\" -l", shell_path(path));
    let mut command = Command::new("wsl.exe");
    command.args(["-d", distro, "--cd", "~", "--", "bash", "-c", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // A console of its own (the app has none to share).
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        command.creation_flags(CREATE_NEW_CONSOLE);
    }
    command.spawn().map(|_| ()).context("couldn't start wsl.exe")
}
