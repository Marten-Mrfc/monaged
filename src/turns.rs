//! The conversation, turn by turn: an agent's steps (tool calls and its
//! thinking) folded into one line that says what they did and opens on
//! click, a footer that says what each turn took and changed, and a live
//! line with the step that's running while it works.
//!
//! Why: twenty tool calls in a row bury what the agent said; a summary line
//! ("Read 4 files, searched twice and ran a command") keeps the trail one
//! click away without making you scroll past it. Settings → General turns the
//! grouping off for those who want every step in view.

use std::collections::BTreeSet;
use std::ops::Range;

use gpui::{AnyElement, ClipboardItem, Context, div, prelude::*, px, rgb};

use crate::agent::{AgentTab, AgentView, Item, Tone, ToolContent};
use crate::icons::{Icon, icon_sized};
use crate::ui::{self, State};
use crate::{Monaged, theme};

/// What a step did, for the summary line.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum StepKind {
    Read,
    Edit,
    Run,
    Search,
    Fetch,
    Board,
    Ask,
    Think,
    Other,
}

/// A tool call's kind, from ACP's `kind` and the tool's name.
pub(crate) fn step_kind(kind: &str, title: &str) -> StepKind {
    match title.strip_prefix("mcp__monaged__") {
        Some("ask") => return StepKind::Ask,
        Some(_) => return StepKind::Board,
        None => {}
    }
    match kind {
        "read" => StepKind::Read,
        "edit" | "delete" | "move" => StepKind::Edit,
        "execute" => StepKind::Run,
        "search" => StepKind::Search,
        "fetch" => StepKind::Fetch,
        "think" => StepKind::Think,
        _ => StepKind::Other,
    }
}

fn step_icon(kind: StepKind) -> Icon {
    match kind {
        StepKind::Read => Icon::Eye,
        StepKind::Edit => Icon::Edit,
        StepKind::Run => Icon::Terminal,
        StepKind::Search => Icon::Search,
        StepKind::Fetch => Icon::External,
        StepKind::Board => Icon::Board,
        StepKind::Ask => Icon::Ask,
        StepKind::Think => Icon::Question,
        StepKind::Other => Icon::Agent,
    }
}

/// `lunchbox/orders.py:12` → `orders.py`.
fn file_name(location: &str) -> String {
    let path = location.rsplit_once(':').filter(|(_, l)| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit())).map_or(location, |(p, _)| p);
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// "a, b and c".
fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// One line for a run of steps: "Read orders.py, searched twice and ran a
/// command". Each step is its kind and the files it touched.
pub(crate) fn steps_summary(steps: &[(StepKind, Vec<String>)]) -> String {
    let count = |k: StepKind| steps.iter().filter(|(s, _)| *s == k).count();
    let files = |k: StepKind| -> BTreeSet<String> { steps.iter().filter(|(s, _)| *s == k).flat_map(|(_, l)| l.iter().map(|p| file_name(p))).collect() };
    // "read orders.py" for one file, "read 4 files" for more.
    let on_files = |k: StepKind, verb: &str| -> Option<String> {
        let calls = count(k);
        if calls == 0 {
            return None;
        }
        let names = files(k);
        Some(match names.len() {
            1 => format!("{verb} {}", names.first().cloned().unwrap_or_default()),
            0 if calls == 1 => format!("{verb} a file"),
            0 => format!("{verb} {calls} files"),
            n => format!("{verb} {n} files"),
        })
    };
    let several = |n: usize, one: &str, many: &str| -> Option<String> {
        match n {
            0 => None,
            1 => Some(one.to_string()),
            n => Some(many.replace("{n}", &n.to_string())),
        }
    };
    let parts: Vec<String> = [
        on_files(StepKind::Read, "read"),
        several(count(StepKind::Search), "searched once", "searched {n} times").map(|s| if count(StepKind::Search) == 2 { "searched twice".into() } else { s }),
        on_files(StepKind::Edit, "edited"),
        several(count(StepKind::Run), "ran a command", "ran {n} commands"),
        several(count(StepKind::Fetch), "fetched a page", "fetched {n} pages"),
        several(count(StepKind::Board), "used the board", "used the board {n} times"),
        several(count(StepKind::Ask), "asked another agent", "asked {n} questions"),
        several(count(StepKind::Other), "used a tool", "used {n} tools"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let text = if parts.is_empty() { "Thought it through".to_string() } else { and_list(&parts) };
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// The notice an agent's turn ends with ("Done in 3.2 s …", "Stopped after …").
fn is_turn_end(text: &str) -> bool {
    text.starts_with("Done in ") || text.starts_with("Stopped after ")
}

/// The files a run of items edited (from the edits' locations and diffs).
fn edited_files(items: &[Item]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Tool { kind, title, detail, .. } if step_kind(kind, title) == StepKind::Edit => Some(
                detail
                    .locations
                    .iter()
                    .map(|l| l.rsplit_once(':').filter(|(_, n)| n.chars().all(|c| c.is_ascii_digit())).map_or(l.clone(), |(p, _)| p.to_string()))
                    .chain(detail.content.iter().filter_map(|c| match c {
                        ToolContent::Diff { path, .. } => Some(path.clone()),
                        _ => None,
                    }))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect()
}

impl Monaged {
    /// Whether an agent's steps fold into one line (Settings → General).
    pub(crate) fn group_steps(&self) -> bool {
        self.store.setting("ui:group_steps").is_none_or(|v| v != "0")
    }

    /// The conversation's rows: steps folded, turn ends as footers, repeated
    /// notices once. A row containing a search match is marked (and a folded
    /// run with one opens).
    pub(crate) fn transcript_rows(&self, agent: &AgentView, reading: f32, matches: &[usize], cx: &mut Context<Self>) -> Vec<AnyElement> {
        let id = agent.record.id;
        let items = &agent.items;
        let grouped = self.group_steps();
        let wrap = |el: AnyElement, mark: Option<bool>| -> AnyElement {
            let row = div().w(px(reading)).flex_none().child(el);
            match mark {
                Some(current) => row
                    .rounded(px(6.))
                    .bg(rgb(if current { theme::selected() } else { theme::hover() }))
                    .when(current, |d| d.border_1().border_color(rgb(theme::text_3())))
                    .into_any_element(),
                None => row.into_any_element(),
            }
        };
        let mut rows = Vec::new();
        let mut turn_start = 0;
        let mut i = 0;
        while i < items.len() {
            let item = &items[i];
            if matches!(item, Item::User(_)) {
                turn_start = i + 1;
            }
            if grouped && matches!(item, Item::Tool { .. } | Item::Thought(_)) {
                let start = i;
                while i < items.len() && matches!(items[i], Item::Tool { .. } | Item::Thought(_)) {
                    i += 1;
                }
                let mark = (start..i).filter_map(|j| self.find_mark(id, j, matches)).max();
                let el = if i - start == 1 && matches!(items[start], Item::Tool { .. }) {
                    self.render_item(id, start, &items[start], cx)
                } else {
                    let live = agent.is_working() && i == items.len();
                    self.render_steps(agent, start..i, live, mark.is_some(), cx)
                };
                rows.push(wrap(el, mark));
                continue;
            }
            // "Reopened the saved conversation." after every restart: once is enough.
            if let Item::Notice(_, text) = item
                && matches!(items.get(i + 1), Some(Item::Notice(_, next)) if next == text)
            {
                i += 1;
                continue;
            }
            let el = match item {
                Item::Notice(tone, text) if is_turn_end(text) => self.render_turn_footer(agent, turn_start..i, *tone, text, cx),
                _ => self.render_item(id, i, item, cx),
            };
            rows.push(wrap(el, self.find_mark(id, i, matches)));
            i += 1;
        }
        rows
    }

    /// A run of steps as one line: what they did, how many, whether one
    /// failed; a spinner while the last one runs. Click to see each step.
    fn render_steps(&self, agent: &AgentView, range: Range<usize>, live: bool, forced_open: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let start = range.start;
        let items = &agent.items[range.clone()];
        let steps: Vec<(StepKind, Vec<String>)> = items
            .iter()
            .map(|item| match item {
                Item::Tool { kind, title, detail, .. } => (step_kind(kind, title), detail.locations.clone()),
                _ => (StepKind::Think, Vec::new()),
            })
            .collect();
        let tools = items.iter().filter(|i| matches!(i, Item::Tool { .. })).count();
        let running = live && items.iter().any(|i| matches!(i, Item::Tool { status, .. } if status == "in_progress" || status == "pending"));
        let failed = items.iter().filter(|i| matches!(i, Item::Tool { status, .. } if status == "failed")).count();
        let open = forced_open || self.expanded_steps.contains(&(id, start));
        // Thinking on its own reads best as its first words.
        let label = if tools == 0 {
            let thought: String = items.iter().filter_map(|i| if let Item::Thought(t) = i { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join(" ");
            format!("Thought · {}", thought.split_whitespace().collect::<Vec<_>>().join(" "))
        } else {
            steps_summary(&steps)
        };
        // The live line under the conversation has the spinner; this one just
        // brightens while its steps run.
        let lead_icon = icon_sized(if tools == 0 { Icon::Question } else { Icon::Lines }, 13.).text_color(rgb(if running { theme::text() } else { theme::text_3() }));
        let header = div()
            .id(("steps", id as usize * 100_000 + start))
            .flex()
            .items_center()
            .gap(px(9.))
            .h(px(28.))
            .px(px(8.))
            .mx(px(-8.))
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(theme::hover())))
            .tooltip(crate::shell::tip(if open { "Fold these steps" } else { "Show each step" }))
            .child(div().flex_none().size(px(14.)).flex().items_center().justify_center().child(lead_icon))
            .child(
                ui::one_line(div().flex_1())
                    .text_size(px(13.))
                    .text_color(rgb(if tools == 0 { theme::text_3() } else { theme::text_2() }))
                    .when(tools == 0, |d| d.italic())
                    .child(label),
            )
            .when(failed > 0, |d| {
                d.child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_size(px(11.5))
                        .text_color(rgb(theme::red_text()))
                        .child(icon_sized(Icon::Alert, 12.).text_color(rgb(theme::red_text())))
                        .child(format!("{failed} failed")),
                )
            })
            .when(tools > 1, |d| d.child(div().flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_3())).child(format!("{tools} steps"))))
            .child(div().flex_none().child(ui::disclosure(open)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded_steps.remove(&(id, start)) {
                    this.expanded_steps.insert((id, start));
                }
                cx.notify();
            }));
        if !open {
            return header.into_any_element();
        }
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .ml(px(6.))
            .pl(px(16.))
            .py(px(2.))
            .border_l_1()
            .border_color(rgb(theme::line_strong()))
            .children(items.iter().enumerate().map(|(k, item)| self.render_item(id, start + k, item, cx)));
        div().flex().flex_col().gap(px(6.)).child(header).child(body).into_any_element()
    }

    /// The end of a turn: how long it took, how many steps, what it changed
    /// (opens the Changes tab), and copying what the agent said.
    fn render_turn_footer(&self, agent: &AgentView, range: Range<usize>, tone: Tone, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let id = agent.record.id;
        let items = &agent.items[range.start.min(agent.items.len())..range.end.min(agent.items.len())];
        // "Done in 10.1 s · first words after 0.4 s": the first part is what matters.
        let head = text.split(" · ").next().unwrap_or(text).to_string();
        let steps = items.iter().filter(|i| matches!(i, Item::Tool { .. })).count();
        let edited = edited_files(items);
        let reply: String = items.iter().filter_map(|i| if let Item::Agent(t) = i { Some(t.trim()) } else { None }).collect::<Vec<_>>().join("\n\n");
        let stopped = !matches!(tone, Tone::Info);
        let dot = || div().text_color(rgb(theme::text_4())).child("·");
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(24.))
            .text_size(px(12.))
            .text_color(rgb(theme::text_3()))
            .group("turn-footer")
            .child(icon_sized(if stopped { Icon::Stop } else { Icon::Check }, 12.).flex_none().text_color(rgb(if stopped { theme::red_text() } else { theme::text_3() })))
            .child(div().flex_none().when(stopped, |d| d.text_color(rgb(theme::red_text()))).child(head))
            .when(steps > 0, |d| d.child(dot()).child(div().flex_none().child(format!("{steps} step{}", if steps == 1 { "" } else { "s" }))))
            .when(!edited.is_empty(), |d| {
                let n = edited.len();
                let names = edited.iter().map(|p| file_name(p)).collect::<Vec<_>>().join(", ");
                d.child(dot()).child(
                    div()
                        .id(("turn-changes", id as usize * 100_000 + range.end))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .text_color(rgb(theme::text_2()))
                        .cursor_pointer()
                        .hover(|s| s.text_color(rgb(theme::text())))
                        .tooltip(crate::shell::tip_text(format!("Changed {names}. Open the agent's changes")))
                        .child(icon_sized(Icon::Diff, 12.))
                        .child(format!("{n} file{} changed", if n == 1 { "" } else { "s" }))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_tab(id, AgentTab::Changes, cx))),
                )
            })
            .child(div().flex_1())
            .when(!reply.is_empty(), |d| {
                d.child(
                    div()
                        .id(("turn-copy", id as usize * 100_000 + range.end))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .px(px(6.))
                        .h(px(22.))
                        .rounded(px(5.))
                        .invisible()
                        .group_hover("turn-footer", |s| s.visible())
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
                        .child(icon_sized(Icon::Copy, 12.))
                        .child("Copy reply")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(crate::export::mask_all(&reply)));
                            this.toast(Tone::Info, "Copied the reply", cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// While a turn runs: the step that's running (or writing, thinking),
    /// how many steps so far, the time, and Stop.
    pub(crate) fn render_working_line(&self, agent: &AgentView, cx: &mut Context<Self>) -> Option<AnyElement> {
        let turn = agent.turn.as_ref()?;
        let id = agent.record.id;
        let secs = turn.started.elapsed().as_secs();
        let elapsed = if secs >= 60 { format!("{}:{:02}", secs / 60, secs % 60) } else { format!("{secs}s") };
        let waiting = agent.items.iter().any(|i| matches!(i, Item::Permission { chosen: None, .. })) || agent.open_form();
        let turn_from = agent.items.iter().rposition(|i| matches!(i, Item::User(_))).map_or(0, |p| p + 1);
        let steps = agent.items[turn_from..].iter().filter(|i| matches!(i, Item::Tool { .. })).count();
        let (glyph, activity): (Option<Icon>, String) = match agent.items.last() {
            _ if waiting => (None, "Waiting for your answer above".into()),
            _ if agent.compacting => (None, "Compacting the conversation".into()),
            Some(Item::Tool { status, title, kind, .. }) if status == "in_progress" || status == "pending" => {
                let shown = title.strip_prefix("mcp__monaged__").map(|t| format!("monaged · {}", t.replace('_', " "))).unwrap_or_else(|| title.clone());
                (Some(step_icon(step_kind(kind, title))), shown)
            }
            Some(Item::Agent(_)) => (None, "Writing".into()),
            Some(Item::Plan(_)) => (None, "Planning".into()),
            _ => (None, "Thinking".into()),
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .h(px(28.))
                .text_size(px(12.5))
                .text_color(rgb(if waiting { theme::red_text() } else { theme::text_2() }))
                .child(if waiting {
                    ui::status_dot(("working-dot", id as usize), State::NeedsYou)
                } else {
                    ui::spinner(("working-spin", id as usize), 13., theme::text_2())
                })
                .children(glyph.map(|g| icon_sized(g, 13.).flex_none().text_color(rgb(theme::text_3()))))
                .child(ui::one_line(div().flex_shrink()).when(glyph.is_some(), |d| d.font_family(theme::MONO).text_size(px(12.))).child(activity))
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::MONO)
                        .text_size(px(11.5))
                        .text_color(rgb(theme::text_3()))
                        .child(if steps > 0 { format!("step {steps} · {elapsed}") } else { elapsed }),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id(("working-stop", id as usize))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(6.))
                        .h(px(24.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .text_color(rgb(theme::text_3()))
                        .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
                        .child(icon_sized(Icon::Stop, 12.))
                        .child("Stop")
                        .child(ui::kbd("Esc"))
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(id, cx))),
                )
                .into_any_element(),
        )
    }
}
