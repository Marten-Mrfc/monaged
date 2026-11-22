//! When WSL itself breaks. The Claude process dying with exit code -1, and
//! every `wsl.exe` failing with `Wsl/Service/E_UNEXPECTED`, means WSL's
//! Windows service is in a broken state: reconnecting can't work, and only
//! restarting WSL helps. So when the connection drops, Monaged first asks
//! WSL whether it's alive:
//!
//! - it is (or something else failed): reconnect as before;
//! - its service is broken: `wsl --shutdown` by itself (at most once every
//!   ten minutes), check again, and reconnect;
//! - still broken: the connection bar says so, with **Restart WSL**, which
//!   shuts WSL down and, when that isn't enough, restarts WSL's Windows
//!   service as administrator (Windows asks first).
//!
//! `wsl.rs` keeps Monaged from causing it (a few `wsl.exe` at a time) and
//! from hammering a broken service (calls are held until it's back).

use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};

use crate::agent::Tone;
use crate::wsl::{self, Health};
use crate::{Conn, Monaged};

/// The automatic `wsl --shutdown` happens at most this often.
const AUTO_EVERY: Duration = Duration::from_secs(10 * 60);

/// The argument `monaged.exe` is started with, as administrator, to restart WSL's service.
pub const RESTART_ARG: &str = "--restart-wsl-service";

#[derive(Default)]
pub struct Recovery {
    /// Checking or restarting WSL right now.
    pub running: bool,
    /// When Monaged last shut WSL down by itself.
    pub last_auto: Option<Instant>,
    /// WSL's service is broken and restarting it by itself didn't help (the code).
    pub broken: Option<String>,
}

/// What the connection bar says while WSL is broken.
pub fn broken_message(code: &str) -> String {
    format!("WSL itself stopped working ({code}), so Claude can't run. Restart WSL to fix it; if that doesn't help, restart Windows.")
}

/// `monaged.exe --restart-wsl-service`, running as administrator: restarts
/// WSL's service (WslService; LxssManager on older WSL). 0 when it did, 2
/// when neither exists, 1 when it failed.
pub fn restart_service_here() -> i32 {
    for name in ["WSLService", "LxssManager"] {
        match crate::native::restart_service(name) {
            Ok(true) => return 0,
            Ok(false) => continue,
            Err(err) => {
                eprintln!("restart {name}: {err:#}");
                return 1;
            }
        }
    }
    2
}

impl Monaged {
    /// The Claude process stopped: check WSL before reconnecting.
    pub(crate) fn connection_lost(&mut self, cx: &mut Context<Self>) {
        if self.wsl_recovery.running {
            return;
        }
        self.wsl_recovery.running = true;
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let health = cx.background_spawn(async move { wsl::probe(&distro) }).await;
            view.update(cx, |this, cx| {
                this.wsl_recovery.running = false;
                match health {
                    Health::Broken(code) => {
                        let auto = this.wsl_recovery.last_auto.is_none_or(|at| at.elapsed() > AUTO_EVERY);
                        if auto {
                            this.wsl_recovery.last_auto = Some(Instant::now());
                            this.restart_wsl(code, false, cx);
                        } else {
                            this.wsl_broken(code, cx);
                        }
                    }
                    Health::Fine | Health::Other(_) => {
                        this.wsl_recovery.broken = None;
                        this.schedule_reconnect(cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn wsl_broken(&mut self, code: String, cx: &mut Context<Self>) {
        self.conn = Conn::Failed(broken_message(&code));
        self.wsl_recovery.broken = Some(code);
        self.reconnect.at = None;
        cx.notify();
    }

    /// Restarts WSL: `wsl --shutdown`, then (when `elevated` and that wasn't
    /// enough) WSL's service as administrator; reconnects once WSL answers.
    pub(crate) fn restart_wsl(&mut self, code: String, elevated: bool, cx: &mut Context<Self>) {
        if self.wsl_recovery.running {
            return;
        }
        self.wsl_recovery.running = true;
        self.conn = Conn::Failed(format!("WSL itself stopped working ({code}). Restarting WSL…"));
        self.toast(Tone::Info, format!("WSL stopped working ({code}). Restarting it; Claude reconnects when it's back."), cx);
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let outcome = cx
                .background_spawn(async move {
                    let shutdown = wsl::shutdown();
                    if let Err(err) = &shutdown {
                        eprintln!("[wsl] {err:#}");
                    }
                    std::thread::sleep(Duration::from_secs(3));
                    let mut health = wsl::probe(&distro);
                    if elevated && health != Health::Fine {
                        // The service itself hangs: restart it (Windows asks first).
                        let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("{e}"))?;
                        let code = crate::native::run_elevated(&exe, RESTART_ARG)?;
                        if code == 1223 {
                            anyhow::bail!("Restarting WSL's service needs administrator rights, and Windows' prompt was declined.");
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        health = wsl::probe(&distro);
                    }
                    Ok(health)
                })
                .await;
            view.update(cx, |this, cx| {
                this.wsl_recovery.running = false;
                match outcome {
                    Ok(Health::Fine) | Ok(Health::Other(_)) => {
                        this.wsl_recovery.broken = None;
                        this.toast(Tone::Info, "WSL is back. Reconnecting to Claude.", cx);
                        this.reconnect_now(cx);
                    }
                    Ok(Health::Broken(code)) => this.wsl_broken(code, cx),
                    Err(err) => {
                        this.toast_error(format!("{err:#}"), cx);
                        let code = this.wsl_recovery.broken.clone().unwrap_or_else(|| "Wsl/Service/E_UNEXPECTED".into());
                        this.wsl_broken(code, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_says_what_broke_and_what_to_do() {
        let text = broken_message("Wsl/Service/E_UNEXPECTED");
        assert!(text.contains("Wsl/Service/E_UNEXPECTED") && text.contains("Restart WSL"));
        assert!(RESTART_ARG.starts_with("--"));
    }
}
