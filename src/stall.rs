//! The stall guard: no chat waits forever without anyone noticing.
//!
//! A chat used to stop for good, with nothing in Needs you, when:
//! - a turn went silent (the model's stream or a session hung) and the agent
//!   said "working" forever, with its queued messages (and the lead's
//!   updates) behind it;
//! - a turn ended in an error (overloaded, network) and nobody asked again;
//! - the connection to Claude dropped (WSL restarting, say) in the middle of
//!   turns, which were never picked up again;
//! - a session never finished opening;
//! - a pipeline agent finished twice without its result, and the pipeline
//!   waited on it.
//!
//! Every 15 seconds Monaged looks: a turn with no sign of life for 6 minutes
//! (30 while a command, a subagent or compaction runs; never while it waits for you) is
//! stopped and the agent asked to continue; a stop that isn't answered within
//! 90 seconds reopens the session with the same request; failed turns are
//! asked again after 30 seconds; interrupted turns continue after a
//! reconnect; a session that takes over 3 minutes to open is opened again.
//! At most 3 automatic recoveries per agent an hour: after that it waits for
//! you as "Stalled".

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::Context;

use crate::agent::{Status, Tone};
use crate::{Conn, Monaged, Pending};

/// How long a turn may go without a sign of life.
pub const QUIET: Duration = Duration::from_secs(6 * 60);
/// The same while a tool call (a build, the tests, a subagent) or compaction runs.
pub const QUIET_BUSY: Duration = Duration::from_secs(30 * 60);
/// How long a stopped turn has to say it stopped.
pub const CANCEL_GRACE: Duration = Duration::from_secs(90);
/// How long opening a session may take.
pub const OPEN_LIMIT: Duration = Duration::from_secs(3 * 60);
/// When a turn that failed is asked again.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Automatic recoveries per agent per hour before it waits for you.
pub const MAX_PER_HOUR: usize = 3;
/// How often the guard looks.
const EVERY: Duration = Duration::from_secs(15);

#[derive(Default)]
pub struct Guard {
    last_check: Option<Instant>,
    /// Turns Monaged stopped because they went quiet, and when.
    cancelled: HashMap<i64, Instant>,
    /// Turns that failed: when to ask again, and why it failed.
    retry_at: HashMap<i64, (Instant, String)>,
    /// When each agent was recovered automatically (for the hourly cap).
    recoveries: HashMap<i64, Vec<Instant>>,
    /// Sessions being opened, since when.
    opening: HashMap<i64, Instant>,
    /// Stalled more often than the guard may fix: waits for you.
    pub held: HashSet<i64>,
    /// Pipeline agents that finished twice without their result.
    pub undelivered: HashSet<i64>,
}

/// Whether a working turn counts as stalled after `quiet` without a sign of life.
pub fn stalled(quiet: Duration, waiting_on_you: bool, busy: bool) -> bool {
    !waiting_on_you && quiet >= if busy { QUIET_BUSY } else { QUIET }
}

/// Whether another automatic recovery fits in the hourly cap.
pub fn allowed(history: &[Instant], now: Instant) -> bool {
    history.iter().filter(|at| now.saturating_duration_since(**at) < Duration::from_secs(3600)).count() < MAX_PER_HOUR
}

/// What the agent hears when Monaged picks its work up again.
pub fn continue_message(why: &str) -> String {
    format!(
        "[Monaged] {why} Continue where you left off: check what's already done (files, commits, command output) instead of \
         starting over, and finish the task you were given. If a command hung, run it again with a timeout, or another way. \
         End your turn the way your instructions say (with your result block, if you have one)."
    )
}

fn minutes(d: Duration) -> u64 {
    (d.as_secs() + 30) / 60
}

impl Monaged {
    /// Called every second by the clock; looks every 15.
    pub(crate) fn check_stalls(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.stall.last_check.is_some_and(|at| now.duration_since(at) < EVERY) {
            return;
        }
        self.stall.last_check = Some(now);
        if !matches!(self.conn, Conn::Ready) {
            return;
        }

        // Failed turns, asked again.
        let due: Vec<(i64, String)> = self.stall.retry_at.iter().filter(|(_, (at, _))| *at <= now).map(|(id, (_, why))| (*id, why.clone())).collect();
        for (agent_id, why) in due {
            if self.agent(agent_id).is_some_and(|a| a.is_working() || a.paused) {
                continue;
            }
            self.stall.retry_at.remove(&agent_id);
            self.resume(agent_id, continue_message(&format!("Your last turn ended with an error ({why}).")));
        }

        // Quiet turns.
        let working: Vec<i64> = self.agents.iter().filter(|a| a.turn.is_some() && a.live && !a.record.archived).map(|a| a.record.id).collect();
        for agent_id in working {
            if let Some(at) = self.stall.cancelled.get(&agent_id).copied() {
                if now.duration_since(at) > CANCEL_GRACE {
                    self.stall.cancelled.remove(&agent_id);
                    self.reopen_stalled(agent_id, cx);
                }
                continue;
            }
            let Some(agent) = self.agent(agent_id) else { continue };
            let waiting = agent.needs_attention() || agent.open_form() || agent.paused || self.vault.requests.iter().any(|r| r.agent_id == agent_id);
            let quiet = now.duration_since(agent.activity);
            if stalled(quiet, waiting, agent.tool_running() || agent.compacting) {
                self.recover(agent_id, format!("no sign of life for {} minutes", minutes(quiet)), cx);
            }
        }

        // Sessions that don't finish opening.
        let opening: Vec<i64> = self
            .agents
            .iter()
            .filter(|a| matches!(a.status, Status::Starting | Status::Loading))
            .filter(|a| self.pending.values().any(|p| matches!(p, Pending::NewSession(id) | Pending::LoadSession(id) if *id == a.record.id)))
            .map(|a| a.record.id)
            .collect();
        self.stall.opening.retain(|id, _| opening.contains(id));
        for agent_id in opening {
            let since = *self.stall.opening.entry(agent_id).or_insert(now);
            if now.duration_since(since) < OPEN_LIMIT {
                continue;
            }
            self.stall.opening.remove(&agent_id);
            self.pending.retain(|_, p| !matches!(p, Pending::NewSession(id) | Pending::LoadSession(id) if *id == agent_id));
            let history = self.stall.recoveries.entry(agent_id).or_default();
            let again = allowed(history, now);
            if again {
                history.push(now);
            }
            let name = self.agent_name(agent_id);
            if let Some(agent) = self.agent_mut(agent_id) {
                agent.live = false;
                if again {
                    agent.status = Status::Idle;
                    agent.notice(Tone::Warn, format!("Opening the session took over {} minutes. Opening it again.", minutes(OPEN_LIMIT)));
                } else {
                    agent.status = Status::Failed(format!("Opening the session took over {} minutes.", minutes(OPEN_LIMIT)));
                }
            }
            let sub_id = self.agent(agent_id).map(|a| a.record.subproject_id).unwrap_or_default();
            let _ = self.store.log_event(sub_id, Some(agent_id), "stall", &format!("{name}'s session didn't open in time"));
            if again {
                self.ensure_session(agent_id);
            }
        }
        cx.notify();
    }

    /// Stops a quiet turn; it's asked to continue once it says it stopped.
    fn recover(&mut self, agent_id: i64, why: String, cx: &mut Context<Self>) {
        let now = Instant::now();
        let history = self.stall.recoveries.entry(agent_id).or_default();
        if !allowed(history, now) {
            self.hold_stalled(agent_id, why, cx);
            return;
        }
        history.push(now);
        let Some(session_id) = self.agent(agent_id).and_then(|a| a.record.session_id.clone()) else { return };
        self.stall.cancelled.insert(agent_id, now);
        self.notify_agent("session/cancel", serde_json::json!({ "sessionId": session_id }));
        let name = self.agent_name(agent_id);
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.notice(Tone::Warn, format!("This turn stalled ({why}). Monaged stopped it and asks {name} to continue."));
        }
        let sub_id = self.agent(agent_id).map(|a| a.record.subproject_id).unwrap_or_default();
        let _ = self.store.log_event(sub_id, Some(agent_id), "stall", &format!("{name} stalled ({why}); stopped and continued"));
        eprintln!("[stall] {name}: {why}; cancelling the turn");
    }

    /// The stop wasn't answered: the session itself hangs. Drop the turn and
    /// open the session again, with the request to continue waiting for it.
    fn reopen_stalled(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let name = self.agent_name(agent_id);
        self.pending.retain(|_, p| !matches!(p, Pending::Prompt(id) if *id == agent_id));
        let session = self.agent(agent_id).and_then(|a| a.record.session_id.clone());
        if let Some(session_id) = session {
            self.sessions.remove(&session_id);
        }
        let message = continue_message("Your last turn stopped responding, so Monaged reopened your session.");
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.turn = None;
            agent.live = false;
            agent.status = Status::Idle;
            agent.queued_prompt = Some(match agent.queued_prompt.take() {
                Some(earlier) => format!("{message}\n\n{earlier}"),
                None => message,
            });
            agent.notice(Tone::Warn, format!("The session didn't answer the stop either. Reopening it; {name} continues when it's open."));
        }
        eprintln!("[stall] {name}: the session didn't answer session/cancel; reopening it");
        self.ensure_session(agent_id);
        cx.notify();
    }

    /// Too many stalls in an hour: it waits for you.
    fn hold_stalled(&mut self, agent_id: i64, why: String, cx: &mut Context<Self>) {
        if !self.stall.held.insert(agent_id) {
            return;
        }
        let name = self.agent_name(agent_id);
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.notice(
                Tone::Error,
                format!("This turn stalled again ({why}), the {}th time this hour. Monaged leaves it to you: stop it (Esc) and send a message, or restart the session.", MAX_PER_HOUR + 1),
            );
        }
        let sub_id = self.agent(agent_id).map(|a| a.record.subproject_id).unwrap_or_default();
        let _ = self.store.log_event(sub_id, Some(agent_id), "stall", &format!("{name} keeps stalling; waits for you"));
        self.tell_lead(sub_id, format!("{name} keeps stalling ({why}); Monaged stopped retrying and it waits for the user. Tell them in a sentence."));
        self.chime(crate::sound::Cue::NeedsYou);
        cx.notify();
    }

    /// Sends `message`, after anything that was queued for the agent.
    fn resume(&mut self, agent_id: i64, message: String) {
        let queued = self.agent_mut(agent_id).and_then(|a| a.queued_prompt.take());
        let text = match queued {
            Some(earlier) => format!("{message}\n\n{earlier}"),
            None => message,
        };
        self.send_prompt(agent_id, text);
    }

    /// A turn ended (from the prompt's response): one Monaged stopped for
    /// stalling goes on now.
    pub(crate) fn stall_turn_ended(&mut self, agent_id: i64, reason: &str) {
        if self.stall.cancelled.remove(&agent_id).is_some() && reason == "cancelled" {
            self.resume(agent_id, continue_message("Your last turn stalled, so Monaged stopped it."));
        }
    }

    /// A turn failed with an error: ask again in a moment (within the hourly cap).
    pub(crate) fn stall_turn_failed(&mut self, agent_id: i64, error: &str) {
        if self.stall.cancelled.remove(&agent_id).is_some() {
            self.resume(agent_id, continue_message("Your last turn stalled, so Monaged stopped it."));
            return;
        }
        // Signed out or paused by the budget: retrying can't help.
        if self.logged_in == Some(false) || self.agent(agent_id).is_some_and(|a| a.paused || a.record.archived) {
            return;
        }
        let now = Instant::now();
        let history = self.stall.recoveries.entry(agent_id).or_default();
        if !allowed(history, now) {
            self.stall.held.insert(agent_id);
            self.chime(crate::sound::Cue::NeedsYou);
            return;
        }
        history.push(now);
        let short = crate::review::clip(error.trim(), 160);
        self.stall.retry_at.insert(agent_id, (now + RETRY_AFTER, short));
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.notice(Tone::Info, format!("Monaged asks again in {} seconds.", RETRY_AFTER.as_secs()));
        }
    }

    /// The connection dropped in the middle of these turns: each continues
    /// once its session is open again (the reconnect opens sessions with
    /// queued messages).
    pub(crate) fn stall_interrupted(&mut self, agent_ids: &[i64]) {
        let message = continue_message("The connection to Claude was lost in the middle of your turn (WSL or the Claude process restarted).");
        for &agent_id in agent_ids {
            self.stall.cancelled.remove(&agent_id);
            if let Some(agent) = self.agent_mut(agent_id).filter(|a| !a.record.archived) {
                agent.queued_prompt = Some(match agent.queued_prompt.take() {
                    Some(earlier) if earlier.starts_with("[Monaged]") => earlier,
                    Some(earlier) => format!("{message}\n\n{earlier}"),
                    None => message.clone(),
                });
            }
        }
        self.stall.opening.clear();
    }

    /// Something was sent to the agent: whatever the guard held is moving again.
    pub(crate) fn stall_sent(&mut self, agent_id: i64) {
        self.stall.held.remove(&agent_id);
        self.stall.undelivered.remove(&agent_id);
        self.stall.retry_at.remove(&agent_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_turn_stalls_unless_it_waits_for_you() {
        assert!(!stalled(Duration::from_secs(60), false, false));
        assert!(stalled(QUIET, false, false));
        assert!(!stalled(Duration::from_secs(60 * 60), true, false));
    }

    #[test]
    fn a_running_command_gets_longer() {
        assert!(!stalled(QUIET + Duration::from_secs(60), false, true));
        assert!(stalled(QUIET_BUSY, false, true));
    }

    #[test]
    fn recoveries_are_capped_per_hour() {
        let now = Instant::now();
        let recent = vec![now; MAX_PER_HOUR];
        assert!(!allowed(&recent, now));
        assert!(allowed(&recent[..MAX_PER_HOUR - 1], now));
        if let Some(old) = now.checked_sub(Duration::from_secs(2 * 3600)) {
            assert!(allowed(&vec![old; 10], now), "stalls over an hour ago don't count");
        }
    }

    #[test]
    fn the_agent_is_told_to_continue_not_start_over() {
        let text = continue_message("Your last turn stalled.");
        assert!(text.starts_with("[Monaged] Your last turn stalled.") && text.contains("instead of starting over"));
    }
}
