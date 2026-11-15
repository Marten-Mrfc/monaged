//! The merge queue (project overview): every subproject with work to merge,
//! ready ones first, with its confidence, conflicts, merge/pull request and
//! the branch's latest CI run. "Merge ready ones" merges them one at a time.

use std::time::{Duration, Instant};

use gpui::{AnyElement, Context, SharedString, div, prelude::*, px, rgb};

use crate::agent::Tone;
use crate::hosting::{self, Ci};
use crate::icons::{Icon, icon_sized};
use crate::pipeline::Stage;
use crate::shell::tip;
use crate::ui::{self, Kind};
use crate::{Monaged, theme};

/// CI is asked again after this long.
const CI_AGE: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, PartialOrd, Eq, Ord)]
enum Readiness {
    Ready,
    NeedsYou,
    InProgress,
    Merged,
}

impl Monaged {
    /// Asks GitHub or GitLab for each subproject's latest CI run (in the background).
    pub(crate) fn refresh_ci(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let Some(remote) = self.remote(project_id) else { return };
        let subs: Vec<(i64, String)> = self
            .subprojects
            .iter()
            .filter(|s| s.project_id == project_id)
            .filter(|s| self.ci.get(&s.id).is_none_or(|(at, _)| at.elapsed() > CI_AGE))
            .map(|s| (s.id, s.branch.clone()))
            .collect();
        if subs.is_empty() {
            return;
        }
        for (id, _) in &subs {
            self.ci.insert(*id, (Instant::now(), None));
        }
        cx.spawn(async move |view, cx| {
            let results = cx
                .background_spawn(async move { subs.into_iter().map(|(id, branch)| (id, hosting::ci_status(&remote, &branch).ok().flatten())).collect::<Vec<_>>() })
                .await;
            view.update(cx, |this, cx| {
                for (id, ci) in results {
                    this.ci.insert(id, (Instant::now(), ci));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Keeps open requests and running CI current for what you're looking at
    /// (about once a minute; the hosts are asked only while something's pending).
    pub(crate) fn poll_hosts(&mut self, cx: &mut Context<Self>) {
        if self.hosts_polled.is_some_and(|at| at.elapsed() < Duration::from_secs(60)) {
            return;
        }
        self.hosts_polled = Some(Instant::now());
        let Some(project_id) = self.sel_project else { return };
        if self.remote(project_id).is_none_or(|r| r.provider.is_none()) {
            return;
        }
        let subs: Vec<i64> = self.subprojects.iter().filter(|s| s.project_id == project_id).map(|s| s.id).collect();
        let ci_running = subs.iter().any(|id| self.ci.get(id).and_then(|(_, c)| c.as_ref()).is_some_and(|c| c.state == "running"));
        if ci_running {
            self.refresh_ci(project_id, cx);
        }
        for sub_id in subs {
            if self.merges.get(&sub_id).and_then(|m| m.request.as_ref()).is_some_and(|r| r.is_open()) {
                self.refresh_request(sub_id, cx);
            }
        }
    }

    fn readiness(&self, sub_id: i64) -> (Readiness, String) {
        let Some(sub) = self.subprojects.iter().find(|s| s.id == sub_id) else { return (Readiness::Merged, String::new()) };
        let target = self.merge_target(sub_id).unwrap_or_default();
        let ahead = self.branch_info.get(&sub.project_id).and_then(|i| i.ahead.get(&(sub.branch.clone(), target.clone()))).copied();
        let state = self.merges.get(&sub_id);
        let ci = self.ci.get(&sub_id).and_then(|(_, c)| c.clone());
        let running = self.pipelines.get(&sub_id).is_some_and(|p| !matches!(p.stage, Stage::Done | Stage::Stopped));
        let working = self.agents.iter().any(|a| a.record.subproject_id == sub_id && a.is_working());
        if state.is_some_and(|s| s.busy) {
            return (Readiness::InProgress, "merging…".into());
        }
        if state.is_some_and(|s| !s.conflict.is_empty()) {
            return (Readiness::NeedsYou, "conflicts".into());
        }
        if ci.as_ref().is_some_and(|c| c.state == "failure") {
            return (Readiness::NeedsYou, "CI failing".into());
        }
        match ahead {
            None => (Readiness::InProgress, "checking…".into()),
            Some(0) => (Readiness::Merged, format!("nothing new for {target}")),
            Some(n) if running || working => (Readiness::InProgress, format!("{n} commit{} · still working", if n == 1 { "" } else { "s" })),
            Some(n) => match self.sub_confidence(sub_id) {
                // Below the quality bar isn't ready, whatever else is.
                Some((score, _)) if score < self.min_score() => (
                    Readiness::NeedsYou,
                    format!("{n} commit{} · confidence {score}/5, below the {}/5 bar", if n == 1 { "" } else { "s" }, self.min_score()),
                ),
                _ if ci.as_ref().is_some_and(|c| c.state == "running") => (Readiness::InProgress, "CI running".into()),
                _ => (Readiness::Ready, format!("{n} commit{} ready for {target}", if n == 1 { "" } else { "s" })),
            },
        }
    }

    /// Merges every ready subproject, one after another.
    pub(crate) fn merge_ready(&mut self, project_id: i64, cx: &mut Context<Self>) {
        let ready: Vec<i64> = self
            .subprojects
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| s.id)
            .filter(|id| self.readiness(*id).0 == Readiness::Ready)
            .collect();
        if ready.is_empty() {
            self.toast(Tone::Info, "Nothing is ready to merge.", cx);
            return;
        }
        let count = ready.len();
        self.merge_queue.extend(ready);
        self.toast(Tone::Info, format!("Merging {count} subproject{} one at a time.", if count == 1 { "" } else { "s" }), cx);
        self.advance_merge_queue(cx);
    }

    /// Starts the next queued merge when none is running.
    pub(crate) fn advance_merge_queue(&mut self, cx: &mut Context<Self>) {
        if self.merges.values().any(|m| m.busy) {
            return;
        }
        while let Some(sub_id) = self.merge_queue.pop_front() {
            if self.readiness(sub_id).0 == Readiness::Ready {
                self.merge_subproject(sub_id, cx);
                return;
            }
        }
    }

    pub(crate) fn render_merge_queue(&self, project_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<(Readiness, String, i64)> = self
            .subprojects
            .iter()
            .filter(|s| s.project_id == project_id)
            .map(|s| {
                let (r, text) = self.readiness(s.id);
                (r, text, s.id)
            })
            .collect();
        rows.sort_by_key(|(r, _, id)| (*r, *id));
        let ready = rows.iter().filter(|(r, _, _)| *r == Readiness::Ready).count();
        let has_remote = self.remote(project_id).is_some();
        let target = self.chain(project_id).first().cloned().unwrap_or_default();
        // Subprojects with nothing to merge fold into one quiet line under the list.
        let quiet: Vec<(i64, String)> = rows
            .iter()
            .filter(|(r, _, _)| *r == Readiness::Merged)
            .filter_map(|(_, _, id)| self.subprojects.iter().find(|s| s.id == *id).map(|s| (s.id, s.name.clone())))
            .collect();
        rows.retain(|(r, _, _)| *r != Readiness::Merged);
        let empty_note = match (self.subprojects.iter().any(|s| s.project_id == project_id), quiet.is_empty()) {
            (false, _) => Some("No subprojects yet.".to_string()),
            (true, false) if rows.is_empty() => Some(format!("Nothing waits to merge into {target}.")),
            _ => None,
        };
        let footer = (!quiet.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(14.))
                .min_h(px(36.))
                .when(!rows.is_empty() || empty_note.is_some(), |d| d.border_t_1().border_color(rgb(theme::line())))
                .text_size(px(12.))
                .text_color(rgb(theme::text_3()))
                .child(div().flex_none().child(format!("Nothing new for {target}:")))
                .children(quiet.iter().enumerate().map(|(i, (sub_id, name))| {
                    let sub_id = *sub_id;
                    div()
                        .id(("queue-quiet", sub_id as usize))
                        .flex_none()
                        .cursor_pointer()
                        .text_color(rgb(theme::text_2()))
                        .hover(|s| s.text_color(rgb(theme::text())))
                        .child(if i + 1 < quiet.len() { format!("{name},") } else { name.clone() })
                        .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sub_id, cx)))
                }))
        });
        let list = div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(theme::line()))
            .overflow_hidden()
            .children(empty_note.map(|note| div().px(px(14.)).min_h(px(44.)).flex().items_center().text_size(px(13.)).text_color(rgb(theme::text_3())).child(note)))
            .children(rows.iter().enumerate().map(|(i, (readiness, text, sub_id))| {
                let sub_id = *sub_id;
                let sub = self.subprojects.iter().find(|s| s.id == sub_id);
                let name = sub.map(|s| s.name.clone()).unwrap_or_default();
                let ci = self.ci.get(&sub_id).and_then(|(_, c)| c.clone());
                let request = self.merges.get(&sub_id).and_then(|m| m.request.clone());
                let confidence = self.sub_confidence(sub_id);
                let (dot, color) = match readiness {
                    Readiness::Ready => (ui::State::Done, theme::text()),
                    Readiness::NeedsYou => (ui::State::NeedsYou, theme::red_text()),
                    Readiness::InProgress => (ui::State::Working, theme::text_2()),
                    Readiness::Merged => (ui::State::Idle, theme::text_3()),
                };
                let ci_chip = ci.map(|c: Ci| {
                    let url = c.url.clone();
                    let (glyph, fg) = match c.state.as_str() {
                        "success" => (Icon::Check, theme::text_2()),
                        "failure" => (Icon::Alert, theme::red_text()),
                        _ => (Icon::Clock, theme::text_3()),
                    };
                    div()
                        .id(SharedString::from(format!("ci-{sub_id}")))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(rgb(theme::line()))
                        .font_family(theme::MONO)
                        .text_size(px(11.))
                        .text_color(rgb(fg))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(theme::hover())))
                        .tooltip(tip("Open the CI run"))
                        .child(icon_sized(glyph, 11.).text_color(rgb(fg)))
                        .child(format!("CI {}", c.state))
                        .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url)))
                });
                let action: Option<AnyElement> = match readiness {
                    Readiness::Ready => Some(
                        ui::button(("queue-merge", sub_id as usize), Kind::Secondary, Some(Icon::Merge), "Merge")
                            .on_click(cx.listener(move |this, _, _, cx| this.merge_subproject(sub_id, cx)))
                            .into_any_element(),
                    ),
                    Readiness::NeedsYou if text == "conflicts" => Some(
                        ui::button(("queue-resolve", sub_id as usize), Kind::Secondary, Some(Icon::Agent), "Resolve")
                            .on_click(cx.listener(move |this, _, _, cx| this.show_inspector(sub_id, crate::shell::InspectorTab::Merge, cx)))
                            .into_any_element(),
                    ),
                    _ => None,
                };
                div()
                    .id(("queue-row", sub_id as usize))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .h(px(46.))
                    .when(i > 0, |d| d.border_t_1().border_color(rgb(theme::line())))
                    .child(ui::status_dot(("queue-dot", sub_id as usize), dot))
                    .child(
                        div()
                            .id(("queue-name", sub_id as usize))
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .cursor_pointer()
                            .child(div().truncate().child(name))
                            .child(div().text_size(px(12.)).text_color(rgb(color)).child(text.clone()))
                            .on_click(cx.listener(move |this, _, _, cx| this.select_sub(sub_id, cx))),
                    )
                    .children(confidence.map(|(score, _)| ui::score_meter(score)))
                    .children(ci_chip)
                    .children(request.map(|r| {
                        let url = r.url.clone();
                        div()
                            .id(("queue-request", sub_id as usize))
                            .font_family(theme::MONO)
                            .text_size(px(11.))
                            .text_color(rgb(theme::text_2()))
                            .cursor_pointer()
                            .hover(|s| s.text_color(rgb(theme::text())))
                            .child(format!("#{} {}", r.number, if r.draft && r.is_open() { "draft" } else { r.state.as_str() }))
                            .on_click(cx.listener(move |_, _, _, cx| crate::ui::open_web(cx, &url)))
                    }))
                    .children(action)
            }))
            .children(footer);
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().child(ui::label("Merge queue")))
                    .when(has_remote, |d| {
                        d.child(ui::icon_button("queue-ci", Icon::Refresh).tooltip(tip("Ask GitHub or GitLab for CI again")).on_click(cx.listener(move |this, _, _, cx| {
                            this.ci.retain(|id, _| !this.subprojects.iter().any(|s| s.id == *id && s.project_id == project_id));
                            this.refresh_ci(project_id, cx);
                        })))
                    })
                    .child(if ready > 0 {
                        ui::button("queue-merge-all", Kind::Primary, Some(Icon::Merge), format!("Merge {ready} ready")).on_click(cx.listener(move |this, _, _, cx| this.merge_ready(project_id, cx))).into_any_element()
                    } else {
                        ui::disabled_button("queue-merge-all", Kind::Primary, Some(Icon::Merge), "Merge ready ones").into_any_element()
                    }),
            )
            .child(list)
            .into_any_element()
    }
}
