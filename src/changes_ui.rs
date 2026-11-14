//! Reviewing changes:
//! - the diff view used everywhere, where you can comment on a line (point at
//!   it and press +); comments collect per agent and go to it in one message;
//! - a subproject's combined changes: what's on its feature branch plus what
//!   each agent has in its worktree, file by file, with files that more than
//!   one agent touched marked.

use std::collections::BTreeMap;

use gpui::{AnyElement, Context, Entity, Focusable, SharedString, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::diff::{FileDiff, LineKind};
use crate::icons::{Icon, icon, icon_sized};
use crate::shell::tip;
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

/// Long files: the rest is a click away in Zed.
const MAX_LINES: usize = 2500;

pub(crate) struct LineComment {
    pub agent_id: i64,
    pub path: String,
    pub line: u32,
    pub excerpt: String,
    pub text: String,
}

pub(crate) struct CommentDraft {
    pub agent_id: i64,
    pub path: String,
    pub line: u32,
    pub excerpt: String,
    pub input: Entity<TextInput>,
}

#[derive(Default)]
pub(crate) struct SubChanges {
    pub loading: bool,
    /// What the feature branch has that the project's base doesn't.
    pub branch: Vec<FileDiff>,
    pub error: Option<String>,
    pub selected: Option<String>,
}

impl Monaged {
    // ---- Line comments ---------------------------------------------------------------------

    fn start_comment(&mut self, agent_id: i64, path: String, line: u32, excerpt: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new("What should change here? Enter saves, Esc cancels", cx));
        window.focus(&input.focus_handle(cx));
        self.comment_draft = Some(CommentDraft { agent_id, path, line, excerpt, input });
        cx.notify();
    }

    pub(crate) fn comment_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.comment_draft.as_ref().is_some_and(|d| d.input.focus_handle(cx).is_focused(window))
    }

    pub(crate) fn save_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.comment_draft.take() else { return };
        let text = draft.input.read(cx).text().trim().to_string();
        if !text.is_empty() {
            self.comments.push(LineComment { agent_id: draft.agent_id, path: draft.path, line: draft.line, excerpt: draft.excerpt, text });
        }
        cx.notify();
    }

    pub(crate) fn cancel_comment(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.comment_draft.take().is_some();
        cx.notify();
        had
    }

    /// Sends an agent all your comments on its changes, as one message.
    pub(crate) fn send_comments(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let mine: Vec<&LineComment> = self.comments.iter().filter(|c| c.agent_id == agent_id).collect();
        if mine.is_empty() {
            return;
        }
        let mut text = String::from("Review comments on your changes (from the developer). Please address each one:\n");
        for c in &mine {
            text.push_str(&format!("\n- {}:{}: {}\n  > {}", c.path, c.line, c.text, c.excerpt.trim()));
        }
        let count = mine.len();
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        self.comments.retain(|c| c.agent_id != agent_id);
        self.bypass_budget = true;
        self.send_prompt(agent_id, text);
        self.bypass_budget = false;
        self.toast(Tone::Info, format!("Sent {count} comment{} to {name}", if count == 1 { "" } else { "s" }), cx);
        cx.notify();
    }

    /// "3 comments for build-core · Send · Clear", above a diff.
    pub(crate) fn render_comments_bar(&self, agent_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let count = self.comments.iter().filter(|c| c.agent_id == agent_id).count();
        if count == 0 {
            return None;
        }
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(theme::line_strong()))
                .bg(rgb(theme::raised()))
                .child(icon_sized(Icon::Ask, 14.).text_color(rgb(theme::text_2())))
                .child(div().flex_1().text_size(px(13.)).child(format!("{count} comment{} for {name}", if count == 1 { "" } else { "s" })))
                .child(ui::button(("comments-clear", agent_id as usize), Kind::Ghost, None, "Clear").on_click(cx.listener(move |this, _, _, cx| {
                    this.comments.retain(|c| c.agent_id != agent_id);
                    cx.notify();
                })))
                .child(ui::button(("comments-send", agent_id as usize), Kind::Primary, Some(Icon::Send), format!("Send to {name}")).on_click(cx.listener(
                    move |this, _, _, cx| this.send_comments(agent_id, cx),
                )))
                .into_any_element(),
        )
    }

    /// A file's diff. With an owning agent, each line takes comments.
    pub(crate) fn render_diff_body(&self, owner: Option<i64>, file: &FileDiff, cx: &mut Context<Self>) -> AnyElement {
        let shown = file.lines.len().min(MAX_LINES);
        let path = file.path.clone();
        let mut rows: Vec<AnyElement> = Vec::with_capacity(shown);
        for (n, line) in file.lines[..shown].iter().enumerate() {
            let (sign, bg, fg) = match line.kind {
                LineKind::Add => ("+", Some(theme::add_bg()), theme::text()),
                LineKind::Remove => ("−", Some(theme::del_bg()), theme::text()),
                LineKind::Hunk => ("", Some(theme::code_bg()), theme::text_3()),
                LineKind::Context => ("", None, theme::text_2()),
            };
            let sign_color = match line.kind {
                LineKind::Add => theme::add(),
                LineKind::Remove => theme::del(),
                _ => theme::text_4(),
            };
            let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
            let text = if line.text.is_empty() { " ".to_string() } else { line.text.replace('\t', "    ") };
            let line_no = line.new_no.or(line.old_no);
            let commentable = owner.is_some() && line.kind != LineKind::Hunk && line_no.is_some();
            let row_id = SharedString::from(format!("dl-{}-{}-{n}", owner.unwrap_or(0), path));
            let mut row = div()
                .id(row_id)
                .flex()
                .font_family(theme::MONO)
                .text_size(px(12.))
                .line_height(px(19.))
                .group("diff-line")
                .when_some(bg, |d, bg| d.bg(rgb(bg)))
                .child(div().w(px(40.)).flex_none().pr(px(8.)).text_right().text_color(rgb(theme::text_4())).child(number(line.old_no)))
                .child(div().w(px(40.)).flex_none().pr(px(8.)).text_right().text_color(rgb(theme::text_4())).child(number(line.new_no)));
            if commentable {
                let (agent_id, p, no, excerpt) = (owner.unwrap_or(0), path.clone(), line_no.unwrap_or(0), line.text.clone());
                row = row.child(
                    div()
                        .id(SharedString::from(format!("dl-c-{agent_id}-{p}-{n}")))
                        .relative()
                        .w(px(16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(div().group_hover("diff-line", |s| s.invisible()).text_color(rgb(sign_color)).child(sign))
                        .child(
                            div()
                                .absolute()
                                .size(px(14.))
                                .rounded(px(3.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(rgb(theme::text()))
                                .invisible()
                                .group_hover("diff-line", |s| s.visible())
                                .child(icon_sized(Icon::Plus, 10.).text_color(rgb(theme::on_primary()))),
                        )
                        .tooltip(tip("Comment on this line"))
                        .on_click(cx.listener(move |this, _, window, cx| this.start_comment(agent_id, p.clone(), no, excerpt.clone(), window, cx))),
                );
            } else {
                row = row.child(div().w(px(16.)).flex_none().text_color(rgb(sign_color)).child(sign));
            }
            rows.push(row.child(div().flex_1().min_w(px(0.)).text_color(rgb(fg)).child(text)).into_any_element());

            // Comments on this line, and the one being written.
            if let (Some(agent_id), Some(no)) = (owner, line_no) {
                for (i, c) in self.comments.iter().enumerate().filter(|(_, c)| c.agent_id == agent_id && c.path == path && c.line == no) {
                    rows.push(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(8.))
                            .ml(px(96.))
                            .mr(px(8.))
                            .my(px(4.))
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgb(theme::line_strong()))
                            .bg(rgb(theme::raised()))
                            .text_size(px(12.5))
                            .child(icon_sized(Icon::Ask, 13.).text_color(rgb(theme::text_3())))
                            .child(div().flex_1().min_w(px(0.)).child(c.text.clone()))
                            .child(
                                div()
                                    .id(("comment-remove", i))
                                    .cursor_pointer()
                                    .child(icon_sized(Icon::X, 12.).text_color(rgb(theme::text_3())))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if i < this.comments.len() {
                                            this.comments.remove(i);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .into_any_element(),
                    );
                }
                if let Some(draft) = self.comment_draft.as_ref().filter(|d| d.agent_id == agent_id && d.path == path && d.line == no) {
                    rows.push(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .ml(px(96.))
                            .mr(px(8.))
                            .my(px(4.))
                            .child(div().flex_1().child(draft.input.clone()))
                            .child(ui::button("comment-save", Kind::Primary, None, "Comment").on_click(cx.listener(|this, _, _, cx| this.save_comment(cx))))
                            .child(ui::button("comment-cancel", Kind::Ghost, None, "Cancel").on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_comment(cx);
                            })))
                            .into_any_element(),
                    );
                }
            }
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .bg(rgb(theme::panel()))
                    .py(px(4.))
                    .overflow_hidden()
                    .when(file.binary, |d| d.child(div().p(px(10.)).text_color(rgb(theme::text_3())).child("Binary file")))
                    .children(rows),
            )
            .when(file.lines.len() > shown, |d| {
                d.child(ui::meta(format!("{} more lines not shown; open the file in Zed to see everything.", file.lines.len() - shown)))
            })
            .into_any_element()
    }

    // ---- A subproject's combined changes -----------------------------------------------------

    pub(crate) fn open_sub_changes(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        self.form = None;
        if self.sel_sub != Some(sub_id) {
            self.select_sub(sub_id, cx);
        }
        self.sub_view = Some(sub_id);
        self.load_sub_changes(sub_id, cx);
        cx.notify();
    }

    pub(crate) fn close_sub_changes(&mut self, cx: &mut Context<Self>) {
        self.sub_view = None;
        cx.notify();
    }

    pub(crate) fn load_sub_changes(&mut self, sub_id: i64, cx: &mut Context<Self>) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id).cloned() else { return };
        let Some(project) = self.projects.iter().find(|p| p.id == sub.project_id).cloned() else { return };
        // Each agent's worktree diff too.
        let agents: Vec<i64> = self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived).map(|a| a.record.id).collect();
        for id in agents {
            if self.agent(id).is_some_and(|a| !a.loading_changes) {
                self.refresh_changes(id, false, cx);
            }
        }
        self.sub_changes.entry(sub_id).or_default().loading = true;
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let script = format!(
                "cd {repo} || exit 1\nbase=$(git merge-base {base} {branch} 2>/dev/null) || exit 0\ngit -c core.quotepath=off diff --no-color --no-ext-diff -M \"$base\" {branch}",
                repo = crate::wsl::shell_path(&project.path),
                base = crate::wsl::quote(&project.base_branch),
                branch = crate::wsl::quote(&sub.branch),
            );
            let result = cx.background_spawn(async move { crate::wsl::run(&distro, &script) }).await;
            view.update(cx, |this, cx| {
                let state = this.sub_changes.entry(sub_id).or_default();
                state.loading = false;
                match result {
                    Ok(raw) => {
                        state.branch = crate::diff::parse(&raw);
                        state.error = None;
                    }
                    Err(err) => state.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_sub_changes(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return div().into_any_element() };
        let state = self.sub_changes.get(&sub_id);
        let loading = state.is_none_or(|s| s.loading) || self.agents.iter().any(|a| a.record.subproject_id == sub_id && a.loading_changes);
        // path -> sources: (label, owning agent, diff)
        let mut files: BTreeMap<String, Vec<(String, Option<i64>, &FileDiff)>> = BTreeMap::new();
        if let Some(state) = state {
            for f in &state.branch {
                files.entry(f.path.clone()).or_default().push((format!("on {}", sub.branch), None, f));
            }
        }
        for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived) {
            for f in agent.changes.iter().flatten() {
                files.entry(f.path.clone()).or_default().push((agent.record.name.clone(), Some(agent.record.id), f));
            }
        }
        let selected = state.and_then(|s| s.selected.clone()).filter(|p| files.contains_key(p)).or_else(|| files.keys().next().cloned());
        let overlaps = files.values().filter(|sources| sources.iter().filter(|(_, a, _)| a.is_some()).count() > 1).count();

        let header = div()
            .flex_none()
            .h(px(54.))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(16.))
            .border_b_1()
            .border_color(rgb(theme::line()))
            .child(icon(Icon::Diff).text_color(rgb(theme::text_2())))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(ui::heading(format!("All changes in {}", sub.name)))
                    .child(ui::meta(format!(
                        "{} file{} · {} · agents' worktrees{}",
                        files.len(),
                        if files.len() == 1 { "" } else { "s" },
                        sub.branch,
                        if overlaps > 0 { format!(" · {overlaps} touched by more than one agent") } else { String::new() }
                    ))),
            )
            .child(if loading {
                div().id("sub-changes-refresh").size(px(28.)).flex().items_center().justify_center().child(ui::spinner("sub-changes-spin", 14., theme::text_2()))
            } else {
                ui::icon_button("sub-changes-refresh", Icon::Refresh).tooltip(tip("Read the changes again")).on_click(cx.listener(move |this, _, _, cx| this.load_sub_changes(sub_id, cx)))
            })
            .child(ui::button("sub-changes-close", Kind::Secondary, None, "Back to agents").on_click(cx.listener(|this, _, _, cx| this.close_sub_changes(cx))));

        let list = div().w(px(320.)).flex_none().flex().flex_col().gap(px(1.)).children(files.iter().enumerate().map(|(i, (path, sources))| {
            let agents = sources.iter().filter(|(_, a, _)| a.is_some()).count();
            let added: usize = sources.iter().map(|(_, _, f)| f.added).sum();
            let removed: usize = sources.iter().map(|(_, _, f)| f.removed).sum();
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let p = path.clone();
            ui::row(("sub-file", i), selected.as_deref() == Some(path.as_str()))
                .h(px(30.))
                .when(agents > 1, |d| d.child(icon_sized(Icon::Alert, 12.).text_color(rgb(theme::red_text()))))
                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.5)).child(name))
                .child(div().flex_none().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(theme::text_3())).child(format!("{}×", sources.len())))
                .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::add())).child(format!("+{added}")))
                .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::del())).child(format!("−{removed}")))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.sub_changes.entry(sub_id).or_default().selected = Some(p.clone());
                    cx.notify();
                }))
        }));

        let detail: AnyElement = match selected.as_ref().and_then(|p| files.get(p).map(|s| (p, s))) {
            None if loading => ui::skeleton_text("sub-changes-sk", 6).into_any_element(),
            None => div()
                .text_color(rgb(theme::text_3()))
                .child(state.and_then(|s| s.error.clone()).unwrap_or_else(|| {
                    match self.store.setting(&format!("sub:{sub_id}:merged")) {
                        Some(merged) => format!(
                            "Nothing left to show: everything on {} is merged into {}, and the agents' worktrees have no changes.",
                            sub.branch,
                            merged.split(' ').next().unwrap_or("its target")
                        ),
                        None => "No changes yet: nothing on the feature branch and nothing in the agents' worktrees.".into(),
                    }
                }))
                .into_any_element(),
            Some((path, sources)) => {
                let mut col = div().flex_1().min_w(px(0.)).flex().flex_col().gap(px(18.));
                col = col.child(div().font_family(theme::MONO).text_size(px(13.)).child(path.clone()));
                if sources.iter().filter(|(_, a, _)| a.is_some()).count() > 1 {
                    col = col.child(ui::callout(Tone::Warn, "More than one agent changed this file. Merging them may conflict; compare the versions below."));
                }
                for (label, owner, file) in sources {
                    col = col.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .children(owner.and_then(|id| self.render_comments_bar(id, cx)))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(icon_sized(if owner.is_some() { Icon::Agent } else { Icon::Branch }, 13.).text_color(rgb(theme::text_3())))
                                    .child(ui::label(label.clone()))
                                    .child(div().flex_1())
                                    .child(ui::mono(format!("+{} −{}", file.added, file.removed))),
                            )
                            .child(self.render_diff_body(*owner, file, cx)),
                    );
                }
                col.into_any_element()
            }
        };

        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("sub-changes")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(div().flex().items_start().gap(px(20.)).p(px(20.)).child(list).child(detail)),
            )
            .into_any_element()
    }
}
