//! The agent session UI: what the agent protocol (ACP) offers, shown so you
//! can use it.
//! - the message box: multi-line, earlier messages on Up, attachments
//!   (paste, drop or pick), slash commands with suggestions;
//! - settings chips: mode, model, effort, fast mode… exactly the options the
//!   agent reports, so another ACP agent's settings show up the same way;
//! - the agent's question forms (Claude's question cards, MCP input requests
//!   and sign-in links), answered in place;
//! - tool calls that open up to show their input, files, output and diffs.

use gpui::{AnyElement, ClickEvent, Context, Entity, ExternalPaths, FontWeight, PathPromptOptions, SharedString, Window, div, prelude::*, px, relative, rgb};
use serde_json::{Value, json};

use crate::agent::{AgentTab, AgentView, ConfigOption, Elicitation, FieldKind, FormState, Item, Tone, ToolContent, ToolDetail};
use crate::icons::{Icon, icon, icon_sized};
use crate::menu::MenuTarget;
use crate::shell::{tip, tip_key};
use crate::text_input::{InputEvent, TextInput};
use crate::ui::{self, Kind, State};
use crate::{Monaged, Pending, attach, theme};

/// Monaged's own slash commands: handled here, never sent to the agent.
pub(crate) const BUILTINS: [(&str, &str, Option<&str>); 8] = [
    ("review", "Monaged: run the tests and a skeptical review", None),
    ("stop", "Monaged: stop the turn", None),
    ("model", "Monaged: switch this agent's model", Some("<name>")),
    ("mode", "Monaged: switch the permission mode", Some("<manual | accept edits | plan | auto | bypass>")),
    ("effort", "Monaged: set the thinking effort", Some("<low | medium | high | xhigh | max>")),
    ("changes", "Monaged: show this agent's changes", None),
    ("restart", "Monaged: restart the session (its history reloads)", None),
    ("rename", "Monaged: rename this agent", Some("<new name>")),
];

pub(crate) struct Suggestion {
    pub name: String,
    pub description: String,
    pub hint: Option<String>,
}

/// The suggestion list under a message box.
#[derive(Default)]
pub(crate) struct SuggestState {
    pub selected: usize,
    /// Esc closed the list for this text.
    pub dismissed: Option<String>,
}

/// Settings categories in the order the chips show them.
fn category_rank(option: &ConfigOption) -> u8 {
    match option.category.as_str() {
        "mode" => 0,
        "model" => 1,
        "thought_level" => 2,
        "model_config" => 3,
        _ => 4,
    }
}

impl Monaged {
    // ---- Message box wiring ----------------------------------------------------------------

    /// Creates an agent's message box (multi-line, with its earlier messages on Up).
    pub(crate) fn make_composer(&mut self, agent_id: i64, cx: &mut Context<Self>) -> Entity<TextInput> {
        let (name, history) = self.agent(agent_id).map(|a| (a.record.name.clone(), a.typed_messages())).unwrap_or_default();
        let ctrl_send = self.store.setting("composer:send").as_deref() == Some("ctrl-enter");
        let input = cx.new(|cx| {
            let mut input = TextInput::new(format!("Message {name}   ·   / for commands"), cx).bare().multiline();
            input.set_history(history);
            input.set_ctrl_send(ctrl_send, cx);
            input
        });
        let subscription = cx.subscribe(&input, move |this: &mut Self, _input, event: &InputEvent, cx| match event {
            InputEvent::PastedImage(image) => this.attach_image(agent_id, image, cx),
            InputEvent::SuggestUp => this.suggest_move(agent_id, -1, cx),
            InputEvent::SuggestDown => this.suggest_move(agent_id, 1, cx),
            InputEvent::SuggestAccept => this.accept_suggestion(agent_id, cx),
            InputEvent::Changed => this.update_suggesting(agent_id, cx),
        });
        self.input_subs.insert(agent_id, subscription);
        input
    }

    // ---- Slash commands ----------------------------------------------------------------------

    /// What matches the "/…" being typed: Monaged's commands, then the agent's.
    pub(crate) fn suggestions(&self, agent_id: i64, cx: &gpui::App) -> Vec<Suggestion> {
        let Some(input) = self.inputs.get(&agent_id) else { return Vec::new() };
        let text = input.read(cx).text();
        let Some(query) = text.strip_prefix('/') else { return Vec::new() };
        if query.contains(char::is_whitespace) || self.suggest.get(&agent_id).and_then(|s| s.dismissed.as_deref()) == Some(text) {
            return Vec::new();
        }
        let query = query.to_lowercase();
        let Some(agent) = self.agent(agent_id) else { return Vec::new() };
        let mut out: Vec<(u8, Suggestion)> = Vec::new();
        let rank = |name: &str| -> Option<u8> {
            let name = name.to_lowercase();
            if name.starts_with(&query) {
                Some(0)
            } else if name.contains(&query) {
                Some(1)
            } else {
                None
            }
        };
        for (name, description, hint) in BUILTINS {
            if let Some(r) = rank(name) {
                out.push((r, Suggestion { name: name.into(), description: description.into(), hint: hint.map(str::to_owned) }));
            }
        }
        for command in &agent.commands {
            if BUILTINS.iter().any(|(n, _, _)| *n == command.name) {
                continue;
            }
            if let Some(r) = rank(&command.name) {
                out.push((r, Suggestion { name: command.name.clone(), description: command.description.clone(), hint: command.hint.clone() }));
            }
        }
        out.sort_by_key(|(r, _)| *r);
        out.into_iter().map(|(_, s)| s).take(8).collect()
    }

    fn update_suggesting(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let open = !self.suggestions(agent_id, cx).is_empty();
        if let Some(state) = self.suggest.get_mut(&agent_id) {
            state.selected = 0;
        }
        if let Some(input) = self.inputs.get(&agent_id).cloned() {
            input.update(cx, |input, _| input.suggesting = open);
        }
        cx.notify();
    }

    fn suggest_move(&mut self, agent_id: i64, delta: isize, cx: &mut Context<Self>) {
        let count = self.suggestions(agent_id, cx).len();
        if count == 0 {
            return;
        }
        let state = self.suggest.entry(agent_id).or_default();
        state.selected = (state.selected as isize + delta).rem_euclid(count as isize) as usize;
        cx.notify();
    }

    /// Puts the chosen command in the box ("/name "), ready for its input.
    pub(crate) fn accept_suggestion(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let list = self.suggestions(agent_id, cx);
        let selected = self.suggest.get(&agent_id).map_or(0, |s| s.selected).min(list.len().saturating_sub(1));
        let Some(choice) = list.get(selected) else { return };
        let text = format!("/{} ", choice.name);
        if let Some(input) = self.inputs.get(&agent_id).cloned() {
            input.update(cx, |input, cx| {
                input.set_text(text, cx);
                input.suggesting = false;
            });
        }
        cx.notify();
    }

    /// Esc on an open list closes it (for this text).
    pub(crate) fn dismiss_suggestions(&mut self, agent_id: i64, cx: &mut Context<Self>) -> bool {
        if self.suggestions(agent_id, cx).is_empty() {
            return false;
        }
        let text = self.inputs.get(&agent_id).map(|i| i.read(cx).text().to_string());
        self.suggest.entry(agent_id).or_default().dismissed = text;
        if let Some(input) = self.inputs.get(&agent_id).cloned() {
            input.update(cx, |input, _| input.suggesting = false);
        }
        cx.notify();
        true
    }

    /// Runs one of Monaged's own commands; false when it's the agent's to handle.
    pub(crate) fn run_builtin(&mut self, agent_id: i64, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let trimmed = text.trim().trim_start_matches('/');
        let (name, arg) = trimmed.split_once(' ').map_or((trimmed, ""), |(n, a)| (n, a.trim()));
        match name {
            "review" => self.review_agent(agent_id, cx),
            "stop" => self.stop_agent(agent_id, cx),
            "changes" => self.set_tab(agent_id, AgentTab::Changes, cx),
            "restart" => self.retry_session(agent_id, cx),
            "rename" if !arg.is_empty() => self.rename_agent(agent_id, arg, cx),
            "rename" => self.toast(Tone::Warn, "Type the new name after /rename.", cx),
            "model" | "mode" | "effort" => {
                let Some(option) = self.agent(agent_id).and_then(|a| a.config_option(name)).cloned() else {
                    self.toast(Tone::Warn, format!("This agent doesn't offer a {name} setting (yet)."), cx);
                    return true;
                };
                if arg.is_empty() {
                    let names: Vec<String> = option.choices.iter().map(|c| c.name.clone()).collect();
                    self.toast(Tone::Info, format!("{}: {}. Pick one with /{name} <name>.", option.name, names.join(", ")), cx);
                    return true;
                }
                let wanted = arg.to_lowercase();
                let choice = option
                    .choices
                    .iter()
                    .find(|c| c.value.to_lowercase() == wanted || c.name.to_lowercase() == wanted)
                    .or_else(|| option.choices.iter().find(|c| c.name.to_lowercase().contains(&wanted) || c.value.to_lowercase().contains(&wanted)));
                match choice {
                    Some(choice) => {
                        let value = choice.value.clone();
                        self.set_config(agent_id, &option.id, &value, cx);
                    }
                    None => self.toast(Tone::Warn, format!("No {name} called \"{arg}\"."), cx),
                }
            }
            _ => return false,
        }
        let _ = window;
        true
    }

    // ---- Settings (ACP config options) -------------------------------------------------------

    /// Changes one of the session's settings (mode, model, effort…).
    pub(crate) fn set_config(&mut self, agent_id: i64, option_id: &str, value: &str, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let Some(session_id) = agent.record.session_id.clone().filter(|_| agent.live) else {
            self.toast(Tone::Warn, "The session isn't open yet; try again in a moment.", cx);
            return;
        };
        if let Some(option) = agent.config.iter_mut().find(|o| o.id == option_id) {
            option.current = value.to_string();
        }
        self.request(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": option_id, "value": value }),
            Pending::SetConfig(agent_id),
        );
        cx.notify();
    }

    /// The settings chips under the message box.
    fn render_session_chips(&self, agent: &AgentView, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let mut options: Vec<(usize, &ConfigOption)> = agent.config.iter().enumerate().filter(|(_, o)| o.choices.len() > 1).collect();
        options.sort_by_key(|(_, o)| category_rank(o));
        let chips = options.into_iter().map(|(index, option)| {
            let chip_id = SharedString::from(format!("chip-{id}-{}", option.id));
            if option.is_toggle() {
                let on = option.current == "on";
                let option_id = option.id.clone();
                return div()
                    .id(chip_id)
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(rgb(if on { theme::text_3() } else { theme::line() }))
                    .when(on, |d| d.bg(rgb(theme::selected())))
                    .text_size(px(12.))
                    .text_color(rgb(if on { theme::text() } else { theme::text_3() }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .tooltip(tip_static(option.description.clone()))
                    .child(option.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_config(id, &option_id, if on { "off" } else { "on" }, cx)))
                    .into_any_element();
            }
            let label = match option.category.as_str() {
                "thought_level" => format!("{} {}", option.name, option.current_name()),
                "mode" | "model" => {
                    // "Default (recommended)" says less than the model it is.
                    let current = option.choices.iter().find(|c| c.value == option.current);
                    match current {
                        Some(c) if c.value == "default" && !c.description.is_empty() && option.category == "model" => c.description.clone(),
                        Some(c) => c.name.clone(),
                        None => option.current.clone(),
                    }
                }
                _ => format!("{}: {}", option.name, option.current_name()),
            };
            let danger = option.id == "mode" && option.current == "bypassPermissions";
            let glyph = match option.category.as_str() {
                "mode" => Some(Icon::Shield),
                "model" => Some(Icon::Agent),
                "thought_level" => Some(Icon::Gauge),
                _ => None,
            };
            div()
                .id(chip_id)
                .h(px(24.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(5.))
                .border_1()
                .border_color(rgb(if danger { theme::red_line() } else { theme::line() }))
                .text_size(px(12.))
                .text_color(rgb(if danger { theme::red_text() } else { theme::text_2() }))
                .cursor_pointer()
                .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
                .children(glyph.map(|g| icon_sized(g, 12.).text_color(rgb(if danger { theme::red_text() } else { theme::text_3() }))))
                .child(label)
                .child(icon_sized(Icon::ChevronDown, 11.).text_color(rgb(theme::text_3())))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.open_menu(MenuTarget::Config(id, index), event.position(), window, cx);
                }))
                .into_any_element()
        });
        div().flex().flex_wrap().items_center().gap(px(4.)).min_w(px(0.)).children(chips).into_any_element()
    }

    // ---- Attachments -------------------------------------------------------------------------

    fn attach_image(&mut self, agent_id: i64, image: &gpui::Image, cx: &mut Context<Self>) {
        let number = self.agent(agent_id).map_or(0, |a| a.attachments.len()) + 1;
        match attach::from_image(image, number) {
            Ok(attachment) => {
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.attachments.push(attachment);
                }
                cx.notify();
            }
            Err(err) => self.toast(Tone::Warn, format!("{err:#}"), cx),
        }
    }

    pub(crate) fn attach_paths(&mut self, agent_id: i64, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            match attach::from_path(path) {
                Ok(attachment) => {
                    if let Some(agent) = self.agent_mut(agent_id) {
                        agent.attachments.push(attachment);
                    }
                }
                Err(err) => self.toast(Tone::Warn, format!("{err:#}"), cx),
            }
        }
        cx.notify();
    }

    fn pick_files(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn(async move |view, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                view.update(cx, |this, cx| this.attach_paths(agent_id, &paths, cx)).ok();
            }
        })
        .detach();
    }

    // ---- The message box ---------------------------------------------------------------------

    pub(crate) fn render_message_box(&self, agent: &AgentView, reading: f32, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let busy = agent.is_working();
        let Some(input) = self.inputs.get(&id).cloned() else { return div().into_any_element() };
        let has_text = !input.read(cx).text().trim().is_empty() || !agent.attachments.is_empty();
        let round = |el_id: &'static str| {
            div().id((el_id, id as usize)).size(px(28.)).flex_none().rounded_full().flex().items_center().justify_center().cursor_pointer()
        };
        let send = round("composer-send")
            .bg(rgb(if has_text { theme::text() } else { theme::line_strong() }))
            .hover(|s| s.bg(rgb(theme::primary_hover())))
            .active(|s| s.opacity(0.85))
            .tooltip(tip_key(if busy { "Queue: sent when this turn ends" } else { "Send" }, "Enter"))
            .child(icon_sized(Icon::Send, 14.).text_color(rgb(if has_text { theme::on_primary() } else { theme::text_3() })))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.sel_agent = Some(id);
                this.focus_request = Some(id);
                this.on_submit(&crate::Submit, window, cx);
            }));
        let stop = busy.then(|| {
            round("composer-stop")
                .border_1()
                .border_color(rgb(theme::line_strong()))
                .hover(|s| s.bg(rgb(theme::hover())))
                .tooltip(tip_key("Stop this turn", "Esc"))
                .child(icon_sized(Icon::Stop, 13.).text_color(rgb(theme::text())))
                .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(id, cx)))
        });
        let attach_button = ui::icon_button(("composer-attach", id as usize), Icon::Paperclip)
            .tooltip(tip("Attach files or images (or paste, or drop them here)"))
            .on_click(cx.listener(move |this, _, _, cx| this.pick_files(id, cx)));

        // Waiting to go out: queued while busy, or held by the budget.
        let queued = agent.queued_prompt.clone().map(|text| {
            let line = text.lines().next().unwrap_or_default().chars().take(90).collect::<String>();
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .text_size(px(12.))
                .text_color(rgb(theme::text_3()))
                .child(icon_sized(Icon::Clock, 13.).text_color(rgb(theme::text_3())))
                .child(div().flex_none().child(if agent.paused {
                    "Waits for the budget:"
                } else if busy {
                    "Queued for after this turn:"
                } else {
                    "Sent when the session is open:"
                }))
                .child(div().flex_1().min_w(px(0.)).truncate().text_color(rgb(theme::text_2())).child(line))
                .child(
                    div()
                        .id(("queued-cancel", id as usize))
                        .flex_none()
                        .cursor_pointer()
                        .tooltip(tip("Don't send it"))
                        .child(icon_sized(Icon::X, 12.).text_color(rgb(theme::text_3())))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(agent) = this.agent_mut(id) {
                                agent.queued_prompt = None;
                                agent.paused = false;
                            }
                            cx.notify();
                        })),
                )
        });

        let attachments = (!agent.attachments.is_empty()).then(|| {
            div().flex().flex_wrap().gap(px(6.)).children(agent.attachments.iter().enumerate().map(|(i, a)| {
                let image = matches!(a.kind, crate::agent::AttachmentKind::Image { .. });
                div()
                    .id(("attachment", id as usize * 1000 + i))
                    .h(px(26.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(5.))
                    .bg(rgb(theme::raised()))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .text_size(px(12.))
                    .children(a.preview.clone().map(|p| gpui::img(p).size(px(18.)).rounded(px(3.)).object_fit(gpui::ObjectFit::Cover)))
                    .when(a.preview.is_none(), |d| d.child(icon_sized(if image { Icon::Image } else { Icon::Lines }, 13.).text_color(rgb(theme::text_3()))))
                    .child(div().max_w(px(220.)).truncate().child(a.name.clone()))
                    .child(
                        div()
                            .id(("attachment-remove", id as usize * 1000 + i))
                            .size(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(theme::hover())))
                            .child(icon_sized(Icon::X, 11.).text_color(rgb(theme::text_3())))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(agent) = this.agent_mut(id)
                                    && i < agent.attachments.len()
                                {
                                    agent.attachments.remove(i);
                                }
                                cx.notify();
                            })),
                    )
            }))
        });

        let suggestions = self.suggestions(id, cx);
        let selected = self.suggest.get(&id).map_or(0, |s| s.selected).min(suggestions.len().saturating_sub(1));
        let popup = (!suggestions.is_empty()).then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(relative(1.))
                .pb(px(6.))
                .child(
                    div()
                        .id(("slash-list", id as usize))
                        .occlude()
                        .flex()
                        .flex_col()
                        .p(px(4.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(theme::line_strong()))
                        .bg(rgb(theme::raised()))
                        .shadow_lg()
                        .children(suggestions.into_iter().enumerate().map(|(i, s)| {
                            let on = i == selected;
                            div()
                                .id(("slash", id as usize * 100 + i))
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .h(px(30.))
                                .px(px(8.))
                                .rounded(px(5.))
                                .cursor_pointer()
                                .when(on, |d| d.bg(rgb(theme::selected())))
                                .hover(|d| d.bg(rgb(theme::hover())))
                                .child(div().flex_none().font_family(theme::MONO).text_size(px(12.5)).text_color(rgb(theme::text())).child(format!("/{}", s.name)))
                                .children(s.hint.map(|h| div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(h)))
                                .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_3())).child(s.description))
                                .when(on, |d| d.child(ui::kbd("Tab")))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.suggest.entry(id).or_default().selected = i;
                                    this.accept_suggestion(id, cx);
                                }))
                        })),
                )
        });

        let frame = div()
            .id(("composer", id as usize))
            .relative()
            .w(px(reading))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .pl(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(theme::line_strong()))
            .bg(rgb(theme::panel()))
            // Files dropped from Explorer become attachments.
            .drag_over::<ExternalPaths>(|s, _, _, _| s.border_color(rgb(theme::text_2())).bg(rgb(theme::hover())))
            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| this.attach_paths(id, paths.paths(), cx)))
            .children(popup)
            .children(queued)
            .children(attachments)
            .child(input)
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(8.))
                    .child(div().flex_1().min_w(px(0.)).child(self.render_session_chips(agent, cx)))
                    .child(attach_button)
                    .children(stop)
                    .child(send),
            );
        div().flex_none().flex().justify_center().px(px(24.)).pt(px(8.)).pb(px(16.)).child(frame).into_any_element()
    }

    // ---- Question forms (ACP elicitation) ----------------------------------------------------

    /// The agent asks something: record the form in its conversation.
    pub(crate) fn on_elicitation(&mut self, request_id: Value, params: &Value) {
        let session_id = params["sessionId"].as_str().unwrap_or_default();
        let Some(&agent_id) = self.sessions.get(session_id) else {
            // Nothing to show it in: say no rather than leave the agent waiting.
            self.respond(request_id, json!({ "action": "decline" }));
            return;
        };
        let form = Elicitation::parse(request_id, params);
        let on_screen = self.agent_on_screen(agent_id);
        let Some(agent) = self.agent_mut(agent_id) else { return };
        agent.items.push(Item::Elicit(form));
        let (name, sub_id) = (agent.record.name.clone(), agent.record.subproject_id);
        if !on_screen {
            self.queued_toasts.push((
                Tone::Warn,
                format!("{name} has a question for you"),
                Some(("Open", crate::feedback::ToastAction::Open { sub_id, agent_id: Some(agent_id) })),
            ));
        }
    }

    /// Text boxes for the open forms' free-text fields.
    pub(crate) fn prepare_form_inputs(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        let mut wanted = Vec::new();
        for (index, item) in agent.items.iter().enumerate() {
            if let Item::Elicit(form) = item
                && form.state == FormState::Open
            {
                for field in &form.fields {
                    if matches!(field.kind, FieldKind::Text | FieldKind::Number) {
                        wanted.push((index, field.key.clone(), field.description.clone()));
                    }
                }
            }
        }
        for (index, key, placeholder) in wanted {
            self.form_inputs.entry((agent_id, index, key)).or_insert_with(|| {
                let placeholder = if placeholder.is_empty() { "Type your answer".to_string() } else { placeholder };
                cx.new(|cx| TextInput::new(placeholder, cx))
            });
        }
    }

    /// Sends your answers (or that you skipped, or a cancel when the turn stops).
    pub(crate) fn answer_form(&mut self, agent_id: i64, index: usize, action: &str, cx: &mut Context<Self>) {
        let texts: Vec<(String, String)> = self
            .form_inputs
            .iter()
            .filter(|((a, i, _), _)| *a == agent_id && *i == index)
            .map(|((_, _, key), input)| (key.clone(), input.read(cx).text().to_string()))
            .collect();
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let Some(Item::Elicit(form)) = agent.items.get_mut(index) else { return };
        if form.state != FormState::Open {
            return;
        }
        for (key, text) in texts {
            let kind = form.fields.iter().find(|f| f.key == key).map(|f| f.kind.clone());
            if text.trim().is_empty() {
                continue;
            }
            let value = match kind {
                Some(FieldKind::Number) => text.trim().parse::<f64>().map(|n| json!(n)).unwrap_or(Value::String(text.clone())),
                _ => Value::String(text.trim().to_string()),
            };
            form.values.insert(key, value);
        }
        let response = match action {
            "accept" => {
                let content: serde_json::Map<String, Value> = form.values.clone().into_iter().collect();
                form.state = FormState::Answered(form.summary());
                if form.url.is_some() { json!({ "action": "accept" }) } else { json!({ "action": "accept", "content": content }) }
            }
            "decline" => {
                form.state = FormState::Skipped;
                json!({ "action": "decline" })
            }
            _ => {
                form.state = FormState::Withdrawn;
                json!({ "action": "cancel" })
            }
        };
        let request_id = form.request_id.clone();
        self.respond(request_id, response);
        self.form_inputs.retain(|(a, i, _), _| !(*a == agent_id && *i == index));
        self.focus_request = Some(agent_id);
        cx.notify();
    }

    fn pick_choice(&mut self, agent_id: i64, index: usize, key: &str, value: &str, multi: bool, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let Some(Item::Elicit(form)) = agent.items.get_mut(index) else { return };
        if multi {
            let entry = form.values.entry(key.to_string()).or_insert_with(|| json!([]));
            if let Value::Array(list) = entry {
                match list.iter().position(|v| v.as_str() == Some(value)) {
                    Some(at) => {
                        list.remove(at);
                    }
                    None => list.push(Value::String(value.to_string())),
                }
            }
        } else {
            form.values.insert(key.to_string(), Value::String(value.to_string()));
        }
        cx.notify();
    }

    pub(crate) fn render_elicitation(&self, agent_id: i64, index: usize, form: &Elicitation, cx: &mut Context<Self>) -> AnyElement {
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        let settled = |glyph: Icon, text: String| {
            div()
                .flex()
                .items_start()
                .gap(px(9.))
                .text_size(px(12.5))
                .text_color(rgb(theme::text_3()))
                .child(div().pt(px(2.)).child(icon_sized(glyph, 13.).text_color(rgb(theme::text_3()))))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_color(rgb(theme::text_2())).child(form.message.clone()))
                        .child(text),
                )
                .into_any_element()
        };
        match &form.state {
            FormState::Answered(summary) => return settled(Icon::Check, format!("You answered: {summary}")),
            FormState::Skipped => return settled(Icon::X, "You skipped this.".into()),
            FormState::Withdrawn => return settled(Icon::X, "Cancelled.".into()),
            FormState::Open => {}
        }

        let mut card = ui::card()
            .border_color(rgb(theme::red_line()))
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(ui::status_dot(("form-dot", agent_id as usize * 10_000 + index), State::NeedsYou))
                    .child(ui::label(format!("{name} asks"))),
            );
        if !form.message.is_empty() {
            card = card.child(div().text_size(px(14.5)).line_height(px(22.)).font_weight(FontWeight::MEDIUM).child(form.message.clone()));
        }
        if let Some(url) = &form.url {
            let url_for_open = url.clone();
            card = card
                .child(div().font_family(theme::MONO).text_size(px(12.)).text_color(rgb(theme::text_3())).truncate().child(url.clone()))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(ui::button(("form-open", index), Kind::Primary, Some(Icon::External), "Open the link").on_click(cx.listener(
                            move |_, _, _, cx| cx.open_url(&url_for_open),
                        )))
                        .child(ui::button(("form-done", index), Kind::Secondary, Some(Icon::Check), "I've done it").on_click(cx.listener(
                            move |this, _, _, cx| this.answer_form(agent_id, index, "accept", cx),
                        )))
                        .child(ui::button(("form-skip", index), Kind::Ghost, None, "Cancel").on_click(cx.listener(move |this, _, _, cx| {
                            this.answer_form(agent_id, index, "decline", cx)
                        }))),
                );
            return card.into_any_element();
        }

        for (field_no, field) in form.fields.iter().enumerate() {
            let key = field.key.clone();
            let is_other = field.key.ends_with("_custom") || field.title == "Other";
            let mut block = div().flex().flex_col().gap(px(8.));
            if !is_other && (!field.title.is_empty() || !field.description.is_empty()) {
                block = block.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .when(!field.title.is_empty(), |d| d.child(ui::label(field.title.clone())))
                        .when(!field.description.is_empty(), |d| {
                            d.child(div().text_size(px(13.5)).line_height(px(20.)).text_color(rgb(theme::text())).child(field.description.clone()))
                        }),
                );
            }
            match &field.kind {
                FieldKind::Single(choices) | FieldKind::Multi(choices) => {
                    let multi = matches!(field.kind, FieldKind::Multi(_));
                    let chosen = form.values.get(&field.key).cloned();
                    block = block.child(div().flex().flex_col().gap(px(4.)).children(choices.iter().enumerate().map(|(c, choice)| {
                        let on = match &chosen {
                            Some(Value::String(s)) => *s == choice.value,
                            Some(Value::Array(list)) => list.iter().any(|v| v.as_str() == Some(choice.value.as_str())),
                            _ => false,
                        };
                        let (k, value) = (key.clone(), choice.value.clone());
                        let mark = div()
                            .flex_none()
                            .mt(px(2.))
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgb(if on { theme::text() } else { theme::text_3() }))
                            .when(!multi, |d| d.rounded_full())
                            .when(multi, |d| d.rounded(px(3.)))
                            .when(on && multi, |d| d.bg(rgb(theme::text())).child(icon_sized(Icon::Check, 11.).text_color(rgb(theme::on_primary()))))
                            .when(on && !multi, |d| d.child(div().size(px(6.)).rounded_full().bg(rgb(theme::text()))));
                        div()
                            .id(SharedString::from(format!("choice-{agent_id}-{index}-{field_no}-{c}")))
                            .flex()
                            .items_start()
                            .gap(px(10.))
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(rgb(if on { theme::text_3() } else { theme::line() }))
                            .when(on, |d| d.bg(rgb(theme::selected())))
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(theme::hover())))
                            .child(mark)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .child(div().text_size(px(13.)).text_color(rgb(theme::text())).child(choice.name.clone()))
                                    .when(!choice.description.is_empty(), |d| {
                                        d.child(div().text_size(px(12.)).line_height(px(17.)).text_color(rgb(theme::text_3())).child(choice.description.clone()))
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.pick_choice(agent_id, index, &k, &value, multi, cx)))
                    })));
                }
                FieldKind::Text | FieldKind::Number => {
                    if let Some(input) = self.form_inputs.get(&(agent_id, index, field.key.clone())) {
                        block = block.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .when(is_other, |d| d.child(div().text_size(px(12.)).text_color(rgb(theme::text_3())).child("Or in your own words")))
                                .child(input.clone()),
                        );
                    }
                }
                FieldKind::Bool => {
                    let on = form.values.get(&field.key).and_then(Value::as_bool).unwrap_or(false);
                    let k = key.clone();
                    block = block.child(
                        ui::switch(SharedString::from(format!("form-bool-{agent_id}-{index}-{field_no}")), if field.title.is_empty() { "Yes" } else { "On" }, on)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(agent) = this.agent_mut(agent_id)
                                    && let Some(Item::Elicit(form)) = agent.items.get_mut(index)
                                {
                                    form.values.insert(k.clone(), Value::Bool(!on));
                                }
                                cx.notify();
                            })),
                    );
                }
            }
            card = card.child(block);
        }
        card.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(ui::button(("form-send", index), Kind::Primary, Some(Icon::Send), "Send answers").on_click(cx.listener(
                    move |this, _, _, cx| this.answer_form(agent_id, index, "accept", cx),
                )))
                .child(ui::button(("form-skip", index), Kind::Ghost, None, "Skip").on_click(cx.listener(move |this, _, _, cx| {
                    this.answer_form(agent_id, index, "decline", cx)
                })))
                .child(div().flex_1())
                .child(div().text_size(px(11.5)).text_color(rgb(theme::text_3())).child("The agent waits for this.")),
        )
        .into_any_element()
    }

    // ---- Tool calls --------------------------------------------------------------------------

    pub(crate) fn render_tool_item(
        &self,
        agent_id: i64,
        index: usize,
        title: &str,
        kind: &str,
        status: &str,
        detail: &ToolDetail,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let monaged = title.strip_prefix("mcp__monaged__");
        let glyph = match (monaged, kind) {
            (Some("ask"), _) => Icon::Ask,
            (Some(_), _) => Icon::Board,
            (_, "edit" | "delete" | "move") => Icon::Edit,
            (_, "execute") => Icon::Terminal,
            (_, "read") => Icon::Eye,
            (_, "search") => Icon::Search,
            (_, "fetch") => Icon::External,
            (_, "think") => Icon::Question,
            _ => Icon::Agent,
        };
        let shown = match monaged {
            Some(tool) => format!("monaged · {}", tool.replace('_', " ")),
            None => title.to_string(),
        };
        let state: AnyElement = match status {
            "in_progress" | "pending" => ui::status_dot(("tool", agent_id as usize * 100_000 + index), State::Working),
            "failed" => div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(rgb(theme::red_text()))
                .child(icon_sized(Icon::Alert, 12.).text_color(rgb(theme::red_text())))
                .child("failed")
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let has_detail = detail.input.is_some() || !detail.content.is_empty() || !detail.locations.is_empty();
        let open = self.expanded_tools.contains(&(agent_id, index));
        let row = div()
            .id(("tool-row", agent_id as usize * 100_000 + index))
            .flex()
            .items_center()
            .gap(px(9.))
            .text_color(rgb(theme::text_3()))
            .when(has_detail, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if !this.expanded_tools.remove(&(agent_id, index)) {
                        this.expanded_tools.insert((agent_id, index));
                    }
                    cx.notify();
                }))
            })
            .child(icon(glyph).size(px(14.)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(theme::MONO)
                    .text_size(px(12.))
                    .text_color(rgb(theme::text_2()))
                    .hover(|s| s.text_color(rgb(theme::text())))
                    .child(shown),
            )
            .child(div().font_family(theme::MONO).text_size(px(11.)).child(state))
            .when(has_detail, |d| d.child(ui::disclosure(open)));
        if !open {
            return row.into_any_element();
        }
        let mono_box = |text: String, lines: usize| {
            let all: Vec<&str> = text.lines().collect();
            let more = all.len().saturating_sub(lines);
            div()
                .px(px(10.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(rgb(theme::code_bg()))
                .border_1()
                .border_color(rgb(theme::line()))
                .font_family(theme::MONO)
                .text_size(px(11.5))
                .line_height(px(17.))
                .text_color(rgb(theme::text_2()))
                .overflow_hidden()
                .children(all.into_iter().take(lines).map(|l| div().child(if l.is_empty() { " ".to_string() } else { l.replace('\t', "    ") })))
                .when(more > 0, |d| d.child(div().text_color(rgb(theme::text_3())).child(format!("… {more} more lines"))))
        };
        let mut body = div().flex().flex_col().gap(px(8.)).pl(px(23.));
        if !detail.locations.is_empty() {
            body = body.child(div().flex().flex_wrap().gap(px(6.)).children(detail.locations.iter().enumerate().map(|(i, loc)| {
                let path = loc.rsplit_once(':').filter(|(_, l)| l.chars().all(|c| c.is_ascii_digit())).map_or(loc.clone(), |(p, _)| p.to_string());
                div()
                    .id(("tool-loc", agent_id as usize * 100_000 + index * 10 + i))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(rgb(theme::line()))
                    .font_family(theme::MONO)
                    .text_size(px(11.))
                    .text_color(rgb(theme::text_2()))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .tooltip(tip("Open in Zed"))
                    .child(loc.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.open_in_zed(path.clone(), cx)))
            })));
        }
        if let Some(input) = &detail.input
            && input != "{}"
        {
            body = body.child(ui::label("Input")).child(mono_box(input.clone(), 14));
        }
        for content in &detail.content {
            body = match content {
                ToolContent::Text(text) => body.child(ui::label("Output")).child(mono_box(text.clone(), 24)),
                ToolContent::Diff { path, old, new } => body.child(ui::label(format!("Diff · {path}"))).child(render_text_diff(path, old.as_deref(), new)),
                ToolContent::Terminal(id) => body.child(ui::mono(format!("Terminal {id}"))),
            };
        }
        div().flex().flex_col().gap(px(8.)).child(row).child(body).into_any_element()
    }

    pub(crate) fn start_rename(&mut self, agent_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        self.ask_text(
            format!("Rename {name}"),
            "A short name".into(),
            "Rename".into(),
            name,
            crate::feedback::TextAction::RenameAgent(agent_id),
            window,
            cx,
        );
    }

    /// Hides an agent from the tree; its worktree, branch and conversation stay.
    pub(crate) fn archive_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        if self.agent(agent_id).is_some_and(|a| a.is_working()) {
            self.toast(Tone::Warn, "Stop the agent before archiving it.", cx);
            return;
        }
        if let Err(err) = self.store.archive_agent(agent_id) {
            self.toast_error(format!("Couldn't archive: {err:#}"), cx);
            return;
        }
        let name = self.agent(agent_id).map(|a| a.record.name.clone()).unwrap_or_default();
        if let Some(agent) = self.agent_mut(agent_id) {
            agent.record.archived = true;
        }
        self.pinned.retain(|&id| id != agent_id);
        if self.sel_agent == Some(agent_id) {
            let sub = self.agent(agent_id).map(|a| a.record.subproject_id);
            self.sel_agent = self.agents.iter().find(|a| Some(a.record.subproject_id) == sub && !a.record.archived).map(|a| a.record.id);
        }
        if let Some(sub_id) = self.agent(agent_id).map(|a| a.record.subproject_id) {
            let _ = self.store.log_event(sub_id, Some(agent_id), "archive", &format!("Archived {name}"));
        }
        self.toast_action(Tone::Info, format!("Archived {name}"), "Undo", crate::feedback::ToastAction::Restore(agent_id), cx);
        cx.notify();
    }

    /// Brings an archived agent back, if its worktree is still there.
    pub(crate) fn restore_agent(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        let (distro, worktree) = (self.config.distro.clone(), agent.record.worktree.clone());
        cx.spawn(async move |view, cx| {
            let wt = worktree.clone();
            let exists = cx
                .background_spawn(async move { crate::wsl::run(&distro, &format!("test -d {} && echo yes || echo no", crate::wsl::shell_path(&wt))) })
                .await
                .map(|out| out.trim() == "yes")
                .unwrap_or(false);
            view.update(cx, |this, cx| {
                if !exists {
                    this.toast(Tone::Warn, "Its worktree was cleaned up, so it stays read-only. Start a new agent to continue the work.", cx);
                    return;
                }
                if let Err(err) = this.store.unarchive_agent(agent_id) {
                    this.toast_error(format!("Couldn't restore: {err:#}"), cx);
                    return;
                }
                if let Some(agent) = this.agent_mut(agent_id) {
                    agent.record.archived = false;
                    let name = agent.record.name.clone();
                    this.toast(Tone::Info, format!("Restored {name}"), cx);
                }
                this.open_agent(agent_id, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Renames an agent (the name only; its branch and worktree stay).
    pub(crate) fn rename_agent(&mut self, agent_id: i64, name: &str, cx: &mut Context<Self>) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        match self.store.rename_agent(agent_id, &name) {
            Ok(()) => {
                if let Some(agent) = self.agent_mut(agent_id) {
                    agent.record.name = name.clone();
                }
                self.toast(Tone::Info, format!("Renamed to {name}"), cx);
            }
            Err(err) => self.toast_error(format!("Couldn't rename: {err:#}"), cx),
        }
        cx.notify();
    }
}

/// A tooltip with text known only at runtime.
pub(crate) fn tip_static(text: String) -> impl Fn(&mut Window, &mut gpui::App) -> gpui::AnyView {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| crate::shell::Tip::new(text.clone())).into()
}

/// A compact before/after view of an edit: unchanged lines around the change
/// are trimmed, removed lines then added lines.
fn render_text_diff(path: &str, old: Option<&str>, new: &str) -> AnyElement {
    let lang = crate::syntax::lang_from_path(path);
    let old_lines: Vec<&str> = old.map(|o| o.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();
    let prefix = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let suffix = old_lines[prefix..].iter().rev().zip(new_lines[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let removed = &old_lines[prefix..old_lines.len() - suffix];
    let added = &new_lines[prefix..new_lines.len() - suffix];
    const MAX: usize = 40;
    let line = |sign: &'static str, text: &str, fg: u32, bg: u32| {
        div().flex().bg(rgb(bg)).text_color(rgb(fg)).child(div().w(px(16.)).flex_none().child(sign)).child(div().flex_1().min_w(px(0.)).child(crate::syntax::line(text, lang)))
    };
    let shown = removed.len() + added.len();
    div()
        .rounded(px(6.))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(theme::line()))
        .font_family(theme::MONO)
        .text_size(px(11.5))
        .line_height(px(17.))
        .px(px(6.))
        .py(px(4.))
        .when(prefix > 0, |d| d.child(div().text_color(rgb(theme::text_3())).child(format!("  … {prefix} unchanged lines"))))
        .children(removed.iter().take(MAX).map(|l| line("-", l, theme::del(), theme::del_bg())))
        .children(added.iter().take(MAX.saturating_sub(removed.len().min(MAX))).map(|l| line("+", l, theme::add(), theme::add_bg())))
        .when(shown > MAX, |d| d.child(div().text_color(rgb(theme::text_3())).child(format!("  … {} more changed lines", shown - MAX))))
        .when(suffix > 0, |d| d.child(div().text_color(rgb(theme::text_3())).child(format!("  … {suffix} unchanged lines"))))
        .into_any_element()
}
