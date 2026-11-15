//! Local storage (SQLite in %APPDATA%\Monaged) for projects, subprojects and agents.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};

use crate::llm::Usage;
use crate::pipeline::{self, Pipeline, Stage};
use crate::review::{self, Confidence, Summary};

#[derive(Clone, Debug)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub distro: String,
    pub path: String,
    pub base_branch: String,
}

#[derive(Clone, Debug)]
pub struct Subproject {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    /// "feature" or "standing".
    pub kind: String,
    pub branch: String,
}

#[derive(Clone, Debug)]
pub struct AgentRecord {
    pub id: i64,
    pub subproject_id: i64,
    pub name: String,
    pub branch: String,
    pub worktree: String,
    pub session_id: Option<String>,
    /// Pipeline role (see `pipeline::Role::to_db`); empty for a free agent.
    pub role: String,
    /// Its worktree was cleaned up; kept for history only.
    pub archived: bool,
}

/// One entry on a subproject's coordination board (Phase 4).
#[derive(Clone, Debug)]
pub struct BoardEntry {
    pub id: i64,
    pub subproject_id: i64,
    /// The agent that posted it; None for Monaged itself, the librarian or the developer.
    pub agent_id: Option<i64>,
    pub author: String,
    /// "contract", "decision", "note" or "answer".
    pub kind: String,
    pub title: String,
    pub body: String,
    pub paths: Vec<String>,
    pub at: i64,
    /// An answer that already reached the agent that asked (its id).
    pub delivered_to: Option<i64>,
}

pub struct Store {
    conn: Connection,
    /// Set when a damaged database was replaced by a backup at startup.
    pub restored: Option<String>,
}

/// One line in a subproject's timeline. `kind` is "turn", "review", "agent",
/// "run", "board:<kind>" or "event:<kind>"; `data` is a few fields joined by '|'.
#[derive(Clone, Debug)]
pub struct TimelineEvent {
    pub at: i64,
    pub kind: String,
    pub agent_id: Option<i64>,
    pub data: String,
}

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS projects (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    distro TEXT NOT NULL,
    path TEXT NOT NULL,
    base_branch TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS subprojects (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    branch TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS agents (
    id INTEGER PRIMARY KEY,
    subproject_id INTEGER NOT NULL REFERENCES subprojects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    branch TEXT NOT NULL,
    worktree TEXT NOT NULL,
    session_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS reviews (
    agent_id INTEGER PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    summary TEXT,
    status TEXT,
    next_step TEXT,
    score INTEGER,
    verdict TEXT,
    reasons TEXT,
    issues TEXT,
    checks TEXT,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS llm_calls (
    id INTEGER PRIMARY KEY,
    agent_id INTEGER,
    kind TEXT NOT NULL,
    via TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS board (
    id INTEGER PRIMARY KEY,
    subproject_id INTEGER NOT NULL REFERENCES subprojects(id) ON DELETE CASCADE,
    agent_id INTEGER,
    author TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    paths TEXT NOT NULL DEFAULT '[]',
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS claims (
    agent_id INTEGER NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    pattern TEXT NOT NULL,
    PRIMARY KEY (agent_id, pattern)
);
CREATE TABLE IF NOT EXISTS turns (
    id INTEGER PRIMARY KEY,
    agent_id INTEGER NOT NULL,
    subproject_id INTEGER NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cache_write_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY,
    subproject_id INTEGER NOT NULL,
    agent_id INTEGER,
    kind TEXT NOT NULL,
    text TEXT NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS checkpoints (
    agent_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    head TEXT NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    PRIMARY KEY (agent_id, ordinal)
);
CREATE TABLE IF NOT EXISTS env_vars (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    env TEXT NOT NULL,
    name TEXT NOT NULL,
    secret INTEGER NOT NULL DEFAULT 1,
    agents INTEGER NOT NULL DEFAULT 1,
    -- Encrypted with Windows DPAPI (this Windows account only), never plain text.
    value BLOB NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    UNIQUE (project_id, env, name)
);
CREATE TABLE IF NOT EXISTS env_audit (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    what TEXT NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS queued_prompts (
    agent_id INTEGER PRIMARY KEY,
    text TEXT NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS pipeline_runs (
    id INTEGER PRIMARY KEY,
    subproject_id INTEGER NOT NULL,
    size TEXT NOT NULL,
    workstreams INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE TABLE IF NOT EXISTS pipelines (
    subproject_id INTEGER PRIMARY KEY REFERENCES subprojects(id) ON DELETE CASCADE,
    stage TEXT NOT NULL,
    request TEXT NOT NULL,
    auto_mode INTEGER NOT NULL DEFAULT 0,
    research TEXT,
    verifications TEXT,
    plan TEXT,
    done TEXT,
    note TEXT,
    updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
";

/// Columns added after the first release; ALTER fails harmlessly when they exist.
const MIGRATIONS: &[&str] = &[
    "ALTER TABLE agents ADD COLUMN role TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE agents ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE board ADD COLUMN delivered_to INTEGER",
    "ALTER TABLE pipelines ADD COLUMN gate TEXT",
];

/// How many startup backups to keep.
const BACKUPS: usize = 5;

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Opens a database file and checks it's whole.
fn open_checked(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path).context("could not open the Monaged database")?;
    conn.execute_batch(SCHEMA).context("could not set up the database")?;
    for migration in MIGRATIONS {
        let _ = conn.execute(migration, []);
    }
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).context("could not check the database")?;
    anyhow::ensure!(check == "ok", "the database is damaged ({check})");
    Ok(conn)
}

/// Where Monaged keeps its database, backups and log: %APPDATA%\Monaged.
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("Monaged")
}

impl Store {
    /// Opens the database. A damaged one is set aside and the newest good
    /// backup takes its place (`restored` says which); every start makes a
    /// fresh backup in `backups\` (the last five are kept).
    pub fn open() -> Result<Self> {
        let dir = PathBuf::from(std::env::var("APPDATA").context("APPDATA is not set")?).join("Monaged");
        std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let path = dir.join("monaged.db");
        let (conn, restored) = match open_checked(&path) {
            Ok(conn) => (conn, None),
            Err(err) => {
                let backups = Self::backups(&dir);
                let Some(good) = backups.iter().rev().find(|b| open_checked(b).is_ok()) else {
                    return Err(err.context("and there's no good backup in the backups folder"));
                };
                let kept = dir.join(format!("monaged-damaged-{}.db", now_secs()));
                std::fs::rename(&path, &kept).with_context(|| format!("couldn't set the damaged database aside ({err:#})"))?;
                std::fs::copy(good, &path).context("couldn't put the backup in place")?;
                let name = good.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                (open_checked(&path)?, Some(format!("{name} (the damaged file is kept as {})", kept.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())))
            }
        };
        let store = Self { conn, restored };
        store.backup(&dir);
        Ok(store)
    }

    fn backups(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut list: Vec<PathBuf> = std::fs::read_dir(dir.join("backups"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("monaged-") && n.to_string_lossy().ends_with(".db")))
            .collect();
        list.sort();
        list
    }

    /// A consistent copy (VACUUM INTO), even while another process uses the file.
    fn backup(&self, dir: &std::path::Path) {
        let folder = dir.join("backups");
        if std::fs::create_dir_all(&folder).is_err() {
            return;
        }
        let target = folder.join(format!("monaged-{}.db", now_secs()));
        if self.conn.execute("VACUUM INTO ?1", params![target.to_string_lossy()]).is_err() {
            return;
        }
        let list = Self::backups(dir);
        for old in list.iter().take(list.len().saturating_sub(BACKUPS)) {
            let _ = std::fs::remove_file(old);
        }
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare("SELECT id, name, distro, path, base_branch FROM projects ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Project { id: r.get(0)?, name: r.get(1)?, distro: r.get(2)?, path: r.get(3)?, base_branch: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn subprojects(&self) -> Result<Vec<Subproject>> {
        let mut stmt = self.conn.prepare("SELECT id, project_id, name, kind, branch FROM subprojects ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(Subproject { id: r.get(0)?, project_id: r.get(1)?, name: r.get(2)?, kind: r.get(3)?, branch: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn agents(&self) -> Result<Vec<AgentRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, subproject_id, name, branch, worktree, session_id, role, archived FROM agents ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |r| {
            Ok(AgentRecord {
                id: r.get(0)?,
                subproject_id: r.get(1)?,
                name: r.get(2)?,
                branch: r.get(3)?,
                worktree: r.get(4)?,
                session_id: r.get(5)?,
                role: r.get(6)?,
                archived: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn add_project(&self, name: &str, distro: &str, path: &str, base_branch: &str) -> Result<Project> {
        self.conn.execute(
            "INSERT INTO projects (name, distro, path, base_branch) VALUES (?1, ?2, ?3, ?4)",
            params![name, distro, path, base_branch],
        )?;
        Ok(Project {
            id: self.conn.last_insert_rowid(),
            name: name.into(),
            distro: distro.into(),
            path: path.into(),
            base_branch: base_branch.into(),
        })
    }

    pub fn add_subproject(&self, project_id: i64, name: &str, kind: &str, branch: &str) -> Result<Subproject> {
        self.conn.execute(
            "INSERT INTO subprojects (project_id, name, kind, branch) VALUES (?1, ?2, ?3, ?4)",
            params![project_id, name, kind, branch],
        )?;
        Ok(Subproject { id: self.conn.last_insert_rowid(), project_id, name: name.into(), kind: kind.into(), branch: branch.into() })
    }

    pub fn add_agent(&self, subproject_id: i64, name: &str, branch: &str, worktree: &str, role: &str) -> Result<AgentRecord> {
        self.conn.execute(
            "INSERT INTO agents (subproject_id, name, branch, worktree, role) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![subproject_id, name, branch, worktree, role],
        )?;
        Ok(AgentRecord {
            id: self.conn.last_insert_rowid(),
            subproject_id,
            name: name.into(),
            branch: branch.into(),
            worktree: worktree.into(),
            session_id: None,
            role: role.into(),
            archived: false,
        })
    }

    pub fn archive_agent(&self, agent_id: i64) -> Result<()> {
        self.conn.execute("UPDATE agents SET archived = 1 WHERE id = ?1", params![agent_id])?;
        Ok(())
    }

    pub fn unarchive_agent(&self, agent_id: i64) -> Result<()> {
        self.conn.execute("UPDATE agents SET archived = 0 WHERE id = ?1", params![agent_id])?;
        Ok(())
    }

    pub fn rename_agent(&self, agent_id: i64, name: &str) -> Result<()> {
        self.conn.execute("UPDATE agents SET name = ?1 WHERE id = ?2", params![name, agent_id])?;
        Ok(())
    }

    pub fn set_project_base(&self, project_id: i64, base_branch: &str) -> Result<()> {
        self.conn.execute("UPDATE projects SET base_branch = ?1 WHERE id = ?2", params![base_branch, project_id])?;
        Ok(())
    }

    // ---- Pipelines -----------------------------------------------------------

    pub fn save_pipeline(&self, p: &Pipeline) -> Result<()> {
        let json = |v: &Option<serde_json::Value>| v.as_ref().map(|v| v.to_string());
        self.conn.execute(
            "INSERT INTO pipelines (subproject_id, stage, request, auto_mode, research, verifications, plan, done, note, gate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(subproject_id) DO UPDATE SET stage = ?2, request = ?3, auto_mode = ?4, research = ?5,
             verifications = ?6, plan = ?7, done = ?8, note = ?9, gate = ?10, updated_at = strftime('%s','now')",
            params![
                p.subproject_id,
                p.stage.as_str(),
                p.request,
                p.auto_mode,
                json(&p.research),
                pipeline::to_json(&p.verifications),
                json(&p.plan),
                serde_json::to_string(&p.done)?,
                p.note,
                p.gate.to_json().to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn pipelines(&self) -> Result<HashMap<i64, Pipeline>> {
        let mut stmt = self
            .conn
            .prepare("SELECT subproject_id, stage, request, auto_mode, research, verifications, plan, done, note, gate FROM pipelines")?;
        let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let rows = stmt.query_map([], |r| {
            let subproject_id: i64 = r.get(0)?;
            Ok((
                subproject_id,
                Pipeline {
                    subproject_id,
                    stage: Stage::parse(&r.get::<_, String>(1)?),
                    request: r.get(2)?,
                    auto_mode: r.get(3)?,
                    research: parse(r.get(4)?),
                    verifications: parse(r.get(5)?)
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_else(pipeline::empty_verifications),
                    plan: parse(r.get(6)?),
                    done: r.get::<_, Option<String>>(7)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                    note: r.get(8)?,
                    gate: r.get::<_, Option<String>>(9)?.map(|t| pipeline::QualityGate::from_json(&t)).unwrap_or_default(),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_agent_session(&self, agent_id: i64, session_id: &str) -> Result<()> {
        self.conn.execute("UPDATE agents SET session_id = ?1 WHERE id = ?2", params![session_id, agent_id])?;
        Ok(())
    }

    // ---- Reviews -----------------------------------------------------------

    pub fn save_summary(&self, agent_id: i64, summary: &Summary) -> Result<()> {
        self.conn.execute(
            "INSERT INTO reviews (agent_id, summary, status, next_step) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id) DO UPDATE SET summary = ?2, status = ?3, next_step = ?4, updated_at = strftime('%s','now')",
            params![agent_id, summary.text, summary.status, summary.next_step],
        )?;
        Ok(())
    }

    pub fn save_confidence(&self, agent_id: i64, c: &Confidence) -> Result<()> {
        let reasons = serde_json::to_string(&c.reasons)?;
        let issues = review::issues_to_json(&c.issues).to_string();
        self.conn.execute(
            "INSERT INTO reviews (agent_id, score, verdict, reasons, issues, checks) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(agent_id) DO UPDATE SET score = ?2, verdict = ?3, reasons = ?4, issues = ?5, checks = ?6,
             updated_at = strftime('%s','now')",
            params![agent_id, c.score, c.verdict, reasons, issues, c.checks],
        )?;
        Ok(())
    }

    pub fn reviews(&self) -> Result<HashMap<i64, (Option<Summary>, Option<Confidence>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent_id, summary, status, next_step, score, verdict, reasons, issues, checks FROM reviews")?;
        let rows = stmt.query_map([], |r| {
            let summary = r.get::<_, Option<String>>(1)?.map(|text| Summary {
                text,
                status: r.get::<_, Option<String>>(2).ok().flatten().unwrap_or_default(),
                next_step: r.get::<_, Option<String>>(3).ok().flatten().unwrap_or_default(),
            });
            let confidence = r.get::<_, Option<u8>>(4)?.map(|score| Confidence {
                score,
                verdict: r.get::<_, Option<String>>(5).ok().flatten().unwrap_or_default(),
                reasons: r
                    .get::<_, Option<String>>(6)
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                issues: r
                    .get::<_, Option<String>>(7)
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .map(|v| review::issues_from_json(&v))
                    .unwrap_or_default(),
                checks: r.get::<_, Option<String>>(8).ok().flatten().unwrap_or_default(),
            });
            Ok((r.get::<_, i64>(0)?, (summary, confidence)))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Haiku usage -------------------------------------------------------

    pub fn log_llm_call(&self, agent_id: i64, kind: &str, via: &str, usage: &Usage) -> Result<()> {
        self.conn.execute(
            "INSERT INTO llm_calls (agent_id, kind, via, input_tokens, output_tokens, cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![agent_id, kind, via, usage.input_tokens as i64, usage.output_tokens as i64, usage.cost_usd],
        )?;
        Ok(())
    }

    /// Total Haiku cost so far: (paid from the API key, equivalent on the Max plan).
    pub fn llm_spend(&self) -> Result<(f64, f64)> {
        let sum = |via: &str| -> Result<f64> {
            Ok(self
                .conn
                .query_row("SELECT COALESCE(SUM(cost_usd), 0) FROM llm_calls WHERE via = ?1", params![via], |r| r.get(0))?)
        };
        Ok((sum("api")?, sum("max")?))
    }

    // ---- Coordination board ------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn board_post(
        &self,
        subproject_id: i64,
        agent_id: Option<i64>,
        author: &str,
        kind: &str,
        title: &str,
        body: &str,
        paths: &[String],
        delivered_to: Option<i64>,
    ) -> Result<BoardEntry> {
        self.conn.execute(
            "INSERT INTO board (subproject_id, agent_id, author, kind, title, body, paths, delivered_to)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![subproject_id, agent_id, author, kind, title, body, serde_json::to_string(paths)?, delivered_to],
        )?;
        let id = self.conn.last_insert_rowid();
        let at = self.conn.query_row("SELECT at FROM board WHERE id = ?1", params![id], |r| r.get(0))?;
        Ok(BoardEntry {
            id,
            subproject_id,
            agent_id,
            author: author.into(),
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            paths: paths.to_vec(),
            at,
            delivered_to,
        })
    }

    pub fn board(&self) -> Result<Vec<BoardEntry>> {
        let mut stmt =
            self.conn.prepare("SELECT id, subproject_id, agent_id, author, kind, title, body, paths, at, delivered_to FROM board ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(BoardEntry {
                id: r.get(0)?,
                subproject_id: r.get(1)?,
                agent_id: r.get(2)?,
                author: r.get(3)?,
                kind: r.get(4)?,
                title: r.get(5)?,
                body: r.get(6)?,
                paths: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                at: r.get(8)?,
                delivered_to: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_board_entry(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM board WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn add_claim(&self, agent_id: i64, pattern: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO claims (agent_id, pattern) VALUES (?1, ?2)", params![agent_id, pattern])?;
        Ok(())
    }

    /// Every claim as (agent id, path or glob).
    pub fn claims(&self) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare("SELECT agent_id, pattern FROM claims ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Usage (Phase 6) -----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn record_turn(
        &self,
        agent_id: i64,
        subproject_id: i64,
        model: &str,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        cost_usd: f64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO turns (agent_id, subproject_id, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![agent_id, subproject_id, model, input as i64, output as i64, cache_read as i64, cache_write as i64, cost_usd],
        )?;
        Ok(())
    }

    /// API-equivalent cost per agent, all time.
    pub fn cost_by_agent(&self) -> Result<HashMap<i64, f64>> {
        let mut stmt = self.conn.prepare("SELECT agent_id, SUM(cost_usd) FROM turns GROUP BY agent_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// API-equivalent cost per subproject since a time (unix seconds).
    pub fn cost_by_sub_since(&self, since: i64) -> Result<HashMap<i64, f64>> {
        let mut stmt = self.conn.prepare("SELECT subproject_id, SUM(cost_usd) FROM turns WHERE at >= ?1 GROUP BY subproject_id")?;
        let rows = stmt.query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn sub_cost_since(&self, subproject_id: i64, since: i64) -> Result<f64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM turns WHERE subproject_id = ?1 AND at >= ?2",
            params![subproject_id, since],
            |r| r.get(0),
        )?)
    }

    pub fn record_run(&self, subproject_id: i64, size: &str, workstreams: usize, cost_usd: f64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pipeline_runs (subproject_id, size, workstreams, cost_usd) VALUES (?1, ?2, ?3, ?4)",
            params![subproject_id, size, workstreams as i64, cost_usd],
        )?;
        Ok(())
    }

    pub fn runs(&self) -> Result<Vec<crate::usage::Run>> {
        let mut stmt = self.conn.prepare("SELECT size, workstreams, cost_usd FROM pipeline_runs ORDER BY id DESC LIMIT 50")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as usize, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Timeline ------------------------------------------------------------

    /// Something worth seeing later in a subproject's timeline (a merge, a push…).
    pub fn log_event(&self, subproject_id: i64, agent_id: Option<i64>, kind: &str, text: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events (subproject_id, agent_id, kind, text) VALUES (?1, ?2, ?3, ?4)",
            params![subproject_id, agent_id, kind, text],
        )?;
        Ok(())
    }

    /// A subproject's history, newest first: turns, board posts, reviews,
    /// agents created, pipeline runs and logged events.
    pub fn timeline(&self, subproject_id: i64, limit: usize) -> Result<Vec<TimelineEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT at, 'turn', agent_id, model || '|' || printf('%.4f', cost_usd) || '|' || output_tokens FROM turns WHERE subproject_id = ?1
             UNION ALL SELECT at, 'board:' || kind, agent_id, author || '|' || title FROM board WHERE subproject_id = ?1
             UNION ALL SELECT r.updated_at, 'review', r.agent_id, COALESCE(r.score, 0) || '|' || COALESCE(r.verdict, '') FROM reviews r JOIN agents a ON a.id = r.agent_id WHERE a.subproject_id = ?1 AND r.score IS NOT NULL
             UNION ALL SELECT created_at, 'agent', id, name || '|' || role FROM agents WHERE subproject_id = ?1
             UNION ALL SELECT at, 'run', NULL, size || '|' || workstreams || '|' || printf('%.2f', cost_usd) FROM pipeline_runs WHERE subproject_id = ?1
             UNION ALL SELECT at, 'event:' || kind, agent_id, text FROM events WHERE subproject_id = ?1
             ORDER BY 1 DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![subproject_id, limit as i64], |r| {
            Ok(TimelineEvent { at: r.get(0)?, kind: r.get(1)?, agent_id: r.get(2)?, data: r.get::<_, Option<String>>(3)?.unwrap_or_default() })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Checkpoints -----------------------------------------------------------

    pub fn save_checkpoint(&self, agent_id: i64, ordinal: i64, commit: &str, head: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO checkpoints (agent_id, ordinal, commit_sha, head) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id, ordinal) DO UPDATE SET commit_sha = excluded.commit_sha, head = excluded.head, at = strftime('%s','now')",
            params![agent_id, ordinal, commit, head],
        )?;
        Ok(())
    }

    /// (agent, ordinal) -> (commit, head)
    pub fn checkpoints(&self) -> Result<Vec<(i64, i64, String, String)>> {
        let mut stmt = self.conn.prepare("SELECT agent_id, ordinal, commit_sha, head FROM checkpoints")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Environment variables (values are DPAPI blobs) --------------------

    /// (id, project, env, name, secret, agents, encrypted value, updated)
    pub fn env_vars(&self) -> Result<Vec<(i64, i64, String, String, bool, bool, Vec<u8>, i64)>> {
        let mut stmt = self.conn.prepare("SELECT id, project_id, env, name, secret, agents, value, updated_at FROM env_vars ORDER BY project_id, env, name")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0, r.get::<_, i64>(5)? != 0, r.get(6)?, r.get(7)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_env_var(&self, project_id: i64, env: &str, name: &str, secret: bool, agents: bool, value: &[u8]) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO env_vars (project_id, env, name, secret, agents, value) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(project_id, env, name) DO UPDATE SET secret = excluded.secret, agents = excluded.agents, value = excluded.value, updated_at = strftime('%s','now')",
            params![project_id, env, name, secret as i64, agents as i64, value],
        )?;
        Ok(self.conn.query_row("SELECT id FROM env_vars WHERE project_id = ?1 AND env = ?2 AND name = ?3", params![project_id, env, name], |r| r.get(0))?)
    }

    pub fn set_env_flags(&self, id: i64, secret: bool, agents: bool) -> Result<()> {
        self.conn.execute("UPDATE env_vars SET secret = ?2, agents = ?3 WHERE id = ?1", params![id, secret as i64, agents as i64])?;
        Ok(())
    }

    pub fn delete_env_var(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM env_vars WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn delete_env(&self, project_id: i64, env: &str) -> Result<()> {
        self.conn.execute("DELETE FROM env_vars WHERE project_id = ?1 AND env = ?2", params![project_id, env])?;
        Ok(())
    }

    /// What happened to a project's variables (never their values).
    pub fn env_audit(&self, project_id: i64, what: &str) -> Result<()> {
        self.conn.execute("INSERT INTO env_audit (project_id, what) VALUES (?1, ?2)", params![project_id, what])?;
        Ok(())
    }

    /// Newest first: (when, what)
    pub fn env_audit_log(&self, project_id: i64, limit: i64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare("SELECT at, what FROM env_audit WHERE project_id = ?1 ORDER BY id DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![project_id, limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Queued messages ----------------------------------------------------

    /// A message waiting to go out survives a restart.
    pub fn save_queued(&self, agent_id: i64, text: Option<&str>) -> Result<()> {
        match text {
            Some(text) => self.conn.execute(
                "INSERT INTO queued_prompts (agent_id, text) VALUES (?1, ?2) ON CONFLICT(agent_id) DO UPDATE SET text = excluded.text, at = strftime('%s','now')",
                params![agent_id, text],
            )?,
            None => self.conn.execute("DELETE FROM queued_prompts WHERE agent_id = ?1", params![agent_id])?,
        };
        Ok(())
    }

    pub fn queued(&self) -> Result<HashMap<i64, String>> {
        let mut stmt = self.conn.prepare("SELECT agent_id, text FROM queued_prompts")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- Settings ----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).ok()
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }
}

