/// SQLite-backed persistent memory store.
///
/// Tables:
///   code_units          — compressed, indexed source items with term vectors
///   code_members        — fields, variants, methods (linked to units)
///   patterns            — approved code patterns (always manually approved)
///   anti_patterns       — known bad approaches to inject as negative examples
///   annotations         — free-form notes Copilot will see
///   mcp_calls           — log of every tool call Copilot makes
///   pending_observations — file changes waiting for Syn's review
///   content_store        — content-addressed gzip blob store (cache layer)
///   response_cache       — tool response cache keyed by (tool+args+index_version)
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::model::*;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("could not open db: {}", db_path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Expose the connection for cache operations.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS code_units (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                name        TEXT NOT NULL,
                module_path TEXT NOT NULL,
                summary     TEXT NOT NULL,
                compressed  TEXT NOT NULL,
                term_vector TEXT NOT NULL,  -- JSON array of [term, weight] pairs
                indexed_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS code_members (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                parent_id   TEXT NOT NULL REFERENCES code_units(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                name        TEXT NOT NULL,
                type_sig    TEXT NOT NULL,
                doc         TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_members_parent ON code_members(parent_id);
            CREATE INDEX IF NOT EXISTS idx_units_name ON code_units(name);
            CREATE INDEX IF NOT EXISTS idx_units_kind ON code_units(kind);

            CREATE TABLE IF NOT EXISTS patterns (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL,
                intent      TEXT NOT NULL,
                body        TEXT NOT NULL,
                uses        TEXT NOT NULL,  -- JSON array
                tags        TEXT NOT NULL,  -- JSON array
                approved_at TEXT NOT NULL,
                use_count   INTEGER NOT NULL DEFAULT 0,
                reverted_count INTEGER NOT NULL DEFAULT 0,
                survival_rate REAL NOT NULL DEFAULT 1.0
            );

            CREATE TABLE IF NOT EXISTS anti_patterns (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                description TEXT NOT NULL,
                wrong       TEXT NOT NULL,
                correct     TEXT NOT NULL,
                tags        TEXT NOT NULL,  -- JSON array
                added_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS annotations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                topic       TEXT NOT NULL,
                body        TEXT NOT NULL,
                tags        TEXT NOT NULL,  -- JSON array
                added_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mcp_calls (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                tool        TEXT NOT NULL,
                args        TEXT NOT NULL,
                called_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS pending_observations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                path        TEXT NOT NULL,
                summary     TEXT NOT NULL,
                diff_hint   TEXT NOT NULL,
                observed_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS graph_nodes (
                id          TEXT PRIMARY KEY,
                kind        TEXT NOT NULL,
                name        TEXT NOT NULL,
                module_path TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS graph_edges (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                from_id     TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                to_id       TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                relation    TEXT NOT NULL,
                weight      REAL NOT NULL DEFAULT 1.0,
                source      TEXT NOT NULL,
                UNIQUE (from_id, to_id, relation)
            );

            CREATE INDEX IF NOT EXISTS idx_edges_from ON graph_edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_edges_to ON graph_edges(to_id);

            CREATE TABLE IF NOT EXISTS scratchpads (
                id          TEXT PRIMARY KEY,
                task        TEXT NOT NULL,
                state_json  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_scratchpads_updated ON scratchpads(updated_at);
        ")?;
        // Cache tables (managed by cache module)
        crate::cache::migrate(&self.conn)?;

        // Backfill Phase 4 pattern-evolution columns for existing DBs.
        self.ensure_pattern_evolution_columns()?;

        Ok(())
    }

    fn ensure_pattern_evolution_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(patterns)")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let mut cols = std::collections::HashSet::new();
        for c in rows {
            cols.insert(c?);
        }

        if !cols.contains("reverted_count") {
            self.conn.execute(
                "ALTER TABLE patterns ADD COLUMN reverted_count INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !cols.contains("survival_rate") {
            self.conn.execute(
                "ALTER TABLE patterns ADD COLUMN survival_rate REAL NOT NULL DEFAULT 1.0",
                [],
            )?;
        }

        self.conn.execute(
            "UPDATE patterns
             SET survival_rate = CASE
                WHEN use_count <= 0 AND reverted_count > 0 THEN 0.0
                WHEN use_count <= 0 THEN 1.0
                ELSE MAX(0.0, CAST(use_count - reverted_count AS REAL) / CAST(use_count AS REAL))
             END",
            [],
        )?;
        Ok(())
    }

    // ── Code units ────────────────────────────────────────────────────────────

    pub fn upsert_unit(&self, unit: &CodeUnit) -> Result<()> {
        let tv_json = serde_json::to_string(&unit.term_vector)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO code_units
             (id, kind, name, module_path, summary, compressed, term_vector, indexed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                unit.id, unit.kind, unit.name, unit.module_path,
                unit.summary, unit.compressed, tv_json,
                unit.indexed_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn upsert_member(&self, m: &CodeMember) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO code_members (parent_id, kind, name, type_sig, doc)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![m.parent_id, m.kind, m.name, m.type_sig, m.doc],
        )?;
        Ok(())
    }

    pub fn get_unit(&self, name: &str) -> Result<Option<CodeUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, module_path, summary, compressed, term_vector, indexed_at
             FROM code_units WHERE name = ?1 LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![name], row_to_unit)?;
        Ok(rows.next().transpose()?)
    }

    pub fn all_units(&self) -> Result<Vec<CodeUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, module_path, summary, compressed, term_vector, indexed_at
             FROM code_units ORDER BY kind, name"
        )?;
        let rows = stmt.query_map([], row_to_unit)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn units_by_kind(&self, kind: &str) -> Result<Vec<CodeUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, name, module_path, summary, compressed, term_vector, indexed_at
             FROM code_units WHERE kind = ?1 ORDER BY name"
        )?;
        let rows = stmt.query_map(params![kind], row_to_unit)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn members_of(&self, parent_id: &str) -> Result<Vec<CodeMember>> {
        let mut stmt = self.conn.prepare(
            "SELECT parent_id, kind, name, type_sig, doc FROM code_members WHERE parent_id = ?1"
        )?;
        let rows = stmt.query_map(params![parent_id], |row| {
            Ok(CodeMember {
                parent_id: row.get(0)?,
                kind:      row.get(1)?,
                name:      row.get(2)?,
                type_sig:  row.get(3)?,
                doc:       row.get(4)?,
            })
        })?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn unit_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM code_units", [], |r| r.get(0))?)
    }

    // ── Patterns ──────────────────────────────────────────────────────────────

    pub fn insert_pattern(&self, p: &Pattern) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO patterns
             (name, intent, body, uses, tags, approved_at, use_count, reverted_count, survival_rate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 1.0)",
            params![
                p.name, p.intent, p.body,
                serde_json::to_string(&p.uses)?,
                serde_json::to_string(&p.tags)?,
                p.approved_at.to_rfc3339(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn all_patterns(&self) -> Result<Vec<Pattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, intent, body, uses, tags, approved_at, use_count,
                    reverted_count, survival_rate
             FROM patterns ORDER BY survival_rate DESC, use_count DESC, approved_at DESC"
        )?;
        let rows = stmt.query_map([], row_to_pattern)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn pattern_used(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE patterns SET use_count = use_count + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn delete_pattern(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM patterns WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn pattern_reverted(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE patterns
             SET reverted_count = reverted_count + 1
             WHERE id = ?1",
            params![id],
        )?;
        self.recompute_pattern_survival(id)
    }

    pub fn recompute_pattern_survival(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE patterns
             SET survival_rate = CASE
                WHEN use_count <= 0 AND reverted_count > 0 THEN 0.0
                WHEN use_count <= 0 THEN 1.0
                ELSE MAX(0.0, CAST(use_count - reverted_count AS REAL) / CAST(use_count AS REAL))
             END
             WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn pattern_health_rows(&self) -> Result<Vec<(i64, String, i64, i64, f32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, use_count, reverted_count, survival_rate
             FROM patterns
             ORDER BY survival_rate DESC, use_count DESC, approved_at DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f32>(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn graph_counts(&self) -> Result<(i64, i64, i64, i64)> {
        let nodes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))?;
        let edges: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |r| r.get(0))?;
        let inferred: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM graph_edges WHERE source = 'inferred'",
                [],
                |r| r.get(0),
            )?;
        let manual: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM graph_edges WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )?;
        Ok((nodes, edges, inferred, manual))
    }

    pub fn scratchpad_count(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM scratchpads", [], |r| r.get(0))
            .map_err(Into::into)
    }

    pub fn hot_tools_recent(&self, recent_limit: usize, top_n: usize) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT tool, COUNT(*) as n FROM (
                SELECT tool FROM mcp_calls ORDER BY id DESC LIMIT ?1
             ) recent
             GROUP BY tool
             ORDER BY n DESC
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![recent_limit as i64, top_n as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ── Anti-patterns ─────────────────────────────────────────────────────────

    pub fn insert_anti_pattern(&self, ap: &AntiPattern) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO anti_patterns (description, wrong, correct, tags, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ap.description, ap.wrong, ap.correct,
                serde_json::to_string(&ap.tags)?,
                ap.added_at.to_rfc3339(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn all_anti_patterns(&self) -> Result<Vec<AntiPattern>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, description, wrong, correct, tags, added_at FROM anti_patterns"
        )?;
        let rows = stmt.query_map([], row_to_anti_pattern)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn delete_anti_pattern(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM anti_patterns WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Annotations ───────────────────────────────────────────────────────────

    pub fn insert_annotation(&self, a: &Annotation) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO annotations (topic, body, tags, added_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                a.topic, a.body,
                serde_json::to_string(&a.tags)?,
                a.added_at.to_rfc3339(),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn all_annotations(&self) -> Result<Vec<Annotation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, topic, body, tags, added_at FROM annotations ORDER BY added_at DESC"
        )?;
        let rows = stmt.query_map([], row_to_annotation)?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn delete_annotation(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM annotations WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── MCP call log ──────────────────────────────────────────────────────────

    pub fn log_mcp_call(&self, tool: &str, args: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mcp_calls (tool, args, called_at) VALUES (?1, ?2, ?3)",
            params![tool, args, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Most frequently called tools — useful for tuning what to pre-inject.
    pub fn hot_tools(&self, limit: usize) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT tool, COUNT(*) as n FROM mcp_calls GROUP BY tool ORDER BY n DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    // ── Pending observations ──────────────────────────────────────────────────

    pub fn add_observation(&self, o: &PendingObservation) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO pending_observations (path, summary, diff_hint, observed_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![o.path, o.summary, o.diff_hint, o.observed_at.to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn all_observations(&self) -> Result<Vec<PendingObservation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, summary, diff_hint, observed_at
             FROM pending_observations ORDER BY observed_at ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PendingObservation {
                id: Some(row.get(0)?),
                path: row.get(1)?,
                summary: row.get(2)?,
                diff_hint: row.get(3)?,
                observed_at: chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
                    .unwrap().with_timezone(&chrono::Utc),
            })
        })?;
        let items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(items)
    }

    pub fn dismiss_observation(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM pending_observations WHERE id = ?1", params![id]
        )?;
        Ok(())
    }
}

// ── Row mappers ───────────────────────────────────────────────────────────────

fn row_to_unit(row: &rusqlite::Row) -> rusqlite::Result<CodeUnit> {
    let tv_json: String = row.get(6)?;
    let term_vector: Vec<(String, f32)> = serde_json::from_str(&tv_json)
        .unwrap_or_default();
    let indexed_at = chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
        .unwrap().with_timezone(&chrono::Utc);

    Ok(CodeUnit {
        id:          row.get(0)?,
        kind:        row.get(1)?,
        name:        row.get(2)?,
        module_path: row.get(3)?,
        summary:     row.get(4)?,
        compressed:  row.get(5)?,
        term_vector,
        indexed_at,
    })
}

fn row_to_pattern(row: &rusqlite::Row) -> rusqlite::Result<Pattern> {
    let uses: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default();
    let approved_at = chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(6)?)
        .unwrap().with_timezone(&chrono::Utc);
    Ok(Pattern {
        id: Some(row.get(0)?), name: row.get(1)?, intent: row.get(2)?,
        body: row.get(3)?, uses, tags, approved_at,
        use_count: row.get(7)?,
        reverted_count: row.get(8)?,
        survival_rate: row.get(9)?,
    })
}

fn row_to_anti_pattern(row: &rusqlite::Row) -> rusqlite::Result<AntiPattern> {
    let tags: Vec<String> = serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default();
    let added_at = chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
        .unwrap().with_timezone(&chrono::Utc);
    Ok(AntiPattern {
        id: Some(row.get(0)?), description: row.get(1)?,
        wrong: row.get(2)?, correct: row.get(3)?, tags, added_at,
    })
}

fn row_to_annotation(row: &rusqlite::Row) -> rusqlite::Result<Annotation> {
    let tags: Vec<String> = serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default();
    let added_at = chrono::DateTime::parse_from_rfc3339(&row.get::<_, String>(4)?)
        .unwrap().with_timezone(&chrono::Utc);
    Ok(Annotation {
        id: Some(row.get(0)?), topic: row.get(1)?,
        body: row.get(2)?, tags, added_at,
    })
}
