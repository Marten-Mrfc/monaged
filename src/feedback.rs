//! Feedback: how Monaged tells you what happened.
//!
//! One rule decides where a message goes:
//! - an **event** (something happened) is a toast, bottom right; information
//!   fades after a few seconds, errors stay until you close them;
//! - a **condition** (something is true right now: offline, not logged in,
//!   paused by the budget) is a bar above the content, until it's fixed;
//! - a **problem with a field** shows under that field.
//!
//! Destructive steps ask first in a dialog, and the connection to Claude
//! reconnects by itself a few times before it asks you.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, ClipboardItem, Context, FocusHandle, Focusable, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px, rgb,
    rgba,
};

use crate::agent::Tone;
use crate::icons::{Icon, icon, icon_sized};
use crate::shell::InspectorTab;
use crate::ui::{self, Kind};
use crate::{Conn, Monaged, theme};

/// How long a toast stays, by tone. Errors stay until closed.
/// Info toasts with a button (Undo, Restore).
const ACTION_MS: u64 = 12_000;
const INFO_MS: u64 = 4_500;
const WARN_MS: u64 = 8_000;
const MAX_TOASTS: usize = 4;
/// Seconds before each automatic reconnect attempt.
const RECONNECT_DELAYS: [u64; 3] = [2, 6, 15];

pub(crate) struct Toast {
    pub id: u64,
    pub tone: Tone,
    pub text: String,
    /// A later toast with the same key replaces this one ("Merging…" → "Merged").
    pub key: Option<&'static str>,
    /// Something is still running: a spinner, and it stays until replaced.
    pub progress: bool,
    pub action: Option<(&'static str, ToastAction)>,
}

#[derive(Clone)]
pub(crate) enum ToastAction {
    Reconnect,
    OpenSettings,
    Copy(String),
    /// Start a fresh session for an agent whose session failed.
    RetrySession(i64),
    /// Jump to a subproject (and an agent in it).
    Open { sub_id: i64, agent_id: Option<i64> },
    /// Undo an archive.
    Restore(i64),
    /// Download and install the newer release.
    Update,
    /// Put the files back as they were before a rollback.
    UndoRollback(i64),
}

/// Steps that can't be undone ask first.
#[derive(Clone)]
pub(crate) enum ConfirmAction {
    StopPipeline(i64),
    RemoveApiKey,
    CleanupWorktrees(i64),
    Promote(i64, String, String),
    Rerun(i64, crate::pipeline::Stage),
    Discard(i64, crate::git_ops::StatusFile),
    /// (agent, your message number): restore the files to before it.
    Rollback(i64, i64),
    RerunBuilder(i64, String),
    /// Restart Windows to finish installing WSL (the setup guide).
    RestartWindows,
}

pub(crate) struct Confirm {
    pub title: String,
    pub body: String,
    pub confirm: String,
    pub danger: bool,
    pub action: ConfirmAction,
    pub focus: FocusHandle,
}

/// A small dialog asking for one line of text (a new name, a layout's name).
pub(crate) struct TextPrompt {
    pub title: String,
    pub hint: String,
    pub confirm: String,
    pub input: gpui::Entity<crate::text_input::TextInput>,
    pub action: TextAction,
}

#[derive(Clone)]
pub(crate) enum TextAction {
    RenameAgent(i64),
    SaveLayout(i64),
    /// A subproject's weekly cap in dollars ("" or 0 removes it).
    SubCap(i64),
}

/// Automatic reconnects after the Claude process dies.
#[derive(Default)]
pub(crate) struct Reconnect {
    pub attempts: usize,
    /// When the next attempt runs, if one is scheduled.
    pub at: Option<Instant>,
    /// Bumped on every schedule or manual connect, so stale timers do nothing.
    pub generation: u64,
}

impl Monaged {
    // ---- Toasts ------------------------------------------------------------------------------

    pub(crate) fn toast(&mut self, tone: Tone, text: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(tone, text.into(), None, false, None, cx);
    }

    pub(crate) fn toast_action(&mut self, tone: Tone, text: impl Into<String>, label: &'static str, action: ToastAction, cx: &mut Context<Self>) {
        self.push_toast(tone, text.into(), None, false, Some((label, action)), cx);
    }

    /// Something long-running started; `settle` with the same key replaces it.
    pub(crate) fn progress(&mut self, key: &'static str, text: impl Into<String>, cx: &mut Context<Self>) {
        self.push_toast(Tone::Info, text.into(), Some(key), true, None, cx);
    }

    pub(crate) fn settle(&mut self, key: &'static str, tone: Tone, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        // Errors get a copy button: git and WSL messages are worth pasting somewhere.
        let action = matches!(tone, Tone::Error).then(|| ("Copy", ToastAction::Copy(text.clone())));
        self.push_toast(tone, text, Some(key), false, action, cx);
    }

    /// Shows an error with a copy button.
    pub(crate) fn toast_error(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.push_toast(Tone::Error, text.clone(), None, false, Some(("Copy", ToastAction::Copy(text))), cx);
    }

    fn push_toast(
        &mut self,
        tone: Tone,
        text: String,
        key: Option<&'static str>,
        progress: bool,
        action: Option<(&'static str, ToastAction)>,
        cx: &mut Context<Self>,
    ) {
        if key.is_some() {
            self.toasts.retain(|t| t.key != key);
        }
        // The same message twice in a row just stays once.
        self.toasts.retain(|t| t.text != text);
        let id = self.next_toast;
        self.next_toast += 1;
        let has_action = action.is_some();
        self.toasts.push(Toast { id, tone, text, key, progress, action });
        while self.toasts.len() > MAX_TOASTS {
            // Drop the oldest that isn't an error, else the oldest.
            let index = self.toasts.iter().position(|t| !matches!(t.tone, Tone::Error)).unwrap_or(0);
            self.toasts.remove(index);
        }
        let lifetime = match tone {
            _ if progress => None,
            // A toast with a button (Undo, Restore) stays long enough to use it.
            Tone::Info if has_action => Some(ACTION_MS),
            Tone::Info => Some(INFO_MS),
            Tone::Warn => Some(WARN_MS),
            Tone::Error => None,
        };
        if let Some(ms) = lifetime {
            cx.spawn(async move |view, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                view.update(cx, |this, cx| this.dismiss_toast(id, cx)).ok();
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    fn run_toast_action(&mut self, id: u64, action: ToastAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            ToastAction::Reconnect => self.reconnect_now(cx),
            ToastAction::OpenSettings => self.open_settings(cx),
            ToastAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.dismiss_toast(id, cx);
                self.toast(Tone::Info, "Copied to the clipboard", cx);
                return;
            }
            ToastAction::RetrySession(agent_id) => self.retry_session(agent_id, cx),
            ToastAction::Restore(agent_id) => self.restore_agent(agent_id, cx),
            ToastAction::Update => self.install_update(cx),
            ToastAction::UndoRollback(agent_id) => self.undo_rollback(agent_id, cx),
            ToastAction::Open { sub_id, agent_id } => {
                self.form = None;
                self.select_sub(sub_id, cx);
                if let Some(agent_id) = agent_id {
                    self.select_agent(agent_id, cx);
                }
            }
        }
        let _ = window;
        self.dismiss_toast(id, cx);
    }

    pub(crate) fn render_toasts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.toasts.is_empty() {
            return None;
        }
        let toasts = self.toasts.iter().map(|toast| {
            let id = toast.id;
            let attention = !matches!(toast.tone, Tone::Info);
            let glyph: AnyElement = if toast.progress {
                ui::spinner(SharedString::from(format!("toast-spin-{id}")), 15., theme::text_2())
            } else {
                let (glyph, color) = match toast.tone {
                    Tone::Info if toast.text.starts_with("Copied") || toast.text.starts_with("Done") || toast.text.starts_with("Saved") => {
                        (Icon::Check, theme::text_2())
                    }
                    Tone::Info => (Icon::Info, theme::text_3()),
                    Tone::Warn | Tone::Error => (Icon::Alert, theme::red_text()),
                };
                icon_sized(glyph, 15.).text_color(rgb(color)).into_any_element()
            };
            let card = div()
                .id(("toast", id as usize))
                .w_full()
                .flex()
                .items_start()
                .gap(px(10.))
                .pl(px(12.))
                .pr(px(6.))
                .py(px(9.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(if attention { theme::red_line() } else { theme::line_strong() }))
                .bg(rgb(theme::raised()))
                .shadow_lg()
                .text_size(px(12.5))
                .line_height(px(18.))
                .child(div().pt(px(2.)).child(glyph))
                .child(div().flex_1().min_w(px(0.)).pt(px(1.)).text_color(rgb(theme::text())).child(toast.text.clone()))
                .children(toast.action.clone().map(|(label, action)| {
                    div()
                        .id(("toast-action", id as usize))
                        .flex_none()
                        .px(px(8.))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(theme::text()))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(theme::hover())))
                        .active(|s| s.bg(rgb(theme::selected())))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| this.run_toast_action(id, action.clone(), window, cx)))
                }))
                .when(!toast.progress, |d| {
                    d.child(
                        div()
                            .id(("toast-close", id as usize))
                            .flex_none()
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::hover())))
                            .child(icon_sized(Icon::X, 13.).text_color(rgb(theme::text_3())))
                            .on_click(cx.listener(move |this, _, _, cx| this.dismiss_toast(id, cx))),
                    )
                });
            ui::fade_in(("toast-in", id as usize), card)
        });
        Some(
            div()
                .absolute()
                .bottom(px(38.))
                .right(px(16.))
                .w(px(380.))
                .flex()
                .flex_col()
                .justify_end()
                .gap(px(8.))
                .children(toasts)
                .into_any_element(),
        )
    }

    // ---- Confirm dialog ----------------------------------------------------------------------

    pub(crate) fn ask_confirm(&mut self, action: ConfirmAction, window: &mut Window, cx: &mut Context<Self>) {
        let (title, body, confirm, danger) = match &action {
            ConfirmAction::StopPipeline(sub_id) => {
                let name = self.sub_name(*sub_id);
                (
                    format!("Stop the pipeline for {name}?"),
                    "Agents that are working finish their current step, and nothing new starts. The agents, their worktrees and their conversations stay; you can keep talking to them.".to_string(),
                    "Stop pipeline".to_string(),
                    true,
                )
            }
            ConfirmAction::RemoveApiKey => (
                "Remove the API key?".into(),
                "It's deleted from Windows Credential Manager. Summaries and reviews then use the Max plan, if that's allowed in Settings, or stop.".into(),
                "Remove key".into(),
                true,
            ),
            ConfirmAction::CleanupWorktrees(sub_id) => {
                let name = self.sub_name(*sub_id);
                (
                    format!("Clean up {name}'s worktrees?"),
                    "Removes the worktrees of agents that aren't working and have no uncommitted changes. Their branches are deleted only if everything on them is already in the feature branch. Conversations stay readable.".into(),
                    "Clean up".into(),
                    true,
                )
            }
            ConfirmAction::Rerun(_, stage) => (
                format!("Run {} again?", stage.label().to_lowercase()),
                match stage {
                    crate::pipeline::Stage::Research => "A new researcher starts from your request. The verification and the plan after it are done again too.".to_string(),
                    crate::pipeline::Stage::Build => "New builders start on every workstream of the current plan. The earlier builders and their branches stay.".to_string(),
                    _ => "A new agent does this stage again; the earlier ones and their work stay.".to_string(),
                },
                "Run again".to_string(),
                false,
            ),
            ConfirmAction::Rollback(agent_id, ordinal) => (
                format!("Restore {}'s files to before message #{ordinal}?", self.agent(*agent_id).map(|a| a.record.name.clone()).unwrap_or_default()),
                "Every file in its worktree goes back to how it was then, including new and deleted files; commits made since are undone on its branch. The conversation stays, and the agent hears about it with your next message. You can undo this right after.".to_string(),
                "Restore files".to_string(),
                true,
            ),
            ConfirmAction::Discard(_, file) => (
                format!("Discard the changes to {}?", file.path),
                if file.untracked() {
                    "It's a new file that was never committed, so it's deleted for good.".to_string()
                } else {
                    "The file goes back to its last commit. Uncommitted edits to it are lost.".to_string()
                },
                "Discard".to_string(),
                true,
            ),
            ConfirmAction::RerunBuilder(_, workstream) => (
                format!("Start a new builder for {workstream}?"),
                "It gets the same brief and a branch of its own. The earlier builder stays, so you can compare.".to_string(),
                "Start builder".to_string(),
                false,
            ),
            ConfirmAction::RestartWindows => (
                "Restart Windows now?".into(),
                "Windows restarts in a few seconds; save your work in other programs first. Monaged opens again after you sign in and carries on with the setup.".into(),
                "Restart".into(),
                true,
            ),
            ConfirmAction::Promote(_, from, to) => (
                format!("Merge {from} into {to}?"),
                format!("{to} is your top branch, so this asks first. Monaged merges in a clean checkout and stops if there are conflicts."),
                format!("Merge into {to}"),
                true,
            ),
        };
        let focus = cx.focus_handle();
        window.focus(&focus);
        self.confirm = Some(Confirm { title, body, confirm, danger, action, focus });
        cx.notify();
    }

    pub(crate) fn close_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm = None;
        window.focus(&self.focus);
        self.focus_request = self.sel_agent;
        cx.notify();
    }

    pub(crate) fn accept_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirm) = self.confirm.take() else { return };
        window.focus(&self.focus);
        match confirm.action {
            ConfirmAction::StopPipeline(sub_id) => {
                self.stop_pipeline(sub_id, cx);
                self.toast(Tone::Info, "Pipeline stopped. The agents are still there.", cx);
            }
            ConfirmAction::RemoveApiKey => self.remove_api_key(cx),
            ConfirmAction::CleanupWorktrees(sub_id) => self.cleanup_worktrees(sub_id, cx),
            ConfirmAction::Promote(project_id, from, to) => self.promote_confirmed(project_id, from, to, cx),
            ConfirmAction::Rerun(sub_id, stage) => self.rerun_stage(sub_id, stage, cx),
            ConfirmAction::RerunBuilder(sub_id, workstream) => self.rerun_builder(sub_id, &workstream, cx),
            ConfirmAction::Discard(sub_id, file) => self.git_discard(sub_id, file, cx),
            ConfirmAction::Rollback(agent_id, ordinal) => self.rollback_to(agent_id, ordinal, cx),
            ConfirmAction::RestartWindows => {
                crate::onboarding::reopen_after_restart(true);
                match crate::onboarding::restart_windows() {
                    Ok(()) => self.toast(Tone::Info, "Restarting Windows…", cx),
                    Err(err) => self.toast_error(format!("{err:#}"), cx),
                }
            }
        }
        cx.notify();
    }

    fn sub_name(&self, sub_id: i64) -> String {
        self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.name.clone()).unwrap_or_default()
    }

    /// A centred dialog over a scrim. Enter confirms, Esc cancels.
    pub(crate) fn render_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let panel = div()
            .id("confirm")
            .track_focus(&confirm.focus)
            .key_context("Confirm")
            .occlude()
            .w(px(460.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(20.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::raised()))
            .shadow_lg()
            .child(ui::heading(confirm.title.clone()))
            .child(div().text_size(px(13.)).line_height(px(20.)).text_color(rgb(theme::text_2())).child(confirm.body.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(10.))
                    .child(div().flex().items_center().gap(px(6.)).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(ui::kbd("Enter")).child("confirm").child(ui::kbd("Esc")).child("cancel"))
                    .child(div().flex_1())
                    .child(ui::button("confirm-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, window, cx| this.close_confirm(window, cx))))
                    .child(
                        ui::button("confirm-ok", if confirm.danger { Kind::Danger } else { Kind::Primary }, None, confirm.confirm.clone())
                            .on_click(cx.listener(|this, _, window, cx| this.accept_confirm(window, cx))),
                    ),
            );
        Some(
            div()
                .id("confirm-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(theme::scrim()))
                .occlude()
                .on_click(cx.listener(|this, _, window, cx| this.close_confirm(window, cx)))
                .child(ui::fade_in("confirm-in", panel))
                .into_any_element(),
        )
    }

    // ---- Text prompt ---------------------------------------------------------------------------

    pub(crate) fn ask_text(&mut self, title: String, hint: String, confirm: String, initial: String, action: TextAction, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut input = crate::text_input::TextInput::new(hint.clone(), cx);
            input.set_text(initial, cx);
            input
        });
        window.focus(&input.focus_handle(cx));
        self.text_prompt = Some(TextPrompt { title, hint, confirm, input, action });
        cx.notify();
    }

    pub(crate) fn close_text_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_prompt = None;
        window.focus(&self.focus);
        self.focus_request = self.sel_agent;
        cx.notify();
    }

    pub(crate) fn accept_text_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.text_prompt.take() else { return };
        let text = prompt.input.read(cx).text().trim().to_string();
        window.focus(&self.focus);
        self.focus_request = self.sel_agent;
        if text.is_empty() && !matches!(prompt.action, TextAction::SubCap(_)) {
            cx.notify();
            return;
        }
        match prompt.action {
            TextAction::RenameAgent(agent_id) => self.rename_agent(agent_id, &text, cx),
            TextAction::SaveLayout(project_id) => self.save_layout(project_id, &text, cx),
            TextAction::SubCap(sub_id) => {
                let value = text.trim_start_matches('$').replace(',', ".");
                match value.parse::<f64>() {
                    Ok(cap) if cap > 0.0 => self.set_sub_cap(sub_id, Some(cap), cx),
                    _ if value.is_empty() || value == "0" || value.eq_ignore_ascii_case("none") => self.set_sub_cap(sub_id, None, cx),
                    _ => self.toast(Tone::Warn, "Type an amount in dollars, like 20.", cx),
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn render_text_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.text_prompt.as_ref()?;
        let panel = div()
            .id("text-prompt")
            .occlude()
            .w(px(440.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(20.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::raised()))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(ui::heading(prompt.title.clone()))
            .child(prompt.input.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex().items_center().gap(px(6.)).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(ui::kbd("Enter")).child("save").child(ui::kbd("Esc")).child("cancel"))
                    .child(div().flex_1())
                    .child(ui::button("prompt-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, window, cx| this.close_text_prompt(window, cx))))
                    .child(ui::button("prompt-ok", Kind::Primary, None, prompt.confirm.clone()).on_click(cx.listener(|this, _, window, cx| this.accept_text_prompt(window, cx)))),
            );
        let _ = &prompt.hint;
        Some(
            div()
                .id("text-prompt-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(theme::scrim()))
                .occlude()
                .on_click(cx.listener(|this, _, window, cx| this.close_text_prompt(window, cx)))
                .child(ui::fade_in("text-prompt-in", panel))
                .into_any_element(),
        )
    }

    // ---- Keyboard sheet ------------------------------------------------------------------------

    pub(crate) fn toggle_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcuts_open = !self.shortcuts_open;
        if !self.shortcuts_open {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    pub(crate) fn render_shortcuts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.shortcuts_open {
            return None;
        }
        // Your shortcuts as they are now (Settings → Keyboard), then the fixed keys.
        let mut groups: Vec<(&'static str, Vec<(&'static str, &'static str)>)> = Vec::new();
        for s in crate::keys::SHORTCUTS {
            let Some(keys) = crate::keys::hint(s.id) else { continue };
            match groups.iter_mut().find(|(g, _)| *g == s.group) {
                Some((_, rows)) => rows.push((keys, s.label)),
                None => groups.push((s.group, vec![(keys, s.label)])),
            }
        }
        match groups.iter_mut().find(|(g, _)| *g == "Agents") {
            Some((_, rows)) => rows.splice(0..0, [("Enter", "Send your message"), ("Esc", "Stop the turn"), ("Ctrl-click", "Open beside the current one")]).for_each(drop),
            None => groups.push(("Agents", vec![("Enter", "Send your message"), ("Esc", "Stop the turn")])),
        }
        groups.push(("In the sidebar", vec![("↑  ↓", "Move"), ("→", "Open a level"), ("←", "Back up a level"), ("Enter", "Open"), ("Esc", "Leave the sidebar")]));
        groups.push(("Menus and lists", vec![("Right-click", "Actions for a row"), ("↑  ↓", "Move"), ("Enter", "Run"), ("Esc", "Close")]));
        groups.push(("Forms and dialogs", vec![("Enter", "Submit or confirm"), ("Esc", "Cancel")]));
        let column = |title: &'static str, rows: Vec<(&'static str, &'static str)>| {
            div().flex().flex_col().gap(px(8.)).child(ui::label(title)).children(rows.into_iter().map(|(keys, what)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().w(px(120.)).flex_none().flex().child(ui::kbd(keys)))
                    .child(div().text_size(px(13.)).text_color(rgb(theme::text_2())).child(what))
            }))
        };
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for (i, (title, rows)) in groups.into_iter().enumerate() {
            let el = column(title, rows);
            if i % 2 == 0 { left.push(el) } else { right.push(el) }
        }
        let panel = div()
            .id("shortcuts")
            .occlude()
            .w(px(720.))
            .flex()
            .flex_col()
            .gap(px(20.))
            .p(px(24.))
            .rounded(px(10.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::raised()))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(icon(Icon::Keyboard).text_color(rgb(theme::text_2())))
                    .child(div().flex_1().child(ui::heading("Keyboard")))
                    .child(ui::icon_button("shortcuts-close", Icon::X).on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx)))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(32.))
                    .child(div().flex_1().flex().flex_col().gap(px(20.)).children(left))
                    .child(div().flex_1().flex().flex_col().gap(px(20.)).children(right)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .pt(px(12.))
                    .border_t_1()
                    .border_color(rgb(theme::line()))
                    .child(div().flex_1().child(ui::meta("Change any of these in Settings → Keyboard.")))
                    .child(ui::button("shortcuts-edit", ui::Kind::Secondary, Some(Icon::Sliders), "Change shortcuts").on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_shortcuts(window, cx);
                        this.open_settings_at("Keyboard", window, cx);
                    }))),
            );
        Some(
            div()
                .id("shortcuts-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(theme::scrim()))
                .occlude()
                .on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx)))
                .child(ui::fade_in("shortcuts-in", panel))
                .into_any_element(),
        )
    }

    // ---- Connection ------------------------------------------------------------------------------

    /// After the Claude process dies: try again after 2, 6 and 15 seconds, then wait for you.
    pub(crate) fn schedule_reconnect(&mut self, cx: &mut Context<Self>) {
        if self.logged_in == Some(false) {
            return;
        }
        let Some(&secs) = RECONNECT_DELAYS.get(self.reconnect.attempts) else {
            self.reconnect.at = None;
            return;
        };
        self.reconnect.attempts += 1;
        self.reconnect.generation += 1;
        self.reconnect.at = Some(Instant::now() + Duration::from_secs(secs));
        let generation = self.reconnect.generation;
        cx.spawn(async move |view, cx| {
            cx.background_executor().timer(Duration::from_secs(secs)).await;
            view.update(cx, |this, cx| {
                if this.reconnect.generation == generation && this.reconnect.at.is_some() {
                    this.reconnect.at = None;
                    this.connect(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn reconnect_now(&mut self, cx: &mut Context<Self>) {
        self.reconnect = Reconnect { generation: self.reconnect.generation + 1, ..Default::default() };
        self.connect(cx);
    }

    /// The bar shown while Claude is unreachable.
    pub(crate) fn render_connection_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Conn::Failed(reason) = &self.conn else { return None };
        let countdown = self.reconnect.at.map(|at| at.saturating_duration_since(Instant::now()).as_secs() + 1);
        let text = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(div().text_color(rgb(theme::text())).child(match countdown {
                Some(secs) => format!("Lost the connection to Claude. Reconnecting in {secs}s…"),
                None => "Can't reach Claude inside WSL.".to_string(),
            }))
            .child(div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).truncate().child(reason.clone()))
            .into_any_element();
        let reason_text = reason.clone();
        Some(
            self.notice_bar(text, true)
                .child(ui::button("conn-copy", Kind::Ghost, Some(Icon::Copy), "Copy details").on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(reason_text.clone()));
                    this.toast(Tone::Info, "Copied to the clipboard", cx);
                })))
                .child(ui::button("conn-setup", Kind::Secondary, Some(Icon::Flag), "Setup guide").tooltip(crate::shell::tip("Checks WSL, the Claude agent and your sign-in, and fixes what's missing")).on_click(cx.listener(|this, _, _, cx| this.open_setup(cx))))
                .child(ui::button("conn-retry", Kind::Primary, Some(Icon::Refresh), "Reconnect now").on_click(cx.listener(|this, _, _, cx| this.reconnect_now(cx))))
                .into_any_element(),
        )
    }

    /// Something worth redrawing every second: elapsed times and countdowns.
    pub(crate) fn needs_clock(&self) -> bool {
        self.reconnect.at.is_some() || self.agents.iter().any(|a| a.turn.is_some())
    }

    pub(crate) fn start_clock(cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = view
                    .update(cx, |this, cx| {
                        // A Windows notification was clicked: go where it pointed.
                        if let Some((sub_id, agent_id)) = crate::notify::take_click() {
                            this.form = None;
                            this.select_sub(sub_id, cx);
                            if let Some(agent_id) = agent_id {
                                this.select_agent(agent_id, cx);
                            }
                        }
                        this.check_notifications();
                        this.sync_queue();
                        this.maybe_autofetch(cx);
                        this.remember_place();
                        if this.needs_clock() {
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
    }

    /// Windows notifications for new "Needs you" items while you're in another
    /// window, the flashing taskbar button, and the tray icon's summary.
    pub(crate) fn check_notifications(&mut self) {
        let inbox = self.inbox();
        let keys: std::collections::HashSet<String> =
            inbox.iter().map(|i| format!("{}|{}|{:?}", i.title, i.sub_id, i.agent_id)).collect();
        let os = self.store.setting("notify:os").is_none_or(|v| v == "1");
        let flash = self.store.setting("notify:flash").is_none_or(|v| v == "1");
        if !self.window_active {
            let mut any = false;
            for item in &inbox {
                let key = format!("{}|{}|{:?}", item.title, item.sub_id, item.agent_id);
                if !self.seen_inbox.contains(&key) {
                    any = true;
                    if os {
                        crate::notify::send(&item.title, &item.detail, (item.sub_id, item.agent_id));
                    }
                }
            }
            if any && flash {
                crate::notify::flash(true);
            }
        }
        self.seen_inbox = keys;
        let working = self.agents.iter().filter(|a| a.is_working()).count();
        let tip = match (working, inbox.len()) {
            (0, 0) => "Monaged".to_string(),
            (w, 0) => format!("Monaged · {w} working"),
            (0, n) => format!("Monaged · {n} need{} you", if n == 1 { "s" } else { "" }),
            (w, n) => format!("Monaged · {w} working · {n} need{} you", if n == 1 { "s" } else { "" }),
        };
        if self.last_tip != tip {
            crate::notify::set_tip(&tip);
            self.last_tip = tip;
        }
    }

    /// Keeps the queued messages on disk, so they survive a restart.
    pub(crate) fn sync_queue(&mut self) {
        let current: std::collections::HashMap<i64, String> =
            self.agents.iter().filter_map(|a| a.queued_prompt.clone().map(|q| (a.record.id, q))).collect();
        if current == self.saved_queue {
            return;
        }
        for (id, text) in &current {
            if self.saved_queue.get(id) != Some(text) {
                let _ = self.store.save_queued(*id, Some(text));
            }
        }
        for id in self.saved_queue.keys() {
            if !current.contains_key(id) {
                let _ = self.store.save_queued(*id, None);
            }
        }
        self.saved_queue = current;
    }

    /// An agent finished while you're elsewhere (Settings → Notifications).
    pub(crate) fn notify_finished(&self, agent_id: i64) {
        if self.window_active || self.store.setting("notify:finished").is_some_and(|v| v == "0") || self.store.setting("notify:os").is_some_and(|v| v == "0") {
            return;
        }
        let Some(agent) = self.agent(agent_id) else { return };
        let sub = self.subprojects.iter().find(|s| s.id == agent.record.subproject_id).map(|s| s.name.clone()).unwrap_or_default();
        let preview: String = agent.last_reply().lines().find(|l| !l.trim().is_empty()).unwrap_or_default().chars().take(120).collect();
        crate::notify::send(&format!("{} finished", agent.record.name), &format!("{sub} · {preview}"), (agent.record.subproject_id, Some(agent_id)));
    }

    /// Opens a subproject's inspector on a tab (from menus and toasts).
    pub(crate) fn show_inspector(&mut self, sub_id: i64, tab: InspectorTab, cx: &mut Context<Self>) {
        self.form = None;
        if self.sel_sub != Some(sub_id) {
            self.select_sub(sub_id, cx);
        }
        self.inspector_open = true;
        self.inspector_tab = tab;
        cx.notify();
    }
}
