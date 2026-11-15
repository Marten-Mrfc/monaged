//! Installing a Linux distribution for WSL inside Monaged, with real progress,
//! instead of a console window that says "This might take a while...":
//!
//! 1. **Download**: the image comes from WSL's own catalog (the file
//!    `wsl --install` reads), with its SHA-256, so Monaged shows megabytes and
//!    speed and checks the file.
//! 2. **Install**: `wsl --install --from-file` registers it (`wsl --import` on
//!    WSL versions that don't know that).
//! 3. **First start**: Ubuntu sets itself up with cloud-init; Monaged waits for
//!    it and shows its stages.
//! 4. **Your user**: what Ubuntu's own first start would ask for in a console
//!    (a username and password), from the guide's fields: the user is created
//!    in the usual groups, made the default, and the console prompt is turned off.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// WSL's distribution catalog.
pub const CATALOG_URL: &str = "https://raw.githubusercontent.com/microsoft/WSL/master/distributions/DistributionInfo.json";

/// The steps of a full install, as the guide lists them.
pub const STEPS: [&str; 4] = ["Download Ubuntu", "Install it in WSL", "First start", "Create your Linux user"];
/// Just the last two, for a distro that's there but only has root.
pub const USER_STEPS: [&str; 2] = ["First start", "Create your Linux user"];

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub name: String,
    pub friendly: String,
    pub url: String,
    pub sha256: String,
}

/// What the worker reports, in order.
pub enum Event {
    /// Step `index` is running: a detail line and its progress (None: unknown).
    Running(usize, String, Option<f32>),
    /// Step `index` is done, with what to show under it.
    Done(usize, String),
}

/// The image to install for `flavor` (e.g. "Ubuntu") from the catalog: the
/// default entry, for this PC's CPU.
pub fn pick_image(catalog: &Value, flavor: &str, arm64: bool) -> Option<Image> {
    let entries = catalog["ModernDistributions"][flavor].as_array()?;
    let entry = entries.iter().find(|e| e["Default"].as_bool() == Some(true)).or_else(|| entries.first())?;
    let file = if arm64 { &entry["Arm64Url"] } else { &entry["Amd64Url"] };
    Some(Image {
        name: entry["Name"].as_str()?.to_string(),
        friendly: entry["FriendlyName"].as_str().unwrap_or(flavor).to_string(),
        url: file["Url"].as_str()?.to_string(),
        sha256: file["Sha256"].as_str()?.trim_start_matches("0x").to_lowercase(),
    })
}

/// Windows on ARM (Monaged itself runs as x64 there, so ask the system).
pub fn is_arm64() -> bool {
    ["PROCESSOR_ARCHITEW6432", "PROCESSOR_ARCHITECTURE"].iter().any(|v| std::env::var(v).is_ok_and(|a| a.eq_ignore_ascii_case("ARM64")))
        || std::env::var("PROCESSOR_IDENTIFIER").is_ok_and(|id| id.to_uppercase().starts_with("ARM"))
}

/// A username Ubuntu accepts (the same rule as its own first start).
pub fn check_username(name: &str) -> Result<(), &'static str> {
    let mut chars = name.chars();
    match chars.next() {
        None => return Err("Pick a username."),
        Some(c) if !(c.is_ascii_lowercase() || c == '_') => return Err("Start with a lowercase letter or _."),
        _ => {}
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err("Use lowercase letters, digits, _ and - only.");
    }
    if name.len() > 32 {
        return Err("Keep it to 32 characters.");
    }
    if name == "root" {
        return Err("Pick a name other than root.");
    }
    Ok(())
}

/// A username suggested from the Windows one, like Ubuntu's first start does.
pub fn suggest_username(windows_name: &str) -> String {
    let lower = windows_name.trim().to_lowercase().replace(' ', "_");
    let mut name: String = lower.chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-').collect();
    while name.chars().next().is_some_and(|c| !(c.is_ascii_lowercase() || c == '_')) {
        name.remove(0);
    }
    name.truncate(32);
    name
}

pub fn check_password(password: &str, again: &str) -> Result<(), &'static str> {
    if password.is_empty() {
        return Err("Pick a password: installing Linux packages (sudo) asks for it.");
    }
    if password.contains(['\n', '\r']) || password.contains(PASSWORD_END) {
        return Err("That password can't be used here; pick another.");
    }
    if password != again {
        return Err("The two passwords aren't the same.");
    }
    Ok(())
}

/// The last percentage in a line of output ("[=== 42.5% ]", "42,5%").
pub fn percent_in(line: &str) -> Option<f32> {
    let bytes = line.as_bytes();
    let at = line.rfind('%')?;
    let start = bytes[..at].iter().rposition(|b| !(b.is_ascii_digit() || *b == b'.' || *b == b',')).map_or(0, |i| i + 1);
    let number = line[start..at].replace(',', ".");
    let value: f32 = number.trim_matches('.').parse().ok()?;
    (0.0..=100.0).contains(&value).then_some(value / 100.0)
}

/// A program without a console window; output as UTF-8.
fn quiet(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env("WSL_UTF8", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::wsl::CREATE_NO_WINDOW);
    }
    command
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

fn elapsed(since: Instant) -> String {
    let s = since.elapsed().as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// Where Monaged keeps the download until it's installed.
fn download_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    base.join("Monaged").join("downloads")
}

fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(3 * 60 * 60)))
        .build()
        .into()
}

fn fetch_catalog() -> Result<Value> {
    let mut response = http().get(CATALOG_URL).call().context("couldn't reach GitHub for WSL's list of distributions")?;
    if response.status().as_u16() != 200 {
        bail!("GitHub answered {} for WSL's list of distributions", response.status().as_u16());
    }
    response.body_mut().read_json().context("WSL's list of distributions was unreadable")
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads the image with progress, checking its SHA-256 on the way.
fn download(image: &Image, to: &Path, send: &(dyn Fn(Event) + Sync)) -> Result<()> {
    if to.exists() && sha256_file(to).is_ok_and(|h| h == image.sha256) {
        return Ok(());
    }
    let part = to.with_extension("part");
    let mut response = http().get(&image.url).call().with_context(|| format!("couldn't reach {}", image.url))?;
    if response.status().as_u16() != 200 {
        bail!("the download answered {}", response.status().as_u16());
    }
    let total: Option<u64> = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
    let mut file = std::fs::File::create(&part).with_context(|| format!("couldn't write {}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let (started, mut last) = (Instant::now(), Instant::now() - Duration::from_secs(1));
    let mut done: u64 = 0;
    loop {
        let n = body.read(&mut buf).context("the download broke off; try again")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).context("couldn't write the download (is the disk full?)")?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            last = Instant::now();
            let speed = done as f64 / started.elapsed().as_secs_f64().max(0.1) / 1_000_000.0;
            let detail = match total {
                Some(t) => format!("{} of {} · {speed:.1} MB/s", megabytes(done), megabytes(t)),
                None => format!("{} · {speed:.1} MB/s", megabytes(done)),
            };
            send(Event::Running(0, detail, total.map(|t| done as f32 / t.max(1) as f32)));
        }
    }
    drop(file);
    let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if hash != image.sha256 {
        let _ = std::fs::remove_file(&part);
        bail!("the download doesn't match WSL's checksum, so it wasn't used; try again");
    }
    std::fs::rename(&part, to).with_context(|| format!("couldn't save {}", to.display()))?;
    Ok(())
}

/// Runs wsl.exe with a time limit; (succeeded, what it printed).
pub(crate) fn wsl_run(args: &[&str], limit: Duration) -> Result<(bool, String)> {
    let mut child = quiet("wsl.exe")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run wsl.exe")?;
    let mut out = child.stdout.take().context("wsl.exe output missing")?;
    let mut err = child.stderr.take().context("wsl.exe output missing")?;
    let reader = std::thread::spawn(move || {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let _ = out.read_to_end(&mut a);
        let _ = err.read_to_end(&mut b);
        (a, b)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            bail!("wsl.exe didn't finish within {} minutes", limit.as_secs() / 60);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let (a, b) = reader.join().unwrap_or_default();
    let text = format!("{}\n{}", crate::wsl::decode(&a), crate::wsl::decode(&b)).replace('\0', "");
    Ok((status.success(), text.trim().to_string()))
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" ")
}

/// Runs `work` while step `index` shows `label` and the time it has taken,
/// every second (WSL says nothing while it unpacks).
fn ticking<T>(index: usize, label: &str, send: &(dyn Fn(Event) + Sync), work: impl FnOnce() -> T) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    let started = Instant::now();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                send(Event::Running(index, format!("{label} · {}", elapsed(started)), None));
                for _ in 0..10 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        let out = work();
        stop.store(true, Ordering::Relaxed);
        out
    })
}

/// Registers the downloaded image as `name`.
fn register(name: &str, file: &Path, send: &(dyn Fn(Event) + Sync)) -> Result<()> {
    let started = Instant::now();
    let path = file.display().to_string();
    let (ok, out) = ticking(1, "Unpacking it into WSL", send, || wsl_run(&["--install", "--from-file", &path, "--name", name, "--no-launch"], Duration::from_secs(20 * 60)))?;
    if ok {
        send(Event::Done(1, format!("Installed as {name} in {}", elapsed(started))));
        return Ok(());
    }
    // Older WSL: no --from-file. Import it instead (no Start menu entry, same distro).
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let location = base.join("Monaged").join("wsl").join(name);
    std::fs::create_dir_all(&location).with_context(|| format!("couldn't create {}", location.display()))?;
    let location = location.display().to_string();
    let (ok2, out2) = ticking(1, "Unpacking it into WSL (the older way)", send, || wsl_run(&["--import", name, &location, &path, "--version", "2"], Duration::from_secs(20 * 60)))?;
    if !ok2 {
        bail!("WSL couldn't install it: {} / {}", last_lines(&out, 2), last_lines(&out2, 2));
    }
    send(Event::Done(1, format!("Installed as {name} in {}", elapsed(started))));
    Ok(())
}

/// Ends what the provisioning script prints the password inside.
const PASSWORD_END: &str = "MONAGED_PASSWORD_END";

/// Ubuntu's own first start, without the console: wait for cloud-init (with
/// its stage), create the user in the usual groups, set the password, make
/// it the default. Runs as root; the password only travels over stdin.
pub fn provision_script(user: &str, password: &str) -> String {
    r#"set -u
echo "MONAGED_STAGE first-start"
if command -v cloud-init >/dev/null 2>&1 && systemctl is-enabled --quiet cloud-init-local.service 2>/dev/null; then
  ( while sleep 2; do
      echo "MONAGED_CI $(cloud-init status --format json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("stage") or "-") + " " + (d.get("status") or "-"))' 2>/dev/null)"
    done ) &
  watcher=$!
  timeout 900 cloud-init status --wait >/dev/null 2>&1 || true
  kill "$watcher" 2>/dev/null || true
  touch /etc/cloud/cloud-init.disabled 2>/dev/null || true
fi
echo "MONAGED_STAGE user"
set -e
u='__USER__'
if ! id "$u" >/dev/null 2>&1; then
  if command -v adduser >/dev/null 2>&1 && adduser --help 2>&1 | grep -q -- '--disabled-password'; then
    adduser --quiet --gecos '' --disabled-password "$u"
  else
    useradd -m -s /bin/bash "$u"
  fi
fi
for g in adm cdrom sudo dip plugdev wheel; do
  if getent group "$g" >/dev/null; then usermod -aG "$g" "$u"; fi
done
chpasswd <<'MONAGED_PASSWORD_END'
__USER__:__PASSWORD__
MONAGED_PASSWORD_END
conf=/etc/wsl.conf
touch "$conf"
if grep -q '^\[user\]' "$conf"; then
  if sed -n '/^\[user\]/,/^\[/{/^\s*default\s*=/p}' "$conf" | grep -q .; then
    sed -i '/^\[user\]/,/^\[/{s/^\s*default\s*=.*/default='"$u"'/}' "$conf"
  else
    sed -i '/^\[user\]/a\default='"$u" "$conf"
  fi
else
  printf '\n[user]\ndefault=%s\n' "$u" >> "$conf"
fi
echo "MONAGED_UID $(id -u "$u")"
echo "MONAGED_DONE"
"#
    .replace("__USER__", user)
    .replace("__PASSWORD__", password)
}

/// cloud-init's stage as words and a share of the first start.
pub fn stage_progress(stage: &str, status: &str) -> (String, Option<f32>) {
    if status == "done" {
        return ("Done".into(), Some(1.0));
    }
    match stage {
        "init-local" => ("Starting up".into(), Some(0.15)),
        "init" | "init-network" => ("Setting up the system".into(), Some(0.4)),
        "modules-config" => ("Configuring".into(), Some(0.65)),
        "modules-final" => ("Finishing".into(), Some(0.85)),
        _ => ("Starting up".into(), None),
    }
}

/// The registry key WSL keeps for a distro (HKCU\...\Lxss\{guid}).
fn registry_key(name: &str) -> Option<String> {
    let out = quiet("reg.exe")
        .args(["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss", "/s"])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut key: Option<String> = None;
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            key = Some(line.trim().to_string());
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let ["DistributionName", "REG_SZ", rest @ ..] = parts.as_slice()
            && rest.join(" ").eq_ignore_ascii_case(name)
        {
            return key;
        }
    }
    None
}

/// Runs the first start and makes the user, reporting as steps `first`
/// (first start) and `first + 1` (the user).
pub fn provision(name: &str, user: &str, password: &str, first: usize, send: &(dyn Fn(Event) + Sync)) -> Result<()> {
    let started = Instant::now();
    send(Event::Running(first, "Starting Linux for the first time…".into(), None));
    let mut child = quiet("wsl.exe")
        .args(["-d", name, "-u", "root", "--cd", "/", "--exec", "bash", "-s"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't run wsl.exe")?;
    {
        let mut stdin = child.stdin.take().context("wsl.exe input missing")?;
        stdin.write_all(provision_script(user, password).as_bytes())?;
    }
    let stdout = child.stdout.take().context("wsl.exe output missing")?;
    let mut stderr = child.stderr.take().context("wsl.exe output missing")?;
    let errors = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stderr.read_to_end(&mut text);
        crate::wsl::decode(&text)
    });
    let (mut uid, mut finished, mut on_user) = (None, false, false);
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        if let Some(rest) = line.strip_prefix("MONAGED_CI ") {
            let mut words = rest.split_whitespace();
            let (stage, fraction) = stage_progress(words.next().unwrap_or("-"), words.next().unwrap_or("-"));
            send(Event::Running(first, format!("{stage} · {}", elapsed(started)), fraction));
        } else if line == "MONAGED_STAGE user" {
            on_user = true;
            send(Event::Done(first, format!("Ready in {}", elapsed(started))));
            send(Event::Running(first + 1, format!("Creating {user}…"), None));
        } else if let Some(id) = line.strip_prefix("MONAGED_UID ") {
            uid = id.trim().parse::<u32>().ok();
        } else if line == "MONAGED_DONE" {
            finished = true;
        }
    }
    let status = child.wait()?;
    let errors = errors.join().unwrap_or_default();
    if !status.success() || !finished {
        let why = last_lines(&errors, 3);
        bail!("{} didn't finish{}", if on_user { "Creating your user" } else { "The first start" }, if why.is_empty() { String::new() } else { format!(": {why}") });
    }
    // The new default user applies from the next start; Ubuntu's own console
    // setup isn't needed any more.
    let _ = wsl_run(&["--terminate", name], Duration::from_secs(60));
    if let Some(key) = registry_key(name) {
        let set = |value: &str, data: &str| {
            let _ = quiet("reg.exe").args(["add", &key, "/v", value, "/t", "REG_DWORD", "/d", data, "/f"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        };
        set("RunOOBE", "0");
        if let Some(uid) = uid {
            set("DefaultUid", &uid.to_string());
        }
    }
    send(Event::Done(first + 1, format!("{user}, the default user in {name}")));
    Ok(())
}

/// An install is running (it carries on when the guide is closed).
pub static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The whole install: download, register, first start, user. Returns the
/// distro's name in WSL (the catalog's, e.g. "Ubuntu"; another one if that's taken).
pub fn install(user: &str, password: &str, taken: &[String], send: &(dyn Fn(Event) + Sync)) -> Result<String> {
    send(Event::Running(0, "Looking up the newest Ubuntu…".into(), None));
    let catalog = fetch_catalog()?;
    let image = pick_image(&catalog, "Ubuntu", is_arm64()).context("WSL's list of distributions has no Ubuntu")?;
    let name = free_name(&image.name, taken);
    let dir = download_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let file_name = image.url.rsplit('/').next().filter(|n| n.ends_with(".wsl") || n.ends_with(".tar.gz")).unwrap_or("ubuntu.wsl").to_string();
    let file = dir.join(&file_name);
    send(Event::Running(0, format!("{} · starting the download…", image.friendly), Some(0.0)));
    download(&image, &file, send)?;
    send(Event::Done(0, format!("{} · {} · checksum OK", image.friendly, std::fs::metadata(&file).map(|m| megabytes(m.len())).unwrap_or_default())));
    register(&name, &file, send)?;
    // The image isn't needed once WSL has it.
    let _ = std::fs::remove_file(&file);
    provision(&name, user, password, 2, send)?;
    Ok(name)
}

/// `wanted`, or `wanted-2`, `wanted-3`… when WSL already has a distro by that name.
pub fn free_name(wanted: &str, taken: &[String]) -> String {
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if free(wanted) {
        return wanted.to_string();
    }
    (2..).map(|i| format!("{wanted}-{i}")).find(|n| free(n)).unwrap_or_else(|| wanted.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_default_image_for_the_cpu() {
        let catalog = serde_json::json!({
            "ModernDistributions": { "Ubuntu": [
                { "Name": "Ubuntu-24.04", "FriendlyName": "Ubuntu 24.04 LTS", "Default": false,
                  "Amd64Url": { "Url": "https://x/old-amd64.wsl", "Sha256": "aa" } },
                { "Name": "Ubuntu", "FriendlyName": "Ubuntu", "Default": true,
                  "Amd64Url": { "Url": "https://x/new-amd64.wsl", "Sha256": "0xBB" },
                  "Arm64Url": { "Url": "https://x/new-arm64.wsl", "Sha256": "cc" } }
            ] }
        });
        let amd = pick_image(&catalog, "Ubuntu", false).unwrap();
        assert_eq!(amd.url, "https://x/new-amd64.wsl");
        assert_eq!(amd.sha256, "bb");
        assert_eq!(pick_image(&catalog, "Ubuntu", true).unwrap().url, "https://x/new-arm64.wsl");
        assert!(pick_image(&catalog, "Debian", false).is_none());
    }

    #[test]
    fn usernames_follow_ubuntus_rule() {
        assert!(check_username("sam").is_ok());
        assert!(check_username("_dev-1").is_ok());
        assert!(check_username("Sam").is_err());
        assert!(check_username("1sam").is_err());
        assert!(check_username("sa m").is_err());
        assert!(check_username("root").is_err());
        assert!(check_username("").is_err());
        assert_eq!(suggest_username("Marten Sogtoen"), "marten_sogtoen");
        assert_eq!(suggest_username("1Admin"), "admin");
    }

    #[test]
    fn passwords_must_match_and_be_safe() {
        assert!(check_password("secret", "secret").is_ok());
        assert!(check_password("secret", "secrex").is_err());
        assert!(check_password("", "").is_err());
        assert!(check_password("a\nb", "a\nb").is_err());
    }

    #[test]
    fn finds_percentages_in_wsl_output() {
        assert_eq!(percent_in("[==========================60.0%==========                 ]"), Some(0.6));
        assert_eq!(percent_in("Downloaden: Ubuntu 42,5%"), Some(0.425));
        assert_eq!(percent_in("This might take a while..."), None);
        assert_eq!(percent_in("100%"), Some(1.0));
    }

    #[test]
    fn the_provisioning_script_carries_the_user_and_password_only_where_meant() {
        let script = provision_script("sam", "pa$$ 'word'");
        assert!(script.contains("u='sam'"));
        assert!(script.contains("sam:pa$$ 'word'\nMONAGED_PASSWORD_END"));
        assert!(script.contains("<<'MONAGED_PASSWORD_END'"));
        assert!(!script.contains("__USER__") && !script.contains("__PASSWORD__"));
        assert!(script.contains("cloud-init status --wait"));
    }

    #[test]
    fn avoids_names_wsl_already_has() {
        let taken = vec!["ubuntu".to_string(), "Ubuntu-2".to_string()];
        assert_eq!(free_name("Ubuntu", &taken), "Ubuntu-3");
        assert_eq!(free_name("Ubuntu-26.04", &taken), "Ubuntu-26.04");
    }

    #[test]
    fn cloud_init_stages_become_progress() {
        assert_eq!(stage_progress("modules-config", "running").1, Some(0.65));
        assert_eq!(stage_progress("-", "done").1, Some(1.0));
        assert_eq!(stage_progress("-", "-").1, None);
    }
}
