use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ml::TrainingSample;
use crate::protocol::{
    MigrationStatusResponse, RecentEvent, RecordCommandRequest, StatsResponse,
    PROVENANCE_ACCEPTED_COMPLETION, PROVENANCE_CONFIDENCE_EXACT, PROVENANCE_CONFIDENCE_HEURISTIC,
    PROVENANCE_CONFIDENCE_UNKNOWN, PROVENANCE_LEGACY, PROVENANCE_PASTED, PROVENANCE_SOURCE_UNKNOWN,
    PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE, PROVENANCE_SOURCE_ZSH_PASTE_HEURISTIC,
    PROVENANCE_TYPED_MANUAL, PROVENANCE_UNKNOWN, TRUST_INTERACTIVE, TRUST_LEGACY,
    TRUST_SCRIPT_LIKE, TRUST_UNKNOWN,
};

const LEGACY_PENALTY: f64 = 0.15;
const PASTE_PENALTY: f64 = 0.25;
const TRUST_MIGRATION_KEY: &str = "trust_migration_v1";

#[derive(Debug, Clone)]
pub struct StoredDoc {
    pub command: String,
    pub item_type: String,
    pub item_value: String,
    pub description: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub command: String,
    pub count: f64,
    pub last_seen: i64,
}

#[derive(Debug, Clone)]
pub struct TransitionEntry {
    pub next: String,
    pub count: f64,
}

#[derive(Debug, Clone)]
pub struct IndexTarget {
    pub id: i64,
    pub target_type: String,
    pub value: String,
    pub recursive: bool,
    pub full: bool,
    pub max_depth: i64,
    pub created_ts: i64,
    pub last_indexed_ts: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct LoggedCompletionItem {
    pub rank: usize,
    pub item_key: String,
    pub insert_text: String,
    pub display: String,
    pub kind: String,
    pub source: String,
    pub score: f64,
    pub feature_json: String,
}

#[derive(Debug, Clone)]
pub struct ClassifiedEvent {
    pub trust: String,
    pub provenance: String,
    pub provenance_source: String,
    pub provenance_confidence: String,
    pub origin: String,
    pub tty_present: bool,
    pub shell: Option<String>,
    pub accepted_request_id: Option<i64>,
    pub accepted_item_key: Option<String>,
    pub accepted_rank: Option<i64>,
}

pub struct AppDb {
    conn: Connection,
}

impl AppDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("open sqlite db")?;
        conn.busy_timeout(Duration::from_millis(1_000))
            .context("set sqlite busy timeout")?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS app_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS commands (
                name TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                path TEXT,
                mtime INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS command_docs (
                id INTEGER PRIMARY KEY,
                command TEXT NOT NULL,
                item_type TEXT NOT NULL,
                item_value TEXT NOT NULL,
                description TEXT NOT NULL,
                source TEXT NOT NULL,
                UNIQUE(command, item_type, item_value)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS command_docs_fts USING fts5(
                command,
                item_type,
                item_value,
                description,
                content='command_docs',
                content_rowid='id'
            );

            CREATE TRIGGER IF NOT EXISTS command_docs_ai AFTER INSERT ON command_docs BEGIN
                INSERT INTO command_docs_fts(rowid, command, item_type, item_value, description)
                VALUES (new.id, new.command, new.item_type, new.item_value, new.description);
            END;

            CREATE TRIGGER IF NOT EXISTS command_docs_ad AFTER DELETE ON command_docs BEGIN
                INSERT INTO command_docs_fts(command_docs_fts, rowid, command, item_type, item_value, description)
                VALUES ('delete', old.id, old.command, old.item_type, old.item_value, old.description);
            END;

            CREATE TRIGGER IF NOT EXISTS command_docs_au AFTER UPDATE ON command_docs BEGIN
                INSERT INTO command_docs_fts(command_docs_fts, rowid, command, item_type, item_value, description)
                VALUES ('delete', old.id, old.command, old.item_type, old.item_value, old.description);
                INSERT INTO command_docs_fts(rowid, command, item_type, item_value, description)
                VALUES (new.id, new.command, new.item_type, new.item_value, new.description);
            END;

            CREATE TABLE IF NOT EXISTS history_events (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                cwd TEXT NOT NULL,
                command TEXT NOT NULL,
                shell TEXT,
                trust TEXT NOT NULL DEFAULT 'legacy',
                provenance TEXT NOT NULL DEFAULT 'legacy',
                provenance_source TEXT NOT NULL DEFAULT 'unknown',
                provenance_confidence TEXT NOT NULL DEFAULT 'unknown',
                origin TEXT NOT NULL DEFAULT 'unknown',
                tty_present INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_history_command ON history_events(command);
            CREATE INDEX IF NOT EXISTS idx_history_cwd ON history_events(cwd);

            CREATE TABLE IF NOT EXISTS transitions (
                prev_command TEXT NOT NULL,
                next_command TEXT NOT NULL,
                count INTEGER NOT NULL,
                interactive_count INTEGER NOT NULL DEFAULT 0,
                legacy_count INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(prev_command, next_command)
            );

            CREATE TABLE IF NOT EXISTS project_profiles (
                project_root TEXT NOT NULL,
                tool TEXT NOT NULL,
                count INTEGER NOT NULL,
                interactive_count INTEGER NOT NULL DEFAULT 0,
                legacy_count INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(project_root, tool)
            );

            CREATE TABLE IF NOT EXISTS dir_cache (
                dir_path TEXT PRIMARY KEY,
                mtime INTEGER NOT NULL,
                entries TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS index_targets (
                id INTEGER PRIMARY KEY,
                target_type TEXT NOT NULL,
                value TEXT NOT NULL,
                recursive INTEGER NOT NULL DEFAULT 0,
                full INTEGER NOT NULL DEFAULT 0,
                max_depth INTEGER NOT NULL DEFAULT 0,
                created_ts INTEGER NOT NULL,
                last_indexed_ts INTEGER,
                UNIQUE(target_type, value)
            );

            CREATE TABLE IF NOT EXISTS completion_requests (
                id INTEGER PRIMARY KEY,
                ts INTEGER NOT NULL,
                shell TEXT NOT NULL,
                cwd TEXT NOT NULL,
                line TEXT NOT NULL,
                cursor INTEGER NOT NULL,
                active_token TEXT NOT NULL,
                prev_command TEXT,
                trust TEXT NOT NULL DEFAULT 'legacy',
                provenance TEXT NOT NULL DEFAULT 'legacy',
                eligible_for_learning INTEGER NOT NULL DEFAULT 0,
                accepted_command TEXT,
                accepted_item_key TEXT,
                accepted_rank INTEGER,
                accepted_trust TEXT,
                accepted_provenance TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_completion_requests_ts ON completion_requests(ts);
            CREATE INDEX IF NOT EXISTS idx_completion_requests_cwd ON completion_requests(cwd);

            CREATE TABLE IF NOT EXISTS completion_items (
                id INTEGER PRIMARY KEY,
                request_id INTEGER NOT NULL REFERENCES completion_requests(id) ON DELETE CASCADE,
                rank INTEGER NOT NULL,
                item_key TEXT NOT NULL,
                insert_text TEXT NOT NULL,
                display TEXT NOT NULL,
                kind TEXT NOT NULL,
                source TEXT NOT NULL,
                score REAL NOT NULL,
                feature_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_completion_items_request ON completion_items(request_id);
            "#,
        )?;

        self.ensure_column("history_events", "trust", "TEXT NOT NULL DEFAULT 'legacy'")?;
        self.ensure_column(
            "history_events",
            "provenance",
            "TEXT NOT NULL DEFAULT 'legacy'",
        )?;
        self.ensure_column(
            "history_events",
            "provenance_source",
            "TEXT NOT NULL DEFAULT 'unknown'",
        )?;
        self.ensure_column(
            "history_events",
            "provenance_confidence",
            "TEXT NOT NULL DEFAULT 'unknown'",
        )?;
        self.ensure_column(
            "history_events",
            "origin",
            "TEXT NOT NULL DEFAULT 'unknown'",
        )?;
        self.ensure_column(
            "history_events",
            "tty_present",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        self.ensure_column(
            "completion_requests",
            "trust",
            "TEXT NOT NULL DEFAULT 'legacy'",
        )?;
        self.ensure_column(
            "completion_requests",
            "provenance",
            "TEXT NOT NULL DEFAULT 'legacy'",
        )?;
        self.ensure_column(
            "completion_requests",
            "eligible_for_learning",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column("completion_requests", "accepted_trust", "TEXT")?;
        self.ensure_column("completion_requests", "accepted_provenance", "TEXT")?;

        self.ensure_column(
            "transitions",
            "interactive_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column("transitions", "legacy_count", "INTEGER NOT NULL DEFAULT 0")?;

        self.ensure_column(
            "project_profiles",
            "interactive_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        self.ensure_column(
            "project_profiles",
            "legacy_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_trust ON history_events(trust, provenance)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_completion_requests_trust ON completion_requests(trust, provenance)",
            [],
        )?;

        self.run_trust_migration_if_needed()?;
        Ok(())
    }

    pub fn upsert_command(
        &self,
        name: &str,
        kind: &str,
        path: Option<&str>,
        mtime: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO commands(name, kind, path, mtime) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET kind=excluded.kind, path=excluded.path, mtime=excluded.mtime",
            params![name, kind, path, mtime],
        )?;
        Ok(())
    }

    pub fn list_commands(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, kind FROM commands ORDER BY name")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn replace_docs_for_command(&self, command: &str, docs: &[StoredDoc]) -> Result<()> {
        self.conn
            .execute("DELETE FROM command_docs WHERE command = ?1", [command])?;
        let mut stmt = self.conn.prepare(
            "INSERT OR REPLACE INTO command_docs(command, item_type, item_value, description, source)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for doc in docs {
            stmt.execute(params![
                doc.command,
                doc.item_type,
                doc.item_value,
                doc.description,
                doc.source
            ])?;
        }
        Ok(())
    }

    pub fn command_has_docs(&self, command: &str) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM command_docs WHERE command = ?1)",
                [command],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }

    pub fn docs_for_command(&self, command: &str) -> Result<Vec<StoredDoc>> {
        let mut stmt = self.conn.prepare(
            "SELECT command, item_type, item_value, description, source
             FROM command_docs WHERE command = ?1",
        )?;
        let rows = stmt.query_map([command], |row| {
            Ok(StoredDoc {
                command: row.get(0)?,
                item_type: row.get(1)?,
                item_value: row.get(2)?,
                description: row.get(3)?,
                source: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn search_docs(&self, query: &str, limit: usize) -> Result<Vec<StoredDoc>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.command, d.item_type, d.item_value, d.description, d.source
             FROM command_docs_fts f
             JOIN command_docs d ON d.id = f.rowid
             WHERE command_docs_fts MATCH ?1
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok(StoredDoc {
                command: row.get(0)?,
                item_type: row.get(1)?,
                item_value: row.get(2)?,
                description: row.get(3)?,
                source: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn classify_record_event(&self, request: &RecordCommandRequest) -> ClassifiedEvent {
        let tty_present = request.tty_present.unwrap_or(false);
        let mut trust = sanitize_trust(request.trust.as_deref()).unwrap_or_else(|| {
            if tty_present {
                TRUST_INTERACTIVE.to_string()
            } else {
                TRUST_UNKNOWN.to_string()
            }
        });
        let mut provenance = sanitize_provenance(request.provenance.as_deref())
            .unwrap_or_else(|| PROVENANCE_UNKNOWN.to_string());
        let mut provenance_source =
            sanitize_provenance_source(request.provenance_source.as_deref())
                .unwrap_or_else(|| PROVENANCE_SOURCE_UNKNOWN.to_string());
        let mut provenance_confidence =
            sanitize_provenance_confidence(request.provenance_confidence.as_deref())
                .unwrap_or_else(|| PROVENANCE_CONFIDENCE_UNKNOWN.to_string());
        let origin = request
            .origin
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_string());

        if looks_script_like(&request.command) {
            trust = TRUST_SCRIPT_LIKE.to_string();
            if provenance == PROVENANCE_ACCEPTED_COMPLETION {
                provenance = PROVENANCE_UNKNOWN.to_string();
            }
            provenance_source = PROVENANCE_SOURCE_UNKNOWN.to_string();
            provenance_confidence = PROVENANCE_CONFIDENCE_UNKNOWN.to_string();
        } else if tty_present && trust == TRUST_UNKNOWN {
            trust = TRUST_INTERACTIVE.to_string();
        }

        if !matches!(
            provenance.as_str(),
            PROVENANCE_TYPED_MANUAL
                | PROVENANCE_ACCEPTED_COMPLETION
                | PROVENANCE_PASTED
                | "history_expansion"
                | PROVENANCE_UNKNOWN
        ) {
            provenance = PROVENANCE_UNKNOWN.to_string();
        }

        if provenance != PROVENANCE_PASTED {
            provenance_source = PROVENANCE_SOURCE_UNKNOWN.to_string();
            provenance_confidence = PROVENANCE_CONFIDENCE_UNKNOWN.to_string();
        } else {
            if provenance_source == PROVENANCE_SOURCE_UNKNOWN {
                provenance_confidence = PROVENANCE_CONFIDENCE_UNKNOWN.to_string();
            }
            if provenance_confidence == PROVENANCE_CONFIDENCE_UNKNOWN
                && provenance_source != PROVENANCE_SOURCE_UNKNOWN
            {
                provenance_confidence =
                    if provenance_source == PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE {
                        PROVENANCE_CONFIDENCE_EXACT.to_string()
                    } else {
                        PROVENANCE_CONFIDENCE_HEURISTIC.to_string()
                    };
            }
        }

        ClassifiedEvent {
            trust,
            provenance,
            provenance_source,
            provenance_confidence,
            origin,
            tty_present,
            shell: request.shell.clone(),
            accepted_request_id: request.accepted_request_id,
            accepted_item_key: request.accepted_item_key.clone(),
            accepted_rank: request.accepted_rank,
        }
    }

    pub fn record_history(&self, request: &RecordCommandRequest) -> Result<ClassifiedEvent> {
        let classified = self.classify_record_event(request);
        let ts = unix_ts();
        let prev = self.latest_command()?;
        self.conn.execute(
            "INSERT INTO history_events(ts, cwd, command, shell, trust, provenance, provenance_source, provenance_confidence, origin, tty_present)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                ts,
                request.cwd,
                request.command,
                classified.shell,
                classified.trust,
                classified.provenance,
                classified.provenance_source,
                classified.provenance_confidence,
                classified.origin,
                if classified.tty_present { 1 } else { 0 }
            ],
        )?;

        if is_clean_personalization_signal(&classified) {
            if let Some(prev_command) = prev {
                self.conn.execute(
                    "INSERT INTO transitions(prev_command, next_command, count, interactive_count, legacy_count)
                     VALUES (?1, ?2, 1, 1, 0)
                     ON CONFLICT(prev_command, next_command)
                     DO UPDATE SET count = count + 1, interactive_count = interactive_count + 1",
                    params![prev_command, request.command],
                )?;
            }

            if let Some(project_root) = detect_project_root(&request.cwd) {
                self.conn.execute(
                    "INSERT INTO project_profiles(project_root, tool, count, interactive_count, legacy_count)
                     VALUES (?1, ?2, 1, 1, 0)
                     ON CONFLICT(project_root, tool)
                     DO UPDATE SET count = count + 1, interactive_count = interactive_count + 1",
                    params![project_root, first_word(&request.command)],
                )?;
            }

            let _ = self.mark_completion_accepted(&request.command, &request.cwd, &classified);
        }

        Ok(classified)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_completion_request(
        &self,
        shell: &str,
        cwd: &str,
        line: &str,
        cursor: usize,
        active_token: &str,
        prev_command: Option<&str>,
        request_trust: &str,
        items: &[LoggedCompletionItem],
    ) -> Result<i64> {
        let ts = unix_ts();
        let trust =
            sanitize_trust(Some(request_trust)).unwrap_or_else(|| TRUST_UNKNOWN.to_string());
        let eligible = if trust == TRUST_INTERACTIVE { 1 } else { 0 };
        self.conn.execute(
            "INSERT INTO completion_requests(ts, shell, cwd, line, cursor, active_token, prev_command, trust, provenance, eligible_for_learning)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                ts,
                shell,
                cwd,
                line,
                cursor as i64,
                active_token,
                prev_command,
                trust,
                PROVENANCE_UNKNOWN,
                eligible
            ],
        )?;
        let request_id = self.conn.last_insert_rowid();
        let mut stmt = self.conn.prepare(
            "INSERT INTO completion_items(request_id, rank, item_key, insert_text, display, kind, source, score, feature_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        for item in items {
            stmt.execute(params![
                request_id,
                item.rank as i64,
                item.item_key,
                item.insert_text,
                item.display,
                item.kind,
                item.source,
                item.score,
                item.feature_json
            ])?;
        }
        Ok(request_id)
    }

    pub fn mark_completion_accepted(
        &self,
        executed_command: &str,
        cwd: &str,
        event: &ClassifiedEvent,
    ) -> Result<bool> {
        if !is_clean_personalization_signal(event) {
            return Ok(false);
        }

        let mut candidate_request_ids = Vec::new();
        if let Some(request_id) = event.accepted_request_id {
            candidate_request_ids.push(request_id);
        }

        if let (Some(request_id), Some(item_key), Some(rank)) = (
            event.accepted_request_id,
            event.accepted_item_key.as_deref(),
            event.accepted_rank,
        ) {
            self.conn.execute(
                "UPDATE completion_requests
                 SET accepted_command = ?1,
                     accepted_item_key = ?2,
                     accepted_rank = ?3,
                     accepted_trust = ?4,
                     accepted_provenance = ?5
                 WHERE id = ?6
                   AND eligible_for_learning = 1",
                params![
                    executed_command,
                    item_key,
                    rank,
                    event.trust,
                    event.provenance,
                    request_id
                ],
            )?;
            if self.conn.changes() > 0 {
                return Ok(true);
            }
        }

        let now = unix_ts();
        let mut stmt = self.conn.prepare(
            "SELECT id
             FROM completion_requests
             WHERE accepted_command IS NULL
               AND eligible_for_learning = 1
               AND trust = ?1
               AND cwd = ?2
               AND ts >= ?3
               AND (?4 IS NULL OR shell = ?4)
             ORDER BY ts DESC
             LIMIT 20",
        )?;
        let recent = stmt
            .query_map(
                params![TRUST_INTERACTIVE, cwd, now - 300, event.shell.as_deref()],
                |row| row.get::<_, i64>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for request_id in recent {
            if !candidate_request_ids.contains(&request_id) {
                candidate_request_ids.push(request_id);
            }
        }

        for request_id in candidate_request_ids {
            let mut item_stmt = self.conn.prepare(
                "SELECT item_key, rank
                 FROM completion_items
                 WHERE request_id = ?1
                 ORDER BY rank ASC",
            )?;
            let items = item_stmt
                .query_map([request_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let matched = items
                .into_iter()
                .filter(|(item_key, _)| command_matches_completion(executed_command, item_key))
                .max_by_key(|(item_key, rank)| (item_key.len(), -rank));

            if let Some((item_key, rank)) = matched {
                self.conn.execute(
                    "UPDATE completion_requests
                     SET accepted_command = ?1,
                         accepted_item_key = ?2,
                         accepted_rank = ?3,
                         accepted_trust = ?4,
                         accepted_provenance = ?5
                     WHERE id = ?6",
                    params![
                        executed_command,
                        item_key,
                        rank,
                        event.trust,
                        event.provenance,
                        request_id
                    ],
                )?;
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn latest_command(&self) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT command
                 FROM history_events
                 WHERE trust = ?1
                   AND provenance IN (?2, ?3)
                 ORDER BY ts DESC, id DESC
                 LIMIT 1",
                params![
                    TRUST_INTERACTIVE,
                    PROVENANCE_TYPED_MANUAL,
                    PROVENANCE_ACCEPTED_COMPLETION
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn frequent_history(
        &self,
        prefix: &str,
        cwd: &str,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let mut out = self.weighted_history(prefix, None, limit)?;
        if !cwd.is_empty() {
            for entry in self.weighted_history(prefix, Some(cwd), limit)? {
                if !out.iter().any(|known| known.command == entry.command) {
                    out.push(entry);
                }
            }
        }
        out.sort_by(|left, right| {
            right
                .count
                .partial_cmp(&left.count)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(right.last_seen.cmp(&left.last_seen))
        });
        out.truncate(limit);
        Ok(out)
    }

    pub fn transitions_from(
        &self,
        prev_command: &str,
        limit: usize,
    ) -> Result<Vec<TransitionEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT next_command, (interactive_count + legacy_count * ?2) AS weighted_count
             FROM transitions
             WHERE prev_command = ?1
               AND (interactive_count > 0 OR legacy_count > 0)
             ORDER BY weighted_count DESC, count DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![prev_command, LEGACY_PENALTY, limit as i64], |row| {
            Ok(TransitionEntry {
                next: row.get(0)?,
                count: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn project_tool_count(&self, cwd: &str, tool: &str) -> Result<f64> {
        if let Some(project_root) = detect_project_root(cwd) {
            let value = self
                .conn
                .query_row(
                    "SELECT (interactive_count + legacy_count * ?3)
                     FROM project_profiles
                     WHERE project_root = ?1 AND tool = ?2",
                    params![project_root, tool, LEGACY_PENALTY],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0.0);
            return Ok(value);
        }
        Ok(0.0)
    }

    pub fn get_dir_cache(&self, dir: &str) -> Result<Option<(i64, String)>> {
        self.conn
            .query_row(
                "SELECT mtime, entries FROM dir_cache WHERE dir_path = ?1",
                [dir],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn upsert_dir_cache(&self, dir: &str, mtime: i64, entries: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO dir_cache(dir_path, mtime, entries) VALUES (?1, ?2, ?3)
             ON CONFLICT(dir_path) DO UPDATE SET mtime = excluded.mtime, entries = excluded.entries",
            params![dir, mtime, entries],
        )?;
        Ok(())
    }

    pub fn upsert_index_target(
        &self,
        target_type: &str,
        value: &str,
        recursive: bool,
        full: bool,
        max_depth: i64,
    ) -> Result<()> {
        let ts = unix_ts();
        self.conn.execute(
            "INSERT INTO index_targets(target_type, value, recursive, full, max_depth, created_ts, last_indexed_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(target_type, value)
             DO UPDATE SET recursive = excluded.recursive,
                           full = excluded.full,
                           max_depth = excluded.max_depth,
                           last_indexed_ts = excluded.last_indexed_ts",
            params![
                target_type,
                value,
                if recursive { 1 } else { 0 },
                if full { 1 } else { 0 },
                max_depth,
                ts
            ],
        )?;
        Ok(())
    }

    pub fn list_index_targets(&self) -> Result<Vec<IndexTarget>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, target_type, value, recursive, full, max_depth, created_ts, last_indexed_ts
             FROM index_targets
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(IndexTarget {
                id: row.get(0)?,
                target_type: row.get(1)?,
                value: row.get(2)?,
                recursive: row.get::<_, i64>(3)? != 0,
                full: row.get::<_, i64>(4)? != 0,
                max_depth: row.get(5)?,
                created_ts: row.get(6)?,
                last_indexed_ts: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn stats(&self) -> Result<StatsResponse> {
        Ok(StatsResponse {
            commands: count(&self.conn, "commands")?,
            docs: count(&self.conn, "command_docs")?,
            history_events: count(&self.conn, "history_events")?,
            transitions: count(&self.conn, "transitions")?,
            project_profiles: count(&self.conn, "project_profiles")?,
            dir_cache_entries: count(&self.conn, "dir_cache")?,
            completion_requests: count(&self.conn, "completion_requests")?,
            completion_items: count(&self.conn, "completion_items")?,
            accepted_completions: self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM completion_requests WHERE accepted_command IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?,
            legacy_history_events: count_where(&self.conn, "history_events", "trust = 'legacy'")?,
            interactive_history_events: count_where(
                &self.conn,
                "history_events",
                "trust = 'interactive'",
            )?,
            script_like_history_events: count_where(
                &self.conn,
                "history_events",
                "trust = 'script_like'",
            )?,
            clean_completion_requests: count_where(
                &self.conn,
                "completion_requests",
                "trust = 'interactive' AND eligible_for_learning = 1",
            )?,
            legacy_completion_requests: count_where(
                &self.conn,
                "completion_requests",
                "trust = 'legacy'",
            )?,
            accepted_clean_completions: count_where(
                &self.conn,
                "completion_requests",
                "accepted_trust = 'interactive' AND accepted_provenance IN ('typed_manual', 'accepted_completion')",
            )?,
            pasted_history_events: count_where(
                &self.conn,
                "history_events",
                "provenance = 'pasted'",
            )?,
            exact_pasted_history_events: count_where(
                &self.conn,
                "history_events",
                "provenance = 'pasted' AND provenance_confidence = 'exact'",
            )?,
            heuristic_pasted_history_events: count_where(
                &self.conn,
                "history_events",
                "provenance = 'pasted' AND provenance_confidence = 'heuristic'",
            )?,
        })
    }

    pub fn migration_status(&self) -> Result<MigrationStatusResponse> {
        let stats = self.stats()?;
        Ok(MigrationStatusResponse {
            history_events: stats.history_events,
            legacy_history_events: stats.legacy_history_events,
            interactive_history_events: stats.interactive_history_events,
            script_like_history_events: stats.script_like_history_events,
            completion_requests: stats.completion_requests,
            clean_completion_requests: stats.clean_completion_requests,
            legacy_completion_requests: stats.legacy_completion_requests,
            accepted_clean_completions: stats.accepted_clean_completions,
            pasted_history_events: stats.pasted_history_events,
            exact_pasted_history_events: stats.exact_pasted_history_events,
            heuristic_pasted_history_events: stats.heuristic_pasted_history_events,
        })
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<RecentEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, cwd, command, shell, trust, provenance, provenance_source, provenance_confidence, origin, tty_present
             FROM history_events
             ORDER BY ts DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(RecentEvent {
                id: row.get(0)?,
                ts: row.get(1)?,
                cwd: row.get(2)?,
                command: row.get(3)?,
                shell: row.get(4)?,
                trust: row.get(5)?,
                provenance: row.get(6)?,
                provenance_source: row.get(7)?,
                provenance_confidence: row.get(8)?,
                origin: row.get(9)?,
                tty_present: row.get::<_, i64>(10)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn training_samples(&self, limit: usize) -> Result<Vec<TrainingSample>> {
        let mut stmt = self.conn.prepare(
            "SELECT i.kind, i.source, i.feature_json,
                    CASE WHEN r.accepted_item_key = i.item_key THEN 1.0 ELSE 0.0 END AS label
             FROM completion_items i
             JOIN completion_requests r ON r.id = i.request_id
             WHERE r.eligible_for_learning = 1
               AND r.trust = ?1
               AND r.accepted_trust = ?1
               AND r.accepted_provenance IN (?2, ?3)
               AND r.accepted_command IS NOT NULL
             ORDER BY i.id DESC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                TRUST_INTERACTIVE,
                PROVENANCE_TYPED_MANUAL,
                PROVENANCE_ACCEPTED_COMPLETION,
                limit as i64
            ],
            |row| {
                let feature_json: String = row.get(2)?;
                let features = serde_json::from_str(&feature_json).unwrap_or_default();
                Ok(TrainingSample {
                    kind: row.get(0)?,
                    source: row.get(1)?,
                    features,
                    label: row.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn reset_personalization(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            DELETE FROM completion_items;
            DELETE FROM completion_requests;
            DELETE FROM transitions;
            DELETE FROM project_profiles;
            DELETE FROM history_events;
            "#,
        )?;
        Ok(())
    }

    fn weighted_history(
        &self,
        prefix: &str,
        cwd: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let like = format!("{prefix}%");
        let weighted_case = weighted_history_case();
        let sql = if cwd.is_some() {
            format!(
                "SELECT command,
                    SUM({weighted_case}) AS weighted_cnt,
                    MAX(ts) AS last_seen
             FROM history_events
             WHERE cwd = ?1 AND command LIKE ?2
             GROUP BY command
             HAVING weighted_cnt > 0
             ORDER BY weighted_cnt DESC, last_seen DESC
             LIMIT ?3"
            )
        } else {
            format!(
                "SELECT command,
                    SUM({weighted_case}) AS weighted_cnt,
                    MAX(ts) AS last_seen
             FROM history_events
             WHERE command LIKE ?1
             GROUP BY command
             HAVING weighted_cnt > 0
             ORDER BY weighted_cnt DESC, last_seen DESC
             LIMIT ?2"
            )
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if let Some(cwd) = cwd {
            stmt.query_map(params![cwd, like, limit as i64], |row| {
                Ok(HistoryEntry {
                    command: row.get(0)?,
                    count: row.get(1)?,
                    last_seen: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map(params![like, limit as i64], |row| {
                Ok(HistoryEntry {
                    command: row.get(0)?,
                    count: row.get(1)?,
                    last_seen: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    fn ensure_column(&self, table: &str, column: &str, definition: &str) -> Result<()> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .with_context(|| format!("inspect schema for {table}"))?;
        let existing = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !existing.iter().any(|name| name == column) {
            self.conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
        Ok(())
    }

    fn run_trust_migration_if_needed(&self) -> Result<()> {
        if self.meta_value(TRUST_MIGRATION_KEY)?.as_deref() == Some("done") {
            return Ok(());
        }

        self.conn.execute(
            "UPDATE history_events
             SET trust = ?1,
                 provenance = ?2,
                 provenance_source = ?3,
                 provenance_confidence = ?4,
                 origin = COALESCE(origin, 'unknown'),
                 tty_present = COALESCE(tty_present, 0)
             WHERE trust IS NULL OR trust = '' OR trust = ?1",
            params![
                TRUST_LEGACY,
                PROVENANCE_LEGACY,
                PROVENANCE_SOURCE_UNKNOWN,
                PROVENANCE_CONFIDENCE_UNKNOWN
            ],
        )?;
        self.conn.execute(
            "UPDATE completion_requests
             SET trust = ?1,
                 provenance = ?2,
                 eligible_for_learning = 0
             WHERE trust IS NULL OR trust = '' OR trust = ?1",
            params![TRUST_LEGACY, PROVENANCE_LEGACY],
        )?;
        self.conn.execute(
            "UPDATE transitions SET legacy_count = count WHERE legacy_count = 0 AND count > 0",
            [],
        )?;
        self.conn.execute(
            "UPDATE project_profiles SET legacy_count = count WHERE legacy_count = 0 AND count > 0",
            [],
        )?;
        self.set_meta_value(TRUST_MIGRATION_KEY, "done")?;
        Ok(())
    }

    fn meta_value(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    fn set_meta_value(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

fn count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?,
    )
}

fn count_where(conn: &Connection, table: &str, condition: &str) -> Result<i64> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {condition}"),
        [],
        |row| row.get(0),
    )?)
}

fn unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn sanitize_trust(value: Option<&str>) -> Option<String> {
    match value?.trim() {
        TRUST_INTERACTIVE | TRUST_SCRIPT_LIKE | TRUST_UNKNOWN | TRUST_LEGACY => {
            Some(value?.trim().to_string())
        }
        _ => None,
    }
}

fn sanitize_provenance(value: Option<&str>) -> Option<String> {
    match value?.trim() {
        PROVENANCE_TYPED_MANUAL
        | PROVENANCE_ACCEPTED_COMPLETION
        | PROVENANCE_PASTED
        | PROVENANCE_UNKNOWN
        | PROVENANCE_LEGACY
        | "history_expansion" => Some(value?.trim().to_string()),
        _ => None,
    }
}

fn sanitize_provenance_source(value: Option<&str>) -> Option<String> {
    match value?.trim() {
        PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE
        | PROVENANCE_SOURCE_ZSH_PASTE_HEURISTIC
        | PROVENANCE_SOURCE_UNKNOWN => Some(value?.trim().to_string()),
        _ => None,
    }
}

fn sanitize_provenance_confidence(value: Option<&str>) -> Option<String> {
    match value?.trim() {
        PROVENANCE_CONFIDENCE_EXACT
        | PROVENANCE_CONFIDENCE_HEURISTIC
        | PROVENANCE_CONFIDENCE_UNKNOWN => Some(value?.trim().to_string()),
        _ => None,
    }
}

fn is_clean_personalization_signal(event: &ClassifiedEvent) -> bool {
    event.trust == TRUST_INTERACTIVE
        && matches!(
            event.provenance.as_str(),
            PROVENANCE_TYPED_MANUAL | PROVENANCE_ACCEPTED_COMPLETION
        )
}

fn weighted_history_case() -> String {
    format!(
        "CASE
            WHEN trust = 'interactive' AND provenance IN ('typed_manual', 'accepted_completion') THEN 1.0
            WHEN trust = 'interactive' AND provenance = 'pasted' THEN {PASTE_PENALTY}
            WHEN trust = 'legacy' THEN {LEGACY_PENALTY}
            ELSE 0.0
         END"
    )
}

fn looks_script_like(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains('\n')
        || trimmed.contains("&&")
        || trimmed.contains("||")
        || trimmed.contains(";;")
        || trimmed.contains("<<")
    {
        return true;
    }

    let shell_words = trimmed.split_whitespace().collect::<Vec<_>>();
    if shell_words.is_empty() {
        return false;
    }

    let first = shell_words[0];
    if matches!(first, "source" | ".") {
        return true;
    }
    if matches!(first, "make" | "just" | "task") {
        return true;
    }
    if matches!(first, "bash" | "sh" | "zsh")
        && shell_words.get(1).is_some_and(|arg| is_script_path(arg))
    {
        return true;
    }
    if matches!(first, "python" | "python3")
        && shell_words.get(1).is_some_and(|arg| is_script_path(arg))
    {
        return true;
    }
    if first == "npm" && shell_words.get(1) == Some(&"run") {
        return true;
    }
    if first == "cargo" && shell_words.get(1) == Some(&"run") {
        return true;
    }
    first.starts_with("./") || first.ends_with(".sh") || first.ends_with(".py")
}

fn is_script_path(value: &str) -> bool {
    value.ends_with(".sh")
        || value.ends_with(".py")
        || value.ends_with(".zsh")
        || value.ends_with(".bash")
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('/')
}

fn first_word(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or(command)
}

fn command_matches_completion(executed_command: &str, item_key: &str) -> bool {
    executed_command == item_key
        || executed_command.starts_with(&format!("{item_key} "))
        || executed_command.starts_with(item_key)
}

fn detect_project_root(cwd: &str) -> Option<String> {
    let mut path = Path::new(cwd);
    loop {
        for marker in [
            ".git",
            "package.json",
            "Cargo.toml",
            "*.csproj",
            "*.sln",
            "pyproject.toml",
            "Dockerfile",
            "Makefile",
        ] {
            if let Some(extension) = marker.strip_prefix("*.") {
                if let Ok(entries) = std::fs::read_dir(path) {
                    if entries.flatten().any(|entry| {
                        entry
                            .path()
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| ext == extension)
                    }) {
                        return Some(path.to_string_lossy().to_string());
                    }
                }
            } else if path.join(marker).exists() {
                return Some(path.to_string_lossy().to_string());
            }
        }
        path = path.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_db() -> AppDb {
        AppDb::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn script_like_classifier_catches_shell_wrappers() {
        assert!(looks_script_like("bash deploy.sh"));
        assert!(looks_script_like("npm run build"));
        assert!(looks_script_like("cargo run --bin shac"));
        assert!(!looks_script_like("git checkout main"));
    }

    #[test]
    fn exact_accept_metadata_wins_over_command_matching() {
        let mut path = std::env::temp_dir();
        path.push(format!("shac-test-{}-exact.db", unix_ts()));
        std::fs::remove_file(&path).ok();

        let db = AppDb::open(PathBuf::from(&path).as_path()).expect("open db");
        let request_id = db
            .record_completion_request(
                "zsh",
                "/tmp",
                "pyt",
                3,
                "pyt",
                None,
                TRUST_INTERACTIVE,
                &[LoggedCompletionItem {
                    rank: 0,
                    item_key: "python3".to_string(),
                    insert_text: "python3".to_string(),
                    display: "python3".to_string(),
                    kind: "command".to_string(),
                    source: "path_index".to_string(),
                    score: 1.0,
                    feature_json: "{}".to_string(),
                }],
            )
            .expect("record request");

        db.record_history(&RecordCommandRequest {
            command: "python3".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_ACCEPTED_COMPLETION.to_string()),
            provenance_source: None,
            provenance_confidence: None,
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: Some(request_id),
            accepted_item_key: Some("python3".to_string()),
            accepted_rank: Some(0),
        })
        .expect("record history");

        let accepted = db
            .conn
            .query_row(
                "SELECT accepted_command, accepted_item_key, accepted_rank, accepted_provenance
                 FROM completion_requests
                 WHERE id = ?1",
                [request_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .expect("fetch accepted row");

        assert_eq!(accepted.0.as_deref(), Some("python3"));
        assert_eq!(accepted.1.as_deref(), Some("python3"));
        assert_eq!(accepted.2, Some(0));
        assert_eq!(accepted.3.as_deref(), Some(PROVENANCE_ACCEPTED_COMPLETION));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn pasted_events_are_counted_but_not_clean() {
        let mut path = std::env::temp_dir();
        path.push(format!("shac-test-{}-paste.db", unix_ts()));
        std::fs::remove_file(&path).ok();

        let db = AppDb::open(PathBuf::from(&path).as_path()).expect("open db");
        db.record_history(&RecordCommandRequest {
            command: "echo pasted".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_PASTED.to_string()),
            provenance_source: Some(PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE.to_string()),
            provenance_confidence: Some(PROVENANCE_CONFIDENCE_EXACT.to_string()),
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: None,
            accepted_item_key: None,
            accepted_rank: None,
        })
        .expect("record pasted history");

        let stats = db.stats().expect("stats");
        assert_eq!(stats.pasted_history_events, 1);
        assert_eq!(stats.exact_pasted_history_events, 1);
        assert_eq!(stats.heuristic_pasted_history_events, 0);
        assert_eq!(stats.accepted_clean_completions, 0);

        let history = db.frequent_history("echo", "", 10).expect("history lookup");
        assert!(history.iter().any(|entry| entry.command == "echo pasted"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn pasted_history_is_weighted_below_manual_history() {
        let mut path = std::env::temp_dir();
        path.push(format!("shac-test-{}-paste-weight.db", unix_ts()));
        std::fs::remove_file(&path).ok();

        let db = AppDb::open(PathBuf::from(&path).as_path()).expect("open db");
        db.record_history(&RecordCommandRequest {
            command: "echo shac-manual-weight".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_TYPED_MANUAL.to_string()),
            provenance_source: None,
            provenance_confidence: None,
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: None,
            accepted_item_key: None,
            accepted_rank: None,
        })
        .expect("record manual history");
        db.record_history(&RecordCommandRequest {
            command: "echo shac-pasted-weight".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_PASTED.to_string()),
            provenance_source: Some(PROVENANCE_SOURCE_ZSH_PASTE_HEURISTIC.to_string()),
            provenance_confidence: Some(PROVENANCE_CONFIDENCE_HEURISTIC.to_string()),
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: None,
            accepted_item_key: None,
            accepted_rank: None,
        })
        .expect("record pasted history");

        let history = db
            .frequent_history("echo shac-", "", 10)
            .expect("history lookup");
        let manual = history
            .iter()
            .find(|entry| entry.command == "echo shac-manual-weight")
            .expect("manual history entry");
        let pasted = history
            .iter()
            .find(|entry| entry.command == "echo shac-pasted-weight")
            .expect("pasted history entry");

        assert_eq!(manual.count, 1.0);
        assert_eq!(pasted.count, PASTE_PENALTY);
        assert!(manual.count > pasted.count);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn pasted_history_does_not_create_strong_transitions() {
        let mut path = std::env::temp_dir();
        path.push(format!("shac-test-{}-paste-transition.db", unix_ts()));
        std::fs::remove_file(&path).ok();

        let db = AppDb::open(PathBuf::from(&path).as_path()).expect("open db");
        db.record_history(&RecordCommandRequest {
            command: "git status".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_TYPED_MANUAL.to_string()),
            provenance_source: None,
            provenance_confidence: None,
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: None,
            accepted_item_key: None,
            accepted_rank: None,
        })
        .expect("record clean previous command");
        db.record_history(&RecordCommandRequest {
            command: "git checkout main".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_PASTED.to_string()),
            provenance_source: Some(PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE.to_string()),
            provenance_confidence: Some(PROVENANCE_CONFIDENCE_EXACT.to_string()),
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: None,
            accepted_item_key: None,
            accepted_rank: None,
        })
        .expect("record pasted next command");

        let transitions = db
            .transitions_from("git status", 10)
            .expect("transition lookup");
        assert!(
            transitions.is_empty(),
            "pasted command should not create full-strength transition: {transitions:?}"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn training_samples_include_clean_accepts_and_exclude_paste() {
        let mut path = std::env::temp_dir();
        path.push(format!("shac-test-{}-training-clean.db", unix_ts()));
        std::fs::remove_file(&path).ok();

        let db = AppDb::open(PathBuf::from(&path).as_path()).expect("open db");
        let clean_request_id = db
            .record_completion_request(
                "zsh",
                "/tmp",
                "pyt",
                3,
                "pyt",
                None,
                TRUST_INTERACTIVE,
                &[
                    LoggedCompletionItem {
                        rank: 0,
                        item_key: "python3".to_string(),
                        insert_text: "python3".to_string(),
                        display: "python3".to_string(),
                        kind: "command".to_string(),
                        source: "path_index".to_string(),
                        score: 1.0,
                        feature_json: r#"{"prefix_score":1.0}"#.to_string(),
                    },
                    LoggedCompletionItem {
                        rank: 1,
                        item_key: "python3-config".to_string(),
                        insert_text: "python3-config".to_string(),
                        display: "python3-config".to_string(),
                        kind: "command".to_string(),
                        source: "path_index".to_string(),
                        score: 0.7,
                        feature_json: r#"{"prefix_score":0.7}"#.to_string(),
                    },
                ],
            )
            .expect("record clean completion request");
        db.record_history(&RecordCommandRequest {
            command: "python3".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_ACCEPTED_COMPLETION.to_string()),
            provenance_source: None,
            provenance_confidence: None,
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: Some(clean_request_id),
            accepted_item_key: Some("python3".to_string()),
            accepted_rank: Some(0),
        })
        .expect("record clean accept");

        let pasted_request_id = db
            .record_completion_request(
                "zsh",
                "/tmp",
                "ech",
                3,
                "ech",
                None,
                TRUST_INTERACTIVE,
                &[LoggedCompletionItem {
                    rank: 0,
                    item_key: "echo pasted".to_string(),
                    insert_text: "echo pasted".to_string(),
                    display: "echo pasted".to_string(),
                    kind: "history".to_string(),
                    source: "history".to_string(),
                    score: 1.0,
                    feature_json: r#"{"prefix_score":1.0}"#.to_string(),
                }],
            )
            .expect("record pasted completion request");
        db.record_history(&RecordCommandRequest {
            command: "echo pasted".to_string(),
            cwd: "/tmp".to_string(),
            shell: Some("zsh".to_string()),
            trust: Some(TRUST_INTERACTIVE.to_string()),
            provenance: Some(PROVENANCE_PASTED.to_string()),
            provenance_source: Some(PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE.to_string()),
            provenance_confidence: Some(PROVENANCE_CONFIDENCE_EXACT.to_string()),
            origin: Some("zsh_precmd".to_string()),
            tty_present: Some(true),
            exit_status: None,
            accepted_request_id: Some(pasted_request_id),
            accepted_item_key: Some("echo pasted".to_string()),
            accepted_rank: Some(0),
        })
        .expect("record pasted command");

        let samples = db.training_samples(10).expect("training samples");
        assert_eq!(samples.len(), 2);
        assert_eq!(
            samples.iter().filter(|sample| sample.label == 1.0).count(),
            1
        );
        assert_eq!(
            samples.iter().filter(|sample| sample.label == 0.0).count(),
            1
        );
        assert!(samples.iter().all(|sample| sample.kind == "command"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn command_has_docs_returns_false_when_empty() {
        let db = test_db();
        assert!(!db.command_has_docs("nonexistent_cmd"));
    }

    #[test]
    fn command_has_docs_returns_true_after_replace() {
        let db = test_db();
        let doc = StoredDoc {
            command: "mycmd".into(),
            item_type: "subcommand".into(),
            item_value: "run".into(),
            description: "Run something".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("mycmd", &[doc]).unwrap();
        assert!(db.command_has_docs("mycmd"));
    }

    #[test]
    fn command_has_docs_does_not_bleed_across_commands() {
        let db = test_db();
        let doc = StoredDoc {
            command: "mycmd".into(),
            item_type: "subcommand".into(),
            item_value: "run".into(),
            description: "Run something".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("mycmd", &[doc]).unwrap();
        assert!(!db.command_has_docs("othercmd"));
    }
}
