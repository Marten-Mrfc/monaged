//! Minimal Agent Client Protocol (ACP) transport.
//!
//! Runs one ACP agent process inside a WSL distro through `wsl.exe` and
//! exchanges newline-delimited JSON-RPC over its stdin/stdout. One process
//! hosts every agent session (each session has its own cwd/worktree).
//! Everything that blocks lives on plain threads; the UI receives
//! [`AgentEvent`]s on a channel.
//!
//! Hand-rolled for the spike so every message is visible. A real app should
//! use the official `agent-client-protocol` crate.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context as _, Result, anyhow};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::Value;

use crate::wsl;

#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// WSL distro to run the agent in.
    pub distro: String,
    /// Path of the ACP agent executable inside Linux (`~` allowed).
    pub agent_cmd: String,
    /// Agents may use your git accounts (Settings → Git accounts). Off: git
    /// in agents' commands has no credential helpers, and Monaged's helper
    /// refuses them, so they can't read your tokens by asking git for them.
    pub agents_use_git_accounts: bool,
}

/// What agent processes start with when they may not use your git accounts.
pub const AGENT_GIT_GUARD: &str = "export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=credential.helper GIT_CONFIG_VALUE_0= MONAGED_AGENT=1; \
export WSLENV=\"MONAGED_AGENT${WSLENV:+:$WSLENV}\"; ";

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            distro: "Ubuntu".into(),
            agent_cmd: "~/.local/share/claude-agent-acp/current/claude-agent-acp".into(),
            agents_use_git_accounts: false,
        }
    }
}

pub enum AgentEvent {
    /// The agent process is running.
    Started { writer: AgentWriter },
    /// One JSON-RPC message from the agent.
    Message(Value),
    /// A line the agent wrote to stderr (its log).
    Log(String),
    /// Startup failed or the connection broke.
    Failed(String),
    /// The agent process exited.
    Exited(Option<i32>),
}

/// Write side of the connection. Cheap to clone.
#[derive(Clone)]
pub struct AgentWriter {
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
}

impl AgentWriter {
    pub fn send(&self, message: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().map_err(|_| anyhow!("agent stdin lock poisoned"))?;
        serde_json::to_writer(&mut *stdin, message)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

/// Starts the agent on a background thread and returns its event stream.
pub fn launch(config: AgentConfig) -> UnboundedReceiver<AgentEvent> {
    let (tx, rx) = unbounded();
    thread::Builder::new()
        .name("acp-agent".into())
        .spawn(move || {
            if let Err(err) = run(&config, &tx) {
                let _ = tx.unbounded_send(AgentEvent::Failed(format!("{err:#}")));
            }
        })
        .expect("failed to spawn the agent thread");
    rx
}

fn run(config: &AgentConfig, tx: &UnboundedSender<AgentEvent>) -> Result<()> {
    let guard = if config.agents_use_git_accounts { "" } else { AGENT_GIT_GUARD };
    let mut child = wsl::bash(&config.distro, &format!("cd ~ && {}{guard}exec {}", wsl::TOOL_PATHS, config.agent_cmd))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not start the agent through wsl.exe")?;
    let stdin = child.stdin.take().context("agent stdin missing")?;
    let stdout = child.stdout.take().context("agent stdout missing")?;
    let stderr = child.stderr.take().context("agent stderr missing")?;

    let writer = AgentWriter { stdin: Arc::new(Mutex::new(stdin)), child: Arc::new(Mutex::new(child)) };
    if tx.unbounded_send(AgentEvent::Started { writer: writer.clone() }).is_err() {
        writer.kill();
        return Ok(());
    }

    let log_tx = tx.clone();
    thread::Builder::new().name("acp-stderr".into()).spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if log_tx.unbounded_send(AgentEvent::Log(line)).is_err() {
                break;
            }
        }
    })?;

    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading from the agent failed")?;
        if line.trim().is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<Value>(&line) {
            Ok(message) => AgentEvent::Message(message),
            Err(_) => AgentEvent::Log(format!("non-JSON output: {line}")),
        };
        if tx.unbounded_send(event).is_err() {
            writer.kill();
            return Ok(());
        }
    }

    let code = writer.child.lock().ok().and_then(|mut child| child.wait().ok()).and_then(|status| status.code());
    let _ = tx.unbounded_send(AgentEvent::Exited(code));
    Ok(())
}
