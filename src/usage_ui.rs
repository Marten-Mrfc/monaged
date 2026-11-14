//! Phase 6: usage awareness inside the app. Turn costs and weekly readings
//! come in through `usage_update`s; the budget pauses automatic work; models
//! are set per role; plans get a cost estimate. The pure logic is in `usage.rs`.

use gpui::{AnyElement, Context, FontWeight, SharedString, Task, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::agent::Tone;
use crate::pipeline::{self, Gate};
use crate::usage::{self, Block, Budget, Meter, PlanUsage};
use crate::{Monaged, Pending, button, theme};

/// Usage state loaded at startup.
pub(crate) struct UsageState {
    pub meter: Meter,
    pub budget: Budget,
    pub plan_usage: PlanUsage,
    pub models: Vec<(String, String)>,
}

pub(crate) fn load(store: &crate::store::Store) -> UsageState {
    let meter = store.setting("usage:meter").map(|j| Meter::from_json(&j)).unwrap_or_default();
    let pct = |key: &str, default: u32| store.setting(key).and_then(|v| v.parse().ok()).unwrap_or(default);
    let budget = Budget {
        share_pct: pct("usage:budget_pct", usage::DEFAULT_BUDGET_PCT),
        ceiling_pct: pct("usage:ceiling_pct", usage::DEFAULT_CEILING_PCT),
    };
    let plan_usage = store
        .setting("usage:plan")
        .and_then(|j| serde_json::from_str::<Value>(&j).ok())
        .and_then(|v| usage::parse_rate_limit(&v))
        .unwrap_or_default();
    let models = store
        .setting("usage:models")
        .and_then(|j| serde_json::from_str::<Vec<(String, String)>>(&j).ok())
        .unwrap_or_default();
    UsageState { meter, budget, plan_usage, models }
}

/// Re-checks the week once a minute (resets, paused work).
pub(crate) fn start_tick(cx: &mut Context<Monaged>) -> Task<()> {
    cx.spawn(async move |view, cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
            if view.update(cx, |this, cx| this.usage_tick(cx)).is_err() {
                break;
            }
        }
    })
}

impl Monaged {
    // ---- Measuring -------------------------------------------------------------------

    fn save_meter(&self) {
        let _ = self.store.set_setting("usage:meter", &self.meter.to_json());
    }

    /// A `usage_update` from one of our sessions: the model, the session's
    /// cumulative cost, and sometimes the plan's weekly usage.
    pub(crate) fn on_usage_update(&mut self, agent_id: i64, update: &Value) {
        let model = update["_meta"]["_claude/model"].as_str().map(str::to_owned);
        let cost = update["cost"]["amount"].as_f64();
        let mut delta = 0.0;
        if let Some(agent) = self.agent_mut(agent_id) {
            if model.is_some() {
                agent.model = model;
            }
            if let Some(now) = cost {
                delta = if std::mem::take(&mut agent.cost_baseline) { 0.0 } else { usage::cost_delta(agent.cost_seen, now) };
                agent.cost_seen = Some(now);
                agent.turn_cost += delta;
                agent.cost_total += delta;
            }
        }
        if let Some(now) = cost {
            let _ = self.store.set_setting(&format!("agent:{agent_id}:cost_seen"), &now.to_string());
            if let Some(sub) = self.agent(agent_id).map(|a| a.record.subproject_id) {
                *self.sub_costs.entry(sub).or_default() += delta;
            }
            self.meter.add_cost(delta);
            self.save_meter();
        }
        let rate_limit = &update["_meta"]["_claude/rateLimit"];
        if let Some(plan) = usage::parse_rate_limit(rate_limit) {
            let week_before = self.meter.week_key;
            if let Some(week) = &plan.week {
                self.meter.reading(week.utilization, week.resets_at);
            }
            self.plan_usage = plan;
            let _ = self.store.set_setting("usage:plan", &rate_limit.to_string());
            self.save_meter();
            if self.meter.week_key != week_before {
                self.reload_sub_costs();
            }
            self.resume_paused(false);
        }
    }

    /// The `session/prompt` result: record the turn with its tokens and cost.
    pub(crate) fn record_turn(&mut self, agent_id: i64, usage_json: &Value) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        let cost = std::mem::take(&mut agent.turn_cost);
        let model = agent.model.clone().unwrap_or_default();
        let sub = agent.record.subproject_id;
        let n = |key: &str| usage_json[key].as_u64().unwrap_or(0);
        if let Err(err) = self.store.record_turn(
            agent_id,
            sub,
            &model,
            n("inputTokens"),
            n("outputTokens"),
            n("cachedReadTokens"),
            n("cachedWriteTokens"),
            cost,
        ) {
            eprintln!("[usage] couldn't record a turn: {err:#}");
        }
        if !self.agents.iter().any(|a| a.is_working()) {
            self.meter.went_idle();
            self.save_meter();
        }
    }

    /// Logs a Haiku call. On the Max plan (no API key) it also counts towards
    /// Monaged's share of the week.
    pub(crate) fn log_haiku(&mut self, agent_id: i64, kind: &str, via: &str, usage: &crate::llm::Usage) {
        let _ = self.store.log_llm_call(agent_id, kind, via, usage);
        if via == "max" {
            self.meter.add_cost(usage.cost_usd);
            self.save_meter();
        }
    }

    pub(crate) fn reload_sub_costs(&mut self) {
        let since = self.week_start();
        self.sub_costs = self.store.cost_by_sub_since(since).unwrap_or_default();
    }

    fn week_start(&self) -> i64 {
        let key = self.plan_usage.week.as_ref().map(|w| w.resets_at).unwrap_or(self.meter.week_key);
        if key > 0 { key - usage::WEEK_SECS } else { usage::now_unix() - usage::WEEK_SECS }
    }

    pub(crate) fn usage_tick(&mut self, cx: &mut Context<Self>) {
        let now = usage::now_unix();
        let mut changed = false;
        if self.meter.roll_over(now) {
            self.save_meter();
            changed = true;
        }
        if let Some(week) = &mut self.plan_usage.week
            && week.resets_at <= now
        {
            week.utilization = 0.0;
            week.status = "allowed".into();
            week.resets_at = self.meter.week_key.max(week.resets_at + usage::WEEK_SECS);
            changed = true;
        }
        if self.plan_usage.five_hour.as_ref().is_some_and(|w| w.resets_at <= now) {
            self.plan_usage.five_hour = None;
            changed = true;
        }
        if changed {
            self.reload_sub_costs();
        }
        self.resume_paused(false);
        cx.notify();
    }

    // ---- The budget --------------------------------------------------------------------

    pub(crate) fn budget_block(&self) -> Option<Block> {
        usage::check(self.budget, &self.meter, &self.plan_usage)
    }

    /// A subproject's weekly spending cap, in dollars (API-equivalent).
    pub(crate) fn sub_cap(&self, sub_id: i64) -> Option<f64> {
        self.store.setting(&format!("sub:{sub_id}:cap")).and_then(|v| v.parse::<f64>().ok()).filter(|c| *c > 0.0)
    }

    /// The subproject spent its cap this week.
    pub(crate) fn sub_cap_block(&self, sub_id: i64) -> Option<String> {
        let cap = self.sub_cap(sub_id)?;
        let spent = self.sub_costs.get(&sub_id).copied().unwrap_or(0.0);
        (spent >= cap).then(|| {
            let name = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.name.clone()).unwrap_or_default();
            format!("{name} spent ${spent:.2} of its ${cap:.0} weekly cap.")
        })
    }

    pub(crate) fn ask_sub_cap(&mut self, sub_id: i64, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let name = self.subprojects.iter().find(|s| s.id == sub_id).map(|s| s.name.clone()).unwrap_or_default();
        let current = self.sub_cap(sub_id).map(|c| format!("{c:.0}")).unwrap_or_default();
        self.ask_text(
            format!("Weekly spending cap for {name}"),
            "Dollars a week, e.g. 20. Leave empty for no cap.".into(),
            "Save cap".into(),
            current,
            crate::feedback::TextAction::SubCap(sub_id),
            window,
            cx,
        );
    }

    pub(crate) fn set_sub_cap(&mut self, sub_id: i64, cap: Option<f64>, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(&format!("sub:{sub_id}:cap"), &cap.map(|c| format!("{c:.2}")).unwrap_or_default());
        self.resume_paused(false);
        match cap {
            Some(c) => self.toast(Tone::Info, format!("Cap set: ${c:.0} a week for this subproject's automatic work."), cx),
            None => self.toast(Tone::Info, "No cap for this subproject now.", cx),
        }
        cx.notify();
    }

    /// Automatic work hit the budget: keep the message for later.
    pub(crate) fn pause_prompt(&mut self, agent_id: i64, text: String, block: &Block) {
        self.pause_with(agent_id, text, block.message());
    }

    /// Keeps a message for later, saying why.
    pub(crate) fn pause_with(&mut self, agent_id: i64, text: String, message: String) {
        let Some(agent) = self.agent_mut(agent_id) else { return };
        agent.queued_prompt = Some(match agent.queued_prompt.take() {
            Some(earlier) => format!("{earlier}\n\n{text}"),
            None => text,
        });
        if !agent.paused {
            agent.paused = true;
            agent.notice(Tone::Warn, format!("Paused: {message} Messages you type still go through."));
        }
    }

    /// Sends paused messages once the budget allows (or now, when forced).
    pub(crate) fn resume_paused(&mut self, force: bool) {
        let paused: Vec<i64> = self
            .agents
            .iter()
            .filter(|a| a.paused)
            .filter(|a| force || self.sub_cap_block(a.record.subproject_id).is_none())
            .map(|a| a.record.id)
            .collect();
        if paused.is_empty() || (!force && self.budget_block().is_some()) {
            return;
        }
        for id in paused {
            if let Some(agent) = self.agent_mut(id) {
                agent.paused = false;
                agent.notice(Tone::Info, if force { "Resumed by you (over budget)." } else { "Resumed: the budget allows it again." });
            }
            self.bypass_budget = force;
            self.flush_queued_prompt(id);
            self.bypass_budget = false;
        }
    }

    pub(crate) fn set_budget(&mut self, share_pct: Option<u32>, ceiling_pct: Option<u32>, cx: &mut Context<Self>) {
        if let Some(pct) = share_pct {
            self.budget.share_pct = pct;
            let _ = self.store.set_setting("usage:budget_pct", &pct.to_string());
        }
        if let Some(pct) = ceiling_pct {
            self.budget.ceiling_pct = pct;
            let _ = self.store.set_setting("usage:ceiling_pct", &pct.to_string());
        }
        self.resume_paused(false);
        cx.notify();
    }

    // ---- Models per role ------------------------------------------------------------------

    pub(crate) fn model_for(&self, role_key: &str) -> String {
        self.store.setting(&format!("model:{role_key}")).unwrap_or_else(|| usage::default_model(role_key).to_string())
    }

    fn set_model_for(&mut self, role_key: &str, value: &str, cx: &mut Context<Self>) {
        let _ = self.store.set_setting(&format!("model:{role_key}"), value);
        cx.notify();
    }

    /// Remembers the model list the adapter offers (from `configOptions`).
    pub(crate) fn note_models(&mut self, config_options: &Value) {
        let Some(option) = config_options.as_array().into_iter().flatten().find(|o| o["id"] == "model") else { return };
        let models: Vec<(String, String)> = option["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| {
                let value = o["value"].as_str()?.to_string();
                let name = if value == "default" {
                    format!("Default ({})", o["description"].as_str().unwrap_or("recommended"))
                } else {
                    o["name"].as_str().unwrap_or(&value).to_string()
                };
                Some((value, name))
            })
            .collect();
        if !models.is_empty() && models != self.models {
            let _ = self.store.set_setting("usage:models", &serde_json::to_string(&models).unwrap_or_default());
            self.models = models;
        }
    }

    /// Switches a freshly opened session to its role's model. Returns true if
    /// a switch was requested (the first message waits for it).
    pub(crate) fn apply_model(&mut self, agent_id: i64, config_options: &Value) -> bool {
        let Some(agent) = self.agent(agent_id) else { return false };
        let Some(session_id) = agent.record.session_id.clone() else { return false };
        let wanted = self.model_for(usage::role_key(&agent.role()));
        let current = config_options
            .as_array()
            .into_iter()
            .flatten()
            .find(|o| o["id"] == "model")
            .and_then(|o| o["currentValue"].as_str())
            .unwrap_or("default");
        let offered = self.models.is_empty() || self.models.iter().any(|(v, _)| *v == wanted);
        if wanted == current || !offered {
            return false;
        }
        self.request(
            "session/set_config_option",
            json!({ "sessionId": session_id, "configId": "model", "value": wanted }),
            Pending::SetModel(agent_id),
        );
        true
    }

    pub(crate) fn model_name(&self, value: &str) -> String {
        self.models.iter().find(|(v, _)| v == value).map(|(_, n)| n.clone()).unwrap_or_else(|| {
            // Before the adapter has listed its models: "sonnet" -> "Sonnet".
            value.chars().take(1).flat_map(char::to_uppercase).chain(value.chars().skip(1)).collect()
        })
    }

    // ---- Plans ----------------------------------------------------------------------------

    /// (cost in $, past runs it's based on, share of the week in %).
    pub(crate) fn plan_estimate(&self, plan: &Value) -> (f64, usize, f64) {
        let size = plan["size"].as_str().unwrap_or("small");
        let workstreams = pipeline::workstreams(plan).len().max(1);
        let (cost, runs) = usage::estimate_cost(size, workstreams, &self.runs);
        (cost, runs, cost * self.meter.share_per_usd() * 100.0)
    }

    pub(crate) fn plan_estimate_line(&self, plan: &Value) -> String {
        let (cost, runs, pct) = self.plan_estimate(plan);
        let basis = match runs {
            0 => "a rough guess until Monaged has finished a few pipelines".to_string(),
            1 => "based on 1 earlier pipeline".to_string(),
            n => format!("based on {n} earlier pipelines"),
        };
        let calibrated = if self.meter.calibrated() { "" } else { " (not calibrated yet)" };
        format!("Estimated: about ${cost:.0} API-equivalent, ≈{pct:.1}% of your week{calibrated}; {basis}.")
    }

    /// The plan gate also asks when the plan likely won't fit in what's left of the budget.
    pub(crate) fn budget_gate(&self, plan: &Value, gate: Gate) -> Gate {
        let Gate::Auto(reason) = gate else { return gate };
        if self.budget.share_pct >= 100 {
            return Gate::Auto(reason);
        }
        let (_, _, pct) = self.plan_estimate(plan);
        let left = self.budget.share_pct as f64 - self.meter.live_share() * 100.0;
        if pct > left {
            return Gate::Ask(format!("It may need ≈{pct:.1}% of your week, and only ≈{:.1}% of Monaged's budget is left", left.max(0.0)));
        }
        Gate::Auto(reason)
    }

    pub(crate) fn mark_pipeline_started(&self, sub_id: i64) {
        let _ = self.store.set_setting(&format!("sub:{sub_id}:pipeline_started"), &usage::now_unix().to_string());
    }

    /// A pipeline finished: remember what it cost, for future estimates.
    pub(crate) fn record_pipeline_run(&mut self, sub_id: i64) {
        let Some(plan) = self.pipelines.get(&sub_id).and_then(|p| p.plan.clone()) else { return };
        let since = self.store.setting(&format!("sub:{sub_id}:pipeline_started")).and_then(|v| v.parse().ok()).unwrap_or(0);
        let cost = self.store.sub_cost_since(sub_id, since).unwrap_or(0.0);
        if cost <= 0.0 {
            return;
        }
        let size = plan["size"].as_str().unwrap_or("small").to_string();
        let workstreams = pipeline::workstreams(&plan).len().max(1);
        if self.store.record_run(sub_id, &size, workstreams, cost).is_ok() {
            self.runs.insert(0, (size, workstreams, cost));
        }
    }

    // ---- Rendering --------------------------------------------------------------------------

    fn week_pct(&self) -> Option<f64> {
        self.plan_usage.week.as_ref().map(|w| w.utilization * 100.0)
    }

    /// A thin bar: the whole week (all your use) with Monaged's part on top.
    fn usage_bar(&self, height: f32) -> gpui::Div {
        let week = self.week_pct().unwrap_or(0.0).clamp(0.0, 100.0) as f32;
        // Monaged's part is part of the week, so it never draws past it.
        let mine = ((self.meter.live_share() * 100.0) as f32).clamp(0.0, week);
        div()
            .w_full()
            .h(px(height))
            .rounded_full()
            .bg(rgb(theme::line()))
            .relative()
            .child(div().absolute().left_0().top_0().h_full().rounded_full().bg(rgb(theme::text_3())).w(gpui::relative(week / 100.0)))
            .child(div().absolute().left_0().top_0().h_full().rounded_full().bg(rgb(theme::text())).w(gpui::relative(mine / 100.0)))
            .when(self.budget.share_pct < 100, |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(px(2.))
                        .bg(rgb(theme::red_text()))
                        .left(gpui::relative(self.budget.share_pct as f32 / 100.0)),
                )
            })
    }

    /// Shown above the content while agents wait on the budget.
    pub(crate) fn render_paused_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let paused = self.agents.iter().filter(|a| a.paused).count();
        if paused == 0 {
            return None;
        }
        let message = self
            .budget_block()
            .map(|b| b.message())
            .or_else(|| self.agents.iter().filter(|a| a.paused).find_map(|a| self.sub_cap_block(a.record.subproject_id)))
            .unwrap_or_else(|| "Waiting to resume.".into());
        Some(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(rgb(theme::panel()))
                .border_b_1()
                .border_color(rgb(theme::red_text()))
                .text_size(px(13.))
                .child(div().flex_1().min_w(px(0.)).child(format!(
                    "{paused} agent{} paused. {message}",
                    if paused == 1 { "" } else { "s" }
                )))
                .child(button("resume-anyway", "Resume anyway", theme::raised()).on_click(cx.listener(|this, _, _, cx| {
                    this.resume_paused(true);
                    cx.notify();
                })))
                .child(button("budget-settings", "Budget", theme::raised()).on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))))
                .into_any_element(),
        )
    }

    /// "Sonnet 5.5 · $0.42" for the agent bar.
    pub(crate) fn agent_usage_label(&self, agent: &crate::agent::AgentView) -> Option<String> {
        let model = agent.model.as_deref().map(usage::pretty_model);
        let cost = (agent.cost_total > 0.0).then(|| format!("${:.2}", agent.cost_total));
        match (model, cost) {
            (Some(m), Some(c)) => Some(format!("{m} · {c}")),
            (Some(m), None) => Some(m),
            (None, Some(c)) => Some(c),
            (None, None) => None,
        }
    }

    pub(crate) fn render_usage_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = |text: String| div().text_size(px(13.)).text_color(rgb(theme::text_3())).child(text);
        let mut col = div().flex().flex_col().gap_3();
        match &self.plan_usage.week {
            Some(week) => {
                col = col
                    .child(div().text_size(px(13.)).child(format!(
                        "Your week: {:.0}% used (all your Claude use), resets {}.",
                        week.utilization * 100.0,
                        usage::when(week.resets_at)
                    )))
                    .child(self.usage_bar(8.));
            }
            None => {
                col = col.child(muted("Monaged learns your weekly usage from the first agent turn it runs.".into()));
            }
        }
        if let Some(five) = &self.plan_usage.five_hour {
            col = col.child(muted(format!("5-hour window: {:.0}% used, resets {}.", five.utilization * 100.0, usage::when(five.resets_at))));
        }
        let spent: f64 = self.sub_costs.values().sum::<f64>().max(0.0);
        let k = self.meter.share_per_usd();
        col = col.child(div().text_size(px(13.)).child(format!(
            "Monaged this week: ${spent:.2} API-equivalent, ≈{:.1}% of your week.",
            self.meter.live_share() * 100.0
        )));
        col = col.child(muted(if self.meter.calibrated() {
            format!("Measured: $1 of agent work ≈ {:.2}% of your week (from ${:.0} of Monaged's work).", k * 100.0, self.meter.spent)
        } else {
            format!(
                "Not calibrated yet: assuming $1 ≈ {:.1}% of your week (on the safe side) until Monaged has seen ~$1 of its own work move your weekly usage.",
                k * 100.0
            )
        }));
        let mut by_sub: Vec<(String, f64)> = self
            .sub_costs
            .iter()
            .filter(|(_, c)| **c > 0.0)
            .filter_map(|(id, c)| self.subprojects.iter().find(|s| s.id == *id).map(|s| (s.name.clone(), *c)))
            .collect();
        by_sub.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (name, cost) in by_sub.into_iter().take(8) {
            col = col.child(
                div()
                    .flex()
                    .gap_3()
                    .text_size(px(13.))
                    .child(div().w(px(220.)).flex_none().child(name))
                    .child(div().text_color(rgb(theme::text_3())).child(format!("${cost:.2} · ≈{:.1}%", cost * k * 100.0))),
            );
        }
        if let Some(block) = self.budget_block() {
            col = col.child(div().text_size(px(13.)).text_color(rgb(theme::red_text())).child(block.message()));
        }

        let chips = |id: &'static str, values: &[u32], current: u32, label: fn(u32) -> String, ceiling: bool, cx: &mut Context<Self>| {
            div().flex().flex_wrap().gap_2().children(values.iter().map(|&v| {
                let selected = v == current;
                div()
                    .id(SharedString::from(format!("{id}-{v}")))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .text_size(px(13.))
                    .border_color(rgb(if selected { theme::text() } else { theme::line() }))
                    .when(selected, |d| d.bg(rgb(theme::selected())))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .child(label(v))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if ceiling { this.set_budget(None, Some(v), cx) } else { this.set_budget(Some(v), None, cx) }
                    }))
            }))
        };
        col = col
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Monaged may use"))
            .child(chips("budget", &usage::BUDGET_CHOICES, self.budget.share_pct, |v| if v >= 100 { "No cap".into() } else { format!("{v}% of my week") }, false, cx))
            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child("Pause automatic work when my whole week reaches"))
            .child(chips("ceiling", &usage::CEILING_CHOICES, self.budget.ceiling_pct, |v| if v >= 100 { "Never".into() } else { format!("{v}%") }, true, cx))
            .child(muted(
                "Paused agents keep their next message and continue when the budget allows (a new week, or a higher \
                 budget). Messages you type, and answers you click, always go through."
                    .into(),
            ));
        col.into_any_element()
    }

    pub(crate) fn render_model_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let offered: Vec<&str> = usage::MODEL_CHOICES
            .iter()
            .copied()
            .filter(|v| self.models.is_empty() || self.models.iter().any(|(m, _)| m == v))
            .collect();
        let mut col = div().flex().flex_col().gap_2();
        col = col.child(div().text_size(px(13.)).text_color(rgb(theme::text_3())).child(
            "Which model each role uses. Cheaper models stretch your weekly usage: reading, checking and merging \
             rarely need the strongest one. Applies to agents started (or reopened) from now on.",
        ));
        for (key, label) in usage::ROLE_KEYS {
            let current = self.model_for(key);
            let chips = offered.iter().map(|&value| {
                let selected = value == current;
                let name = self.model_name(value);
                let value = value.to_string();
                div()
                    .id(SharedString::from(format!("model-{key}-{value}")))
                    .px_2p5()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .text_size(px(12.))
                    .border_color(rgb(if selected { theme::text() } else { theme::line() }))
                    .when(selected, |d| d.bg(rgb(theme::selected())))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme::hover())))
                    .child(name)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_model_for(key, &value, cx)))
            });
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(130.)).flex_none().text_size(px(13.)).child(label))
                    .child(div().flex().flex_wrap().gap_1p5().children(chips)),
            );
        }
        col.into_any_element()
    }
}
