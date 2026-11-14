//! Phase 2: the Changes tab (diff), Haiku summaries, the confidence score,
//! Markdown rendering and the Settings page.

use std::ops::Range;

use gpui::{
    AnyElement, Context, FontStyle, FontWeight, HighlightStyle, SharedString, Stateful, StyledText, div, prelude::*,
    px, rgb, rgba,
};

use crate::agent::{AgentTab, AgentView, Tone};
use crate::diff::{self, LineKind};
use crate::llm::Backend;
use crate::markdown::{self, Block, Inline, Style};
use crate::review::{self, TurnInfo};
use crate::{Form, Monaged, button, secrets, theme, tone_color};

const MAX_DIFF_LINES: usize = 2500;

impl Monaged {
    /// The API key when set; otherwise the Claude CLI (Max plan) if allowed.
    pub(crate) fn backend(&self) -> Option<Backend> {
        if let Some(key) = &self.api_key {
            return Some(Backend::Api { key: key.clone() });
        }
        self.cli_fallback.then(|| Backend::Cli { distro: self.config.distro.clone() })
    }

    /// (distro, worktree, branch to diff against). Agents diff against the
    /// subproject branch they started from; the integrator works *on* that
    /// branch, so it diffs against the project's base branch instead.
    fn agent_context(&self, agent_id: i64) -> Option<(String, String, String)> {
        let agent = self.agent(agent_id)?;
        let sub = self.subprojects.iter().find(|s| s.id == agent.record.subproject_id)?;
        let base = if agent.role() == crate::pipeline::Role::Integrator {
            self.projects.iter().find(|p| p.id == sub.project_id).map(|p| p.base_branch.clone())?
        } else {
            sub.branch.clone()
        };
        Some((self.config.distro.clone(), agent.record.worktree.clone(), base))
    }

    pub(crate) fn refresh_spend(&mut self) {
        self.spend = self.store.llm_spend().unwrap_or_default();
    }

    /// A turn ended: move the pipeline on, send anything queued, refresh the
    /// diff, then (if enabled) summarise and review.
    pub(crate) fn after_turn(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        // A turn that only answered another agent's question isn't pipeline work.
        let answered = self.resolve_asks(agent_id);
        let other_result = self.agent(agent_id).is_some_and(|a| {
            crate::pipeline::directives(&a.last_reply()).iter().any(|d| !matches!(d, crate::pipeline::Directive::Answer(_)))
        });
        if !answered || other_result {
            self.pipeline_on_turn(agent_id, cx);
        }
        self.flush_queued_prompt(agent_id);
        // Agents commit and merge, so keep the merge card's counts current.
        if let Some(project_id) = self
            .agent(agent_id)
            .and_then(|a| self.subprojects.iter().find(|s| s.id == a.record.subproject_id))
            .map(|s| s.project_id)
        {
            self.refresh_branches(project_id, cx);
        }
        // Without an API key the reviews run on the Max plan, so they respect the budget too.
        let on_max = matches!(self.backend(), Some(Backend::Cli { .. }));
        let auto = !answered && self.auto_review && self.backend().is_some() && !(on_max && self.budget_block().is_some());
        self.refresh_changes(agent_id, auto, cx);
    }

    pub(crate) fn refresh_changes(&mut self, agent_id: i64, then_review: bool, cx: &mut Context<Self>) {
        let Some((distro, worktree, base)) = self.agent_context(agent_id) else { return };
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.loading_changes = true;
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_spawn(async move { diff::collect(&distro, &worktree, &base) }).await;
            view.update(cx, |this, cx| {
                let mut has_changes = false;
                if let Some(agent) = this.agent_mut(agent_id) {
                    agent.loading_changes = false;
                    match result {
                        Ok(changes) => {
                            has_changes = !changes.files.is_empty();
                            agent.selected_file = agent.selected_file.min(changes.files.len().saturating_sub(1));
                            agent.changes = Some(changes.files);
                            agent.changes_raw = changes.raw;
                            agent.changes_truncated = changes.truncated;
                        }
                        Err(err) => agent.review_error = Some(format!("Couldn't read the changes: {err:#}")),
                    }
                }
                if then_review {
                    this.summarize(agent_id, cx);
                    if has_changes {
                        this.review_agent(agent_id, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn no_backend(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.review_error =
                Some("Add an Anthropic API key in Settings, or allow the Max plan fallback there.".into());
        }
        cx.notify();
    }

    pub(crate) fn summarize(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(backend) = self.backend() else { return self.no_backend(agent_id, cx) };
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.summarizing {
            return;
        }
        let task = agent.task();
        let last_request = agent.last_request();
        let reply = agent.last_reply();
        let tools = agent.tools_this_turn();
        let diffstat = agent.changes.as_deref().map(diff::diffstat).unwrap_or_default();
        let previous = agent.summary.as_ref().map(|s| s.text.clone());
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.summarizing = true;
            agent.review_error = None;
        }
        cx.notify();
        cx.spawn(async move |view, cx| {
            let via = backend.via();
            let result = cx
                .background_spawn(async move {
                    let turn = TurnInfo {
                        task: &task,
                        last_request: &last_request,
                        agent_reply: &reply,
                        tools: &tools,
                        diffstat: &diffstat,
                        previous: previous.as_deref(),
                    };
                    review::summarize(&backend, &turn)
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok((summary, usage)) => {
                        let _ = this.store.save_summary(agent_id, &summary);
                        this.log_haiku(agent_id, "summary", via, &usage);
                        this.refresh_spend();
                        if let Some(agent) = this.agent_mut(agent_id) {
                            agent.summary = Some(summary);
                            agent.summarizing = false;
                        }
                    }
                    Err(err) => {
                        if let Some(agent) = this.agent_mut(agent_id) {
                            agent.summarizing = false;
                            agent.review_error = Some(format!("Summary failed: {err:#}"));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-reads the diff, runs the project's checks, then asks Haiku for a skeptical review.
    pub(crate) fn review_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(backend) = self.backend() else { return self.no_backend(agent_id, cx) };
        let Some((distro, worktree, base)) = self.agent_context(agent_id) else { return };
        let Some(agent) = self.agent_mut(agent_id) else { return };
        if agent.reviewing {
            return;
        }
        let task = agent.task();
        agent.reviewing = true;
        agent.review_error = None;
        cx.notify();
        cx.spawn(async move |view, cx| {
            let via = backend.via();
            let result = cx
                .background_spawn(async move {
                    let changes = diff::collect(&distro, &worktree, &base)?;
                    if changes.files.is_empty() {
                        return anyhow::Ok((changes, None));
                    }
                    let checks = review::run_checks(&distro, &worktree);
                    let reviewed = review::review(&backend, &task, &changes.raw, &checks)?;
                    Ok((changes, Some(reviewed)))
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok((changes, reviewed)) => {
                        if let Some((confidence, usage)) = &reviewed {
                            let _ = this.store.save_confidence(agent_id, confidence);
                            this.log_haiku(agent_id, "review", via, usage);
                            this.refresh_spend();
                        }
                        if let Some(agent) = this.agent_mut(agent_id) {
                            agent.reviewing = false;
                            agent.selected_file = agent.selected_file.min(changes.files.len().saturating_sub(1));
                            agent.changes = Some(changes.files);
                            agent.changes_raw = changes.raw;
                            agent.changes_truncated = changes.truncated;
                            match reviewed {
                                Some((confidence, _)) => agent.confidence = Some(confidence),
                                None => agent.review_error = Some("Nothing to review yet: this worktree has no changes.".into()),
                            }
                        }
                        // A new score may be what an auto-merge was waiting for.
                        if let Some(sub_id) = this.agent(agent_id).map(|a| a.record.subproject_id) {
                            this.maybe_auto_merge(sub_id, cx);
                        }
                    }
                    Err(err) => {
                        if let Some(agent) = this.agent_mut(agent_id) {
                            agent.reviewing = false;
                            agent.review_error = Some(format!("Review failed: {err:#}"));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_tab(&mut self, agent_id: i64, tab: AgentTab, cx: &mut Context<Self>) {
        let needs_load = self.agent(agent_id).is_some_and(|a| a.changes.is_none() && !a.loading_changes);
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.tab = tab;
        }
        if tab == AgentTab::Changes && needs_load {
            self.refresh_changes(agent_id, false, cx);
        }
        cx.notify();
    }

    // ---- Settings ----------------------------------------------------------

    fn paste_api_key(&mut self, cx: &mut Context<Self>) {
        let pasted = cx.read_from_clipboard().and_then(|item| item.text()).map(|t| t.trim().to_string());
        let error = match pasted {
            Some(key) if key.starts_with("sk-ant-") => match secrets::save_api_key(&key) {
                Ok(()) => {
                    self.api_key = Some(key);
                    self.banner = Some((Tone::Info, "API key saved in Windows Credential Manager.".into()));
                    None
                }
                Err(err) => Some(format!("{err:#}")),
            },
            _ => Some("The clipboard doesn't hold an Anthropic API key (it starts with sk-ant-). Copy it first.".into()),
        };
        if let Some(Form::Settings { error: slot }) = &mut self.form {
            *slot = error;
        }
        cx.notify();
    }

    fn remove_api_key(&mut self, cx: &mut Context<Self>) {
        let error = secrets::delete_api_key().err().map(|e| format!("{e:#}"));
        if error.is_none() {
            self.api_key = secrets::load_api_key();
        }
        if let Some(Form::Settings { error: slot }) = &mut self.form {
            *slot = error;
        }
        cx.notify();
    }

    fn toggle_setting(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let value = match key {
            "cli_fallback" => {
                self.cli_fallback = !self.cli_fallback;
                self.cli_fallback
            }
            _ => {
                self.auto_review = !self.auto_review;
                self.auto_review
            }
        };
        let _ = self.store.set_setting(key, if value { "1" } else { "0" });
        cx.notify();
    }

    pub(crate) fn render_settings(&self, error: Option<&String>, cx: &mut Context<Self>) -> AnyElement {
        let key_line = match &self.api_key {
            Some(key) => format!("Saved: {}", secrets::mask(key)),
            None => "Not set".to_string(),
        };
        let active = match self.backend() {
            Some(Backend::Api { .. }) => "Summaries and reviews use your API key (Claude Haiku 5.5).",
            Some(Backend::Cli { .. }) => "No API key: summaries and reviews run through the Claude CLI and count against your Max plan.",
            None => "Summaries and reviews are off: add an API key or allow the Max plan fallback.",
        };
        let toggle = |id: &'static str, label: &'static str, on: bool, key: &'static str, cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .child(
                    div()
                        .w(px(34.))
                        .h(px(18.))
                        .rounded_full()
                        .p(px(2.))
                        .flex()
                        .when(on, |d| d.justify_end().bg(rgb(theme::ALLOW_BG)))
                        .when(!on, |d| d.bg(rgb(theme::BORDER)))
                        .child(div().size(px(14.)).rounded_full().bg(rgb(theme::TEXT))),
                )
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_setting(key, cx)))
        };
        let (api_spend, max_spend) = self.spend;

        div()
            .id("settings")
            .flex_1()
            .overflow_y_scroll()
            .p_8()
            .flex()
            .flex_col()
            .gap_6()
            .max_w(px(720.))
            .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child("Settings"))
            .child(section("Usage and budget").child(self.render_usage_settings(cx)))
            .child(section("Models").child(self.render_model_settings(cx)))
            .child(
                section("Anthropic API key")
                    .child(div().text_color(rgb(theme::MUTED)).text_size(px(13.)).child(
                        "Pays for the cheap Haiku work (summaries, reviews) from your API credit instead of your Max plan. \
                         Stored in Windows Credential Manager, never in Monaged's database. Copy the key, then click Paste.",
                    ))
                    .child(div().font_family("Consolas").text_size(px(13.)).child(key_line))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                button("paste-key", "Paste key from clipboard", theme::ALLOW_BG)
                                    .on_click(cx.listener(|this, _, _, cx| this.paste_api_key(cx))),
                            )
                            .when(self.api_key.is_some(), |d| {
                                d.child(
                                    button("remove-key", "Remove key", theme::DENY_BG)
                                        .on_click(cx.listener(|this, _, _, cx| this.remove_api_key(cx))),
                                )
                            }),
                    )
                    .children(error.map(|e| div().text_size(px(13.)).text_color(rgb(theme::BAD)).child(e.clone()))),
            )
            .child(
                section("Summaries and reviews")
                    .child(div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(active))
                    .child(toggle("toggle-auto", "Summarize and review after every agent turn", self.auto_review, "auto_review", cx))
                    .child(toggle(
                        "toggle-cli",
                        "When there's no API key, use the Max plan (Claude CLI) instead",
                        self.cli_fallback,
                        "cli_fallback",
                        cx,
                    )),
            )
            .child(
                section("Haiku spend so far").child(div().text_size(px(13.)).child(format!(
                    "${api_spend:.4} from your API credit · ${max_spend:.4} worth of Max plan usage"
                ))),
            )
            .child(self.render_credentials_section(cx))
            .child(
                section("Git identity in WSL")
                    .child(div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(
                        "Builders commit their work, so git inside WSL needs a name and email. This copies the ones \
                         from your Windows git config (git config --global user.name / user.email).",
                    ))
                    .child(div().flex().child(
                        button("settings-copy-identity", "Copy my Windows git identity into WSL", theme::INPUT_BG)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_git_identity(cx))),
                    )),
            )
            .into_any_element()
    }

    // ---- Agent bar and Changes tab -------------------------------------------

    pub(crate) fn render_agent_bar(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let agent_id = agent.record.id;
        let changes_label: SharedString = match agent.changes.as_ref().map(Vec::len) {
            Some(n) => format!("Changes · {n}").into(),
            None => "Changes".into(),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .py_1p5()
            .border_b_1()
            .border_color(rgb(theme::BORDER))
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(tab_button("tab-conversation", "Conversation".into(), agent.tab == AgentTab::Conversation).on_click(
                        cx.listener(move |this, _, _, cx| this.set_tab(agent_id, AgentTab::Conversation, cx)),
                    ))
                    .child(tab_button("tab-changes", changes_label, agent.tab == AgentTab::Changes).on_click(cx.listener(
                        move |this, _, _, cx| this.set_tab(agent_id, AgentTab::Changes, cx),
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(self.agent_usage_label(agent).map(|l| div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child(l)))
                    .when(agent.summarizing, |d| {
                        d.child(div().text_size(px(12.)).text_color(rgb(theme::MUTED)).child("Summarizing…"))
                    })
                    .children(agent.confidence.as_ref().map(|c| confidence_badge(c.score, false)))
                    .child(
                        button("review-now", if agent.reviewing { "Reviewing…" } else { "Review" }, theme::INPUT_BG)
                            .on_click(cx.listener(move |this, _, _, cx| this.review_agent(agent_id, cx))),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn render_changes(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let agent_id = agent.record.id;

        let summary_body: AnyElement = match (&agent.summary, agent.summarizing) {
            (_, true) if agent.summary.is_none() => muted("Writing a summary…"),
            (Some(summary), _) => div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().flex().child(status_pill(&summary.status)))
                .child(div().line_height(px(21.)).child(summary.text.clone()))
                .when(!summary.next_step.is_empty(), |d| {
                    d.child(div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(format!("Next: {}", summary.next_step)))
                })
                .into_any_element(),
            (None, _) => muted("No summary yet. One is written after each turn, or click Summarize."),
        };

        let confidence_body: AnyElement = match &agent.confidence {
            None if agent.reviewing => muted("Running the checks and a skeptical review…"),
            None => muted("Not reviewed yet. Review runs the project's tests and asks Haiku to find what's broken."),
            Some(c) => div()
                .flex()
                .items_start()
                .gap_4()
                .child(confidence_badge(c.score, true))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(px(13.))
                        .children(c.reasons.iter().map(|r| div().child(r.clone())))
                        .children(c.issues.iter().map(|issue| {
                            let color = match issue.severity.as_str() {
                                "high" => theme::BAD,
                                "medium" => theme::WARN,
                                _ => theme::MUTED,
                            };
                            div()
                                .flex()
                                .gap_2()
                                .child(div().flex_none().text_color(rgb(color)).child(issue.severity.clone()))
                                .child(div().flex_none().font_family("Consolas").text_color(rgb(theme::MUTED)).child(issue.file.clone()))
                                .child(div().flex_1().min_w(px(0.)).child(issue.problem.clone()))
                        })),
                )
                .into_any_element(),
        };

        let files: AnyElement = match &agent.changes {
            None if agent.loading_changes => muted("Reading the changes…"),
            None => muted("Changes not loaded yet."),
            Some(files) if files.is_empty() => muted("No changes in this worktree yet."),
            Some(files) => {
                let selected = agent.selected_file.min(files.len() - 1);
                let file = &files[selected];
                let file_path = format!("{}/{}", agent.record.worktree, file.path);
                let list = div().w(px(280.)).flex_none().flex().flex_col().gap_0p5().children(files.iter().enumerate().map(|(i, f)| {
                    let (mark, color) = match f.status {
                        "added" => ("A", theme::GOOD),
                        "deleted" => ("D", theme::BAD),
                        "renamed" => ("R", theme::ACCENT),
                        _ => ("M", theme::WARN),
                    };
                    div()
                        .id(("file", i))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(px(13.))
                        .when(i == selected, |d| d.bg(rgb(theme::SELECTED)))
                        .hover(|s| s.bg(rgb(theme::HOVER)))
                        .child(div().flex_none().font_family("Consolas").text_color(rgb(color)).child(mark))
                        .child(div().flex_1().min_w(px(0.)).overflow_hidden().child(f.path.clone()))
                        .child(div().flex_none().text_size(px(11.)).text_color(rgb(theme::GOOD)).child(format!("+{}", f.added)))
                        .child(div().flex_none().text_size(px(11.)).text_color(rgb(theme::BAD)).child(format!("−{}", f.removed)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(agent) = this.agent_mut(agent_id) {
                                agent.selected_file = i;
                            }
                            cx.notify();
                        }))
                }));
                let shown = file.lines.len().min(MAX_DIFF_LINES);
                let lines = file.lines[..shown].iter().map(|line| {
                    let (sign, bg) = match line.kind {
                        LineKind::Add => ("+", Some(rgba(0x2ea04330))),
                        LineKind::Remove => ("−", Some(rgba(0xf8514930))),
                        LineKind::Hunk => ("", Some(rgba(0x6ea8fe14))),
                        LineKind::Context => ("", None),
                    };
                    let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
                    let text = if line.text.is_empty() { " ".to_string() } else { line.text.replace('\t', "    ") };
                    div()
                        .flex()
                        .font_family("Consolas")
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .when_some(bg, |d, bg| d.bg(bg))
                        .child(div().w(px(38.)).flex_none().text_color(rgb(theme::MUTED)).child(number(line.old_no)))
                        .child(div().w(px(38.)).flex_none().text_color(rgb(theme::MUTED)).child(number(line.new_no)))
                        .child(div().w(px(14.)).flex_none().child(sign))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .when(line.kind == LineKind::Hunk, |d| d.text_color(rgb(theme::ACCENT)))
                                .child(text),
                        )
                });
                let diff_view = div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().font_family("Consolas").text_size(px(13.)).child(file.path.clone()))
                            .child(button("file-zed", "Open in Zed", theme::INPUT_BG).on_click(cx.listener(
                                move |this, _, _, cx| this.open_in_zed(file_path.clone(), cx),
                            ))),
                    )
                    .child(
                        div()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(theme::BORDER))
                            .bg(rgb(theme::PANEL))
                            .py_1()
                            .when(file.binary, |d| d.child(muted("Binary file")))
                            .children(lines),
                    )
                    .when(file.lines.len() > shown, |d| {
                        d.child(muted(&format!("{} more lines not shown; open the file in Zed to see everything.", file.lines.len() - shown)))
                    })
                    .when(agent.changes_truncated, |d| d.child(muted("This diff is very large; only the first part was loaded.")));
                div().flex().items_start().gap_4().child(list).child(diff_view).into_any_element()
            }
        };

        div()
            .id("changes")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_4()
                    .children(agent.review_error.as_ref().map(|e| {
                        div().text_size(px(13.)).text_color(rgb(tone_color(Tone::Error))).child(e.clone())
                    }))
                    .child(
                        card("Summary").child(summary_body).child(
                            div().flex().child(
                                button("summarize", if agent.summarizing { "Summarizing…" } else { "Summarize" }, theme::INPUT_BG)
                                    .on_click(cx.listener(move |this, _, _, cx| this.summarize(agent_id, cx))),
                            ),
                        ),
                    )
                    .child(card("Confidence").child(confidence_body))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child(format!(
                                "CHANGES ON {} (VS. WHERE IT STARTED)",
                                agent.record.branch.to_uppercase()
                            )))
                            .child(button("refresh-changes", if agent.loading_changes { "Refreshing…" } else { "Refresh" }, theme::INPUT_BG)
                                .on_click(cx.listener(move |this, _, _, cx| this.refresh_changes(agent_id, false, cx)))),
                    )
                    .child(files),
            )
            .into_any_element()
    }
}

// ---- Markdown ----------------------------------------------------------------

pub(crate) fn render_markdown(text: &str) -> AnyElement {
    div().flex().flex_col().gap_2().children(markdown::parse(text).into_iter().map(render_block)).into_any_element()
}

fn styled(inline: Inline) -> StyledText {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = inline.styles.iter().map(|(range, style)| (range.clone(), highlight(*style))).collect();
    StyledText::new(inline.text).with_highlights(highlights)
}

fn highlight(style: Style) -> HighlightStyle {
    let mut h = HighlightStyle::default();
    if style.bold {
        h.font_weight = Some(FontWeight::BOLD);
    }
    if style.italic {
        h.font_style = Some(FontStyle::Italic);
    }
    if style.code {
        h.color = Some(rgb(theme::CODE).into());
        h.background_color = Some(rgba(0xffffff14).into());
    }
    if style.link {
        h.color = Some(rgb(theme::ACCENT).into());
    }
    h
}

fn render_block(block: Block) -> AnyElement {
    match block {
        Block::Heading(level, inline) => div()
            .pt_1()
            .text_size(px(match level {
                1 => 18.,
                2 => 16.,
                _ => 15.,
            }))
            .font_weight(FontWeight::BOLD)
            .child(styled(inline))
            .into_any_element(),
        Block::Paragraph(inline) => div().line_height(px(21.)).child(styled(inline)).into_any_element(),
        Block::Item { depth, marker, content } => div()
            .flex()
            .gap_2()
            .pl(px(depth as f32 * 18.))
            .child(div().flex_none().text_color(rgb(theme::MUTED)).child(marker))
            .child(div().flex_1().min_w(px(0.)).line_height(px(21.)).child(styled(content)))
            .into_any_element(),
        Block::Quote(inline) => div()
            .pl_3()
            .border_l_2()
            .border_color(rgb(theme::BORDER))
            .text_color(rgb(theme::MUTED))
            .child(styled(inline))
            .into_any_element(),
        Block::Code(text) => div()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(theme::PANEL))
            .border_1()
            .border_color(rgb(theme::BORDER))
            .font_family("Consolas")
            .text_size(px(13.))
            .children(text.lines().map(|l| div().child(if l.is_empty() { " ".to_string() } else { l.replace('\t', "    ") })))
            .into_any_element(),
        Block::Rule => div().h(px(1.)).bg(rgb(theme::BORDER)).into_any_element(),
    }
}

// ---- Small helpers ------------------------------------------------------------

fn muted(text: &str) -> AnyElement {
    div().text_size(px(13.)).text_color(rgb(theme::MUTED)).child(text.to_string()).into_any_element()
}

fn card(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(rgb(theme::PANEL))
        .border_1()
        .border_color(rgb(theme::BORDER))
        .child(div().text_size(px(11.)).text_color(rgb(theme::MUTED)).child(title.to_uppercase()))
}

fn section(title: &'static str) -> gpui::Div {
    div().flex().flex_col().gap_2().child(div().font_weight(FontWeight::SEMIBOLD).child(title))
}

fn tab_button(id: &'static str, label: SharedString, selected: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .text_size(px(13.))
        .cursor_pointer()
        .when(selected, |d| d.bg(rgb(theme::SELECTED)))
        .when(!selected, |d| d.text_color(rgb(theme::MUTED)))
        .hover(|s| s.bg(rgb(theme::HOVER)))
        .child(label)
}

pub(crate) fn score_color(score: u8) -> u32 {
    match score {
        5 | 4 => theme::GOOD,
        3 => theme::WARN,
        _ => theme::BAD,
    }
}

pub(crate) fn confidence_badge(score: u8, large: bool) -> AnyElement {
    let color = score_color(score);
    div()
        .flex_none()
        .px(px(if large { 12. } else { 6. }))
        .py(px(if large { 6. } else { 1. }))
        .rounded_md()
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .font_weight(FontWeight::BOLD)
        .text_size(px(if large { 22. } else { 12. }))
        .child(format!("{score}/5"))
        .into_any_element()
}

fn status_pill(status: &str) -> AnyElement {
    let (label, color) = match status {
        "done" => ("Done", theme::GOOD),
        "blocked" => ("Blocked", theme::BAD),
        "needs_input" => ("Needs your input", theme::WARN),
        _ => ("In progress", theme::ACCENT),
    };
    div()
        .px_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(color))
        .text_color(rgb(color))
        .text_size(px(11.))
        .child(label)
        .into_any_element()
}
