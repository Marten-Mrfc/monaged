//! Layout you can shape: drag the sidebar's, the inspector's and the panes'
//! edges (double-click resets them), save a project's layout under a name and
//! bring it back from Ctrl K, go to zen mode (F11: one conversation, nothing
//! else) and pick a compact density.

use gpui::{AnyElement, Context, CursorStyle, MouseButton, MouseDownEvent, MouseMoveEvent, Window, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::agent::Tone;
use crate::shell::{InspectorTab, tip};
use crate::{Monaged, theme};

const SIDEBAR_MIN: f32 = 200.;
const SIDEBAR_MAX: f32 = 520.;
const INSPECTOR_MIN: f32 = 280.;
const INSPECTOR_MAX: f32 = 720.;
const PANE_MIN: f32 = 360.;

/// An edge being dragged.
pub(crate) enum Drag {
    Sidebar { start_x: f32, start_w: f32 },
    Inspector { start_x: f32, start_w: f32 },
    /// The divider left of pane `index` (between `index - 1` and `index`).
    Pane { key: (i64, usize), index: usize, start_x: f32, widths: Vec<f32> },
}

/// A project's layout, saved under a name.
#[derive(Clone, Debug)]
pub(crate) struct SavedLayout {
    pub name: String,
    pub sidebar_open: bool,
    pub inspector_open: bool,
    pub inspector_tab: String,
    pub sidebar_w: Option<f32>,
    pub inspector_w: Option<f32>,
    pub sub: Option<i64>,
    pub agent: Option<i64>,
    pub pinned: Vec<i64>,
}

impl SavedLayout {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name, "sidebarOpen": self.sidebar_open, "inspectorOpen": self.inspector_open,
            "inspectorTab": self.inspector_tab, "sidebarW": self.sidebar_w, "inspectorW": self.inspector_w,
            "sub": self.sub, "agent": self.agent, "pinned": self.pinned,
        })
    }

    fn from_json(v: &Value) -> Option<SavedLayout> {
        Some(SavedLayout {
            name: v["name"].as_str()?.to_string(),
            sidebar_open: v["sidebarOpen"].as_bool().unwrap_or(true),
            inspector_open: v["inspectorOpen"].as_bool().unwrap_or(true),
            inspector_tab: v["inspectorTab"].as_str().unwrap_or("pipeline").to_string(),
            sidebar_w: v["sidebarW"].as_f64().map(|f| f as f32),
            inspector_w: v["inspectorW"].as_f64().map(|f| f as f32),
            sub: v["sub"].as_i64(),
            agent: v["agent"].as_i64(),
            pinned: v["pinned"].as_array().into_iter().flatten().filter_map(Value::as_i64).collect(),
        })
    }
}

fn tab_key(tab: InspectorTab) -> &'static str {
    match tab {
        InspectorTab::Pipeline => "pipeline",
        InspectorTab::Merge => "merge",
        InspectorTab::Git => "git",
        InspectorTab::Timeline => "timeline",
        InspectorTab::Run => "run",
    }
}

fn tab_from(key: &str) -> InspectorTab {
    match key {
        "merge" => InspectorTab::Merge,
        "git" => InspectorTab::Git,
        "timeline" => InspectorTab::Timeline,
        "run" => InspectorTab::Run,
        _ => InspectorTab::Pipeline,
    }
}

impl Monaged {
    // ---- Widths ------------------------------------------------------------------------------

    pub(crate) fn sidebar_px(&self, window_w: f32) -> f32 {
        self.sidebar_w.unwrap_or_else(|| Self::sidebar_width(window_w)).clamp(SIDEBAR_MIN, SIDEBAR_MAX)
    }

    pub(crate) fn inspector_px(&self, window_w: f32) -> f32 {
        self.inspector_w.unwrap_or_else(|| Self::inspector_width(window_w)).clamp(INSPECTOR_MIN, INSPECTOR_MAX)
    }

    /// Each pane's width: equal, unless you dragged a divider.
    pub(crate) fn pane_widths(&self, sub_id: i64, count: usize, main_w: f32) -> Vec<f32> {
        if count == 0 {
            return Vec::new();
        }
        let weights = self.pane_weights.get(&(sub_id, count)).filter(|w| w.len() == count).cloned().unwrap_or_else(|| vec![1.; count]);
        let sum: f32 = weights.iter().sum::<f32>().max(0.001);
        weights.iter().map(|w| (main_w * w / sum).floor()).collect()
    }

    // ---- Dragging edges ----------------------------------------------------------------------

    pub(crate) fn drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let x = f32::from(event.position.x);
        match drag {
            Drag::Sidebar { start_x, start_w } => self.sidebar_w = Some((start_w + x - start_x).clamp(SIDEBAR_MIN, SIDEBAR_MAX)),
            Drag::Inspector { start_x, start_w } => self.inspector_w = Some((start_w - (x - start_x)).clamp(INSPECTOR_MIN, INSPECTOR_MAX)),
            Drag::Pane { key, index, start_x, widths } => {
                let (left, right) = (widths[index - 1], widths[*index]);
                let delta = (x - start_x).clamp(PANE_MIN - left, right - PANE_MIN);
                let mut next = widths.clone();
                next[index - 1] = left + delta;
                next[*index] = right - delta;
                self.pane_weights.insert(*key, next);
            }
        }
        cx.notify();
    }

    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Sidebar { .. } => {
                if let Some(w) = self.sidebar_w {
                    let _ = self.store.set_setting("ui:sidebar_w", &w.round().to_string());
                }
            }
            Drag::Inspector { .. } => {
                if let Some(w) = self.inspector_w {
                    let _ = self.store.set_setting("ui:inspector_w", &w.round().to_string());
                }
            }
            Drag::Pane { key, .. } => {
                if let Some(w) = self.pane_weights.get(&key) {
                    let text: Vec<String> = w.iter().map(|v| v.round().to_string()).collect();
                    let _ = self.store.set_setting(&format!("ui:panes:{}:{}", key.0, key.1), &text.join(","));
                }
            }
        }
        cx.notify();
    }

    /// Saved pane widths come back with the subproject.
    pub(crate) fn load_pane_weights(&mut self, sub_id: i64, count: usize) {
        if self.pane_weights.contains_key(&(sub_id, count)) {
            return;
        }
        if let Some(text) = self.store.setting(&format!("ui:panes:{sub_id}:{count}")) {
            let weights: Vec<f32> = text.split(',').filter_map(|v| v.parse().ok()).collect();
            if weights.len() == count {
                self.pane_weights.insert((sub_id, count), weights);
            }
        }
    }

    /// A thin grab strip on an edge; `which`: "sidebar", "inspector" or a pane divider.
    pub(crate) fn resize_handle(&self, id: impl Into<gpui::ElementId>, on_down: impl Fn(&mut Monaged, f32, &mut Context<Monaged>) + 'static, on_reset: impl Fn(&mut Monaged, &mut Context<Monaged>) + 'static, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(7.))
            .cursor(CursorStyle::ResizeLeftRight)
            .group("resize-handle")
            .child(div().mx_auto().w(px(1.)).h_full().group_hover("resize-handle", |s| s.bg(rgb(theme::text_3()))))
            .tooltip(tip("Drag to resize · double-click to reset"))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if event.click_count >= 2 {
                    return;
                }
                on_down(this, f32::from(event.position.x), cx);
            }))
            // A double-click resets: handled on release, after the drag the first click began has ended.
            .on_mouse_up(MouseButton::Left, cx.listener(move |this, event: &gpui::MouseUpEvent, _, cx| {
                if event.click_count >= 2 {
                    cx.stop_propagation();
                    this.drag = None;
                    on_reset(this, cx);
                    cx.notify();
                }
            }))
    }

    pub(crate) fn sidebar_handle(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        self.resize_handle(
            "resize-sidebar",
            move |this, x, _| this.drag = Some(Drag::Sidebar { start_x: x, start_w: width }),
            |this, _| {
                this.sidebar_w = None;
                let _ = this.store.set_setting("ui:sidebar_w", "");
            },
            cx,
        )
        .right(px(-4.))
        .into_any_element()
    }

    pub(crate) fn inspector_handle(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        self.resize_handle(
            "resize-inspector",
            move |this, x, _| this.drag = Some(Drag::Inspector { start_x: x, start_w: width }),
            |this, _| {
                this.inspector_w = None;
                let _ = this.store.set_setting("ui:inspector_w", "");
            },
            cx,
        )
        .left(px(-4.))
        .into_any_element()
    }

    pub(crate) fn pane_handle(&self, sub_id: i64, index: usize, widths: Vec<f32>, cx: &mut Context<Self>) -> AnyElement {
        let key = (sub_id, widths.len());
        self.resize_handle(
            ("resize-pane", index),
            move |this, x, _| this.drag = Some(Drag::Pane { key, index, start_x: x, widths: widths.clone() }),
            move |this, _| {
                this.pane_weights.remove(&key);
                let _ = this.store.set_setting(&format!("ui:panes:{}:{}", key.0, key.1), "");
            },
            cx,
        )
        .left(px(-4.))
        .into_any_element()
    }

    // ---- Saved layouts -----------------------------------------------------------------------

    pub(crate) fn layouts(&self, project_id: i64) -> Vec<SavedLayout> {
        self.store
            .setting(&format!("layouts:{project_id}"))
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(SavedLayout::from_json).collect()))
            .unwrap_or_default()
    }

    fn store_layouts(&self, project_id: i64, layouts: &[SavedLayout]) {
        let value = Value::Array(layouts.iter().map(SavedLayout::to_json).collect());
        let _ = self.store.set_setting(&format!("layouts:{project_id}"), &value.to_string());
    }

    pub(crate) fn ask_save_layout(&mut self, project_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.layouts(project_id).len();
        self.ask_text(
            "Save this layout".into(),
            "A name, like Review or Three builders".into(),
            "Save".into(),
            format!("Layout {}", count + 1),
            crate::feedback::TextAction::SaveLayout(project_id),
            window,
            cx,
        );
    }

    pub(crate) fn save_layout(&mut self, project_id: i64, name: &str, cx: &mut Context<Self>) {
        let layout = SavedLayout {
            name: name.to_string(),
            sidebar_open: self.sidebar_open,
            inspector_open: self.inspector_open,
            inspector_tab: tab_key(self.inspector_tab).into(),
            sidebar_w: self.sidebar_w,
            inspector_w: self.inspector_w,
            sub: self.sel_sub,
            agent: self.sel_agent,
            pinned: self.pinned.clone(),
        };
        let mut layouts = self.layouts(project_id);
        layouts.retain(|l| l.name != name);
        layouts.push(layout);
        self.store_layouts(project_id, &layouts);
        self.toast(Tone::Info, format!("Saved the layout \"{name}\". Bring it back with Ctrl K."), cx);
    }

    pub(crate) fn apply_layout(&mut self, project_id: i64, name: &str, cx: &mut Context<Self>) {
        let Some(layout) = self.layouts(project_id).into_iter().find(|l| l.name == name) else { return };
        self.form = None;
        self.sidebar_open = layout.sidebar_open;
        self.inspector_open = layout.inspector_open;
        self.inspector_tab = tab_from(&layout.inspector_tab);
        self.sidebar_w = layout.sidebar_w;
        self.inspector_w = layout.inspector_w;
        match layout.sub.filter(|id| self.subprojects.iter().any(|s| s.id == *id)) {
            Some(sub_id) => {
                self.select_sub(sub_id, cx);
                if let Some(agent_id) = layout.agent.filter(|id| self.agent(*id).is_some()) {
                    self.select_agent(agent_id, cx);
                }
                self.pinned = layout.pinned.into_iter().filter(|id| self.agent(*id).is_some()).collect();
                let pinned = self.pinned.clone();
                for id in pinned {
                    self.ensure_session(id);
                }
            }
            None => self.select_project(project_id, cx),
        }
        self.toast(Tone::Info, format!("Layout \"{name}\""), cx);
        cx.notify();
    }

    pub(crate) fn delete_layout(&mut self, project_id: i64, name: &str, cx: &mut Context<Self>) {
        let mut layouts = self.layouts(project_id);
        layouts.retain(|l| l.name != name);
        self.store_layouts(project_id, &layouts);
        self.toast(Tone::Info, format!("Deleted the layout \"{name}\""), cx);
    }

    // ---- Zen and density ---------------------------------------------------------------------

    pub(crate) fn toggle_zen(&mut self, cx: &mut Context<Self>) {
        self.zen = !self.zen;
        if self.zen && self.sel_agent.is_none() {
            self.toast(Tone::Info, "Zen mode shows one agent's conversation. Pick an agent first.", cx);
            self.zen = false;
        }
        if self.zen {
            self.focus_request = self.focus_target();
        }
        cx.notify();
    }

    pub(crate) fn set_density(&mut self, compact: bool, cx: &mut Context<Self>) {
        crate::ui::set_compact(compact);
        let _ = self.store.set_setting("ui:density", if compact { "compact" } else { "comfortable" });
        cx.notify();
    }
}
