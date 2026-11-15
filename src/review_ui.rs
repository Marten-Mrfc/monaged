//! Phase 2: the Changes tab (diff), Haiku summaries, the confidence score,
//! Markdown rendering and the Settings page.

use std::ops::Range;

use gpui::{AnyElement, Context, FontStyle, FontWeight, HighlightStyle, StyledText, div, prelude::*, px, rgb};

use crate::agent::{AgentTab, AgentView, Tone};
use crate::icons::{Icon, icon, icon_sized};
use crate::diff;
use crate::llm::Backend;
use crate::markdown::{self, Block, Inline, Style};
use crate::review::{self, TurnInfo};
use crate::{Form, Monaged, dots, secrets, theme, tone_color, ui};

const SETTINGS_SECTIONS: [(&str, Icon); 15] = [
    ("Appearance", Icon::Eye),
    ("Agents", Icon::Agent),
    ("Message box", Icon::Send),
    ("Keyboard", Icon::Keyboard),
    ("Usage and budget", Icon::Gauge),
    ("Models", Icon::Layers),
    ("API key", Icon::Key),
    ("Reviews", Icon::Shield),
    ("Git", Icon::Branch),
    ("Git accounts", Icon::User),
    ("Notifications", Icon::Alert),
    ("Sounds", Icon::Sound),
    ("General", Icon::Sliders),
    ("Connect Claude", Icon::Ask),
    ("Updates", Icon::ArrowDown),
];

impl Monaged {
    /// Settings, scrolled to one section.
    pub(crate) fn open_settings_at(&mut self, name: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.open_settings(cx);
        if let Some(i) = SETTINGS_SECTIONS.iter().position(|(n, _)| *n == name) {
            let scroll = self.settings_scroll.clone();
            cx.on_next_frame(window, move |_, window, cx| {
                scroll.scroll_to_top_of_item(i + 1);
                cx.on_next_frame(window, |_, _, cx| cx.notify());
                cx.notify();
            });
        }
    }

    /// Settings at a section, without a window at hand (the scroll applies at the next layout).
    pub(crate) fn open_settings_page(&mut self, name: &str, cx: &mut Context<Self>) {
        self.open_settings(cx);
        if let Some(i) = SETTINGS_SECTIONS.iter().position(|(n, _)| *n == name) {
            self.settings_scroll.scroll_to_top_of_item(i + 1);
        }
        cx.notify();
    }

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
        self.take_checkpoint(agent_id, cx);
        self.flush_queued_prompt(agent_id);
        self.notify_finished(agent_id);
        // A free agent you weren't watching is done, and doesn't need you (that
        // sound comes from the inbox). Pipeline agents' milestones have their own.
        if self.agent(agent_id).is_some_and(|a| a.role() == crate::pipeline::Role::Free)
            && (self.sound.settings.while_watching || !self.watching(agent_id))
            && !self.inbox().iter().any(|i| i.agent_id == Some(agent_id))
        {
            self.chime(crate::sound::Cue::Done);
        }
        // A free agent you aren't looking at finished: say so, with a way there.
        if let Some(agent) = self.agent(agent_id)
            && agent.role() == crate::pipeline::Role::Free
            && !(self.form.is_none() && self.sel_sub == Some(agent.record.subproject_id) && (self.sel_agent == Some(agent_id) || self.pinned.contains(&agent_id)))
        {
            let (name, sub_id) = (agent.record.name.clone(), agent.record.subproject_id);
            self.toast_action(
                Tone::Info,
                format!("{name} finished its turn"),
                "Open",
                crate::feedback::ToastAction::Open { sub_id, agent_id: Some(agent_id) },
                cx,
            );
        }
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
        // The lead only talks; there's nothing of its own to summarise or review.
        let is_lead = self.agent(agent_id).is_some_and(|a| a.role() == crate::pipeline::Role::Lead);
        let auto = !answered && !is_lead && self.auto_review && self.backend().is_some() && !(on_max && self.budget_block().is_some());
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
                            agent.changes_secrets = changes.secrets;
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
        // The test command from Project settings → Run, when there is one.
        let test_command = self
            .agent(agent_id)
            .and_then(|a| self.subprojects.iter().find(|s| s.id == a.record.subproject_id))
            .and_then(|s| self.store.setting(&format!("run:{}", s.project_id)))
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["test"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned));
        let project_id = self.project_of_agent(agent_id);
        let shared_notes = project_id.and_then(|p| self.shared_notes_path(p));
        let settled = project_id.map(|p| self.settled_findings(p)).unwrap_or_default();
        let Some(agent) = self.agent_mut(agent_id) else { return };
        if agent.reviewing {
            return;
        }
        let mut context = review::ReviewContext {
            task: agent.task(),
            follow_ups: agent.follow_ups(),
            report: agent.last_reply(),
            settled,
            ..Default::default()
        };
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
                    let checks = review::run_checks(&distro, &worktree, test_command.as_deref(), Some(&base));
                    // The notes and the changed files in full, so the reviewer sees what the diff leans on.
                    let changed: Vec<String> = changes.files.iter().filter(|f| f.status != "deleted").map(|f| f.path.clone()).collect();
                    let (knowledge, files, left_out) = review::read_context(&distro, &worktree, shared_notes.as_deref(), &changed);
                    context.knowledge = knowledge;
                    context.files = files;
                    context.files_left_out = left_out;
                    let reviewed = review::review(&backend, &context, &changes.raw, &checks)?;
                    Ok((changes, Some(reviewed)))
                })
                .await;
            view.update(cx, |this, cx| {
                // The quality gate asked for a review of newer work: run that one instead.
                if let Some(agent) = this.agent_mut(agent_id)
                    && agent.review_again
                {
                    agent.review_again = false;
                    agent.reviewing = false;
                    this.review_agent(agent_id, cx);
                    return;
                }
                let outcome: (Option<u8>, String) = match &result {
                    Ok((_, Some((c, _)))) => (Some(c.score), String::new()),
                    Ok((_, None)) => (None, "says it's done but changed nothing in its worktree. Look at what it did, or accept it as it is.".into()),
                    Err(err) => (None, format!("couldn't be reviewed ({err:#}). Review it again, or accept it as it is.")),
                };
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
                            agent.changes_secrets = changes.secrets;
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
                let name = this.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
                this.lead_review_done(agent_id);
                this.gate_on_review(agent_id, outcome.0, format!("{name} {}", outcome.1), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Findings you ruled out in this project (the reviewer is told about them).
    pub(crate) fn settled_findings(&self, project_id: i64) -> Vec<review::Settled> {
        self.store.setting(&format!("review:settled:{project_id}")).map(|t| review::settled_from_json(&t)).unwrap_or_default()
    }

    /// Sends the review's findings (and failing tests) to the agent to fix.
    pub(crate) fn fix_findings(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(prompt) = self.agent(agent_id).and_then(|a| a.confidence.as_ref()).and_then(review::fix_prompt) else {
            self.toast(Tone::Info, "The review found nothing to fix.", cx);
            return;
        };
        self.send_prompt(agent_id, prompt);
        self.set_tab(agent_id, AgentTab::Conversation, cx);
        self.toast(Tone::Info, "Sent the findings to the agent. It's reviewed again when it's done.", cx);
        cx.notify();
    }

    /// Asks why a finding isn't a problem (optional), then rules it out.
    pub(crate) fn ask_rule_out(&mut self, agent_id: i64, index: usize, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(issue) = self.agent(agent_id).and_then(|a| a.confidence.as_ref()).and_then(|c| c.issues.get(index)).cloned() else { return };
        self.ask_text(
            "Not a problem?".into(),
            "Why not (optional): the next reviews are told".into(),
            "Rule it out".into(),
            String::new(),
            crate::feedback::TextAction::RuleOut { agent_id, file: issue.file, problem: issue.problem },
            window,
            cx,
        );
    }

    /// Drops a finding from the score and remembers it for the project's next reviews.
    pub(crate) fn rule_out_finding(&mut self, agent_id: i64, file: &str, problem: &str, reason: &str, cx: &mut Context<Self>) {
        let project_id = self.project_of_agent(agent_id);
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let Some(confidence) = agent.confidence.as_mut() else { return };
        let Some(index) = confidence.issues.iter().position(|i| i.file == file && i.problem == problem) else { return };
        review::rule_out(confidence, index);
        let confidence = confidence.clone();
        let _ = self.store.save_confidence(agent_id, &confidence);
        if let Some(project_id) = project_id {
            let mut settled = self.settled_findings(project_id);
            settled.retain(|s| !(s.file == file && s.problem == problem));
            settled.push(review::Settled { file: file.to_string(), problem: problem.to_string(), reason: reason.trim().to_string() });
            // The latest ones matter most; keep the list short.
            let excess = settled.len().saturating_sub(40);
            settled.drain(..excess);
            let _ = self.store.set_setting(&format!("review:settled:{project_id}"), &review::settled_to_json(&settled).to_string());
        }
        // A better score may be what an auto-merge was waiting for.
        if let Some(sub_id) = self.agent(agent_id).map(|a| a.record.subproject_id) {
            self.maybe_auto_merge(sub_id, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_tab(&mut self, agent_id: i64, tab: AgentTab, cx: &mut Context<Self>) {
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
                    self.toast(Tone::Info, "Saved the API key in Windows Credential Manager.", cx);
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

    pub(crate) fn remove_api_key(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn render_settings(&self, error: Option<&String>, main_w: f32, window_h: f32, cx: &mut Context<Self>) -> AnyElement {
        let key_line = match &self.api_key {
            Some(key) => format!("Saved: {}", secrets::mask(key)),
            None => "Not set".to_string(),
        };
        let active = match self.backend() {
            Some(Backend::Api { .. }) => "Summaries and reviews use your API key (Claude Haiku 5.5).",
            Some(Backend::Cli { .. }) => "No API key, so summaries and reviews run through the Claude CLI and count against your Max plan.",
            None => "Summaries and reviews are off: add an API key or allow the Max plan fallback.",
        };
        let (api_spend, max_spend) = self.spend;

        // Appearance: a card per theme with a miniature of the app in its colours.
        let theme_card = |key: &'static str, name: &'static str, blurb: &'static str, palette: &'static theme::Palette, cx: &mut Context<Self>| {
            let selected = self.theme_key == key;
            let mini = div()
                .h(px(86.))
                .rounded(px(5.))
                .overflow_hidden()
                .border_1()
                .border_color(rgb(palette.line_strong))
                .bg(rgb(palette.bg))
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(12.))
                        .bg(rgb(palette.panel))
                        .border_b_1()
                        .border_color(rgb(palette.line))
                        .flex()
                        .items_center()
                        .px(px(5.))
                        .child(div().size(px(4.)).rounded_full().bg(rgb(palette.red))),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .child(
                            div()
                                .w(px(42.))
                                .bg(rgb(palette.panel))
                                .border_r_1()
                                .border_color(rgb(palette.line))
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .p(px(5.))
                                .child(div().h(px(3.)).w(px(28.)).rounded_full().bg(rgb(palette.text_2)))
                                .child(div().h(px(3.)).w(px(22.)).rounded_full().bg(rgb(palette.text_3)))
                                .child(div().h(px(3.)).w(px(26.)).rounded_full().bg(rgb(palette.text_3))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(5.))
                                .p(px(7.))
                                .child(div().h(px(4.)).w(px(60.)).rounded_full().bg(rgb(palette.text)))
                                .child(div().h(px(3.)).w(px(90.)).rounded_full().bg(rgb(palette.text_3)))
                                .child(div().h(px(3.)).w(px(76.)).rounded_full().bg(rgb(palette.text_3)))
                                .child(div().mt(px(3.)).h(px(14.)).w(px(54.)).rounded(px(3.)).bg(rgb(palette.raised)).border_1().border_color(rgb(palette.line))),
                        ),
                );
            div()
                .id(key)
                .w(px(162.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(8.))
                .p(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(if selected { theme::text_2() } else { theme::line() }))
                .when(selected, |d| d.bg(rgb(theme::selected())))
                .when(!selected, |d| d.hover(|s| s.bg(rgb(theme::hover()))))
                .cursor_pointer()
                .child(mini)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(name))
                        .when(selected, |d| d.child(icon_sized(Icon::Check, 13.).text_color(rgb(theme::text())))),
                )
                .child(div().text_size(px(11.5)).text_color(rgb(theme::text_3())).child(blurb))
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(key, window, cx)))
        };
        let mut themes = div().flex().flex_wrap().gap(px(12.));
        for palette in theme::THEMES {
            themes = themes.child(theme_card(palette.key, palette.name, palette.blurb, palette, cx));
        }
        let system_palette = if theme::current().dark { &theme::CARBON } else { &theme::PAPER };
        themes = themes.child(theme_card("system", "Follow Windows", "Carbon in dark mode, Paper in light", system_palette, cx));

        let section = |title: &'static str, blurb: Option<&'static str>| {
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .py(px(26.))
                .border_t_1()
                .border_color(rgb(theme::line()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().font_family(theme::DISPLAY).text_size(px(15.)).font_weight(FontWeight::MEDIUM).child(title))
                        .children(blurb.map(|b| div().text_size(px(13.)).text_color(rgb(theme::text_3())).child(b))),
                )
        };

        // Each section is a direct child of the scrolling column, so the nav can
        // scroll to it and tell which one you're reading.
        let scroll = self.settings_scroll.clone();
        let reading = scroll.top_item().max(1);
        let nav = (main_w >= 1240.).then(|| {
            div()
                .w(px(210.))
                .flex_none()
                .pt(px(40.))
                .px(px(14.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(div().px(px(10.)).pb(px(8.)).child(ui::label("Settings")))
                .children(SETTINGS_SECTIONS.iter().enumerate().map(|(i, (name, glyph))| {
                    let index = i + 1;
                    let scroll = scroll.clone();
                    ui::row(("settings-nav", i), reading == index)
                        .child(icon(*glyph).size(px(14.)))
                        .child(*name)
                        .on_click(cx.listener(move |_, _, window, cx| {
                            scroll.scroll_to_top_of_item(index);
                            // The scroll lands while painting; draw the nav again once it has.
                            cx.on_next_frame(window, |_, window, cx| cx.on_next_frame(window, |_, _, cx| cx.notify()));
                            cx.notify();
                        }))
                }))
                .child(div().h(px(12.)))
                .child(
                    ui::row("settings-nav-keys", false)
                        .child(icon(Icon::Keyboard).size(px(14.)))
                        .child(div().flex_1().child("Shortcut sheet"))
                        .children(crate::keys::hint("shortcuts").map(ui::kbd))
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_shortcuts(window, cx))),
                )
        });
        let column = |el: gpui::Div| div().w_full().max_w(px(900.)).child(el);
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .children(nav)
            .child(
                div()
                    .id("settings")
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(32.))
                    .pt(px(36.))
                    // Room under the last section so every section can reach the top.
                    .pb(px((window_h - 260.).max(240.)))
                        .child(column(div().pb(px(22.)).child(ui::title("Settings"))))
                        .child(column(section("Appearance", Some("Every theme keeps one rule: red only means something needs you.")).child(themes)))
                        .child(column(
                            section("Agents", Some("The AI agent Monaged runs your sessions with, and others you can switch to.")).child(self.render_agent_settings(cx)),
                        ))
                        .child(column(section("Message box", None).child(self.render_message_box_settings(cx))))
                        .child(column(
                            section("Keyboard", Some("Change any shortcut: click Change, then press the keys you want.")).child(self.render_keyboard_settings(cx)),
                        ))
                        .child(column(section("Usage and budget", None).child(self.render_usage_settings(cx))))
                        .child(column(section("Models", None).child(self.render_model_settings(cx))))
                        .child(column(
                            section(
                                "Anthropic API key",
                                Some("Pays for the cheap Haiku work (summaries, reviews, the librarian) from your API credit instead of your Max plan. Stored in Windows Credential Manager, never in Monaged's database."),
                            )
                            .child(div().flex().items_center().gap(px(10.)).child(icon(Icon::Key)).child(div().font_family(theme::MONO).text_size(px(12.5)).child(key_line)))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(ui::button("paste-key", ui::Kind::Primary, None, "Paste key from clipboard").on_click(cx.listener(|this, _, _, cx| this.paste_api_key(cx))))
                                    .when(self.api_key.is_some(), |d| {
                                        d.child(ui::button("remove-key", ui::Kind::Danger, None, "Remove key").on_click(cx.listener(|this, _, window, cx| this.ask_confirm(crate::feedback::ConfirmAction::RemoveApiKey, window, cx))))
                                    }),
                            )
                            .children(error.map(|e| ui::callout(Tone::Error, e.clone()))),
                        ))
                        .child(column(
                            section("Summaries and reviews", Some(active))
                                .child(ui::switch("toggle-auto", "Summarize and review after every agent turn", self.auto_review).on_click(cx.listener(|this, _, _, cx| this.toggle_setting("auto_review", cx))))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(6.))
                                        .child(div().text_size(px(13.)).text_color(rgb(theme::text_body())).child("Pipeline work goes on by itself from"))
                                        .child(div().flex().gap(px(6.)).children([3u8, 4, 5].map(|n| {
                                            ui::choice(("min-score", n as usize), format!("{n}/5"), self.min_score() == n).on_click(cx.listener(move |this, _, _, cx| {
                                                let _ = this.store.set_setting("review:min_score", &n.to_string());
                                                cx.notify();
                                            }))
                                        })))
                                        .child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(
                                            "Builders below it get their findings back to fix (twice), then wait for you; the integrator too. Merging into develop by itself needs it as well.",
                                        )),
                                )
                                .child(
                                    ui::switch("toggle-cli", "When there's no API key, use the Max plan (Claude CLI) instead", self.cli_fallback)
                                        .on_click(cx.listener(|this, _, _, cx| this.toggle_setting("cli_fallback", cx))),
                                )
                                .child(ui::mono(format!("Haiku so far: ${api_spend:.4} from API credit · ${max_spend:.4} worth of Max usage"))),
                        ))
                        .child(column(
                            section("Git", Some("Builders commit their work and Monaged pushes branches, so git inside WSL needs your identity."))
                                .child(self.render_git_settings(cx))
                                .child(
                                    div().flex().child(
                                        ui::button("settings-copy-identity", ui::Kind::Secondary, Some(Icon::User), "Copy my Windows git identity into WSL")
                                            .on_click(cx.listener(|this, _, _, cx| this.copy_git_identity(cx))),
                                    ),
                                ),
                        ))
                        .child(column(
                            section(
                                "Git accounts",
                                Some("Sign in once and git in WSL can clone, pull and push (Monaged's and yours in a terminal), and Monaged opens pull and merge requests without gh or glab. Tokens stay in Windows Credential Manager; Git Credential Manager is optional."),
                            )
                            .child(self.render_git_accounts(cx)),
                        ))
                        .child(column(section("Notifications", None).child(self.render_notification_settings(cx))))
                        .child(column(section("Sounds", Some("A few short sounds for moments you might miss because you're looking elsewhere.")).child(self.render_sound_settings(cx))))
                        .child(column(section("General", Some("Density, where Monaged opens, and where it keeps its data.")).child(self.render_general_settings(cx))))
                        .child(column(
                            section(
                                "Connect Claude",
                                Some("Let Claude see what happens in Monaged: export a project to read or paste into a chat, or connect Claude Desktop or Claude Code so it can look for itself (an MCP server). Secrets are masked either way."),
                            )
                            .child(self.render_connect_settings(cx)),
                        ))
                        .child(column(section("Updates", None).child(self.render_update_settings(cx)))),
            )
            .into_any_element()
    }

    // ---- Changes ----------------------------------------------------------------

    pub(crate) fn render_changes(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let agent_id = agent.record.id;

        let summary_body: AnyElement = match (&agent.summary, agent.summarizing) {
            (_, true) if agent.summary.is_none() => loading(("summary-spin", agent_id as usize), "Haiku is writing a summary…"),
            (Some(summary), _) => div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(status_label(&summary.status))
                .child(div().text_size(px(13.5)).line_height(px(21.)).text_color(rgb(theme::text_body())).child(summary.text.clone()))
                .when(!summary.next_step.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(12.5))
                            .text_color(rgb(theme::text_3()))
                            .child(div().flex_none().child("Next"))
                            .child(div().flex_1().min_w(px(0.)).text_color(rgb(theme::text_2())).child(summary.next_step.clone())),
                    )
                })
                .into_any_element(),
            (None, _) => muted("No summary yet. One is written after each turn."),
        };

        let can_fix = !agent.reviewing && agent.confidence.as_ref().is_some_and(|c| review::fix_prompt(c).is_some());
        let confidence_body: AnyElement = match &agent.confidence {
            None if agent.reviewing => loading(("review-spin-card", agent_id as usize), "Running the tests and a skeptical review…"),
            None => muted("Not reviewed yet. Review runs the project's tests and asks Haiku to find what's broken."),
            Some(c) => div()
                .flex()
                .items_start()
                .gap(px(18.))
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(dots::dot_text(&format!("{}/5", c.score), 4., rgb(if c.score <= 2 { theme::red() } else { theme::text() })))
                        .child(ui::score_meter(c.score)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .text_size(px(13.))
                        .children(c.reasons.iter().map(|r| div().text_color(rgb(theme::text_2())).child(r.clone())))
                        .children(c.issues.iter().enumerate().map(|(index, issue)| {
                            let attention = matches!(issue.severity.as_str(), "high" | "medium");
                            div()
                                .flex()
                                .items_start()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .w(px(52.))
                                        .flex_none()
                                        .font_family(theme::MONO)
                                        .text_size(px(11.5))
                                        .text_color(rgb(if attention { theme::red_text() } else { theme::text_3() }))
                                        .child(issue.severity.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .child(div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(issue.file.clone()))
                                        .child(div().text_color(rgb(theme::text_body())).child(issue.problem.clone())),
                                )
                                .child(
                                    div().flex_none().child(
                                        ui::icon_button(("rule-out", index), Icon::Check)
                                            .tooltip(crate::shell::tip("Not a problem: rule it out (the next reviews are told)"))
                                            .on_click(cx.listener(move |this, _, window, cx| this.ask_rule_out(agent_id, index, window, cx))),
                                    ),
                                )
                        })),
                )
                .into_any_element(),
        };

        let files: AnyElement = match &agent.changes {
            None if agent.loading_changes => div()
                .flex()
                .gap(px(16.))
                .child(
                    div()
                        .w(px(240.))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .children((0..5).map(|i| ui::skeleton(("changes-sk-file", i), gpui::relative([0.9, 0.7, 0.8, 0.6, 0.75][i]).into(), 10.))),
                )
                .child(div().flex_1().child(ui::skeleton_text(&format!("changes-sk-{agent_id}"), 5)))
                .into_any_element(),
            None => muted("Changes not loaded yet."),
            Some(files) if files.is_empty() => muted("No changes in this worktree yet."),
            Some(files) => {
                let selected = agent.selected_file.min(files.len() - 1);
                let file = &files[selected];
                let file_path = format!("{}/{}", agent.record.worktree, file.path);
                let list = div().w(px(240.)).flex_none().flex().flex_col().gap(px(1.)).children(files.iter().enumerate().map(|(i, f)| {
                    let mark = match f.status {
                        "added" => "A",
                        "deleted" => "D",
                        "renamed" => "R",
                        _ => "M",
                    };
                    let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                    ui::row(("file", i), i == selected)
                        .h(px(26.))
                        .child(div().w(px(12.)).flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(mark))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.5)).child(name))
                        .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::add())).child(format!("+{}", f.added)))
                        .child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::del())).child(format!("−{}", f.removed)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(agent) = this.agent_mut(agent_id) {
                                agent.selected_file = i;
                            }
                            cx.notify();
                        }))
                }));
                let diff_view = div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(icon(Icon::Diff).size(px(14.)).text_color(rgb(theme::text_3())))
                            .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(12.5)).child(file.path.clone()))
                            .child(
                                ui::icon_button("file-zed", Icon::External)
                                    .tooltip(crate::shell::tip("Open this file in Zed"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(file_path.clone(), cx))),
                            ),
                    )
                    .children(self.render_comments_bar(agent_id, cx))
                    .child(self.render_diff_body(Some(agent_id), file, cx))
                    .when(agent.changes_truncated, |d| d.child(muted("This diff is very large; only the first part was loaded.")));
                let secrets = (!agent.changes_secrets.is_empty()).then(|| {
                    ui::callout(
                        Tone::Error,
                        format!(
                            "These changes contain the value of {}. Don't commit, merge or push them: ask the agent to read it from the environment instead (it's masked here).",
                            agent.changes_secrets.join(", ")
                        ),
                    )
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(secrets)
                    .child(div().flex().items_start().gap(px(16.)).child(list).child(diff_view))
                    .into_any_element()
            }
        };

        div()
            .id(("changes", agent_id as usize))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .p(px(16.))
                    .children(agent.review_error.as_ref().map(|e| {
                        div().flex().items_center().gap(px(8.)).text_size(px(13.)).text_color(rgb(tone_color(Tone::Error))).child(icon(Icon::Alert).text_color(rgb(theme::red_text()))).child(e.clone())
                    }))
                    .child(
                        div()
                            .flex()
                            .gap(px(14.))
                            .flex_wrap()
                            .child(
                                ui::card()
                                    .flex_1()
                                    .min_w(px(380.))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(ui::label("Summary"))
                                            .child(
                                                ui::icon_button("summarize", Icon::Refresh)
                                                    .tooltip(crate::shell::tip("Write the summary again"))
                                                    .on_click(cx.listener(move |this, _, _, cx| this.summarize(agent_id, cx))),
                                            ),
                                    )
                                    .child(summary_body),
                            )
                            .child(
                                ui::card()
                                    .flex_1()
                                    .min_w(px(380.))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .child(ui::label("Confidence"))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap(px(6.))
                                                    .when(can_fix, |d| {
                                                        d.child(
                                                            ui::button("review-fix", ui::Kind::Secondary, Some(Icon::Edit), "Fix findings")
                                                                .tooltip(crate::shell::tip("Send the findings and failing tests to the agent to fix"))
                                                                .on_click(cx.listener(move |this, _, _, cx| this.fix_findings(agent_id, cx))),
                                                        )
                                                    })
                                                    .child(
                                                        ui::icon_button("review-again", Icon::Eye)
                                                            .tooltip(crate::shell::tip("Review: tests and a skeptical read"))
                                                            .on_click(cx.listener(move |this, _, _, cx| this.review_agent(agent_id, cx))),
                                                    ),
                                            ),
                                    )
                                    .child(confidence_body),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pt(px(6.))
                            .child(ui::label("Files"))
                            .child(div().flex_1().min_w(px(0.)).truncate().child(ui::mono(format!("{} vs. where it started", agent.record.branch))))
                            .child(
                                ui::icon_button("refresh-changes", Icon::Refresh)
                                    .tooltip(crate::shell::tip("Read the diff again"))
                                    .on_click(cx.listener(move |this, _, _, cx| this.refresh_changes(agent_id, false, cx))),
                            ),
                    )
                    .child(files),
            )
            .into_any_element()
    }
}

// ---- Markdown ----------------------------------------------------------------

pub(crate) fn render_markdown(text: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .text_size(px(14.))
        .text_color(rgb(theme::text_body()))
        .children(markdown::parse(text).into_iter().map(render_block))
        .into_any_element()
}

fn styled(inline: Inline) -> StyledText {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = inline.styles.iter().map(|(range, style)| (range.clone(), highlight(*style))).collect();
    StyledText::new(inline.text).with_highlights(highlights)
}

fn highlight(style: Style) -> HighlightStyle {
    let mut h = HighlightStyle::default();
    if style.bold {
        h.font_weight = Some(FontWeight::SEMIBOLD);
        h.color = Some(rgb(theme::text()).into());
    }
    if style.italic {
        h.font_style = Some(FontStyle::Italic);
    }
    if style.code {
        h.color = Some(rgb(theme::text()).into());
        h.background_color = Some(rgb(theme::code_bg()).into());
    }
    if style.link {
        h.color = Some(rgb(theme::text()).into());
        h.underline = Some(gpui::UnderlineStyle { thickness: px(1.), color: Some(rgb(theme::text_3()).into()), wavy: false });
    }
    h
}

fn render_block(block: Block) -> AnyElement {
    match block {
        Block::Heading(level, inline) => div()
            .pt(px(6.))
            .font_family(theme::DISPLAY)
            .text_size(px(match level {
                1 => 18.,
                2 => 16.,
                _ => 14.5,
            }))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(theme::text()))
            .child(styled(inline))
            .into_any_element(),
        Block::Paragraph(inline) => div().line_height(px(22.)).child(styled(inline)).into_any_element(),
        Block::Item { depth, marker, content } => div()
            .flex()
            .gap(px(10.))
            .pl(px(depth as f32 * 18.))
            .child(div().flex_none().min_w(px(12.)).line_height(px(22.)).text_color(rgb(theme::text_3())).child(marker))
            .child(div().flex_1().min_w(px(0.)).line_height(px(22.)).child(styled(content)))
            .into_any_element(),
        Block::Quote(inline) => div()
            .pl(px(12.))
            .border_l_2()
            .border_color(rgb(theme::line_strong()))
            .text_color(rgb(theme::text_2()))
            .line_height(px(22.))
            .child(styled(inline))
            .into_any_element(),
        Block::Code(text, lang) => {
            let lang = crate::syntax::lang_from_name(&lang);
            div()
            .px(px(12.))
            .py(px(10.))
            .rounded(px(6.))
            .bg(rgb(theme::code_bg()))
            .border_1()
            .border_color(rgb(theme::line()))
            .font_family(theme::MONO)
            .text_size(px(12.5))
            .line_height(px(19.))
            .text_color(rgb(theme::text()))
            .overflow_hidden()
            .children(text.lines().map(|l| div().child(crate::syntax::line(l, lang))))
            .into_any_element()
        }
        Block::Rule => div().h(px(1.)).bg(rgb(theme::line())).into_any_element(),
    }
}

// ---- Small helpers ------------------------------------------------------------

/// A spinner with what's happening, for work that takes seconds.
fn loading(id: impl Into<gpui::ElementId>, text: &str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(px(13.))
        .text_color(rgb(theme::text_3()))
        .child(ui::spinner(id, 13., theme::text_3()))
        .child(text.to_string())
        .into_any_element()
}

fn muted(text: &str) -> AnyElement {
    div().text_size(px(13.)).text_color(rgb(theme::text_3())).child(text.to_string()).into_any_element()
}

/// The summary's status as a quiet mono label; "needs you" and "blocked" in red.
fn status_label(status: &str) -> AnyElement {
    let (text, attention) = match status {
        "done" => ("Done", false),
        "blocked" => ("Blocked", true),
        "needs_input" => ("Needs you", true),
        _ => ("In progress", false),
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(rgb(if attention { theme::red_text() } else { theme::text_3() }))
        .child(div().size(px(6.)).rounded_full().bg(rgb(if attention { theme::red() } else { theme::text_3() })))
        .child(text.to_uppercase())
        .into_any_element()
}

impl Monaged {
    /// Settings → Connect Claude.
    pub(crate) fn render_connect_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.debug_on();
        let actions = self.debug_actions();
        let project = self.sel_project.and_then(|id| self.projects.iter().find(|p| p.id == id)).map(|p| (p.id, p.name.clone()));
        let hint = |text: &'static str| div().text_size(px(12.5)).text_color(rgb(theme::text_3())).child(text);
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(ui::label("Export for analysis"))
            .child(hint("A Markdown report (to read, or to paste or upload into any chat with Claude) and a zip with every conversation, the pipeline data, git's view, Monaged's log and a screenshot. It goes to your Downloads folder."))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(project.map(|(id, name)| {
                        ui::button("export-project", ui::Kind::Primary, Some(Icon::ArrowDown), format!("Export {name}"))
                            .on_click(cx.listener(move |this, _, _, cx| this.export(crate::export::Scope::Project(id), cx)))
                    }))
                    .child(
                        ui::button("export-all", ui::Kind::Secondary, Some(Icon::ArrowDown), "Export everything")
                            .on_click(cx.listener(|this, _, _, cx| this.export(crate::export::Scope::Everything, cx))),
                    ),
            )
            .child(div().h(px(4.)))
            .child(ui::label("Let Claude look (MCP)"))
            .child(
                ui::switch("toggle-debug", "Let Claude Desktop or Claude Code read Monaged: projects, agents, conversations, reviews, the log and screenshots", on)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_debug(!on, cx))),
            )
            .when(on, |d| {
                d.child(
                    ui::switch("toggle-debug-actions", "Let Claude act too: send agents messages, stop them, run reviews", actions)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_debug_actions(!actions, cx))),
                )
            })
            .child(hint("Agents run as you, so they could read this too; leave actions off unless you're debugging with Claude. Monaged writes the connection details to its data folder only while this is on."))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        ui::button("connect-desktop", ui::Kind::Secondary, Some(Icon::Ask), "Add to Claude Desktop")
                            .tooltip(crate::shell::tip("Adds Monaged to Claude Desktop's MCP servers (its config keeps everything else); restart Claude Desktop after"))
                            .on_click(cx.listener(|this, _, _, cx| this.connect_claude_desktop(cx))),
                    )
                    .child(
                        ui::button("connect-code-wsl", ui::Kind::Secondary, Some(Icon::Terminal), "Add to Claude Code in WSL")
                            .tooltip(crate::shell::tip("Runs `claude mcp add` in WSL, so `claude` in a WSL terminal can look at Monaged"))
                            .on_click(cx.listener(|this, _, _, cx| this.connect_claude_code_wsl(cx))),
                    )
                    .child(
                        ui::button("connect-code-copy", ui::Kind::Secondary, Some(Icon::Copy), "Copy the command for Claude Code on Windows")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(crate::debug_mcp::claude_code_windows_command()));
                                this.toast(Tone::Info, "Copied: paste it into a terminal where `claude` runs.", cx);
                            })),
                    ),
            )
            .child(hint("Then ask Claude things like \"what is Monaged doing?\", \"why is build-api stuck?\" or \"show me the Order history dashboard\". It has: status, projects, subproject, agent, changes, inbox, log, screenshot, show and export."))
            .into_any_element()
    }
}
