//! Finding things:
//! - Ctrl F: a find bar over the focused conversation; Enter jumps to the next
//!   match, Shift+Enter to the previous one, Esc closes it;
//! - Ctrl Shift F (or typing in Ctrl K): every agent's conversation;
//! - Ctrl K also lists files in the current worktree, opened in Zed.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, Entity, Focusable, Subscription, Window, div, prelude::*, px, rgb};

use crate::agent::{Item, Tone};
use crate::icons::{Icon, icon_sized};
use crate::text_input::TextInput;
use crate::{Monaged, theme, ui};

/// Files are listed again when the index is older than this.
const FILE_INDEX_AGE: Duration = Duration::from_secs(120);

pub(crate) struct FindState {
    pub agent_id: i64,
    pub input: Entity<TextInput>,
    /// Which match is current (index into `find_matches`).
    pub current: usize,
    _changed: Subscription,
}

/// A conversation hit for Ctrl K / Ctrl Shift F.
pub(crate) struct Hit {
    pub agent_id: i64,
    pub item: usize,
    pub snippet: String,
}

/// The searchable text of a conversation item.
pub(crate) fn item_text(item: &Item) -> Option<String> {
    match item {
        Item::User(t) | Item::Agent(t) | Item::Thought(t) => Some(t.clone()),
        Item::Notice(_, t) => Some(t.clone()),
        Item::Tool { title, .. } => Some(title.clone()),
        Item::Permission { title, .. } => Some(title.clone()),
        Item::Elicit(form) => Some(format!("{} {}", form.message, form.fields.iter().map(|f| f.description.as_str()).collect::<Vec<_>>().join(" "))),
        Item::Plan(entries) => Some(entries.iter().map(|e| e.content.as_str()).collect::<Vec<_>>().join(" ")),
        Item::Attachments(names) => Some(names.join(" ")),
    }
}

/// A short line around the first match.
fn snippet(text: &str, at: usize, len: usize) -> String {
    let start = text[..at].char_indices().rev().nth(30).map_or(0, |(i, _)| i);
    let end = text[at + len..].char_indices().nth(60).map_or(text.len(), |(i, _)| at + len + i);
    let mut out = text[start..end].replace(['\n', '\r'], " ");
    if start > 0 {
        out = format!("…{out}");
    }
    if end < text.len() {
        out.push('…');
    }
    out
}

impl Monaged {
    // ---- Find in a conversation ---------------------------------------------------------------

    pub(crate) fn on_find(&mut self, _: &crate::FindInConversation, window: &mut Window, cx: &mut Context<Self>) {
        let Some(agent_id) = self.sel_agent else {
            self.toast(Tone::Info, "Open an agent first; Ctrl Shift F searches every conversation.", cx);
            return;
        };
        self.open_find(agent_id, String::new(), window, cx);
    }

    pub(crate) fn open_find(&mut self, agent_id: i64, query: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &self.find
            && find.agent_id == agent_id
            && query.is_empty()
        {
            window.focus(&find.input.focus_handle(cx));
            return;
        }
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Find in this conversation", cx).bare();
            input.set_text(query, cx);
            input
        });
        let changed = cx.observe(&input, |this, _, cx| {
            if let Some(find) = &mut this.find {
                find.current = 0;
            }
            this.jump_to_match(cx);
            cx.notify();
        });
        window.focus(&input.focus_handle(cx));
        self.find = Some(FindState { agent_id, input, current: 0, _changed: changed });
        self.jump_to_match(cx);
        cx.notify();
    }

    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find.take() {
            self.focus_request = Some(find.agent_id);
        }
        let _ = window;
        cx.notify();
    }

    /// Indexes of the conversation items that contain the query.
    pub(crate) fn find_matches(&self, cx: &gpui::App) -> Vec<usize> {
        let Some(find) = &self.find else { return Vec::new() };
        let query = find.input.read(cx).text().trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let Some(agent) = self.agent(find.agent_id) else { return Vec::new() };
        agent
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item_text(item).is_some_and(|t| t.to_lowercase().contains(&query)))
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.find_matches(cx).len();
        if count == 0 {
            return;
        }
        if let Some(find) = &mut self.find {
            find.current = (find.current as isize + delta).rem_euclid(count as isize) as usize;
        }
        self.jump_to_match(cx);
        cx.notify();
    }

    fn jump_to_match(&mut self, cx: &mut Context<Self>) {
        let matches = self.find_matches(cx);
        let Some(find) = &self.find else { return };
        let Some(&item) = matches.get(find.current.min(matches.len().saturating_sub(1))) else { return };
        if let Some(handle) = self.scrolls.get(&find.agent_id) {
            handle.scroll_to_top_of_item(item);
        }
    }

    /// The find bar over a conversation.
    pub(crate) fn render_find_bar(&self, agent_id: i64, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref().filter(|f| f.agent_id == agent_id)?;
        let matches = self.find_matches(cx);
        let has_query = !find.input.read(cx).text().trim().is_empty();
        let count = match (has_query, matches.len()) {
            (false, _) => String::new(),
            (true, 0) => "no matches".into(),
            (true, n) => format!("{} of {n}", find.current.min(n - 1) + 1),
        };
        Some(
            div()
                .flex_none()
                .h(px(40.))
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(14.))
                .border_b_1()
                .border_color(rgb(theme::line()))
                .bg(rgb(theme::panel()))
                .child(icon_sized(Icon::Search, 14.).text_color(rgb(theme::text_3())))
                .child(div().flex_1().min_w(px(0.)).child(find.input.clone()))
                .child(div().flex_none().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(if has_query && matches.is_empty() { theme::red_text() } else { theme::text_3() })).child(count))
                .child(ui::icon_button("find-prev", Icon::ArrowUp).tooltip(crate::shell::tip("Previous match")).on_click(cx.listener(|this, _, _, cx| this.find_step(-1, cx))))
                .child(ui::icon_button("find-next", Icon::ArrowDown).tooltip(crate::shell::tip_key("Next match", "Enter")).on_click(cx.listener(|this, _, _, cx| this.find_step(1, cx))))
                .child(ui::icon_button("find-close", Icon::X).tooltip(crate::shell::tip_key("Close", "Esc")).on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))))
                .into_any_element(),
        )
    }

    /// Is this item a match, and the current one?
    pub(crate) fn find_mark(&self, agent_id: i64, item: usize, matches: &[usize]) -> Option<bool> {
        let find = self.find.as_ref().filter(|f| f.agent_id == agent_id)?;
        let at = matches.iter().position(|&m| m == item)?;
        Some(at == find.current.min(matches.len().saturating_sub(1)))
    }

    // ---- Across conversations -----------------------------------------------------------------

    pub(crate) fn search_conversations(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query = query.trim().to_lowercase();
        if query.chars().count() < 3 {
            return Vec::new();
        }
        let mut hits = Vec::new();
        // Newest agents first, newest messages first within each.
        let mut agents: Vec<_> = self.agents.iter().collect();
        agents.sort_by_key(|a| std::cmp::Reverse(a.record.id));
        'outer: for agent in agents {
            for (i, item) in agent.items.iter().enumerate().rev() {
                let Some(text) = item_text(item) else { continue };
                let lower = text.to_lowercase();
                // Lowercasing can shift byte offsets for some letters; use the original only when lengths agree.
                if let Some(at) = lower.find(&query) {
                    let snip = if lower.len() == text.len() { snippet(&text, at, query.len()) } else { snippet(&lower, at, query.len()) };
                    hits.push(Hit { agent_id: agent.record.id, item: i, snippet: snip });
                    if hits.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        hits
    }

    pub(crate) fn on_search_agents(&mut self, _: &crate::SearchAgents, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette_with(crate::palette::PaletteMode::Conversations, window, cx);
    }

    /// Opens an agent at a conversation item, with the find bar on the query.
    pub(crate) fn open_hit(&mut self, agent_id: i64, item: usize, query: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent(agent_id, cx);
        self.open_find(agent_id, query, window, cx);
        let matches = self.find_matches(cx);
        if let Some(find) = &mut self.find
            && let Some(at) = matches.iter().position(|&m| m == item)
        {
            find.current = at;
        }
        self.jump_to_match(cx);
    }

    // ---- Files -------------------------------------------------------------------------------

    /// Where Ctrl K lists files from: the focused agent's worktree, else the project.
    pub(crate) fn file_root(&self) -> Option<String> {
        if let Some(agent) = self.sel_agent.and_then(|id| self.agent(id)).filter(|a| !a.record.archived) {
            return Some(agent.record.worktree.clone());
        }
        self.sel_project.and_then(|id| self.projects.iter().find(|p| p.id == id)).map(|p| p.path.clone())
    }

    /// Lists the root's files in the background (git ls-files), if the list is old.
    pub(crate) fn refresh_file_index(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.file_root() else { return };
        if self.file_index.get(&root).is_some_and(|(at, _)| at.elapsed() < FILE_INDEX_AGE) || self.files_loading.contains(&root) {
            return;
        }
        self.files_loading.insert(root.clone());
        let distro = self.config.distro.clone();
        cx.spawn(async move |view, cx| {
            let r = root.clone();
            let listed = cx
                .background_spawn(async move {
                    crate::wsl::run(&distro, &format!("git -C {} ls-files --cached --others --exclude-standard 2>/dev/null | head -n 60000", crate::wsl::shell_path(&r)))
                })
                .await;
            view.update(cx, |this, cx| {
                this.files_loading.remove(&root);
                let files: Vec<String> = listed.map(|out| out.lines().map(str::to_owned).collect()).unwrap_or_default();
                this.file_index.insert(root, (Instant::now(), files));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Files whose path matches; the file name counts most.
    pub(crate) fn search_files(&self, query: &str, limit: usize) -> Vec<(String, String)> {
        let query = query.trim().to_lowercase();
        if query.chars().count() < 2 {
            return Vec::new();
        }
        let Some(root) = self.file_root() else { return Vec::new() };
        let Some((_, files)) = self.file_index.get(&root) else { return Vec::new() };
        let mut scored: Vec<(i32, &String)> = files
            .iter()
            .filter_map(|path| {
                let lower = path.to_lowercase();
                let name = lower.rsplit('/').next().unwrap_or(&lower);
                let score = if name.starts_with(&query) {
                    300 - name.len() as i32
                } else if name.contains(&query) {
                    200 - name.len() as i32
                } else if lower.contains(&query) {
                    100 - lower.len().min(90) as i32
                } else {
                    return None;
                };
                Some((score, path))
            })
            .collect();
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        scored.into_iter().take(limit).map(|(_, path)| (path.clone(), format!("{}/{}", root.trim_end_matches('/'), path))).collect()
    }
}
