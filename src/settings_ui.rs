//! More of Settings:
//! - Agents: which ACP agent Monaged runs (Claude by default), what it says it
//!   can do, and profiles for other ACP agents (Monaged's UI follows whatever
//!   an agent reports: its modes, models, commands and question forms);
//! - Message box: Enter or Ctrl+Enter to send;
//! - Notifications;
//! - General: where Monaged opens, and its data folder.

use gpui::{AnyElement, Context, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::agent::Tone;
use crate::icons::Icon;
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

/// The built-in profile: Claude through claude-agent-acp in WSL.
pub(crate) const CLAUDE_COMMAND: &str = "~/.local/share/claude-agent-acp/current/claude-agent-acp";

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AgentProfile {
    pub name: String,
    pub command: String,
}

/// Saved profiles (Claude is always first).
pub(crate) fn profiles(store: &crate::store::Store) -> Vec<AgentProfile> {
    let mut out = vec![AgentProfile { name: "Claude".into(), command: CLAUDE_COMMAND.into() }];
    if let Some(list) = store.setting("agents:profiles").and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        for p in list.as_array().into_iter().flatten() {
            if let (Some(name), Some(command)) = (p["name"].as_str(), p["command"].as_str()) {
                out.push(AgentProfile { name: name.into(), command: command.into() });
            }
        }
    }
    out
}

/// The command of the active profile.
pub(crate) fn active_command(store: &crate::store::Store) -> String {
    let active = store.setting("agents:active").unwrap_or_else(|| "Claude".into());
    profiles(store).into_iter().find(|p| p.name == active).map(|p| p.command).unwrap_or_else(|| CLAUDE_COMMAND.into())
}

impl Monaged {
    fn save_profiles(&self, list: &[AgentProfile]) {
        let extra: Vec<Value> = list.iter().filter(|p| p.name != "Claude").map(|p| json!({ "name": p.name, "command": p.command })).collect();
        let _ = self.store.set_setting("agents:profiles", &Value::Array(extra).to_string());
    }

    /// Switches the agent Monaged runs, and reconnects.
    fn use_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(profile) = profiles(&self.store).into_iter().find(|p| p.name == name) else { return };
        let _ = self.store.set_setting("agents:active", name);
        self.config.agent_cmd = profile.command;
        self.toast(Tone::Info, format!("Now using {name}. Reconnecting; open sessions start again in the new agent."), cx);
        self.reconnect_now(cx);
    }

    fn add_profile(&mut self, cx: &mut Context<Self>) {
        let Some((name, command)) = &self.profile_inputs else { return };
        let (name_text, command_text) = (name.read(cx).text().trim().to_string(), command.read(cx).text().trim().to_string());
        if name_text.is_empty() || command_text.is_empty() {
            self.toast(Tone::Warn, "Give the agent a name and the command that starts it in WSL.", cx);
            return;
        }
        let mut list = profiles(&self.store);
        if list.iter().any(|p| p.name.eq_ignore_ascii_case(&name_text)) {
            self.toast(Tone::Warn, format!("There's already an agent called {name_text}."), cx);
            return;
        }
        list.push(AgentProfile { name: name_text.clone(), command: command_text });
        self.save_profiles(&list);
        name.update(cx, |i, cx| i.clear(cx));
        command.update(cx, |i, cx| i.clear(cx));
        self.toast(Tone::Info, format!("Added {name_text}. Pick it to switch."), cx);
        cx.notify();
    }

    fn remove_profile(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut list = profiles(&self.store);
        list.retain(|p| p.name != name);
        self.save_profiles(&list);
        if self.store.setting("agents:active").as_deref() == Some(name) {
            self.use_profile("Claude", cx);
        }
        cx.notify();
    }

    pub(crate) fn ensure_profile_inputs(&mut self, cx: &mut Context<Self>) {
        if self.profile_inputs.is_none() {
            let name = cx.new(|cx| TextInput::new("Name, e.g. Gemini", cx));
            let command = cx.new(|cx| TextInput::new("Command in WSL, e.g. gemini --experimental-acp", cx));
            self.profile_inputs = Some((name, command));
        }
    }

    pub(crate) fn render_agent_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.store.setting("agents:active").unwrap_or_else(|| "Claude".into());
        let list = profiles(&self.store);
        let info = self.agent_info.clone().unwrap_or(Value::Null);
        let caps = &info["agentCapabilities"];
        let yes_no = |v: &Value| if v.as_bool().unwrap_or(false) || v.is_object() { "yes" } else { "no" };
        let name = info["agentInfo"]["title"].as_str().or(info["agentInfo"]["name"].as_str()).unwrap_or("Not connected yet").to_string();
        let version = info["agentInfo"]["version"].as_str().unwrap_or_default().to_string();
        let mut col = div().flex().flex_col().gap(px(14.));
        col = col.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(theme::line()))
                .bg(rgb(theme::panel()))
                .child(div().flex().items_center().gap(px(8.)).child(ui::heading(name)).child(ui::mono(version)))
                .child(ui::mono(self.config.agent_cmd.clone()))
                .when(!info.is_null(), |d| {
                    d.child(div().pt(px(6.)).flex().flex_wrap().gap(px(14.)).children([
                        ("Images", yes_no(&caps["promptCapabilities"]["image"])),
                        ("Files in messages", yes_no(&caps["promptCapabilities"]["embeddedContext"])),
                        ("Reopen sessions", yes_no(&caps["loadSession"])),
                        ("Fork sessions", yes_no(&caps["sessionCapabilities"]["fork"])),
                        ("Session list", yes_no(&caps["sessionCapabilities"]["list"])),
                        ("MCP over HTTP", yes_no(&caps["mcpCapabilities"]["http"])),
                    ].into_iter().map(|(k, v)| {
                        div().flex().gap(px(5.)).text_size(px(12.)).child(div().text_color(rgb(theme::text_3())).child(k)).child(div().text_color(rgb(if v == "yes" { theme::text() } else { theme::text_4() })).child(v))
                    })))
                }),
        );
        col = col.child(div().flex().flex_col().gap(px(1.)).children(list.iter().enumerate().map(|(i, p)| {
            let on = p.name == active;
            let (n, removable) = (p.name.clone(), p.name != "Claude");
            let n2 = n.clone();
            ui::row(("profile", i), on)
                .h(px(34.))
                .child(div().w(px(16.)).flex_none().children(on.then(|| crate::icons::icon_sized(Icon::Check, 13.).text_color(rgb(theme::text())))))
                .child(div().w(px(140.)).flex_none().child(p.name.clone()))
                .child(div().flex_1().min_w(px(0.)).truncate().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(p.command.clone()))
                .when(removable, |d| {
                    d.child(ui::icon_button(("profile-remove", i), Icon::Trash).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.remove_profile(&n2, cx);
                    })))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.store.setting("agents:active").unwrap_or_else(|| "Claude".into()) != n {
                        this.use_profile(&n, cx);
                    }
                }))
        })));
        if let Some((name, command)) = &self.profile_inputs {
            col = col.child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(8.))
                    .child(div().w(px(180.)).flex_none().child(name.clone()))
                    .child(div().flex_1().child(command.clone()))
                    .child(ui::button("profile-add", Kind::Secondary, Some(Icon::Plus), "Add agent").on_click(cx.listener(|this, _, _, cx| this.add_profile(cx)))),
            );
        }
        col.child(ui::meta(
            "Any agent that speaks the Agent Client Protocol works: Monaged shows the modes, models, commands and question forms it reports. Switching reconnects; agents that are working stop.",
        ))
        .into_any_element()
    }

    pub(crate) fn render_message_box_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let ctrl = self.store.setting("composer:send").as_deref() == Some("ctrl-enter");
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().flex().gap(px(6.)).child(ui::choice("send-enter", "Enter sends, Shift+Enter adds a line", !ctrl).on_click(cx.listener(|this, _, _, cx| this.set_send_key(false, cx)))).child(
                ui::choice("send-ctrl", "Ctrl+Enter sends, Enter adds a line", ctrl).on_click(cx.listener(|this, _, _, cx| this.set_send_key(true, cx))),
            ))
            .child(ui::meta("Up in an empty message box brings back what you sent before; / lists commands."))
            .into_any_element()
    }

    fn set_send_key(&mut self, ctrl: bool, cx: &mut Context<Self>) {
        let _ = self.store.set_setting("composer:send", if ctrl { "ctrl-enter" } else { "enter" });
        for input in self.inputs.values() {
            input.update(cx, |i, cx| i.set_ctrl_send(ctrl, cx));
        }
        cx.notify();
    }

    pub(crate) fn render_notification_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let flag = |key: &str| self.store.setting(key).is_none_or(|v| v == "1");
        let toggle = |id: &'static str, key: &'static str, text: &'static str, on: bool, cx: &mut Context<Self>| {
            ui::switch(id, text, on).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.store.set_setting(key, if on { "0" } else { "1" });
                cx.notify();
            }))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(toggle("notify-os", "notify:os", "Windows notifications when something needs you and you're in another window", flag("notify:os"), cx))
            .child(toggle("notify-finished", "notify:finished", "Also when an agent finishes its turn", flag("notify:finished"), cx))
            .child(toggle("notify-flash", "notify:flash", "Flash the taskbar button until you come back", flag("notify:flash"), cx))
            .child(ui::meta("They come from Monaged's tray icon; clicking one opens the agent. Windows' Do not disturb holds them back."))
            .into_any_element()
    }

    pub(crate) fn render_general_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let resume = self.store.setting("ui:resume").is_none_or(|v| v == "1");
        let compact = ui::compact();
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(div().flex().gap(px(6.)).child(ui::choice("density-comfortable", "Comfortable", !compact).on_click(cx.listener(|this, _, _, cx| this.set_density(false, cx)))).child(
                ui::choice("density-compact", "Compact", compact).on_click(cx.listener(|this, _, _, cx| this.set_density(true, cx))),
            ))
            .child(ui::switch("resume", "Open where you left off (the last subproject and agent)", resume).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.store.set_setting("ui:resume", if resume { "0" } else { "1" });
                cx.notify();
            })))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(ui::button("open-data", Kind::Secondary, Some(Icon::Folder), "Open the data folder").on_click(cx.listener(|this, _, _, cx| {
                        let dir = std::env::var("APPDATA").map(|a| format!("{a}\\Monaged")).unwrap_or_default();
                        if let Err(err) = std::process::Command::new("explorer.exe").arg(&dir).spawn() {
                            this.toast_error(format!("Couldn't open {dir}: {err}"), cx);
                        }
                    })))
                    .child(ui::button("open-log", Kind::Secondary, Some(Icon::Lines), "Open the log").on_click(cx.listener(|this, _, _, cx| {
                        let log = crate::store::data_dir().join("monaged.log");
                        if !log.exists() {
                            this.toast(Tone::Info, "There's no log yet: development builds write to their console instead.", cx);
                        } else if let Err(err) = std::process::Command::new("notepad.exe").arg(&log).spawn() {
                            this.toast_error(format!("Couldn't open {}: {err}", log.display()), cx);
                        }
                    })))
                    .child(ui::button("open-setup", Kind::Secondary, Some(Icon::Flag), "Setup guide").on_click(cx.listener(|this, _, _, cx| this.open_setup(cx)))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .child(ui::meta(format!(
                        "Monaged {} · MIT license with the Commons Clause · an independent project, not affiliated with Anthropic.",
                        env!("CARGO_PKG_VERSION")
                    )))
                    .child(ui::button("open-website", Kind::Ghost, Some(Icon::External), "GitHub").on_click(cx.listener(|_, _, _, cx| cx.open_url("https://github.com/Marten-Mrfc/monaged"))))
                    .child(ui::button("open-docs", Kind::Ghost, Some(Icon::External), "Help").on_click(cx.listener(|_, _, _, cx| cx.open_url("https://github.com/Marten-Mrfc/monaged/blob/main/docs/user-guide.md")))),
            )
            .into_any_element()
    }

    /// Remembers where you are, for "Open where you left off".
    pub(crate) fn remember_place(&mut self) {
        let place = format!("{}|{}", self.sel_sub.unwrap_or(0), self.sel_agent.unwrap_or(0));
        if self.last_place != place {
            let _ = self.store.set_setting("ui:place", &place);
            self.last_place = place;
        }
    }

    /// Goes back to the saved place at start.
    pub(crate) fn restore_place(&mut self, cx: &mut Context<Self>) {
        if self.store.setting("ui:resume").is_some_and(|v| v == "0") {
            return;
        }
        let Some(place) = self.store.setting("ui:place") else { return };
        let mut parts = place.split('|').map(|p| p.parse::<i64>().unwrap_or(0));
        let (sub, agent) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        if sub != 0 && self.subprojects.iter().any(|s| s.id == sub) {
            self.select_sub(sub, cx);
            if agent != 0 && self.agent(agent).is_some_and(|a| a.record.subproject_id == sub && !a.record.archived) {
                self.select_agent(agent, cx);
            }
        }
        self.last_place = place;
    }
}
