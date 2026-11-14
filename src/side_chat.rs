//! The side chat: ask Haiku about an agent's work without touching the
//! agent's own context. Haiku gets the task, the latest summary, the end of
//! the conversation and the changed files; the agent never sees any of it.

use gpui::{AnyElement, Context, Entity, Focusable, Window, div, prelude::*, px, rgb};
use serde_json::json;

use crate::agent::{Item, Tone};
use crate::icons::{Icon, icon_sized};
use crate::text_input::TextInput;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

pub(crate) const SIDE_WIDTH: f32 = 360.;

pub(crate) struct SideChat {
    pub open: bool,
    /// (asked by you, text)
    pub messages: Vec<(bool, String)>,
    pub input: Entity<TextInput>,
    pub busy: bool,
}

const SYSTEM: &str = "You answer the developer's questions about a coding agent's work: what it did, why, what's risky, what's left. \
Use only the context given; say when it doesn't tell you. Be brief and concrete; quote file names. Plain text or short Markdown.";

impl Monaged {
    pub(crate) fn toggle_side_chat(&mut self, agent_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.side_chats.entry(agent_id).or_insert_with(|| SideChat {
            open: false,
            messages: Vec::new(),
            input: cx.new(|cx| TextInput::new("Ask about this agent's work…", cx).multiline().max_lines(4)),
            busy: false,
        });
        chat.open = !chat.open;
        if chat.open {
            window.focus(&chat.input.focus_handle(cx));
        } else {
            self.focus_request = Some(agent_id);
        }
        cx.notify();
    }

    /// The side chat whose box has the cursor.
    pub(crate) fn focused_side_chat(&self, window: &Window, cx: &gpui::App) -> Option<i64> {
        self.side_chats.iter().find(|(_, c)| c.open && c.input.focus_handle(cx).is_focused(window)).map(|(id, _)| *id)
    }

    fn side_context(&self, agent_id: i64) -> String {
        let Some(agent) = self.agent(agent_id) else { return String::new() };
        let mut recent = String::new();
        for item in agent.items.iter().rev().take(40).collect::<Vec<_>>().into_iter().rev() {
            match item {
                Item::User(t) => recent.push_str(&format!("\n[developer] {}\n", t.chars().take(1500).collect::<String>())),
                Item::Agent(t) => recent.push_str(&format!("\n[agent] {}\n", t.chars().take(2500).collect::<String>())),
                Item::Tool { title, status, .. } => recent.push_str(&format!("[tool] {title} ({status})\n")),
                _ => {}
            }
        }
        let files = agent.changes.as_ref().map(|c| crate::diff::diffstat(c)).unwrap_or_else(|| "(not loaded)".into());
        let summary = agent.summary.as_ref().map(|s| s.text.clone()).unwrap_or_default();
        format!(
            "Agent: {} ({}), branch {}.\nTask: {}\nLatest summary: {}\nChanged files:\n{}\n\nEnd of the conversation:\n{}",
            agent.record.name,
            agent.record.role,
            agent.record.branch,
            agent.task().chars().take(2000).collect::<String>(),
            summary,
            files,
            recent.chars().rev().take(16_000).collect::<String>().chars().rev().collect::<String>()
        )
    }

    pub(crate) fn ask_side_chat(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(backend) = self.backend() else {
            self.toast(Tone::Warn, "The side chat needs an API key or the Max plan fallback (Settings → API key).", cx);
            return;
        };
        let context = self.side_context(agent_id);
        let Some(chat) = self.side_chats.get_mut(&agent_id) else { return };
        let question = chat.input.read(cx).text().trim().to_string();
        if question.is_empty() || chat.busy {
            return;
        }
        chat.input.update(cx, |i, cx| i.clear(cx));
        chat.messages.push((true, question.clone()));
        chat.busy = true;
        let history: String = chat.messages.iter().rev().skip(1).take(6).collect::<Vec<_>>().into_iter().rev().map(|(you, t)| format!("{}: {t}\n", if *you { "Developer" } else { "You" })).collect();
        cx.notify();
        cx.spawn(async move |view, cx| {
            let b = backend.clone();
            let result = cx
                .background_spawn(async move {
                    let schema = json!({ "type": "object", "properties": { "answer": { "type": "string" } }, "required": ["answer"], "additionalProperties": false });
                    let prompt = format!("{context}\n\nEarlier in this side chat:\n{history}\nQuestion: {question}");
                    crate::llm::complete_json(&b, SYSTEM, &prompt, &schema)
                })
                .await;
            view.update(cx, |this, cx| {
                match result {
                    Ok((value, usage)) => {
                        this.log_haiku(agent_id, "side-chat", backend.via(), &usage);
                        if let Some(chat) = this.side_chats.get_mut(&agent_id) {
                            chat.messages.push((false, value["answer"].as_str().unwrap_or("(no answer)").to_string()));
                        }
                    }
                    Err(err) => this.toast_error(format!("Haiku couldn't answer: {err:#}"), cx),
                }
                if let Some(chat) = this.side_chats.get_mut(&agent_id) {
                    chat.busy = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_side_chat(&self, agent_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = self.side_chats.get(&agent_id).filter(|c| c.open)?;
        let messages = chat.messages.iter().enumerate().map(|(i, (you, text))| {
            let body: AnyElement = if *you {
                div().text_size(px(13.)).text_color(rgb(theme::text())).child(text.clone()).into_any_element()
            } else {
                crate::review_ui::render_markdown(text)
            };
            div()
                .id(("side-msg", i))
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .when(*you, |d| d.bg(rgb(theme::raised())).border_1().border_color(rgb(theme::line())))
                .child(ui::label(if *you { "You" } else { "Haiku" }))
                .child(body)
        });
        Some(
            div()
                .w(px(SIDE_WIDTH))
                .flex_none()
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(rgb(theme::line()))
                .bg(rgb(theme::panel()))
                .child(
                    div()
                        .flex_none()
                        .h(px(44.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(12.))
                        .border_b_1()
                        .border_color(rgb(theme::line()))
                        .child(icon_sized(Icon::Ask, 14.).text_color(rgb(theme::text_2())))
                        .child(div().flex_1().flex().flex_col().child(div().text_size(px(13.)).child("Side chat")).child(ui::meta("Haiku · the agent doesn't see this")))
                        .child(ui::icon_button(("side-close", agent_id as usize), Icon::X).on_click(cx.listener(move |this, _, window, cx| this.toggle_side_chat(agent_id, window, cx)))),
                )
                .child(
                    div()
                        .id(("side-list", agent_id as usize))
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .p(px(10.))
                        .when(chat.messages.is_empty(), |d| {
                            d.child(ui::meta("Ask what the agent changed, why, what could break, or what's left. Haiku reads its conversation and diff; nothing reaches the agent."))
                        })
                        .children(messages)
                        .when(chat.busy, |d| d.child(div().flex().items_center().gap(px(8.)).child(ui::spinner(("side-spin", agent_id as usize), 13., theme::text_3())).child(ui::meta("Haiku is reading…")))),
                )
                .child(
                    div()
                        .flex_none()
                        .p(px(10.))
                        .border_t_1()
                        .border_color(rgb(theme::line()))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.))
                                .p(px(8.))
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(theme::line_strong()))
                                .bg(rgb(theme::raised()))
                                .child(chat.input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .child(div().flex_1().child(ui::meta("Enter asks")))
                                        .child(ui::button(("side-ask", agent_id as usize), Kind::Secondary, Some(Icon::Send), "Ask").on_click(cx.listener(move |this, _, _, cx| this.ask_side_chat(agent_id, cx)))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
