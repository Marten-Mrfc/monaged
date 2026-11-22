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

// ---- Keeping WSL's own service healthy -------------------------------------------
//
// Every `wsl.exe` asks WSL's Windows service (WslService) for a new session.
// Monaged used to start as many as it liked at once (git for every panel,
// diffs, reviews, checks, ten builders' tests), and a burst like that is what
// leaves the service failing every call with `Wsl/Service/E_UNEXPECTED`
// ("Catastrophic failure"), the Claude process dying with exit code -1 and
// `wsl -d Ubuntu` failing in your own terminal too. So: a few at a time, and
// when the service has broken, no more calls into it until it's restarted.

/// At most this many `wsl.exe` commands run at once. The Claude process,
/// dev servers and terminals are long-lived and don't count.
const MAX_PARALLEL: usize = 5;
/// Of which test runs and other heavy scripts (`run_heavy`), so ten
/// builders' reviews don't build and test all at once, and quick git calls
/// always find a free slot.
const MAX_HEAVY: usize = 2;

/// How long calls are refused after the service was seen broken (a restart
/// that works clears it sooner).
const DOWN_FOR: std::time::Duration = std::time::Duration::from_secs(45);

struct Slots {
    max: usize,
    used: std::sync::Mutex<usize>,
    freed: std::sync::Condvar,
}

static QUICK: Slots = Slots { max: MAX_PARALLEL - MAX_HEAVY, used: std::sync::Mutex::new(0), freed: std::sync::Condvar::new() };
static HEAVY: Slots = Slots { max: MAX_HEAVY, used: std::sync::Mutex::new(0), freed: std::sync::Condvar::new() };

thread_local! {
    /// This thread holds a slot already (a script that runs another inside it).
    static HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A turn to run `wsl.exe`; the next one waiting goes when it's dropped.
struct Slot {
    pool: Option<&'static Slots>,
}

fn take(pool: &'static Slots) -> Slot {
    if HELD.with(std::cell::Cell::get) {
        return Slot { pool: None };
    }
    let mut used = pool.used.lock().unwrap_or_else(|e| e.into_inner());
    while *used >= pool.max {
        used = pool.freed.wait(used).unwrap_or_else(|e| e.into_inner());
    }
    *used += 1;
    HELD.with(|h| h.set(true));
    Slot { pool: Some(pool) }
}

/// The longest a `wsl.exe` may take before Monaged stops it: a hung one
/// would otherwise hold its slot (and whatever waits on it, like a chat
/// waiting for its checkpoint) forever.
const QUICK_LIMIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);
/// Test runs, installs, `run_with_env` commands.
const HEAVY_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// Clones and setup steps that stream their progress.
const STREAM_LIMIT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Waits for `child` at most `limit`, stopping it when it runs over. `read`
/// takes stdout meanwhile. Returns how it ended and what it wrote to stderr.
fn wait_within(mut child: std::process::Child, limit: std::time::Duration, read: impl FnOnce(std::process::ChildStdout)) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    let stdout = child.stdout.take().context("wsl stdout missing")?;
    let mut stderr = child.stderr.take().context("wsl stderr missing")?;
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let child = Arc::new(Mutex::new(child));
    let fired = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel::<()>();
    let watchdog = {
        let (child, fired) = (child.clone(), fired.clone());
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(limit) {
                fired.store(true, Ordering::SeqCst);
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
            }
        })
    };
    read(stdout);
    // Its output can end before it does: the limit covers the wait too.
    let status = loop {
        let exited = child.lock().map_err(|_| anyhow::anyhow!("wsl.exe's handle was lost"))?.try_wait().context("waiting for wsl.exe failed")?;
        if let Some(status) = exited {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let _ = done.send(());
    let _ = watchdog.join();
    let stderr = errors.join().unwrap_or_default();
    if fired.load(Ordering::SeqCst) {
        bail!("wsl.exe didn't finish within {} minutes, so Monaged stopped it", limit.as_secs() / 60);
    }
    Ok((status, stderr))
}

impl Drop for Slot {
    fn drop(&mut self) {
        let Some(pool) = self.pool else { return };
        HELD.with(|h| h.set(false));
        let mut used = pool.used.lock().unwrap_or_else(|e| e.into_inner());
        *used = used.saturating_sub(1);
        pool.freed.notify_one();
    }
}

/// When WSL's service was last seen broken, and the code it gave.
static DOWN: std::sync::Mutex<Option<(std::time::Instant, String)>> = std::sync::Mutex::new(None);

/// The error code in what `wsl.exe` said when WSL's own service (not the
/// distro, not the script) has failed. The codes are the same in every
/// language ("Catastrophic failure" is "Onherstelbare fout" in Dutch).
pub fn service_error(text: &str) -> Option<String> {
    let text = text.replace('\0', "");
    // A missing or misnamed distro is your setup, not a broken service.
    if text.contains("WSL_E_DISTRO_NOT_FOUND") {
        return None;
    }
    const CODES: [&str; 8] = [
        "Wsl/Service/E_UNEXPECTED",
        "E_UNEXPECTED",
        "Wsl/Service/CreateInstance/CreateVm",
        "HCS_E_",
        "0x8000ffff",
        "0x800706be",
        "0x800706ba",
        "RPC_S_SERVER_UNAVAILABLE",
    ];
    let lower = text.to_ascii_lowercase();
    CODES.iter().find(|c| lower.contains(&c.to_ascii_lowercase())).map(|code| {
        // The whole code as wsl.exe printed it ("Wsl/Service/E_UNEXPECTED"), when there is one.
        text.split_whitespace().find(|w| w.to_ascii_lowercase().contains(&code.to_ascii_lowercase())).unwrap_or(code).trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '_').to_string()
    })
}

fn mark_down(code: String) {
    if let Ok(mut down) = DOWN.lock() {
        eprintln!("[wsl] WSL's service failed ({code}); holding wsl.exe calls until it's restarted");
        *down = Some((std::time::Instant::now(), code));
    }
}

/// WSL answers again.
pub fn mark_up() {
    if let Ok(mut down) = DOWN.lock() {
        *down = None;
    }
}

/// The code WSL's service failed with, while calls are held back.
pub fn down() -> Option<String> {
    let down = DOWN.lock().ok()?;
    down.as_ref().filter(|(at, _)| at.elapsed() < DOWN_FOR).map(|(_, code)| code.clone())
}

fn down_message(code: &str) -> String {
    format!("WSL's own service stopped working ({code}), so Monaged is holding its WSL commands until WSL restarts.")
}

/// Refuses to start a `wsl.exe` while the service is known to be broken.
fn check_up() -> Result<()> {
    if let Some(code) = down() {
        bail!(down_message(&code));
    }
    Ok(())
}

/// Notes a broken service from a failed `wsl.exe`'s output.
fn note_failure(text: &str) {
    if let Some(code) = service_error(text) {
        mark_down(code);
    }
}

/// How WSL is doing, from a command that does nothing.
#[derive(Debug, PartialEq)]
pub enum Health {
    Fine,
    /// WSL's service is broken (the code), or stopped answering.
    Broken(String),
    /// Something else failed (the distro is missing, …): not ours to restart.
    Other(String),
}

/// Starts `true` in the distro and says how that went (within 45 seconds).
pub fn probe(distro: &str) -> Health {
    match crate::distro::wsl_run(&["-d", distro, "--cd", "~", "--exec", "true"], std::time::Duration::from_secs(45)) {
        Ok((true, _)) => {
            mark_up();
            Health::Fine
        }
        Ok((false, text)) => match service_error(&text) {
            Some(code) => {
                mark_down(code.clone());
                Health::Broken(code)
            }
            None => Health::Other(text),
        },
        Err(err) => {
            let code = "wsl.exe stopped answering".to_string();
            eprintln!("[wsl] probe: {err:#}");
            mark_down(code.clone());
            Health::Broken(code)
        }
    }
}

/// `wsl --shutdown`: stops every distro and WSL's VM, which is how WSL gets
/// out of a broken state. What was running in WSL stops (it already had).
pub fn shutdown() -> Result<()> {
    let (ok, text) = crate::distro::wsl_run(&["--shutdown"], std::time::Duration::from_secs(120))?;
    if !ok {
        bail!("wsl --shutdown failed: {}", text.lines().take(3).collect::<Vec<_>>().join(" · "));
    }
    Ok(())
}

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
    run_in(&QUICK, QUICK_LIMIT, distro, script)
}

/// `run` for test runs and other scripts that take minutes and memory: only
/// a couple at a time, apart from the quick calls.
pub fn run_heavy(distro: &str, script: &str) -> Result<String> {
    run_in(&HEAVY, HEAVY_LIMIT, distro, script)
}

fn run_in(pool: &'static Slots, limit: std::time::Duration, distro: &str, script: &str) -> Result<String> {
    if !installed() {
        bail!(NOT_INSTALLED);
    }
    check_up()?;
    let _slot = take(pool);
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
    finish(child, limit)
}

/// Waits (within `limit`) for a script started with its stdout and stderr
/// piped; its trimmed stdout, or what went wrong.
fn finish(child: std::process::Child, limit: std::time::Duration) -> Result<String> {
    use std::io::Read;
    let mut out = Vec::new();
    let (status, err) = wait_within(child, limit, |mut stdout| {
        let _ = stdout.read_to_end(&mut out);
    })?;
    if !status.success() {
        let stderr = decode(&err);
        let stdout = decode(&out);
        let message = if stderr.trim().is_empty() { stdout } else { stderr };
        note_failure(&message);
        let short: Vec<&str> = message.trim().lines().take(4).collect();
        bail!("{}", short.join(" · "));
    }
    Ok(decode(&out).trim().to_string())
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
    check_up()?;
    let _slot = take(&HEAVY);
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
    let (status, err) = wait_within(child, STREAM_LIMIT, |mut stdout| {
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
    })?;
    if !status.success() {
        let stderr = decode(&err);
        let message = if stderr.trim().is_empty() { all.clone() } else { stderr };
        note_failure(&message);
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
    // Worked out before taking a slot: it may run a script of its own.
    let prelude = clean_prelude(distro);
    check_up()?;
    // Commands with secrets: tests and whatever you approved, so heavy.
    let _slot = take(&HEAVY);
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
    finish(child, HEAVY_LIMIT)
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

#[cfg(test)]
mod health_tests {
    use super::*;

    #[test]
    fn a_broken_service_is_recognised_in_any_language() {
        let dutch = "Onherstelbare fout\r\nFoutcode: Wsl/Service/E_UNEXPECTED\r\n";
        assert_eq!(service_error(dutch).as_deref(), Some("Wsl/Service/E_UNEXPECTED"));
        let english = "Catastrophic failure \nError code: Wsl/Service/E_UNEXPECTED";
        assert_eq!(service_error(english).as_deref(), Some("Wsl/Service/E_UNEXPECTED"));
        assert!(service_error("The remote procedure call failed. Error code: Wsl/Service/0x800706be").is_some());
        assert!(service_error("Error code: Wsl/Service/CreateInstance/CreateVm/HCS_E_SERVICE_NOT_AVAILABLE").is_some());
    }

    #[test]
    fn script_failures_and_a_missing_distro_are_not_a_broken_service() {
        assert_eq!(service_error("fatal: not a git repository"), None);
        assert_eq!(service_error("There is no distribution with the supplied name.\nError code: Wsl/Service/WSL_E_DISTRO_NOT_FOUND"), None);
    }

    #[test]
    fn calls_wait_for_a_free_slot() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NOW: AtomicUsize = AtomicUsize::new(0);
        static MOST: AtomicUsize = AtomicUsize::new(0);
        let threads: Vec<_> = (0..12)
            .map(|_| {
                std::thread::spawn(|| {
                    let _slot = take(&QUICK);
                    // Nested calls on the same thread don't wait for themselves.
                    let _inner = take(&QUICK);
                    let n = NOW.fetch_add(1, Ordering::SeqCst) + 1;
                    MOST.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    NOW.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert!(MOST.load(Ordering::SeqCst) <= MAX_PARALLEL - MAX_HEAVY);
    }

    #[test]
    fn a_hung_command_is_stopped_at_its_limit() {
        // Not wsl.exe, but the same waiting: a command that would run for a minute.
        let child = Command::new("sleep").arg("60").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let Ok(child) = child else { return }; // no `sleep` here (Windows)
        let started = std::time::Instant::now();
        let err = wait_within(child, std::time::Duration::from_millis(300), |_| {}).unwrap_err();
        assert!(err.to_string().contains("didn't finish"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn calls_are_held_while_the_service_is_down() {
        mark_down("Wsl/Service/E_UNEXPECTED".into());
        assert!(check_up().unwrap_err().to_string().contains("E_UNEXPECTED"));
        mark_up();
        assert!(check_up().is_ok());
    }
}
