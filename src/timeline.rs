//! A subproject's timeline (inspector → Timeline): what happened, newest
//! first, by day. Turns with their model and cost, reviews, board posts,
//! agents joining, merges, pushes, pipeline stages and finished runs.

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};

use crate::icons::{Icon, icon_sized};
use crate::store::TimelineEvent;
use crate::{Monaged, theme, ui};

/// Local time for a unix timestamp, as (day label, "14:05").
fn local_time(at: i64) -> (String, String) {
    let offset = local_offset_seconds();
    let t = at + offset;
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let today = (now() + offset).div_euclid(86_400);
    let day = match today - days {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => {
            // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
            format!("{d} {}", MONTHS[(m - 1) as usize])
        }
    };
    (day, format!("{:02}:{:02}", secs / 3600, (secs % 3600) / 60))
}

fn now() -> i64 {
    crate::usage::now_unix()
}

fn local_offset_seconds() -> i64 {
    crate::usage::local_offset_secs()
}

impl Monaged {
    pub(crate) fn render_timeline(&self, sub_id: i64, cx: &mut Context<Self>) -> AnyElement {
        let events = self.store.timeline(sub_id, 400).unwrap_or_default();
        if events.is_empty() {
            return ui::panel_section("Activity").child(ui::meta("Nothing yet. Reviews, merges, the pipeline's steps and board posts show up here, newest first.")).into_any_element();
        }
        // Each turn is an entry; most of the time the rest is what matters.
        let show_turns = self.store.setting("ui:activity_turns").is_some_and(|v| v == "1");
        let agent_name = |id: Option<i64>| id.and_then(|id| self.agent(id)).map(|a| a.record.name.clone()).unwrap_or_else(|| "an agent".into());
        let total: f64 = events.iter().filter(|e| e.kind == "turn").filter_map(|e| e.data.split('|').nth(1)?.parse::<f64>().ok()).sum();
        let turns = events.iter().filter(|e| e.kind == "turn").count();

        let mut col = div().flex().flex_col();
        col = col.child(
            ui::panel_section("Activity")
                .child(div().flex().gap(px(16.)).child(ui::mono(format!("{turns} turns"))).child(ui::mono(format!("${total:.2} API-equivalent"))))
                .child(ui::switch("activity-turns", "Show every turn", show_turns).on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.store.set_setting("ui:activity_turns", if show_turns { "0" } else { "1" });
                    cx.notify();
                }))),
        );
        let mut day_list = div().flex().flex_col().px(px(16.)).pb(px(16.));
        let mut last_day = String::new();
        for (n, event) in events.iter().enumerate().filter(|(_, e)| show_turns || e.kind != "turn") {
            let (day, time) = local_time(event.at);
            if day != last_day {
                day_list = day_list.child(div().pt(px(14.)).pb(px(6.)).child(ui::label(day.clone())));
                last_day = day;
            }
            let (glyph, text, red) = describe(event, &agent_name);
            day_list = day_list.child(
                div()
                    .id(SharedString::from(format!("tl-{n}")))
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .py(px(5.))
                    .child(div().w(px(38.)).flex_none().font_family(theme::MONO).text_size(px(11.)).text_color(rgb(theme::text_4())).pt(px(1.)).child(time))
                    .child(div().pt(px(1.)).child(icon_sized(glyph, 13.).text_color(rgb(if red { theme::red_text() } else { theme::text_3() }))))
                    .child(div().flex_1().min_w(px(0.)).text_size(px(12.5)).line_height(px(18.)).text_color(rgb(if red { theme::red_text() } else { theme::text_2() })).child(text)),
            );
        }
        col.child(day_list).into_any_element()
    }
}

/// Icon, sentence and whether it's bad news.
pub(crate) fn describe(event: &TimelineEvent, agent_name: &dyn Fn(Option<i64>) -> String) -> (Icon, String, bool) {
    let parts: Vec<&str> = event.data.split('|').collect();
    let part = |i: usize| parts.get(i).copied().unwrap_or_default();
    match event.kind.as_str() {
        "turn" => {
            let cost: f64 = part(1).parse().unwrap_or(0.);
            // The model isn't always known (an adapter that doesn't say); leave it out then.
            let mut text = format!("{} worked a turn", agent_name(event.agent_id));
            if !part(0).trim().is_empty() {
                text.push_str(&format!(" · {}", crate::usage::pretty_model(part(0))));
            }
            if cost >= 0.005 {
                text.push_str(&format!(" · ${cost:.2}"));
            }
            (Icon::Agent, text, false)
        }
        "review" => {
            let score: u8 = part(0).parse().unwrap_or(0);
            (Icon::Eye, format!("{} reviewed: {score}/5", agent_name(event.agent_id)), score <= 2)
        }
        "agent" => {
            let role = part(1);
            let role = if role.is_empty() || role == "free" { String::new() } else { format!(" as {role}") };
            (Icon::Plus, format!("{} joined{role}", part(0)), false)
        }
        "run" => (Icon::Flow, format!("Pipeline finished: {} plan, {} workstream(s), ${}", part(0), part(1), part(2)), false),
        kind if kind.starts_with("board:") => {
            let what = kind.trim_start_matches("board:");
            (Icon::Board, format!("{} posted a {what}: {}", part(0), part(1)), false)
        }
        kind if kind.starts_with("event:") => {
            let what = kind.trim_start_matches("event:");
            let glyph = match what {
                "merge" => Icon::Merge,
                "push" => Icon::Push,
                "stage" => Icon::Flow,
                "conflict" | "failed" => Icon::Alert,
                "archive" | "restore" => Icon::Archive,
                "commit" => Icon::Commit,
                "gate" | "ci" => Icon::Shield,
                "lead" => Icon::Flow,
                "debug" => Icon::Ask,
                "stall" => Icon::Refresh,
                _ => Icon::Dot,
            };
            // Work held below the bar waits for you: worth seeing in red.
            let waits = matches!(what, "gate" | "stall") && event.data.contains("waits for you");
            (glyph, event.data.clone(), matches!(what, "conflict" | "failed") || waits)
        }
        _ => (Icon::Dot, event.data.clone(), false),
    }
}
