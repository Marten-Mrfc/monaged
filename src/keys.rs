//! Keyboard shortcuts you can change (Settings → Keyboard), and moving
//! through the sidebar without a mouse: the tree key (Ctrl 0) puts a cursor
//! in the sidebar; ↑ ↓ move, → opens a level, ← goes back up, Enter opens the
//! row and Esc leaves.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{AnyElement, App, Context, KeyBinding, KeyDownEvent, Keystroke, Window, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::store::Store;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

pub(crate) struct Shortcut {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub default: &'static str,
}

/// Every shortcut you can change. Keys are GPUI keystrokes ("ctrl-shift-f").
pub(crate) const SHORTCUTS: &[Shortcut] = &[
    Shortcut { id: "palette", label: "Search and run a command", group: "Move around", default: "ctrl-k" },
    Shortcut { id: "prev-agent", label: "Previous agent", group: "Move around", default: "alt-up" },
    Shortcut { id: "next-agent", label: "Next agent", group: "Move around", default: "alt-down" },
    Shortcut { id: "next-needs-you", label: "The next thing that needs you", group: "Move around", default: "f8" },
    Shortcut { id: "tree", label: "Move through the sidebar", group: "Move around", default: "ctrl-0" },
    Shortcut { id: "find", label: "Find in this conversation", group: "Move around", default: "ctrl-f" },
    Shortcut { id: "search", label: "Search all agents", group: "Move around", default: "ctrl-shift-f" },
    Shortcut { id: "sidebar", label: "Sidebar", group: "Layout", default: "ctrl-b" },
    Shortcut { id: "inspector", label: "Dock (show or hide)", group: "Layout", default: "ctrl-alt-b" },
    Shortcut { id: "zen", label: "Zen mode", group: "Layout", default: "f11" },
    Shortcut { id: "dock-pipeline", label: "Dock: Pipeline", group: "Layout", default: "alt-1" },
    Shortcut { id: "dock-merge", label: "Dock: Merge", group: "Layout", default: "alt-2" },
    Shortcut { id: "dock-git", label: "Dock: Git", group: "Layout", default: "alt-3" },
    Shortcut { id: "dock-run", label: "Dock: Run", group: "Layout", default: "alt-4" },
    Shortcut { id: "dock-activity", label: "Dock: Activity", group: "Layout", default: "alt-5" },
    Shortcut { id: "new-agent", label: "New agent here", group: "Agents", default: "ctrl-n" },
    Shortcut { id: "side-chat", label: "Side chat with Haiku", group: "Agents", default: "ctrl-j" },
    Shortcut { id: "settings", label: "Settings", group: "App", default: "ctrl-," },
    Shortcut { id: "shortcuts", label: "Keyboard shortcuts", group: "App", default: "ctrl-/" },
];

/// Keys text boxes need for themselves.
const TEXT_KEYS: &[&str] = &["ctrl-a", "ctrl-c", "ctrl-v", "ctrl-x", "ctrl-backspace", "shift-enter", "ctrl-enter"];

thread_local! {
    /// The keys each shortcut has now, as shown to you ("Ctrl K"). They're
    /// `&'static` so menus, tooltips and the palette can keep their simple
    /// types; a new string is kept only when you change a shortcut.
    static LABELS: RefCell<HashMap<&'static str, &'static str>> = RefCell::new(HashMap::new());
}

/// The keystrokes for a shortcut ("" when you turned it off).
pub(crate) fn keys_for(store: &Store, id: &str) -> String {
    match store.setting(&format!("keys:{id}")) {
        Some(v) if v == "none" => String::new(),
        Some(v) if valid(&v) => v,
        _ => SHORTCUTS.iter().find(|s| s.id == id).map(|s| s.default.to_string()).unwrap_or_default(),
    }
}

fn valid(keys: &str) -> bool {
    !keys.trim().is_empty() && keys.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// "ctrl-shift-f" → "Ctrl Shift F", "alt-up" → "Alt ↑".
pub(crate) fn pretty(keys: &str) -> String {
    keys.split_whitespace()
        .filter_map(|k| Keystroke::parse(k).ok())
        .map(|k| {
            let mut parts: Vec<String> = Vec::new();
            if k.modifiers.control {
                parts.push("Ctrl".into());
            }
            if k.modifiers.alt {
                parts.push("Alt".into());
            }
            if k.modifiers.shift {
                parts.push("Shift".into());
            }
            if k.modifiers.platform {
                parts.push("Win".into());
            }
            parts.push(match k.key.as_str() {
                "up" => "↑".into(),
                "down" => "↓".into(),
                "left" => "←".into(),
                "right" => "→".into(),
                "escape" => "Esc".into(),
                "enter" => "Enter".into(),
                "space" => "Space".into(),
                "tab" => "Tab".into(),
                "backspace" => "Backspace".into(),
                "delete" => "Delete".into(),
                "home" => "Home".into(),
                "end" => "End".into(),
                "pageup" => "Page Up".into(),
                "pagedown" => "Page Down".into(),
                key => key.to_uppercase(),
            });
            parts.join(" ")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The keys of a shortcut as shown to you ("" when it's off).
pub(crate) fn label(id: &str) -> &'static str {
    LABELS.with(|l| l.borrow().get(id).copied().unwrap_or(""))
}

/// The same, for places that show nothing when there's no key.
pub(crate) fn hint(id: &str) -> Option<&'static str> {
    Some(label(id)).filter(|l| !l.is_empty())
}

fn binding(id: &str, keys: &str) -> Option<KeyBinding> {
    use crate::*;
    if !valid(keys) {
        return None;
    }
    Some(match id {
        "palette" => KeyBinding::new(keys, TogglePalette, None),
        "prev-agent" => KeyBinding::new(keys, PrevAgent, None),
        "next-agent" => KeyBinding::new(keys, NextAgent, None),
        "tree" => KeyBinding::new(keys, FocusTree, None),
        "next-needs-you" => KeyBinding::new(keys, NextNeedsYou, None),
        "find" => KeyBinding::new(keys, FindInConversation, None),
        "search" => KeyBinding::new(keys, SearchAgents, None),
        "sidebar" => KeyBinding::new(keys, ToggleSidebar, None),
        "inspector" => KeyBinding::new(keys, ToggleInspector, None),
        "zen" => KeyBinding::new(keys, ToggleZen, None),
        "dock-pipeline" => KeyBinding::new(keys, DockPipeline, None),
        "dock-merge" => KeyBinding::new(keys, DockMerge, None),
        "dock-git" => KeyBinding::new(keys, DockGit, None),
        "dock-run" => KeyBinding::new(keys, DockRun, None),
        "dock-activity" => KeyBinding::new(keys, DockActivity, None),
        "new-agent" => KeyBinding::new(keys, NewAgentHere, None),
        "side-chat" => KeyBinding::new(keys, ToggleSideChat, None),
        "settings" => KeyBinding::new(keys, OpenSettings, None),
        "shortcuts" => KeyBinding::new(keys, ShowShortcuts, None),
        _ => return None,
    })
}

/// (Re)binds every key: text boxes, the fixed keys, then your shortcuts.
pub(crate) fn apply(cx: &mut App, store: &Store) {
    cx.clear_key_bindings();
    cx.bind_keys(crate::text_input::key_bindings());
    cx.bind_keys(crate::fixed_bindings());
    let mut list = Vec::new();
    LABELS.with(|labels| {
        let mut labels = labels.borrow_mut();
        for s in SHORTCUTS {
            let keys = keys_for(store, s.id);
            let shown = pretty(&keys);
            if labels.get(s.id).is_none_or(|l| *l != shown) {
                labels.insert(s.id, Box::leak(shown.into_boxed_str()));
            }
            list.extend(binding(s.id, &keys));
        }
    });
    cx.bind_keys(list);
}

/// A row of the sidebar the keyboard cursor can be on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TreeNode {
    Project(i64),
    Sub(i64),
    Agent(i64),
}

impl Monaged {
    // ---- Moving through the sidebar ---------------------------------------------

    /// The rows the sidebar shows, top to bottom (as `render_sidebar` draws them).
    pub(crate) fn tree_rows(&self) -> Vec<TreeNode> {
        let mut rows = Vec::new();
        for project in &self.projects {
            rows.push(TreeNode::Project(project.id));
            if self.collapsed.contains(&project.id) {
                continue;
            }
            for sub in self.subprojects.iter().filter(|s| s.project_id == project.id) {
                rows.push(TreeNode::Sub(sub.id));
                if self.sel_sub != Some(sub.id) || self.form.is_some() {
                    continue;
                }
                // The same rows as the sidebar: folded agents only when unfolded.
                let (visible, folded) = self.sidebar_agents(sub.id);
                rows.extend(visible.iter().map(|a| TreeNode::Agent(a.record.id)));
                if self.show_helpers.contains(&sub.id) {
                    rows.extend(folded.iter().map(|a| TreeNode::Agent(a.record.id)));
                }
                if self.show_archived.contains(&sub.id) {
                    for agent in self.agents.iter().filter(|a| a.record.subproject_id == sub.id && a.record.archived) {
                        rows.push(TreeNode::Agent(agent.record.id));
                    }
                }
            }
        }
        rows
    }

    /// Where the cursor is: where you moved it, else what's open.
    pub(crate) fn tree_current(&self) -> Option<TreeNode> {
        let rows = self.tree_rows();
        if let Some(node) = self.tree_cursor.filter(|n| rows.contains(n)) {
            return Some(node);
        }
        let open = match (self.sel_agent, self.sel_sub, self.sel_project) {
            (Some(a), _, _) => Some(TreeNode::Agent(a)),
            (None, Some(s), _) => Some(TreeNode::Sub(s)),
            (None, None, Some(p)) => Some(TreeNode::Project(p)),
            _ => None,
        };
        open.filter(|n| rows.contains(n)).or_else(|| rows.first().copied())
    }

    pub(crate) fn focus_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_open {
            self.sidebar_open = true;
        }
        self.zen = false;
        self.tree_cursor = self.tree_current();
        self.tree_scroll_pending.set(true);
        window.focus(&self.tree_focus);
        cx.notify();
    }

    fn tree_set(&mut self, node: TreeNode, cx: &mut Context<Self>) {
        self.tree_cursor = Some(node);
        self.tree_scroll_pending.set(true);
        cx.notify();
    }

    pub(crate) fn tree_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.tree_rows();
        let Some(current) = self.tree_current() else { return };
        let at = rows.iter().position(|n| *n == current).unwrap_or(0) as isize;
        let next = (at + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.tree_set(rows[next], cx);
    }

    fn tree_sub_of(&self, agent_id: i64) -> Option<i64> {
        self.agent(agent_id).map(|a| a.record.subproject_id)
    }

    fn tree_project_of(&self, sub_id: i64) -> Option<i64> {
        self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.project_id)
    }

    /// → : open a level (expand a project, show a subproject's agents) or go into it.
    pub(crate) fn tree_right(&mut self, cx: &mut Context<Self>) {
        match self.tree_current() {
            Some(TreeNode::Project(pid)) => {
                if self.collapsed.remove(&pid) {
                    cx.notify();
                } else if let Some(sub) = self.subprojects.iter().find(|s| s.project_id == pid) {
                    let sid = sub.id;
                    self.tree_set(TreeNode::Sub(sid), cx);
                }
            }
            Some(TreeNode::Sub(sid)) => {
                if self.sel_sub != Some(sid) {
                    self.select_sub(sid, cx);
                    // Stay in the sidebar: you're still moving.
                    self.focus_request = None;
                }
                if let Some(agent) = self.agents.iter().find(|a| a.record.subproject_id == sid && !a.record.archived) {
                    let aid = agent.record.id;
                    self.tree_set(TreeNode::Agent(aid), cx);
                }
            }
            _ => {}
        }
    }

    /// ← : back up a level, or fold the project you're on.
    pub(crate) fn tree_left(&mut self, cx: &mut Context<Self>) {
        match self.tree_current() {
            Some(TreeNode::Agent(aid)) => {
                if let Some(sid) = self.tree_sub_of(aid) {
                    self.tree_set(TreeNode::Sub(sid), cx);
                }
            }
            Some(TreeNode::Sub(sid)) => {
                if let Some(pid) = self.tree_project_of(sid) {
                    self.tree_set(TreeNode::Project(pid), cx);
                }
            }
            Some(TreeNode::Project(pid)) => {
                self.collapsed.insert(pid);
                cx.notify();
            }
            None => {}
        }
    }

    /// Enter: open the row. An agent gets your cursor in its message box.
    pub(crate) fn tree_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tree_current() {
            Some(TreeNode::Project(pid)) => self.select_project(pid, cx),
            Some(TreeNode::Sub(sid)) => {
                self.select_sub(sid, cx);
                self.focus_request = None;
            }
            Some(TreeNode::Agent(aid)) => {
                if let Some(sid) = self.tree_sub_of(aid)
                    && self.sel_sub != Some(sid)
                {
                    self.select_sub(sid, cx);
                }
                self.select_agent(aid, cx);
                self.tree_cursor = None;
                window.focus(&self.focus);
            }
            None => {}
        }
    }

    /// Esc in the sidebar: back to what you were doing.
    pub(crate) fn leave_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tree_cursor = None;
        self.focus_request = self.focus_target();
        window.focus(&self.focus);
        cx.notify();
    }

    // ---- Settings → Keyboard -------------------------------------------------------

    fn start_key_capture(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.key_capture = Some(id);
        // Nothing fires while you press the new keys.
        cx.clear_key_bindings();
        window.focus(&self.capture_focus);
        self.capture_blur = Some(cx.on_blur(&self.capture_focus, window, |this, _, cx| this.end_key_capture(cx)));
        cx.notify();
    }

    fn end_key_capture(&mut self, cx: &mut Context<Self>) {
        self.capture_blur = None;
        if self.key_capture.take().is_some() {
            apply(cx, &self.store);
        }
        cx.notify();
    }

    fn set_shortcut(&mut self, id: &'static str, value: Option<&str>, cx: &mut Context<Self>) {
        let key = format!("keys:{id}");
        let _ = match value {
            Some(v) => self.store.set_setting(&key, v),
            None => self.store.delete_setting(&key),
        };
        apply(cx, &self.store);
        cx.notify();
    }

    fn captured_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.key_capture else { return };
        let k = &event.keystroke;
        if matches!(k.key.as_str(), "" | "shift" | "control" | "alt" | "platform" | "function") {
            return;
        }
        cx.stop_propagation();
        let m = &k.modifiers;
        let plain = !(m.control || m.alt || m.platform || m.shift);
        if plain && k.key == "escape" {
            self.end_key_capture(cx);
            window.focus(&self.focus);
            return;
        }
        let label = SHORTCUTS.iter().find(|s| s.id == id).map(|s| s.label).unwrap_or(id);
        if plain && k.key == "backspace" {
            self.capture_blur = None;
            self.key_capture = None;
            self.set_shortcut(id, Some("none"), cx);
            self.toast(Tone::Info, format!("{label}: no shortcut"), cx);
            window.focus(&self.focus);
            return;
        }
        let f_key = k.key.len() >= 2 && k.key.starts_with('f') && k.key[1..].chars().all(|c| c.is_ascii_digit());
        if !(m.control || m.alt || m.platform || f_key) {
            self.toast(Tone::Warn, "Use Ctrl, Alt or Win with a key, or an F key: a plain key would type instead.", cx);
            return;
        }
        let keys = k.unparse();
        if TEXT_KEYS.contains(&keys.as_str()) {
            self.toast(Tone::Warn, format!("{} is for text boxes; pick another.", pretty(&keys)), cx);
            return;
        }
        if let Some(other) = SHORTCUTS.iter().find(|s| s.id != id && keys_for(&self.store, s.id) == keys) {
            self.toast(Tone::Warn, format!("{} is already “{}”. Change that one first.", pretty(&keys), other.label), cx);
            return;
        }
        self.capture_blur = None;
        self.key_capture = None;
        let default = SHORTCUTS.iter().find(|s| s.id == id).map(|s| s.default).unwrap_or("");
        self.set_shortcut(id, if keys == default { None } else { Some(&keys) }, cx);
        self.toast(Tone::Info, format!("{label}: {}", pretty(&keys)), cx);
        window.focus(&self.focus);
    }

    pub(crate) fn render_keyboard_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let customized = SHORTCUTS.iter().any(|s| self.store.setting(&format!("keys:{}", s.id)).is_some());
        let rows = SHORTCUTS.iter().enumerate().map(|(i, s)| {
            let id = s.id;
            let keys = keys_for(&self.store, id);
            let custom = self.store.setting(&format!("keys:{id}")).is_some();
            let capturing = self.key_capture == Some(id);
            let current: AnyElement = if capturing {
                div()
                    .id(("key-capture", i))
                    .track_focus(&self.capture_focus)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.captured_key(event, window, cx)))
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .border_1()
                    .border_color(rgb(theme::text_2()))
                    .bg(rgb(theme::raised()))
                    .text_size(px(12.5))
                    .text_color(rgb(theme::text()))
                    .child("Press the new keys · Backspace: none · Esc: cancel")
                    .into_any_element()
            } else if keys.is_empty() {
                ui::meta("None").into_any_element()
            } else {
                ui::kbd(pretty(&keys)).into_any_element()
            };
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .py(px(5.))
                .when(i > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                .child(div().w(px(110.)).flex_none().child(ui::meta(s.group)))
                .child(div().flex_1().min_w(px(0.)).text_size(px(13.)).text_color(rgb(theme::text())).child(s.label))
                .child(current)
                .when(!capturing, |d| {
                    d.child(ui::button(("key-change", i), Kind::Ghost, None, "Change").on_click(cx.listener(move |this, _, window, cx| this.start_key_capture(id, window, cx))))
                })
                .when(custom && !capturing, |d| {
                    d.child(ui::button(("key-reset", i), Kind::Ghost, None, "Default").on_click(cx.listener(move |this, _, _, cx| this.set_shortcut(id, None, cx))))
                })
        });
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().flex().flex_col().children(rows))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().child(ui::meta("In the sidebar: ↑ ↓ move, → opens a level, ← goes back, Enter opens, Esc leaves.")))
                    .when(customized, |d| {
                        d.child(ui::button("keys-reset-all", Kind::Secondary, None, "Reset all").on_click(cx.listener(|this, _, _, cx| {
                            for s in SHORTCUTS {
                                let _ = this.store.delete_setting(&format!("keys:{}", s.id));
                            }
                            apply(cx, &this.store);
                            this.toast(Tone::Info, "Shortcuts are back to their defaults.", cx);
                            cx.notify();
                        })))
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_nicely() {
        assert_eq!(pretty("ctrl-shift-f"), "Ctrl Shift F");
        assert_eq!(pretty("alt-up"), "Alt ↑");
        assert_eq!(pretty("ctrl-,"), "Ctrl ,");
        assert_eq!(pretty("f11"), "F11");
    }

    #[test]
    fn every_binding_parses() {
        // KeyBinding::new panics on a bad key or context, at startup.
        assert!(!crate::text_input::key_bindings().is_empty());
        assert!(!crate::fixed_bindings().is_empty());
    }

    #[test]
    fn defaults_are_valid_and_distinct() {
        let mut seen = std::collections::HashSet::new();
        for s in SHORTCUTS {
            assert!(valid(s.default), "{}", s.id);
            assert!(seen.insert(s.default), "{} twice", s.default);
            assert!(binding(s.id, s.default).is_some(), "{}", s.id);
        }
    }
}
