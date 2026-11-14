//! Phase 6: usage awareness, the pure parts.
//!
//! What we can measure (all from the Claude adapter, no extra API calls):
//! - per turn: the API-equivalent cost (`usage_update.cost`, cumulative per
//!   session) and token counts (the `session/prompt` result's `usage`);
//! - now and then: how much of the plan's weekly limit is used, for *all*
//!   your Claude use (`usage_update._meta["_claude/rateLimit"]`, a fraction
//!   in roughly 1% steps, with the reset time).
//!
//! Monaged's own share of the week isn't reported, so it's estimated:
//! increases in weekly usage while Monaged was busy the whole time count as
//! Monaged's, and they calibrate how much of the week one dollar of
//! API-equivalent cost is. Where Monaged was idle in between, only the part
//! its cost explains is counted. Your own parallel use can only make the
//! estimate too high, never too low, which is the safe side for a budget.

use serde_json::{Value, json};

use crate::pipeline::Role;

/// Before there's data, assume one dollar of API-equivalent cost is 0.2% of a
/// week. Deliberately on the high side so an uncalibrated budget errs safe.
pub const DEFAULT_SHARE_PER_USD: f64 = 0.002;
pub const DEFAULT_BUDGET_PCT: u32 = 25;
pub const DEFAULT_CEILING_PCT: u32 = 90;
pub const BUDGET_CHOICES: [u32; 5] = [10, 25, 50, 75, 100];
pub const CEILING_CHOICES: [u32; 4] = [80, 90, 95, 100];
pub const WEEK_SECS: i64 = 7 * 86_400;

// ---- The plan's limits -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    /// 0.0..=1.0
    pub utilization: f64,
    /// Unix seconds.
    pub resets_at: i64,
    /// "allowed", "allowed_warning" or "rejected".
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanUsage {
    pub week: Option<Window>,
    pub five_hour: Option<Window>,
    pub status: String,
}

impl PlanUsage {
    pub fn limited(&self) -> Option<i64> {
        [&self.week, &self.five_hour].into_iter().flatten().find(|w| w.status == "rejected").map(|w| w.resets_at)
    }
}

/// Reads `_meta["_claude/rateLimit"]` from a `usage_update`.
pub fn parse_rate_limit(v: &Value) -> Option<PlanUsage> {
    if !v.is_object() {
        return None;
    }
    let window = |w: &Value| -> Option<Window> {
        Some(Window {
            utilization: w["utilization"].as_f64()?,
            resets_at: w["resetsAt"].as_i64()?,
            status: w["status"].as_str().unwrap_or("allowed").to_string(),
        })
    };
    let windows = &v["unifiedWindows"];
    let mut week = window(&windows["seven_day"]);
    let mut five_hour = window(&windows["five_hour"]);
    // Older shape: one window at the top level.
    if week.is_none() && five_hour.is_none() {
        let top = window(v);
        match v["rateLimitType"].as_str() {
            Some("five_hour") => five_hour = top,
            _ => week = top,
        }
    }
    Some(PlanUsage { week, five_hour, status: v["status"].as_str().unwrap_or("allowed").to_string() })
}

// ---- Monaged's share of the week --------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meter {
    /// The week's reset time; identifies the week.
    pub week_key: i64,
    /// Last weekly usage reading (fraction).
    pub last: f64,
    /// Monaged's share of the week so far, settled at readings (fraction).
    pub share: f64,
    /// Monaged's API-equivalent cost since the last reading.
    pub pending_cost: f64,
    /// Monaged was completely idle at some point since the last reading.
    pub idle_gap: bool,
    /// Calibration over busy-only stretches: weekly usage gained, and cost spent.
    pub gained: f64,
    pub spent: f64,
}

impl Meter {
    /// Share of the week per dollar of API-equivalent cost.
    pub fn share_per_usd(&self) -> f64 {
        if self.spent < 1.0 {
            return DEFAULT_SHARE_PER_USD;
        }
        // Readings come in ~1% steps; count half a step for the one in progress.
        (self.gained + 0.005) / self.spent
    }

    pub fn calibrated(&self) -> bool {
        self.spent >= 1.0
    }

    pub fn add_cost(&mut self, usd: f64) {
        if usd > 0.0 {
            self.pending_cost += usd;
        }
    }

    pub fn went_idle(&mut self) {
        self.idle_gap = true;
    }

    /// A new weekly reading. Returns the part counted as Monaged's.
    pub fn reading(&mut self, utilization: f64, resets_at: i64) -> f64 {
        if resets_at != self.week_key {
            // The first reading, or a new week.
            *self = Meter { week_key: resets_at, last: utilization, gained: self.gained, spent: self.spent, ..Meter::default() };
            return 0.0;
        }
        let delta = (utilization - self.last).max(0.0);
        let counted = if self.idle_gap {
            delta.min(self.pending_cost * self.share_per_usd())
        } else {
            if self.pending_cost > 0.0 {
                self.gained += delta;
                self.spent += self.pending_cost;
            }
            delta
        };
        self.share += counted;
        self.last = utilization;
        self.pending_cost = 0.0;
        self.idle_gap = false;
        counted
    }

    /// The week ended without a reading: start the next one.
    pub fn roll_over(&mut self, now: i64) -> bool {
        if self.week_key == 0 || now < self.week_key {
            return false;
        }
        let mut next = self.week_key;
        while next <= now {
            next += WEEK_SECS;
        }
        *self = Meter { week_key: next, gained: self.gained, spent: self.spent, idle_gap: true, ..Meter::default() };
        true
    }

    /// Settled share plus an estimate for the cost since the last reading.
    pub fn live_share(&self) -> f64 {
        self.share + self.pending_cost * self.share_per_usd()
    }

    pub fn to_json(&self) -> String {
        json!({
            "week_key": self.week_key, "last": self.last, "share": self.share, "pending_cost": self.pending_cost,
            "idle_gap": self.idle_gap, "gained": self.gained, "spent": self.spent,
        })
        .to_string()
    }

    pub fn from_json(text: &str) -> Self {
        let v: Value = serde_json::from_str(text).unwrap_or_default();
        Meter {
            week_key: v["week_key"].as_i64().unwrap_or(0),
            last: v["last"].as_f64().unwrap_or(0.0),
            share: v["share"].as_f64().unwrap_or(0.0),
            pending_cost: v["pending_cost"].as_f64().unwrap_or(0.0),
            // After a restart we don't know what happened in between.
            idle_gap: true,
            gained: v["gained"].as_f64().unwrap_or(0.0),
            spent: v["spent"].as_f64().unwrap_or(0.0),
        }
    }
}

/// How much of a cumulative session cost is new.
pub fn cost_delta(seen: Option<f64>, now: f64) -> f64 {
    match seen {
        Some(seen) if now >= seen => now - seen,
        // First figure, or the session restarted and counts from zero again.
        _ => now,
    }
}

// ---- Budget -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    /// Monaged may use this much of the week (100 = no cap of its own).
    pub share_pct: u32,
    /// Pause automatic work once the whole week (all your use) reaches this.
    pub ceiling_pct: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { share_pct: DEFAULT_BUDGET_PCT, ceiling_pct: DEFAULT_CEILING_PCT }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// Monaged used its share of the week.
    Share { used_pct: f64, budget_pct: u32, resets_at: i64 },
    /// The whole week is past the ceiling.
    Ceiling { week_pct: f64, ceiling_pct: u32, resets_at: i64 },
    /// The plan itself refused (limit reached).
    Limited { resets_at: i64 },
}

impl Block {
    pub fn message(&self) -> String {
        match self {
            Block::Share { used_pct, budget_pct, resets_at } => format!(
                "Monaged has used about {used_pct:.0}% of your week, its budget is {budget_pct}%. Automatic work is paused until {}.",
                when(*resets_at)
            ),
            Block::Ceiling { week_pct, ceiling_pct, resets_at } => format!(
                "Your week is {week_pct:.0}% used (the pause point is {ceiling_pct}%). Automatic work is paused until {}.",
                when(*resets_at)
            ),
            Block::Limited { resets_at } => format!("Your Claude plan's limit is reached until {}.", when(*resets_at)),
        }
    }
}

pub fn check(budget: Budget, meter: &Meter, plan: &PlanUsage) -> Option<Block> {
    if let Some(resets_at) = plan.limited() {
        return Some(Block::Limited { resets_at });
    }
    let resets_at = plan.week.as_ref().map(|w| w.resets_at).unwrap_or(meter.week_key);
    if let Some(week) = &plan.week {
        let week_pct = week.utilization * 100.0;
        if budget.ceiling_pct < 100 && week_pct >= budget.ceiling_pct as f64 {
            return Some(Block::Ceiling { week_pct, ceiling_pct: budget.ceiling_pct, resets_at });
        }
    }
    let used_pct = meter.live_share() * 100.0;
    if budget.share_pct < 100 && used_pct >= budget.share_pct as f64 {
        return Some(Block::Share { used_pct, budget_pct: budget.share_pct, resets_at });
    }
    None
}

/// "Sat 21 Nov 14:00" in local time (Windows' clock via chrono-free math is
/// awkward, so UTC offset comes from the caller's environment once).
pub fn when(unix: i64) -> String {
    if unix <= 0 {
        return "the week resets".into();
    }
    let local = unix + local_offset_secs();
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(m - 1) as usize];
    let _ = y;
    format!("{weekday} {d} {month} {:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// The local UTC offset, read once from Windows (falls back to UTC).
fn local_offset_secs() -> i64 {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        #[cfg(windows)]
        {
            #[repr(C)]
            struct SystemTime([u16; 8]);
            #[repr(C)]
            struct TimeZoneInformation {
                bias: i32,
                standard_name: [u16; 32],
                standard_date: SystemTime,
                standard_bias: i32,
                daylight_name: [u16; 32],
                daylight_date: SystemTime,
                daylight_bias: i32,
            }
            unsafe extern "system" {
                fn GetTimeZoneInformation(info: *mut TimeZoneInformation) -> u32;
            }
            let mut info: TimeZoneInformation = unsafe { std::mem::zeroed() };
            let mode = unsafe { GetTimeZoneInformation(&mut info) };
            let extra = if mode == 2 { info.daylight_bias } else { info.standard_bias };
            -((info.bias + extra) as i64) * 60
        }
        #[cfg(not(windows))]
        {
            0
        }
    })
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ---- Models per role ----------------------------------------------------------------

/// Settings key per role.
pub fn role_key(role: &Role) -> &'static str {
    match role {
        Role::Free => "free",
        Role::Researcher => "researcher",
        Role::Verifier(_) => "verifier",
        Role::Planner => "planner",
        Role::Builder(_) => "builder",
        Role::Integrator => "integrator",
    }
}

pub const ROLE_KEYS: [(&str, &str); 6] = [
    ("researcher", "Researcher"),
    ("verifier", "Verifiers"),
    ("planner", "Planner"),
    ("builder", "Builders"),
    ("integrator", "Integrator"),
    ("free", "Your own agents"),
];

/// Reading, checking and merging don't need the strongest model.
pub fn default_model(role_key: &str) -> &'static str {
    match role_key {
        "researcher" | "verifier" | "integrator" => "sonnet",
        _ => "default",
    }
}

/// The choices offered in Settings (if the adapter lists them).
pub const MODEL_CHOICES: [&str; 4] = ["default", "opus", "sonnet", "haiku"];

/// `claude-sonnet-5-5` -> `Sonnet 5.5`; `claude-haiku-4-5-20251001` -> `Haiku 4.5`.
pub fn pretty_model(id: &str) -> String {
    let mut parts = id.trim_start_matches("claude-").split('-');
    let Some(family) = parts.next() else { return id.to_string() };
    let mut name: String = family.chars().take(1).flat_map(char::to_uppercase).chain(family.chars().skip(1)).collect();
    let version: Vec<&str> = parts.filter(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

// ---- Estimates ------------------------------------------------------------------------

/// A finished pipeline: (plan size, number of workstreams, API-equivalent cost).
pub type Run = (String, usize, f64);

/// Rough per-workstream cost before there's any history.
fn default_per_workstream(size: &str) -> f64 {
    match size {
        "large" => 20.0,
        "medium" => 8.0,
        _ => 3.0,
    }
}

/// Expected API-equivalent cost of a plan, and how many past runs it's based on.
pub fn estimate_cost(size: &str, workstreams: usize, history: &[Run]) -> (f64, usize) {
    let ws = workstreams.max(1) as f64;
    let same: Vec<&Run> = history.iter().filter(|r| r.0 == size && r.2 > 0.0).collect();
    let pool: Vec<&Run> = if same.is_empty() { history.iter().filter(|r| r.2 > 0.0).collect() } else { same };
    if pool.is_empty() {
        // Research, verification, planning and integration, plus the builders.
        return (4.0 + default_per_workstream(size) * ws, 0);
    }
    let per_ws = pool.iter().map(|r| r.2).sum::<f64>() / pool.iter().map(|r| r.1.max(1) as f64).sum::<f64>();
    (per_ws * ws, pool.len())
}
