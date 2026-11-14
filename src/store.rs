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
];

impl Store {
    pub fn open() -> Result<Self> {
        let dir = PathBuf::from(std::env::var("APPDATA").context("APPDATA is not set")?).join("Monaged");
        std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
        let conn = Connection::open(dir.join("monaged.db")).context("could not open the Monaged database")?;
        conn.execute_batch(SCHEMA).context("could not set up the database")?;
        for migration in MIGRATIONS {
            let _ = conn.execute(migration, []);
        }
        Ok(Self { conn })
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
            "INSERT INTO pipelines (subproject_id, stage, request, auto_mode, research, verifications, plan, done, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(subproject_id) DO UPDATE SET stage = ?2, request = ?3, auto_mode = ?4, research = ?5,
             verifications = ?6, plan = ?7, done = ?8, note = ?9, updated_at = strftime('%s','now')",
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
            ],
        )?;
        Ok(())
    }

    pub fn pipelines(&self) -> Result<HashMap<i64, Pipeline>> {
        let mut stmt = self
            .conn
            .prepare("SELECT subproject_id, stage, request, auto_mode, research, verifications, plan, done, note FROM pipelines")?;
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

    // ---- Settings ----------------------------------------------------------

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn.query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0)).ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }
}
