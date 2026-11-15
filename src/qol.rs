//! Small things that save time or trouble: keeping the PC awake while
//! agents work, asking before closing on working agents, a summary of what
//! happened while you were away, a key for the next thing that needs you,
//! names for your agents from what you asked them, copying a conversation,
//! a terminal in a worktree, and folding pipeline agents whose part is done.

use std::time::{Duration, Instant};

use gpui::{AnyElement, ClipboardItem, Context, Window, div, prelude::*, px, rgb};

use crate::agent::{AgentView, Item, Tone};
use crate::icons::{Icon, icon};
use crate::pipeline::{Role, Stage};
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

/// Away at least this long (Monaged not the active window) before coming
/// back shows what happened.
const AWAY: Duration = Duration::from_secs(10 * 60);

// ---- Agent names -------------------------------------------------------------------

/// Words that say nothing about the work.
const FILLER: &[&str] = &[
    "a", "an", "the", "to", "and", "or", "of", "in", "on", "for", "with", "at", "by", "from", "into", "please", "pls", "can", "could", "would", "will",
    "you", "your", "i", "me", "my", "we", "our", "us", "it", "its", "this", "that", "these", "those", "is", "are", "be", "been", "was", "just", "let", "lets",
    "make", "sure", "so", "then", "also", "some", "all", "any", "hey", "hi", "hello", "now", "there", "here", "what", "how", "why", "when", "where", "do",
    "does", "did", "need", "want", "like", "should", "have", "has", "had", "get", "go", "look", "see", "try", "able", "ok", "okay", "yes", "no", "not",
    "new", "but", "if", "as", "up", "out", "about", "very", "really", "thing", "things", "stuff", "using", "use",
];

/// "Please fix the failing coupon test in tests/test_orders.py" → "fix-failing-coupon".
pub fn name_from_message(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .lines()
        .next()?
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 1 && !FILLER.contains(&w.as_str()) && !w.chars().all(|c| c.is_ascii_digit()))
        .take(3)
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut name = String::new();
    for word in words {
        let next = if name.is_empty() { word } else { format!("{name}-{word}") };
        if next.chars().count() > 28 {
            break;
        }
        name = next;
    }
    (!name.is_empty()).then_some(name)
}

/// Monaged's own default name for an agent you start ("agent-3").
pub fn is_default_name(name: &str) -> bool {
    name.strip_prefix("agent-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

// ---- Answering a question in one click ------------------------------------------------

/// What you'd type most often after "Shall I…?".
pub const QUICK_REPLIES: [&str; 2] = ["Yes, go ahead", "No, that's all for now"];

/// The agent's last reply ends on a question to you (not a question card,
/// which has its own buttons).
pub fn ends_on_question(agent: &AgentView) -> bool {
    let Some(last) = agent.items.iter().rposition(|i| matches!(i, Item::Agent(_) | Item::User(_))) else { return false };
    let Item::Agent(text) = &agent.items[last] else { return false };
    question_at_end(text)
}

pub fn question_at_end(text: &str) -> bool {
    if text.contains("```monaged-") {
        return false;
    }
    // The last line with words, without Markdown emphasis.
    let line = text.lines().rev().map(str::trim).find(|l| l.chars().any(char::is_alphanumeric)).unwrap_or_default();
    line.trim_end_matches(['*', '_', ')', ' ']).ends_with('?')
}

// ---- Pipeline agents whose part is done ----------------------------------------------

/// Research, verification and planning are over once the pipeline is past
/// them; their agents can fold away in the sidebar unless something's up.
pub fn part_done(role: &Role, stage: Stage) -> bool {
    let order = |s: Stage| crate::pipeline::STAGES.iter().position(|x| *x == s);
    let (Some(now), Some(after)) = (
        order(stage),
        order(match role {
            Role::Researcher => Stage::Research,
            Role::Verifier(_) => Stage::Decide,
            // The planner re-plans when you reply to it during Approve.
            Role::Planner => Stage::Approve,
            _ => return false,
        }),
    ) else {
        return false;
    };
    now > after
}

impl Monaged {
    // ---- Keeping the PC awake ----------------------------------------------------------

    pub(crate) fn keep_awake_on(&self) -> bool {
        self.store.setting("power:awake").is_none_or(|v| v != "0")
    }

    /// While an agent works or a review runs, Windows doesn't go to sleep
    /// (the screen still may): sleep would suspend WSL mid-task.
    pub(crate) fn sync_awake(&mut self) {
        let busy = self.keep_awake_on() && self.agents.iter().any(|a| a.is_working() || a.reviewing);
        if busy != self.awake {
            self.awake = busy;
            crate::native::keep_awake(busy);
        }
    }

    // ---- Closing ----------------------------------------------------------------------

    /// What closing Monaged would stop, in words; None when nothing is running.
    pub(crate) fn running_work(&self) -> Option<String> {
        let working = self.agents.iter().filter(|a| a.is_working()).count();
        let reviews = self.agents.iter().filter(|a| a.reviewing).count();
        let mut parts = Vec::new();
        if working > 0 {
            parts.push(format!("{working} agent{} working", if working == 1 { " is" } else { "s are" }));
        }
        if reviews > 0 {
            parts.push(format!("{reviews} review{} running", if reviews == 1 { " is" } else { "s are" }));
        }
        (!parts.is_empty()).then(|| parts.join(" and "))
    }

    /// The window's close button: asks first when work is running.
    pub(crate) fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.closing || self.running_work().is_none() || self.store.setting("ui:ask_close").is_some_and(|v| v == "0") {
            return true;
        }
        self.ask_confirm(crate::feedback::ConfirmAction::Quit, window, cx);
        false
    }

    pub(crate) fn quit_confirmed(&mut self, window: &mut Window) {
        self.closing = true;
        window.remove_window();
    }

    // ---- While you were away -------------------------------------------------------------

    /// The window became active or inactive.
    pub(crate) fn on_activation(&mut self, active: bool) {
        if !active {
            self.away_since = Some((Instant::now(), now_secs()));
            return;
        }
        let Some((left, at)) = self.away_since.take() else { return };
        // MONAGED_AWAY_SECONDS shortens the wait (for trying it out).
        let away = std::env::var("MONAGED_AWAY_SECONDS").ok().and_then(|s| s.parse().ok()).map(Duration::from_secs).unwrap_or(AWAY);
        if left.elapsed() < away || self.store.setting("ui:away_summary").is_some_and(|v| v == "0") {
            return;
        }
        self.away = self.away_summary(at, left.elapsed());
    }

    /// What happened since `since` (seconds), in a few lines; None when nothing did.
    fn away_summary(&self, since: i64, gone: Duration) -> Option<Away> {
        let mut agents_worked = std::collections::BTreeSet::new();
        let (mut cost, mut lines) = (0.0f64, Vec::new());
        for sub in &self.subprojects {
            let events = self.store.timeline(sub.id, 400).unwrap_or_default();
            for e in events.iter().filter(|e| e.at >= since) {
                match e.kind.as_str() {
                    "turn" => {
                        if let Some(id) = e.agent_id {
                            agents_worked.insert(id);
                        }
                        cost += e.data.split('|').nth(1).and_then(|c| c.parse::<f64>().ok()).unwrap_or(0.);
                    }
                    "run" => lines.push((Icon::Flow, format!("{}: the pipeline finished", sub.name), false)),
                    "event:merge" => lines.push((Icon::Merge, e.data.clone(), false)),
                    "event:conflict" | "event:failed" => lines.push((Icon::Alert, format!("{}: {}", sub.name, e.data), true)),
                    "event:gate" if e.data.contains("waits for you") => lines.push((Icon::Shield, format!("{}: {}", sub.name, e.data), true)),
                    "review" => {
                        let score = e.data.split('|').next().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
                        let name = e.agent_id.map(|id| self.agent_name(id)).unwrap_or_default();
                        lines.push((Icon::Eye, format!("{name} was reviewed: {score}/5"), score < self.min_score()));
                    }
                    _ => {}
                }
            }
        }
        let waiting = self.inbox().len();
        if agents_worked.is_empty() && lines.is_empty() && waiting == 0 {
            return None;
        }
        lines.truncate(6);
        let minutes = gone.as_secs() / 60;
        let gone = match minutes {
            120.. => format!("{} hours", minutes / 60),
            60.. => "an hour".into(),
            2.. => format!("{minutes} minutes"),
            _ => "a minute".into(),
        };
        let mut head = Vec::new();
        if !agents_worked.is_empty() {
            head.push(format!("{} agent{} worked", agents_worked.len(), if agents_worked.len() == 1 { "" } else { "s" }));
        }
        if cost >= 0.01 {
            head.push(format!("${cost:.2}"));
        }
        if waiting > 0 {
            head.push(format!("{waiting} thing{} wait{} for you", if waiting == 1 { "" } else { "s" }, if waiting == 1 { "s" } else { "" }));
        }
        Some(Away { title: format!("While you were away ({gone})"), head: head.join(" · "), lines, waiting })
    }

    pub(crate) fn render_away(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let away = self.away.as_ref()?;
        let waiting = away.waiting;
        Some(
            div()
                .absolute()
                .top(px(48.))
                .right(px(16.))
                .w(px(380.))
                .occlude()
                .child(
                    ui::card()
                        .shadow_lg()
                        .border_color(rgb(theme::line_strong()))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(icon(Icon::Clock).size(px(14.)).text_color(rgb(theme::text_2())))
                                .child(div().flex_1().text_size(px(13.5)).font_weight(gpui::FontWeight::MEDIUM).child(away.title.clone()))
                                .child(ui::icon_button("away-close", Icon::X).on_click(cx.listener(|this, _, _, cx| {
                                    this.away = None;
                                    cx.notify();
                                }))),
                        )
                        .when(!away.head.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(rgb(theme::text_2())).child(away.head.clone())))
                        .children(away.lines.iter().map(|(glyph, text, attention)| {
                            let color = if *attention { theme::red_text() } else { theme::text_2() };
                            div()
                                .flex()
                                .items_start()
                                .gap(px(8.))
                                .text_size(px(12.5))
                                .child(div().flex_none().pt(px(2.)).child(icon(*glyph).size(px(13.)).text_color(rgb(color))))
                                .child(div().w(px(318.)).text_color(rgb(color)).child(text.clone()))
                        }))
                        .when(waiting > 0, |d| {
                            d.child(div().flex().child(
                                ui::button("away-next", Kind::Primary, None, "Show what needs you").on_click(cx.listener(|this, _, _, cx| {
                                    this.away = None;
                                    this.next_needs_you(cx);
                                })),
                            ))
                        }),
                )
                .into_any_element(),
        )
    }

    // ---- The next thing that needs you ------------------------------------------------------

    /// Opens the dashboard of the next subproject that waits for you (after
    /// the one you're in), where it can be answered.
    pub(crate) fn next_needs_you(&mut self, cx: &mut Context<Self>) {
        let mut subs: Vec<i64> = Vec::new();
        for item in self.inbox() {
            if !subs.contains(&item.sub_id) {
                subs.push(item.sub_id);
            }
        }
        if subs.is_empty() {
            self.toast(Tone::Info, "Nothing waits for you.", cx);
            return;
        }
        let here = self.sel_sub.filter(|_| self.on_dashboard()).and_then(|s| subs.iter().position(|x| *x == s));
        let next = match here {
            Some(at) => subs[(at + 1) % subs.len()],
            None => subs[0],
        };
        self.form = None;
        self.select_sub(next, cx);
        if subs.len() > 1 {
            let key = crate::keys::label("next-needs-you");
            self.toast(Tone::Info, format!("{} subprojects wait for you; {key} goes to the next.", subs.len()), cx);
        }
        cx.notify();
    }

    /// The status bar's "N working": the next agent that's working.
    pub(crate) fn next_working_agent(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<i64> = self.agents.iter().filter(|a| a.is_working()).map(|a| a.record.id).collect();
        let Some(&first) = ids.first() else { return };
        let next = self.sel_agent.and_then(|cur| ids.iter().position(|&i| i == cur)).map_or(first, |at| ids[(at + 1) % ids.len()]);
        self.form = None;
        self.open_agent(next, cx);
        cx.notify();
    }

    // ---- Names ----------------------------------------------------------------------------

    /// Your first message to an agent you started names it (it was "agent-3").
    pub(crate) fn name_from_first_message(&mut self, agent_id: i64, text: &str) {
        let Some(agent) = self.agent(agent_id) else { return };
        let first = !agent.items.iter().any(|i| matches!(i, Item::User(_)));
        if agent.role() != Role::Free || !is_default_name(&agent.record.name) || !first {
            return;
        }
        let Some(base) = name_from_message(text) else { return };
        let sub_id = agent.record.subproject_id;
        let taken = |n: &str| self.agents.iter().any(|a| a.record.subproject_id == sub_id && a.record.name == n);
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        if self.store.rename_agent(agent_id, &name).is_ok()
            && let Some(agent) = self.agent_mut(agent_id)
        {
            agent.record.name = name;
        }
    }

    // ---- Copying a conversation --------------------------------------------------------------

    pub(crate) fn copy_conversation(&mut self, agent_id: i64, cx: &mut Context<Self>) {
        let Some(agent) = self.agent(agent_id) else { return };
        if agent.items.is_empty() {
            self.toast(Tone::Info, "Its conversation isn't open in this run yet: open the agent first.", cx);
            return;
        }
        let text = crate::export::mask_all(&format!(
            "# {} ({})\n\n{}\n",
            agent.record.name,
            crate::shell::role_label(&agent.role()),
            crate::export::transcript_from_items(&agent.items, true)
        ));
        let count = agent.items.len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.toast(Tone::Info, format!("Copied the conversation ({count} items, as Markdown, secrets masked)."), cx);
    }

    // ---- A terminal --------------------------------------------------------------------------

    pub(crate) fn open_terminal(&mut self, path: &str, cx: &mut Context<Self>) {
        match crate::wsl::open_terminal(&self.config.distro, path) {
            Ok(()) => self.toast(Tone::Info, format!("Opening a terminal in {}", crate::last_segment(path)), cx),
            Err(err) => self.toast_error(format!("Couldn't open a terminal: {err:#}"), cx),
        }
    }

    // ---- The sidebar ---------------------------------------------------------------------------

    /// A subproject's agents for the sidebar: the ones to show, and the
    /// pipeline agents whose part is done (folded unless you open them).
    pub(crate) fn sidebar_agents(&self, sub_id: i64) -> (Vec<&AgentView>, Vec<&AgentView>) {
        let stage = self.pipelines.get(&sub_id).map(|p| p.stage).filter(|s| *s != Stage::Stopped);
        let (mut shown, mut folded) = (Vec::new(), Vec::new());
        for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub_id && !a.record.archived) {
            let quiet = matches!(crate::shell::agent_state(agent), crate::ui::State::Done | crate::ui::State::Idle);
            let done = stage.is_some_and(|s| part_done(&agent.role(), s));
            if done && quiet && self.sel_agent != Some(agent.record.id) {
                folded.push(agent);
            } else {
                shown.push(agent);
            }
        }
        (shown, folded)
    }
}

/// What the "while you were away" card says.
pub struct Away {
    pub title: String,
    pub head: String,
    pub lines: Vec<(Icon, String, bool)>,
    pub waiting: usize,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_named_after_what_you_asked() {
        assert_eq!(name_from_message("Please fix the failing coupon test in tests/test_orders.py").as_deref(), Some("fix-failing-coupon"));
        assert_eq!(name_from_message("Can you add dark mode to the settings page?").as_deref(), Some("add-dark-mode"));
        assert_eq!(name_from_message("Refactor authentication middleware").as_deref(), Some("refactor-authentication"));
        assert_eq!(name_from_message("hi").as_deref(), None);
        assert_eq!(name_from_message("").as_deref(), None);
        assert_eq!(name_from_message("2024 2025").as_deref(), None);
        // Only the first line counts.
        assert_eq!(name_from_message("Update README\n\nwith lots of detail").as_deref(), Some("update-readme"));
        assert!(is_default_name("agent-3") && is_default_name("agent-12"));
        assert!(!is_default_name("agent-") && !is_default_name("agent-x") && !is_default_name("fix-bug"));
    }

    #[test]
    fn a_closing_question_gets_quick_replies() {
        assert!(question_at_end("Fixed it.\n\nWant me to look at the flaky test next?"));
        assert!(question_at_end("Done. **Shall I push it?**"));
        assert!(!question_at_end("Done. Tests pass."));
        assert!(!question_at_end("Is this right? I checked it, and it is."));
        assert!(!question_at_end("Which one?\n```monaged-question\n{}\n```"));
    }

    #[test]
    fn pipeline_agents_fold_once_their_part_is_done() {
        assert!(!part_done(&Role::Researcher, Stage::Research));
        assert!(part_done(&Role::Researcher, Stage::Verify));
        assert!(!part_done(&Role::Verifier(1), Stage::Decide));
        assert!(part_done(&Role::Verifier(2), Stage::Plan));
        assert!(!part_done(&Role::Planner, Stage::Approve), "you can still reply to the planner");
        assert!(part_done(&Role::Planner, Stage::Build));
        assert!(!part_done(&Role::Builder("api".into()), Stage::Done));
        assert!(!part_done(&Role::Integrator, Stage::Done));
        assert!(!part_done(&Role::Free, Stage::Done));
    }
}
