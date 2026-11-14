//! Checkpoints, rollback and replay.
//!
//! After every turn (and when a session opens) Monaged snapshots the agent's
//! worktree as a commit nobody sees: a temporary index takes every file,
//! new ones included (ignored files excepted), and the commit is kept under
//! `refs/monaged/checkpoints/<agent>/<n>`, where n is how many of your
//! messages the agent had handled. So:
//! - "Restore files to before this message" puts the worktree back exactly
//!   (a snapshot of the current state is taken first, so it can be undone);
//! - the replay shows each turn's own diff, from one snapshot to the next.

use std::collections::HashMap;

use anyhow::Result;
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px, rgb};

use crate::agent::{Item, Tone};
use crate::diff::FileDiff;
use crate::icons::{Icon, icon, icon_sized};
use crate::ui::{self, Kind};
use crate::wsl::{quote, run};
use crate::{Monaged, theme};

/// Snapshots a worktree; returns (checkpoint commit, HEAD at the time).
pub fn create(distro: &str, worktree: &str, agent_id: i64, ordinal: i64) -> Result<(String, String)> {
    let script = format!(
        "cd {wt} || exit 1\n\
         tmp=$(mktemp)\n\
         export GIT_INDEX_FILE=\"$tmp\"\n\
         git read-tree HEAD && git add -A . >/dev/null 2>&1\n\
         tree=$(git write-tree) || exit 1\n\
         head=$(git rev-parse HEAD)\n\
         c=$(git commit-tree \"$tree\" -p \"$head\" -m 'Monaged checkpoint') || exit 1\n\
         unset GIT_INDEX_FILE; rm -f \"$tmp\"\n\
         git update-ref refs/monaged/checkpoints/{agent}/{ordinal} \"$c\"\n\
         echo \"$c $head\"",
        wt = quote(worktree),
        agent = agent_id,
        ordinal = if ordinal < 0 { "undo".to_string() } else { ordinal.to_string() },
    );
    let out = run(distro, &script)?;
    let mut parts = out.split_whitespace();
    Ok((parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string()))
}

/// Puts the worktree back to a snapshot: its HEAD, and every file as it was.
pub fn restore(distro: &str, worktree: &str, commit: &str, head: &str) -> Result<()> {
    let script = format!(
        "cd {wt} || exit 1\n\
         git reset -q --hard {head} && git clean -fdq && git read-tree -u --reset {commit} && git reset -q",
        wt = quote(worktree),
        head = quote(head),
        commit = quote(commit),
    );
    run(distro, &script)?;
    Ok(())
}

/// The diff between two snapshots.
pub fn diff(distro: &str, worktree: &str, from: &str, to: &str) -> Result<Vec<FileDiff>> {
    let out = run(distro, &format!("cd {} || exit 1\ngit -c core.quotepath=off diff --no-color -M {} {}", quote(worktree), quote(from), quote(to)))?;
    Ok(crate::diff::parse(&out))
}

/// One snapshot: the commit and the HEAD it sat on.
#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub commit: String,
    pub head: String,
}

/// The replay page's state.
pub(crate) struct Replay {
    pub agent_id: i64,
    pub focus: gpui::FocusHandle,
    /// The turn shown (1 = your first message).
    pub turn: i64,
    pub diffs: HashMap<i64, Result<Vec<FileDiff>, String>>,
}

/// How many of your messages come before item `index` (inclusive).
fn ordinal_of(items: &[Item], index: usize) -> i64 {
    items[..=index.min(items.len().saturating_sub(1))].iter().filter(|i| matches!(i, Item::User(_))).count() as i64
}

impl Monaged {
    fn checkpoint(&self, agent_id: i64, ordinal: i64) -> Option<Checkpoint> {
        self.checkpoints.get(&(agent_id, ordinal)).cloned()
    }

    /// Snapshots the agent's worktree after it handled `ordinal` messages.
    pub(crate) fn take_checkpoint(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id).filter(|a| !a.record.archived) else { return };
        let ordinal = agent.items.iter().filter(|i| matches!(i, Item::User(_))).count() as i64;
        let (distro, worktree) = (self.config.distro.clone(), agent.record.worktree.clone());
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { create(&distro, &worktree, agent_id, ordinal) }).await;
            view.update(cx, |this, _| {
                if let Ok((commit, head)) = result
                    && !commit.is_empty()
                {
                    let _ = this.store.save_checkpoint(agent_id, ordinal, &commit, &head);
                    this.checkpoints.insert((agent_id, ordinal), Checkpoint { commit, head });
                }
            })
            .ok();
        })
        .detach();
    }

    /// Restores the files to how they were before your message number `ordinal`.
    pub(crate) fn rollback_to(&mut self, agent_id: i64, ordinal: i64, cx: &mut Context<Self>) {
        let Some(target) = self.checkpoint(agent_id, ordinal - 1) else {
            self.toast(Tone::Warn, "There's no snapshot from before that message.", cx);
            return;
        };
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.is_working() {
            self.toast(Tone::Warn, "Stop the agent first; it's still changing files.", cx);
            return;
        }
        let (distro, worktree, name) = (self.config.distro.clone(), agent.record.worktree.clone(), agent.record.name.clone());
        self.progress("rollback", format!("Restoring {name}'s files…"), cx);
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    // The current state first, so the rollback can be undone.
                    let undo = create(&distro, &worktree, agent_id, -1)?;
                    restore(&distro, &worktree, &target.commit, &target.head)?;
                    anyhow::Ok(undo)
                })
                .await;
            view.update(cx, |this, cx| match result {
                Ok((commit, head)) => {
                    this.rollback_undo.insert(agent_id, Checkpoint { commit, head });
                    this.next_note.insert(
                        agent_id,
                        format!("[The developer restored this worktree's files to how they were before their message #{ordinal}. Changes made since are gone; check the files before relying on them.]"),
                    );
                    if let Some(agent) = this.agent_mut(agent_id) {
                        agent.notice(Tone::Info, format!("Files restored to before message #{ordinal}. The agent is told with your next message."));
                        agent.changes = None;
                    }
                    let _ = this.store.log_event(this.agent(agent_id).map(|a| a.record.subproject_id).unwrap_or(0), Some(agent_id), "restore", &format!("Restored {name}'s files to before message #{ordinal}"));
                    this.settle("rollback", Tone::Info, format!("Restored {name}'s files to before message #{ordinal}."), cx);
                    this.toast_action(Tone::Info, "Changed your mind?", "Undo", crate::feedback::ToastAction::UndoRollback(agent_id), cx);
                    this.refresh_changes(agent_id, false, cx);
                }
                Err(err) => this.settle("rollback", Tone::Error, format!("Couldn't restore the files: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn undo_rollback(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(undo) = self.rollback_undo.remove(&agent_id) else { return };
        let Some(agent) = self.agent(agent_id) else { return };
        let (distro, worktree) = (self.config.distro.clone(), agent.record.worktree.clone());
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { restore(&distro, &worktree, &undo.commit, &undo.head) }).await;
            view.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.next_note.remove(&agent_id);
                    this.toast(Tone::Info, "Put the files back as they were before the restore.", cx);
                    this.refresh_changes(agent_id, false, cx);
                }
                Err(err) => this.toast_error(format!("Couldn't undo: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// The ordinal of a user message item, and whether there's a snapshot before it.
    pub(crate) fn rollback_point(&self, agent_id: i64, index: usize) -> Option<i64> {
        let agent = self.agent(agent_id)?;
        let ordinal = ordinal_of(&agent.items, index);
        self.checkpoints.contains_key(&(agent_id, ordinal - 1)).then_some(ordinal)
    }

    // ---- Replay ------------------------------------------------------------------------------

    pub(crate) fn open_replay(&mut self, agent_id: i64, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let turns = self.agent(agent_id).map(|a| a.items.iter().filter(|i| matches!(i, Item::User(_))).count() as i64).unwrap_or(0);
        if turns == 0 {
            self.toast(Tone::Info, "Nothing to replay yet: this agent hasn't had a message.", cx);
            return;
        }
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.form = None;
        self.replay = Some(Replay { agent_id, focus, turn: turns, diffs: HashMap::new() });
        self.load_replay_diff(cx);
        cx.notify();
    }

    pub(crate) fn replay_step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(replay) = &mut self.replay else { return };
        let turns = self.agents.iter().find(|a| a.record.id == replay.agent_id).map(|a| a.items.iter().filter(|i| matches!(i, Item::User(_))).count() as i64).unwrap_or(1);
        replay.turn = (replay.turn + delta).clamp(1, turns.max(1));
        self.load_replay_diff(cx);
        cx.notify();
    }

    fn load_replay_diff(&mut self, cx: &mut Context<Self>) {
        let Some(replay) = &self.replay else { return };
        let (agent_id, turn) = (replay.agent_id, replay.turn);
        if replay.diffs.contains_key(&turn) {
            return;
        }
        let (Some(before), Some(after)) = (self.checkpoint(agent_id, turn - 1), self.checkpoint(agent_id, turn)) else { return };
        let Some(worktree) = self.agent(agent_id).map(|a| a.record.worktree.clone()) else { return };
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { diff(&distro, &worktree, &before.commit, &after.commit) }).await;
            view.update(cx, |this, cx| {
                if let Some(replay) = this.replay.as_mut().filter(|r| r.agent_id == agent_id) {
                    replay.diffs.insert(turn, result.map_err(|e| format!("{e:#}")));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_replay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let replay = self.replay.as_ref()?;
        let agent = self.agent(replay.agent_id)?;
        let users: Vec<usize> = agent.items.iter().enumerate().filter(|(_, i)| matches!(i, Item::User(_))).map(|(i, _)| i).collect();
        let turns = users.len() as i64;
        let turn = replay.turn.clamp(1, turns.max(1));
        let start = users.get(turn as usize - 1).copied().unwrap_or(0);
        let end = users.get(turn as usize).copied().unwrap_or(agent.items.len());
        let agent_id = replay.agent_id;

        // The scrubber: one dot per turn.
        let dots = div().flex().flex_wrap().items_center().gap(px(4.)).children((1..=turns).map(|t| {
            let has = self.checkpoints.contains_key(&(agent_id, t)) && self.checkpoints.contains_key(&(agent_id, t - 1));
            div()
                .id(SharedString::from(format!("replay-dot-{t}")))
                .size(px(if t == turn { 10. } else { 7. }))
                .rounded_full()
                .cursor_pointer()
                .bg(rgb(if t == turn { theme::text() } else if has { theme::text_3() } else { theme::text_4() }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(r) = &mut this.replay {
                        r.turn = t;
                    }
                    this.load_replay_diff(cx);
                    cx.notify();
                }))
        }));
        let header = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(16.))
            .py(px(12.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(Icon::Clock).text_color(rgb(theme::text_2())))
                    .child(div().flex_1().child(ui::heading(format!("Replay · {} · turn {turn} of {turns}", agent.record.name))))
                    .child(ui::icon_button("replay-prev", Icon::ChevronLeft).tooltip(crate::shell::tip_key("Previous turn", "←")).on_click(cx.listener(|this, _, _, cx| this.replay_step(-1, cx))))
                    .child(ui::icon_button("replay-next", Icon::ChevronRight).tooltip(crate::shell::tip_key("Next turn", "→")).on_click(cx.listener(|this, _, _, cx| this.replay_step(1, cx))))
                    .child(ui::button("replay-close", Kind::Secondary, None, "Close").on_click(cx.listener(|this, _, _, cx| {
                        this.replay = None;
                        cx.notify();
                    }))),
            )
            .child(dots);

        // Left: what was said in this turn.
        let mut said = div().w(px(520.)).flex_none().flex().flex_col().gap(px(12.));
        for (i, item) in agent.items[start..end].iter().enumerate() {
            let el: Option<AnyElement> = match item {
                Item::User(t) => Some(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(rgb(theme::raised()))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .text_size(px(13.))
                        .child(t.chars().take(1200).collect::<String>())
                        .into_any_element(),
                ),
                Item::Agent(t) => Some(crate::review_ui::render_markdown(&t.chars().take(4000).collect::<String>())),
                Item::Tool { title, .. } => Some(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon_sized(Icon::Terminal, 12.).text_color(rgb(theme::text_3())))
                        .child(div().truncate().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(title.clone()))
                        .into_any_element(),
                ),
                _ => None,
            };
            if let Some(el) = el {
                said = said.child(div().id(("replay-item", i)).child(el));
            }
        }

        // Right: what changed in this turn.
        let changed: AnyElement = match replay.diffs.get(&turn) {
            _ if !(self.checkpoints.contains_key(&(agent_id, turn - 1)) && self.checkpoints.contains_key(&(agent_id, turn))) => {
                ui::meta("No snapshots around this turn (it happened before checkpoints, or while Monaged wasn't running).").into_any_element()
            }
            None => ui::skeleton_text("replay-sk", 5).into_any_element(),
            Some(Err(err)) => ui::callout(Tone::Error, err.clone()).into_any_element(),
            Some(Ok(files)) if files.is_empty() => ui::meta("No file changes in this turn.").into_any_element(),
            Some(Ok(files)) => div()
                .flex()
                .flex_col()
                .gap(px(16.))
                .children(files.iter().map(|f| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(div().flex().gap(px(8.)).child(div().flex_1().font_family(theme::MONO).text_size(px(12.5)).child(f.path.clone())).child(ui::mono(format!("+{} −{}", f.added, f.removed))))
                        .child(self.render_diff_body(None, f, cx))
                }))
                .into_any_element(),
        };
        Some(
            div()
                .track_focus(&replay.focus)
                .key_context("Replay")
                .on_action(cx.listener(|this, _: &crate::ReplayPrev, _, cx| this.replay_step(-1, cx)))
                .on_action(cx.listener(|this, _: &crate::ReplayNext, _, cx| this.replay_step(1, cx)))
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .child(header)
                .child(
                    div()
                        .id("replay")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .child(div().flex().items_start().gap(px(24.)).p(px(20.)).child(said).child(div().flex_1().min_w(px(0.)).child(changed))),
                )
                .into_any_element(),
        )
    }
}
