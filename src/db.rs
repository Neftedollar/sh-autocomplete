use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

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

/// Maximum gap (seconds) between the previous command and the current one
/// for `record_history` to treat them as a prev->next transition. Without a
/// window, a long idle gap or an interleaved terminal tab would pair
/// unrelated commands together. 600s = 10 minutes.
const TRANSITION_MAX_GAP_SECS: i64 = 600;

/// Default retention window for completion telemetry (`completion_requests` /
/// `completion_items`). These tables are appended on every completion with no
/// other pruning, so inline mode can write tens of MB/day without this cap.
pub const COMPLETION_TELEMETRY_RETENTION_DAYS: i64 = 30;

/// Default retention window for recorded shell-command history
/// (`history_events`). This is the corpus behind history completion and
/// learned transitions, so it defaults to a full year — long enough for
/// suggestions to stay useful, but still bounded so the DB can't grow forever.
pub const HISTORY_RETENTION_DAYS: i64 = 365;

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
pub struct PathFrecency {
    pub path: String,
    pub rank: f64,
    pub last_visit: i64,
    pub visit_count: i64,
    pub source: String,
    pub is_git_repo: bool,
    pub project_marker: Option<String>,
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
        // The command-history DB must not be world-readable (F5). Best-effort
        // (no-op for the `:memory:` test db, which has no backing file).
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
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

            CREATE TABLE IF NOT EXISTS paths_index (
                path TEXT PRIMARY KEY,
                rank REAL NOT NULL DEFAULT 0.0,
                last_visit INTEGER NOT NULL DEFAULT 0,
                visit_count INTEGER NOT NULL DEFAULT 0,
                source TEXT NOT NULL,
                is_git_repo INTEGER NOT NULL DEFAULT 0,
                project_marker TEXT,
                created_ts INTEGER NOT NULL,
                updated_ts INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_paths_index_rank ON paths_index(rank DESC);
            CREATE INDEX IF NOT EXISTS idx_paths_index_last_visit ON paths_index(last_visit DESC);

            CREATE TABLE IF NOT EXISTS tips_state (
                tip_id          TEXT PRIMARY KEY,
                shows_count     INTEGER NOT NULL DEFAULT 0,
                last_shown_at   INTEGER,
                muted           INTEGER NOT NULL DEFAULT 0,
                muted_at        INTEGER,
                first_shown_at  INTEGER
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
        self.ensure_column("history_events", "import_hash", "TEXT")?;
        self.ensure_column("history_events", "imported_at", "INTEGER")?;

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
        self.conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_history_import_hash \
             ON history_events(import_hash) WHERE import_hash IS NOT NULL",
            [],
        )?;

        self.run_trust_migration_if_needed()?;
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
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
        // Wrap the delete + per-row inserts in one transaction: without it, a
        // failure partway through the inserts permanently loses/truncates a
        // command's docs (the DELETE already committed) and every row pays
        // its own commit.
        self.begin_txn()?;
        let result = (|| -> Result<()> {
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
        })();

        match result {
            Ok(()) => self.commit_txn(),
            Err(err) => {
                let _ = self.rollback_txn();
                Err(err)
            }
        }
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

    pub fn search_docs(&self, command: &str, query: &str, limit: usize) -> Result<Vec<StoredDoc>> {
        // FTS5 treats special chars (. * ^ " etc.) as syntax; wrap in double
        // quotes and escape internal quotes so arbitrary prefixes don't error.
        let escaped = query.replace('"', "\"\"");
        let fts_query = format!("\"{escaped}\"*");
        // Scope to the current command: without the `d.command = ?` filter the
        // FTS matched every command's docs, so `git c<Tab>` surfaced mdfind's
        // `-case_sensitive`, another tool's mangled roff, etc.
        let mut stmt = self.conn.prepare(
            "SELECT d.command, d.item_type, d.item_value, d.description, d.source
             FROM command_docs_fts f
             JOIN command_docs d ON d.id = f.rowid
             WHERE command_docs_fts MATCH ?1 AND d.command = ?2
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts_query, command, limit as i64], |row| {
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
        let prev = self.latest_command_with_ts()?;
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
            // Only pair prev->next as a transition when the gap between them
            // is within TRANSITION_MAX_GAP_SECS -- otherwise a long idle gap
            // or an interleaved terminal tab would record a bogus
            // transition between two unrelated commands.
            if let Some((prev_command, prev_ts)) = prev {
                if (ts - prev_ts).abs() <= TRANSITION_MAX_GAP_SECS {
                    self.conn.execute(
                        "INSERT INTO transitions(prev_command, next_command, count, interactive_count, legacy_count)
                         VALUES (?1, ?2, 1, 1, 0)
                         ON CONFLICT(prev_command, next_command)
                         DO UPDATE SET count = count + 1, interactive_count = interactive_count + 1",
                        params![prev_command, request.command],
                    )?;
                }
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

            // Hybrid-cd: extract `cd <path>` events into paths_index for
            // global frecency. Gated on the same clean-personalization signal
            // as transitions/profiles (F4) so pasted or legacy/imported `cd`s
            // don't seed path-jump suggestions from directories the user never
            // actually navigated to interactively. Bulk import seeds
            // paths_index through its own path (see import.rs), so cold-start
            // frecency is unaffected.
            if let Some(target) = extract_cd_target(&request.command) {
                let _ = self.upsert_path_index(&target, "cwd_event", false, None);
            }
        }

        Ok(classified)
    }

    /// Records one completion impression (`completion_requests` +
    /// `completion_items`) for local diagnostics (`shac stats` / `shac
    /// doctor`) only — this data never leaves the machine. Pruned by
    /// [`prune_completion_telemetry`](Self::prune_completion_telemetry)
    /// after `telemetry_retention_days` (config; default
    /// [`COMPLETION_TELEMETRY_RETENTION_DAYS`] days); set it to `0` for
    /// maximum privacy (everything pruned on the next cycle).
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
                let _ = self.set_meta_value_if_unset("first_accept_ts", &unix_ts().to_string());
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
                let _ = self.set_meta_value_if_unset("first_accept_ts", &unix_ts().to_string());
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn set_meta_value_if_unset(&self, key: &str, value: &str) -> Result<()> {
        if self.meta_value(key)?.is_none() {
            self.set_meta_value(key, value)?;
        }
        Ok(())
    }

    pub fn latest_command(&self) -> Result<Option<String>> {
        Ok(self.latest_command_with_ts()?.map(|(command, _ts)| command))
    }

    /// Like [`AppDb::latest_command`], but also returns the event's
    /// timestamp so callers (namely `record_history`) can bound how stale a
    /// "previous command" is before treating it as part of a transition.
    fn latest_command_with_ts(&self) -> Result<Option<(String, i64)>> {
        self.conn
            .query_row(
                "SELECT command, ts
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
                |row| Ok((row.get(0)?, row.get(1)?)),
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

    /// Detects the project root for `cwd` — the nearest ancestor directory
    /// containing a recognized project marker (`.git`, `Cargo.toml`, ...).
    ///
    /// Pure filesystem walk, no DB access, and depends only on `cwd`. A
    /// request scoring many candidates against the same `cwd` should call
    /// this once and reuse the result via [`AppDb::project_tool_count_for_root`]
    /// rather than re-walking the filesystem per candidate.
    pub fn project_root_for_cwd(&self, cwd: &str) -> Option<String> {
        detect_project_root(cwd)
    }

    /// Looks up the recorded usage weight for `tool` under an
    /// already-detected `project_root`, without re-walking the filesystem.
    /// Pairs with [`AppDb::project_root_for_cwd`].
    pub fn project_tool_count_for_root(
        &self,
        project_root: Option<&str>,
        tool: &str,
    ) -> Result<f64> {
        let Some(project_root) = project_root else {
            return Ok(0.0);
        };
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
        Ok(value)
    }

    pub fn project_tool_count(&self, cwd: &str, tool: &str) -> Result<f64> {
        self.project_tool_count_for_root(self.project_root_for_cwd(cwd).as_deref(), tool)
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

    /// Drop all memoized in-process caches: the SQLite `dir_cache` rows that
    /// store directory-listing snapshots keyed by mtime.  After this call the
    /// next completion request re-reads every directory from disk.  Safe to
    /// call at any time; no daemon restart required.
    pub fn invalidate_caches(&self) {
        // Ignore errors — if the table does not exist yet (empty DB) that is fine.
        let _ = self.conn.execute("DELETE FROM dir_cache", []);
    }

    /// Bump frecency for a path (rank += 1.0, clamped to 100.0). On insert, rank=1.0.
    pub fn upsert_path_index(
        &self,
        path: &str,
        source: &str,
        is_git_repo: bool,
        project_marker: Option<&str>,
    ) -> Result<()> {
        let now = unix_ts();
        let git_flag = if is_git_repo { 1 } else { 0 };
        self.conn.execute(
            "INSERT INTO paths_index(path, rank, last_visit, visit_count, source, is_git_repo, project_marker, created_ts, updated_ts)
             VALUES (?1, 1.0, ?2, 1, ?3, ?4, ?5, ?2, ?2)
             ON CONFLICT(path) DO UPDATE SET
                 rank = MIN(rank + 1.0, 100.0),
                 last_visit = excluded.last_visit,
                 visit_count = visit_count + 1,
                 updated_ts = excluded.updated_ts,
                 is_git_repo = MAX(is_git_repo, excluded.is_git_repo),
                 project_marker = COALESCE(excluded.project_marker, project_marker)",
            params![path, now, source, git_flag, project_marker],
        )?;
        Ok(())
    }

    /// Insert/update a path with explicit rank/last_visit (for zoxide / project scan importers).
    /// On conflict: only update rank/last_visit if the new rank is higher.
    pub fn upsert_path_index_with_rank(
        &self,
        path: &str,
        rank: f64,
        last_visit: i64,
        source: &str,
        is_git_repo: bool,
        project_marker: Option<&str>,
    ) -> Result<()> {
        let now = unix_ts();
        let git_flag = if is_git_repo { 1 } else { 0 };
        self.conn.execute(
            "INSERT INTO paths_index(path, rank, last_visit, visit_count, source, is_git_repo, project_marker, created_ts, updated_ts)
             VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(path) DO UPDATE SET
                 rank = MAX(rank, excluded.rank),
                 last_visit = MAX(last_visit, excluded.last_visit),
                 updated_ts = excluded.updated_ts,
                 is_git_repo = MAX(is_git_repo, excluded.is_git_repo),
                 project_marker = COALESCE(excluded.project_marker, project_marker)",
            params![path, rank, last_visit, source, git_flag, project_marker, now],
        )?;
        Ok(())
    }

    /// Top frecent paths, ranked by `rank * decay(now - last_visit)`.
    /// `prefix_filter`: optional case-insensitive substring match on `path`.
    pub fn top_paths(
        &self,
        prefix_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<PathFrecency>> {
        // Decay matches engine.rs::recency_score: 1 / (1 + age_hours).
        // Computed in Rust after pulling rows; we order in SQL by rank, then re-sort.
        // We over-fetch by 4x to give room for decay-based reordering, capped.
        let fetch = (limit * 4).max(limit + 16);
        let now = unix_ts();

        // A single `ORDER BY rank DESC` prefetch can starve a recently
        // visited but low-rank path: if enough stale, higher-rank rows
        // exist, the recent row never makes it into `rows` for the Rust
        // decay pass below to even consider. Pull a second candidate set
        // ordered by recency and union it in so a recent low-rank path
        // always has a chance to surface.
        let mut rows = self.top_paths_candidates(prefix_filter, "rank DESC", fetch)?;
        for candidate in self.top_paths_candidates(prefix_filter, "last_visit DESC", fetch)? {
            if !rows.iter().any(|existing| existing.path == candidate.path) {
                rows.push(candidate);
            }
        }

        rows.sort_by(|a, b| {
            let sa = path_frecency_decayed(a.rank, a.last_visit, now);
            let sb = path_frecency_decayed(b.rank, b.last_visit, now);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        rows.truncate(limit);
        Ok(rows)
    }

    /// Fetches up to `fetch` rows from `paths_index`, optionally filtered by
    /// a case-insensitive substring match on `path`, ordered by `order_by`
    /// (a trusted, internally-controlled SQL fragment — never user input).
    fn top_paths_candidates(
        &self,
        prefix_filter: Option<&str>,
        order_by: &str,
        fetch: usize,
    ) -> Result<Vec<PathFrecency>> {
        let sql = if prefix_filter.is_some() {
            format!(
                "SELECT path, rank, last_visit, visit_count, source, is_git_repo, project_marker
                 FROM paths_index
                 WHERE LOWER(path) LIKE ?1 ESCAPE '\\'
                 ORDER BY {order_by}
                 LIMIT ?2"
            )
        } else {
            format!(
                "SELECT path, rank, last_visit, visit_count, source, is_git_repo, project_marker
                 FROM paths_index
                 ORDER BY {order_by}
                 LIMIT ?1"
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = if let Some(filter) = prefix_filter {
            // `filter` is the raw user-typed token; escape LIKE metacharacters
            // so a literal `_`/`%` someone typed doesn't act as a wildcard
            // and match unrelated paths.
            let pattern = format!("%{}%", escape_like_literal(&filter.to_lowercase()));
            stmt.query_map(params![pattern, fetch as i64], map_path_frecency_row)?
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        } else {
            stmt.query_map(params![fetch as i64], map_path_frecency_row)?
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        Ok(rows)
    }

    /// Maximum rank across paths_index (for normalization).
    pub fn paths_index_max_rank(&self) -> Result<f64> {
        let value: Option<f64> = self
            .conn
            .query_row("SELECT MAX(rank) FROM paths_index", [], |row| row.get(0))
            .optional()?
            .flatten();
        Ok(value.unwrap_or(0.0))
    }

    /// Look up the rank for a single path; 0.0 if missing.
    pub fn path_rank(&self, path: &str) -> Result<f64> {
        let value: Option<f64> = self
            .conn
            .query_row(
                "SELECT rank FROM paths_index WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.unwrap_or(0.0))
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
        let history_total = count(&self.conn, "history_events")?;
        let imported_history = self.count_imported_history()?;
        let imported_zoxide = self.count_paths_index_by_source("zoxide_import")?;
        let scanned_projects = self.count_paths_index_by_source("project_scan")?;
        let paths_index_rows = self.count_paths_index()?;
        let install_ts: Option<i64> = self.meta_value("install_ts")?.and_then(|v| v.parse().ok());
        let first_accept_ts: Option<i64> = self
            .meta_value("first_accept_ts")?
            .and_then(|v| v.parse().ok());
        let time_to_first_accept_seconds = match (install_ts, first_accept_ts) {
            (Some(install), Some(accept)) => Some(accept - install),
            _ => None,
        };
        let import_coverage_pct = if history_total > 0 {
            (imported_history as f64) / (history_total as f64) * 100.0
        } else {
            0.0
        };
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
            imported_history_events: imported_history,
            imported_zoxide_paths: imported_zoxide,
            scanned_project_paths: scanned_projects,
            paths_index_rows,
            time_to_first_accept_seconds,
            import_coverage_pct,
            // The DB layer has no config access; `Engine::stats` overwrites
            // this with the configured `telemetry_retention_days`. The
            // fallback here only matters for direct `AppDb::stats()` callers
            // (e.g. tests) that skip the `Engine` wrapper.
            telemetry_retention_days: COMPLETION_TELEMETRY_RETENTION_DAYS as u32,
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

    #[allow(clippy::too_many_arguments)]
    pub fn insert_imported_history(
        &self,
        ts: i64,
        cwd: &str,
        command: &str,
        shell: Option<&str>,
        import_hash: &str,
        trust: &str,
        provenance: &str,
    ) -> Result<bool> {
        let imported_at = unix_ts();
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO history_events(
                ts, cwd, command, shell, trust, provenance,
                provenance_source, provenance_confidence, origin, tty_present,
                import_hash, imported_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11)",
            params![
                ts,
                cwd,
                command,
                shell,
                trust,
                provenance,
                PROVENANCE_SOURCE_UNKNOWN,
                PROVENANCE_CONFIDENCE_UNKNOWN,
                "import",
                import_hash,
                imported_at
            ],
        )?;
        Ok(changed > 0)
    }

    /// Batch-insert imported history rows in a single multi-VALUES statement
    /// using `INSERT OR IGNORE`. Returns the number of rows actually inserted
    /// (changes() reflects rows the unique partial index accepted). Empty
    /// batches are a no-op and return 0.
    ///
    /// Each row is a tuple `(ts, cwd, command, shell, import_hash, trust,
    /// provenance)`. `imported_at` is filled with `unix_ts()` per call.
    /// `provenance_source`, `provenance_confidence`, `origin`, and
    /// `tty_present` use the same constants as `insert_imported_history`.
    #[allow(clippy::type_complexity)]
    pub fn insert_imported_history_batch(
        &self,
        rows: &[(i64, String, String, Option<String>, String, String, String)],
    ) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let imported_at = unix_ts();
        let mut sql = String::from(
            "INSERT OR IGNORE INTO history_events(\
                ts, cwd, command, shell, trust, provenance,\
                provenance_source, provenance_confidence, origin, tty_present,\
                import_hash, imported_at\
             ) VALUES ",
        );
        // 12 columns per row; build placeholders.
        let placeholders_per_row = "(?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)";
        for i in 0..rows.len() {
            if i > 0 {
                sql.push(',');
            }
            sql.push_str(placeholders_per_row);
        }
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(rows.len() * 11);
        for (ts, cwd, command, shell, import_hash, trust, provenance) in rows {
            params.push(Box::new(*ts));
            params.push(Box::new(cwd.clone()));
            params.push(Box::new(command.clone()));
            params.push(Box::new(shell.clone()));
            params.push(Box::new(trust.clone()));
            params.push(Box::new(provenance.clone()));
            params.push(Box::new(PROVENANCE_SOURCE_UNKNOWN));
            params.push(Box::new(PROVENANCE_CONFIDENCE_UNKNOWN));
            params.push(Box::new("import"));
            params.push(Box::new(import_hash.clone()));
            params.push(Box::new(imported_at));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let changed = self
            .conn
            .execute(&sql, rusqlite::params_from_iter(refs.iter()))?;
        Ok(changed)
    }

    pub fn begin_txn(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }
    pub fn commit_txn(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
    pub fn rollback_txn(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.meta_value(key)
    }
    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.set_meta_value(key, value)
    }
    pub fn meta_set_if_unset(&self, key: &str, value: &str) -> Result<()> {
        if self.meta_value(key)?.is_none() {
            self.set_meta_value(key, value)?;
        }
        Ok(())
    }

    pub fn count_paths_index(&self) -> Result<i64> {
        count(&self.conn, "paths_index")
    }
    pub fn count_paths_index_by_source(&self, source: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM paths_index WHERE source = ?1",
            params![source],
            |row| row.get(0),
        )?)
    }
    pub fn count_imported_history(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM history_events WHERE import_hash IS NOT NULL",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn load_tips_state(
        &self,
    ) -> Result<std::collections::HashMap<String, crate::tips::storage::TipState>> {
        crate::tips::storage::load_all(&self.conn)
    }

    pub fn record_tip_show(&self, tip_id: &str, now: i64) -> Result<()> {
        crate::tips::storage::record_show(&self.conn, tip_id, now)
    }

    /// Atomically claim "first run done". Returns true if this call was the first
    /// to mark it (caller should emit the greeter); false if it was already marked.
    pub fn try_claim_first_run(&self) -> Result<bool> {
        let rows = self
            .conn
            .execute(
                "INSERT INTO app_meta(key, value) VALUES ('tips_first_run_done', '1') \
                 ON CONFLICT(key) DO NOTHING",
                [],
            )
            .context("claim first run")?;
        Ok(rows == 1)
    }

    pub fn command_known(&self, name: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM commands WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // TODO(v0.6): wire to actual acceptance counts from completion_requests.
    // v0.5.0 returns empty (neutral priority signal — see spec § Selection algorithm).
    pub fn zero_acceptance_sources(&self) -> Result<std::collections::HashSet<String>> {
        // v1: returns empty (soft signal not wired). Spec calls this out as deferred.
        Ok(std::collections::HashSet::new())
    }

    /// Deletes ALL `completion_requests` older than `retention_days` (the
    /// configured `telemetry_retention_days`, default
    /// [`COMPLETION_TELEMETRY_RETENTION_DAYS`]), uniformly — including rows
    /// carrying the acceptance-tracking signal. That carve-out existed only
    /// to preserve ML training signal for the (now removed) learner; with no
    /// learner reading this data, there's no reason to keep any row past the
    /// retention window it was told to observe. `retention_days <= 0` prunes
    /// everything on the next cycle (maximum privacy). `completion_items`
    /// rows are removed via the `ON DELETE CASCADE` FK (see schema in
    /// `init`, which also enables `PRAGMA foreign_keys = ON`) rather than a
    /// second DELETE. Returns the number of requests pruned.
    pub fn prune_completion_telemetry(&self, retention_days: i64) -> Result<usize> {
        let cutoff = unix_ts() - retention_days.max(0) * 86_400;
        let deleted = self
            .conn
            .execute(
                "DELETE FROM completion_requests WHERE ts < ?1",
                params![cutoff],
            )
            .context("prune completion telemetry")?;
        Ok(deleted)
    }

    /// Deletes `history_events` rows older than `retention_days` (the
    /// configured `history_retention_days`, default [`HISTORY_RETENTION_DAYS`]).
    /// This is the recorded shell-command corpus behind history completion and
    /// learned transitions. `retention_days <= 0` prunes everything on the next
    /// cycle, for users who don't want persistent history at all. Returns the
    /// number of rows pruned.
    ///
    /// Retention is measured from whichever clock is more recent: the command's
    /// own timestamp (live rows) or when it entered our DB (`imported_at`, for
    /// imported rows). Plain (non-`EXTENDED_HISTORY`) zsh history — the zsh
    /// default — carries no timestamps, so `import` stores `ts = 0` and keeps
    /// the real clock in `imported_at`. Pruning on `ts` alone would delete the
    /// entire imported corpus on the first tick (and re-arm on every re-import,
    /// since the `import_hash` rows vanish with it). A row survives while
    /// EITHER clock is inside the window.
    pub fn prune_history_events(&self, retention_days: i64) -> Result<usize> {
        let cutoff = unix_ts() - retention_days.max(0) * 86_400;
        let deleted = self
            .conn
            .execute(
                "DELETE FROM history_events
                 WHERE ts < ?1 AND (imported_at IS NULL OR imported_at < ?1)",
                params![cutoff],
            )
            .context("prune history events")?;
        Ok(deleted)
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
        // `prefix` is the raw user-typed active token; escape LIKE
        // metacharacters so a literal `_`/`%` someone typed doesn't act as a
        // wildcard and match unrelated history.
        let like = format!("{}%", escape_like_literal(prefix));
        let weighted_case = weighted_history_case();
        let sql = if cwd.is_some() {
            format!(
                "SELECT command,
                    SUM({weighted_case}) AS weighted_cnt,
                    MAX(ts) AS last_seen
             FROM history_events
             WHERE cwd = ?1 AND command LIKE ?2 ESCAPE '\\'
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
             WHERE command LIKE ?1 ESCAPE '\\'
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

fn path_frecency_decayed(rank: f64, last_visit: i64, now: i64) -> f64 {
    let age_secs = (now - last_visit).max(0) as f64;
    let decay = 1.0 / (1.0 + age_secs / 3600.0);
    rank * decay
}

fn map_path_frecency_row(row: &rusqlite::Row) -> rusqlite::Result<PathFrecency> {
    Ok(PathFrecency {
        path: row.get(0)?,
        rank: row.get(1)?,
        last_visit: row.get(2)?,
        visit_count: row.get(3)?,
        source: row.get(4)?,
        is_git_repo: row.get::<_, i64>(5)? != 0,
        project_marker: row.get(6)?,
    })
}

/// Escapes `%`, `_`, and the escape character itself so a raw, user-typed
/// token embedded in a SQL `LIKE` pattern matches literally instead of being
/// interpreted as a wildcard. Callers must pair this with `ESCAPE '\'` on
/// the `LIKE` predicate.
fn escape_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Resolve a `cd` target to an absolute path string.
/// Returns None for relative paths and shell substitutions (skip).
fn resolve_cd_target(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() || target.contains('$') || target.contains('`') {
        return None;
    }
    if let Some(rest) = target.strip_prefix("~/") {
        let home = dirs::home_dir()?;
        return Some(home.join(rest).to_string_lossy().to_string());
    }
    if target == "~" {
        let home = dirs::home_dir()?;
        return Some(home.to_string_lossy().to_string());
    }
    if target.starts_with('/') {
        return Some(target.to_string());
    }
    None
}

/// If `command` is a `cd <path>`, return the resolved absolute target.
///
/// A quoted argument (single or double quotes) is taken whole up to its
/// matching closing quote -- embedded spaces included -- rather than being
/// cut at the first whitespace. An unquoted argument still stops at the
/// first unescaped whitespace (so `cd /a/b c` is still two words), but a
/// backslash-escaped space is treated as part of the path.
fn extract_cd_target(command: &str) -> Option<String> {
    let trimmed = command.trim();
    let rest = trimmed.strip_prefix("cd ")?;
    let arg = rest.trim();
    if arg.is_empty() {
        return None;
    }
    let mut chars = arg.chars();
    let target = match chars.next()? {
        quote @ ('"' | '\'') => {
            // Take everything up to the matching closing quote as the
            // literal path. An unterminated quote falls back to "the rest
            // of the argument" rather than silently truncating.
            let body = chars.as_str();
            match body.find(quote) {
                Some(end) => body[..end].to_string(),
                None => body.to_string(),
            }
        }
        _ => {
            let mut result = String::new();
            let mut it = arg.chars().peekable();
            while let Some(c) = it.next() {
                if c == '\\' {
                    if let Some(&next) = it.peek() {
                        if next.is_whitespace() {
                            result.push(next);
                            it.next();
                            continue;
                        }
                    }
                    result.push(c);
                    continue;
                }
                if c.is_whitespace() {
                    break;
                }
                result.push(c);
            }
            result
        }
    };
    if target.is_empty() {
        return None;
    }
    resolve_cd_target(&target)
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
    // Require a token boundary after `item_key` so a mere string prefix
    // (e.g. `git` vs. an executed `github-cli ...`) isn't credited as an
    // accepted completion.
    if executed_command == item_key || executed_command.starts_with(&format!("{item_key} ")) {
        return true;
    }
    // Path-like item_keys legitimately extend without a space boundary
    // (e.g. `src/foo` completed further into `src/foobar`), so credit a
    // plain prefix match for those. The candidate's completion `kind` isn't
    // in scope here, so fall back to treating a '/' in item_key as the
    // path signal.
    if (item_key.contains('/') || item_key.ends_with('/')) && executed_command.starts_with(item_key)
    {
        return true;
    }
    false
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

    /// Minimal "clean" (interactive, typed_manual) command request for tests
    /// exercising history/transition scoring where no other classification
    /// field is under test.
    fn typed_request(command: &str, cwd: &str) -> RecordCommandRequest {
        RecordCommandRequest {
            command: command.to_string(),
            cwd: cwd.to_string(),
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
        }
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

    #[test]
    fn paths_index_upsert_and_top_paths() {
        let db = test_db();
        let now = unix_ts();
        db.upsert_path_index_with_rank("/tmp/aaa", 3.0, now, "test", false, None)
            .unwrap();
        db.upsert_path_index_with_rank("/tmp/bbb", 5.0, now, "test", false, None)
            .unwrap();
        db.upsert_path_index_with_rank("/tmp/ccc", 1.0, now, "test", false, None)
            .unwrap();

        let top = db.top_paths(None, 5).unwrap();
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].path, "/tmp/bbb");
        assert_eq!(top[1].path, "/tmp/aaa");
        assert_eq!(top[2].path, "/tmp/ccc");
    }

    #[test]
    fn top_paths_filters_by_prefix() {
        let db = test_db();
        let now = unix_ts();
        db.upsert_path_index_with_rank("/tmp/alpha", 2.0, now, "test", false, None)
            .unwrap();
        db.upsert_path_index_with_rank("/tmp/beta", 3.0, now, "test", false, None)
            .unwrap();

        let filtered = db.top_paths(Some("alp"), 10).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].path, "/tmp/alpha");
    }

    #[test]
    fn cd_history_event_populates_paths_index() {
        let db = test_db();
        db.record_history(&RecordCommandRequest {
            command: "cd /tmp/foo".to_string(),
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
        .expect("record cd history");

        let top = db.top_paths(None, 10).unwrap();
        assert!(
            top.iter().any(|p| p.path == "/tmp/foo"),
            "expected /tmp/foo in paths_index, got: {top:?}"
        );
    }

    #[test]
    fn upsert_path_index_increments_rank() {
        let db = test_db();
        db.upsert_path_index("/tmp/foo", "cwd_event", false, None)
            .unwrap();
        db.upsert_path_index("/tmp/foo", "cwd_event", false, None)
            .unwrap();
        db.upsert_path_index("/tmp/foo", "cwd_event", false, None)
            .unwrap();
        let top = db.top_paths(None, 10).unwrap();
        let row = top.iter().find(|p| p.path == "/tmp/foo").unwrap();
        assert_eq!(row.visit_count, 3);
        assert!(row.rank >= 3.0);
    }

    #[test]
    fn invalidate_caches_clears_dir_cache() {
        let db = test_db();
        db.upsert_dir_cache("/tmp/testdir", 12345, "file1\nfile2")
            .unwrap();
        // Confirm the entry is there.
        let cached = db.get_dir_cache("/tmp/testdir").unwrap();
        assert!(
            cached.is_some(),
            "expected dir_cache entry before invalidation"
        );
        // Invalidate.
        db.invalidate_caches();
        // Entry should be gone.
        let cached_after = db.get_dir_cache("/tmp/testdir").unwrap();
        assert!(
            cached_after.is_none(),
            "expected dir_cache entry to be removed after invalidate_caches"
        );
    }

    #[test]
    fn extract_cd_target_resolves_absolute_and_skips_relative() {
        assert_eq!(
            extract_cd_target("cd /tmp/foo"),
            Some("/tmp/foo".to_string())
        );
        assert_eq!(
            extract_cd_target("cd  /tmp/foo "),
            Some("/tmp/foo".to_string())
        );
        assert_eq!(
            extract_cd_target("cd \"/tmp/foo\""),
            Some("/tmp/foo".to_string())
        );
        assert_eq!(extract_cd_target("cd ./relative"), None);
        assert_eq!(extract_cd_target("cd $VAR"), None);
        assert_eq!(extract_cd_target("git cd /tmp"), None);
        assert_eq!(extract_cd_target("cd"), None);
    }

    /// C3: a quoted `cd` argument containing spaces must be indexed as the
    /// full path, not truncated at the first space inside the quotes.
    #[test]
    fn extract_cd_target_quoted_path_with_spaces_keeps_full_path() {
        assert_eq!(
            extract_cd_target("cd \"/Users/roman/My Drive\""),
            Some("/Users/roman/My Drive".to_string())
        );
        assert_eq!(
            extract_cd_target("cd '/Users/roman/My Drive'"),
            Some("/Users/roman/My Drive".to_string())
        );
    }

    /// An unquoted argument with a backslash-escaped space is also a single
    /// path, not two words.
    #[test]
    fn extract_cd_target_escaped_space_keeps_full_path() {
        assert_eq!(
            extract_cd_target("cd /Users/roman/My\\ Drive"),
            Some("/Users/roman/My Drive".to_string())
        );
    }

    /// An unquoted single path is unchanged, and an unquoted argument
    /// followed by an unrelated second word is still cut at the first
    /// (unescaped) space -- `cd /a/b c` really is two words.
    #[test]
    fn extract_cd_target_unquoted_path_unchanged() {
        assert_eq!(
            extract_cd_target("cd /tmp/foo"),
            Some("/tmp/foo".to_string())
        );
        assert_eq!(extract_cd_target("cd /a/b c"), Some("/a/b".to_string()));
    }

    /// `cd -` and bare `cd` (no arg) behave the same as before the fix.
    #[test]
    fn extract_cd_target_cd_dash_and_bare_cd_unchanged() {
        assert_eq!(extract_cd_target("cd -"), None);
        assert_eq!(extract_cd_target("cd"), None);
    }

    /// B4: completion telemetry older than the retention window is pruned
    /// uniformly, regardless of acceptance status — the carve-out for
    /// accepted/eligible-for-learning rows existed only to preserve signal
    /// for the (now removed) ML learner. completion_items cascades via the
    /// schema's `ON DELETE CASCADE` FK (enabled by `PRAGMA foreign_keys =
    /// ON` in `init`) rather than needing an explicit second DELETE.
    #[test]
    fn prune_completion_telemetry_deletes_all_rows_older_than_cutoff() {
        let db = test_db();
        let now = unix_ts();
        let old_ts = now - 40 * 86_400;
        let recent_ts = now - 86_400;

        // Old + never accepted: pure telemetry noise — must be pruned.
        db.conn
            .execute(
                "INSERT INTO completion_requests(ts, shell, cwd, line, cursor, active_token)
                 VALUES (?1, 'zsh', '/tmp', 'ls', 2, 'ls')",
                params![old_ts],
            )
            .expect("insert old unaccepted request");
        let old_unaccepted_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO completion_items(request_id, rank, item_key, insert_text, display, kind, source, score, feature_json)
                 VALUES (?1, 0, 'ls', 'ls', 'ls', 'command', 'path_index', 1.0, '{}')",
                params![old_unaccepted_id],
            )
            .expect("insert old unaccepted item");

        // Old AND accepted + eligible for learning: with no learner left to
        // read this signal, this must now be pruned too (previously it
        // survived indefinitely).
        db.conn
            .execute(
                "INSERT INTO completion_requests(ts, shell, cwd, line, cursor, active_token, eligible_for_learning, accepted_command)
                 VALUES (?1, 'zsh', '/tmp', 'gi', 2, 'gi', 1, 'git status')",
                params![old_ts],
            )
            .expect("insert old accepted request");
        let old_accepted_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO completion_items(request_id, rank, item_key, insert_text, display, kind, source, score, feature_json)
                 VALUES (?1, 0, 'git status', 'git status', 'git status', 'command', 'path_index', 1.0, '{}')",
                params![old_accepted_id],
            )
            .expect("insert old accepted item");

        // Recent + never accepted: within the retention window — must survive
        // regardless of acceptance.
        db.conn
            .execute(
                "INSERT INTO completion_requests(ts, shell, cwd, line, cursor, active_token)
                 VALUES (?1, 'zsh', '/tmp', 'cd', 2, 'cd')",
                params![recent_ts],
            )
            .expect("insert recent request");
        let recent_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO completion_items(request_id, rank, item_key, insert_text, display, kind, source, score, feature_json)
                 VALUES (?1, 0, 'cd', 'cd', 'cd', 'command', 'path_index', 1.0, '{}')",
                params![recent_id],
            )
            .expect("insert recent item");

        let deleted = db
            .prune_completion_telemetry(COMPLETION_TELEMETRY_RETENTION_DAYS)
            .expect("prune");
        assert_eq!(
            deleted, 2,
            "both old rows are pruned regardless of acceptance"
        );

        let remaining_requests: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM completion_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_requests, 1);

        let remaining_items: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM completion_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_items, 1);

        let surviving_ids: Vec<i64> = db
            .conn
            .prepare("SELECT id FROM completion_requests ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            surviving_ids,
            vec![recent_id],
            "only the recent row survives; old-accepted and old-unaccepted are both pruned"
        );

        let old_accepted_items: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM completion_items WHERE request_id = ?1",
                params![old_accepted_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            old_accepted_items, 0,
            "old-accepted row's completion_items must cascade away with it"
        );
    }

    /// `telemetry_retention_days = 0` (max privacy) prunes everything on the
    /// next cycle, including rows recorded moments ago.
    #[test]
    fn prune_completion_telemetry_zero_retention_prunes_everything() {
        let db = test_db();
        let now = unix_ts();

        db.conn
            .execute(
                "INSERT INTO completion_requests(ts, shell, cwd, line, cursor, active_token)
                 VALUES (?1, 'zsh', '/tmp', 'ls', 2, 'ls')",
                params![now - 1],
            )
            .expect("insert request");

        let deleted = db.prune_completion_telemetry(0).expect("prune");
        assert_eq!(deleted, 1);

        let remaining_requests: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM completion_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_requests, 0);
    }

    #[test]
    fn prune_history_events_removes_old_keeps_recent() {
        let db = test_db();
        let now = unix_ts();
        // One row older than the 365-day default window, one recent.
        for (ts, cmd) in [(now - 400 * 86_400, "ls -old"), (now - 1, "ls -recent")] {
            db.conn
                .execute(
                    "INSERT INTO history_events(ts, cwd, command) VALUES (?1, '/tmp', ?2)",
                    params![ts, cmd],
                )
                .expect("insert history");
        }

        let deleted = db.prune_history_events(365).expect("prune history");
        assert_eq!(deleted, 1, "only the 400-day-old row is pruned");

        let surviving: Vec<String> = db
            .conn
            .prepare("SELECT command FROM history_events ORDER BY ts")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(surviving, vec!["ls -recent".to_string()]);
    }

    /// Plain (non-EXTENDED) zsh history imports store `ts = 0` with the real
    /// clock in `imported_at`. Retention must key off `imported_at` for those,
    /// or a single prune tick wipes the whole imported corpus. Recently-imported
    /// zero-ts rows survive; a stale import (imported > window ago) is pruned.
    #[test]
    fn prune_history_events_keeps_recently_imported_zero_ts() {
        let db = test_db();
        let now = unix_ts();
        for (ts, imported_at, cmd) in [
            (0_i64, now - 1, "imported-fresh"), // zero ts, just imported
            (0_i64, now - 400 * 86_400, "imported-stale"), // imported > 365d ago
        ] {
            db.conn
                .execute(
                    "INSERT INTO history_events(ts, cwd, command, imported_at)
                     VALUES (?1, '/tmp', ?2, ?3)",
                    params![ts, cmd, imported_at],
                )
                .expect("insert imported history");
        }

        let deleted = db.prune_history_events(365).expect("prune history");
        assert_eq!(deleted, 1, "only the stale import is pruned");

        let surviving: Vec<String> = db
            .conn
            .prepare("SELECT command FROM history_events")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            surviving,
            vec!["imported-fresh".to_string()],
            "a freshly imported zero-ts row must survive retention"
        );
    }

    /// `history_retention_days = 0` prunes everything on the next cycle, for
    /// users who don't want any persistent shell history.
    #[test]
    fn prune_history_events_zero_retention_prunes_everything() {
        let db = test_db();
        let now = unix_ts();
        db.conn
            .execute(
                "INSERT INTO history_events(ts, cwd, command) VALUES (?1, '/tmp', 'ls')",
                params![now - 1],
            )
            .expect("insert history");

        let deleted = db.prune_history_events(0).expect("prune");
        assert_eq!(deleted, 1);
        let remaining: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM history_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn frequent_history_prefix_escapes_like_wildcards() {
        let db = test_db();
        db.record_history(&typed_request("abc_def build", "/tmp"))
            .expect("record literal-underscore command");
        db.record_history(&typed_request("abcXdef build", "/tmp"))
            .expect("record command that would match `_` as a wildcard");

        let matches = db
            .frequent_history("abc_def", "/tmp", 10)
            .expect("history lookup");
        assert!(matches.iter().any(|entry| entry.command == "abc_def build"));
        assert!(
            !matches.iter().any(|entry| entry.command == "abcXdef build"),
            "underscore in the typed prefix must not act as a SQL LIKE wildcard: {matches:?}"
        );
    }

    #[test]
    fn top_paths_prefix_filter_escapes_like_wildcards() {
        let db = test_db();
        let now = unix_ts();
        db.upsert_path_index_with_rank("/tmp/100%done", 5.0, now, "test", false, None)
            .unwrap();
        db.upsert_path_index_with_rank("/tmp/100xxxdone", 5.0, now, "test", false, None)
            .unwrap();

        let filtered = db.top_paths(Some("100%done"), 10).unwrap();
        assert!(filtered.iter().any(|p| p.path == "/tmp/100%done"));
        assert!(
            !filtered.iter().any(|p| p.path == "/tmp/100xxxdone"),
            "percent sign in the prefix filter must not act as a SQL LIKE wildcard: {filtered:?}"
        );
    }

    #[test]
    fn record_history_transition_recorded_within_gap_window() {
        let db = test_db();
        db.record_history(&typed_request("git status", "/tmp"))
            .expect("record prev command");
        // Backdate the prev command, but keep it comfortably inside
        // TRANSITION_MAX_GAP_SECS.
        db.conn
            .execute(
                "UPDATE history_events SET ts = ts - 60 WHERE command = 'git status'",
                [],
            )
            .expect("backdate prev command");

        db.record_history(&typed_request("git checkout main", "/tmp"))
            .expect("record next command");

        let transitions = db
            .transitions_from("git status", 10)
            .expect("transition lookup");
        assert!(
            transitions.iter().any(|t| t.next == "git checkout main"),
            "commands within the transition window should record a transition: {transitions:?}"
        );
    }

    #[test]
    fn record_history_transition_skipped_when_gap_exceeds_window() {
        let db = test_db();
        db.record_history(&typed_request("git status", "/tmp"))
            .expect("record prev command");
        // Backdate the prev command well past TRANSITION_MAX_GAP_SECS, as if
        // separated by a long idle gap or an interleaved terminal tab.
        db.conn
            .execute(
                "UPDATE history_events SET ts = ts - ?1 WHERE command = 'git status'",
                params![TRANSITION_MAX_GAP_SECS + 60],
            )
            .expect("backdate prev command");

        db.record_history(&typed_request("git checkout main", "/tmp"))
            .expect("record next command");

        let transitions = db
            .transitions_from("git status", 10)
            .expect("transition lookup");
        assert!(
            transitions.is_empty(),
            "commands separated by more than the transition window must not pair up: {transitions:?}"
        );
    }

    #[test]
    fn top_paths_considers_recent_low_rank_path_despite_many_stale_high_rank_rows() {
        let db = test_db();
        let now = unix_ts();
        let stale_last_visit = now - 60 * 60 * 24 * 30; // 30 days old

        // Enough stale, high-rank rows to fill the raw-rank prefetch window
        // (fetch = max(limit*4, limit+16)) so a recent, low-rank path can
        // only surface if a recency-ordered candidate set is also pulled.
        for i in 0..30 {
            db.upsert_path_index_with_rank(
                &format!("/tmp/stale-{i}"),
                100.0 - i as f64,
                stale_last_visit,
                "test",
                false,
                None,
            )
            .unwrap();
        }
        db.upsert_path_index_with_rank("/tmp/recent-low-rank", 0.5, now, "test", false, None)
            .unwrap();

        let top = db.top_paths(None, 5).unwrap();
        assert!(
            top.iter().any(|p| p.path == "/tmp/recent-low-rank"),
            "a recently-visited low-rank path must be considered, not starved by stale high-rank rows: {top:?}"
        );
    }

    #[test]
    fn replace_docs_for_command_all_rows_land() {
        let db = test_db();
        let docs = vec![
            StoredDoc {
                command: "mycmd".into(),
                item_type: "subcommand".into(),
                item_value: "run".into(),
                description: "Run something".into(),
                source: "help".into(),
            },
            StoredDoc {
                command: "mycmd".into(),
                item_type: "subcommand".into(),
                item_value: "build".into(),
                description: "Build something".into(),
                source: "help".into(),
            },
            StoredDoc {
                command: "mycmd".into(),
                item_type: "flag".into(),
                item_value: "--verbose".into(),
                description: "Verbose output".into(),
                source: "help".into(),
            },
        ];
        db.replace_docs_for_command("mycmd", &docs)
            .expect("replace docs");

        let stored = db.docs_for_command("mycmd").expect("read back docs");
        assert_eq!(stored.len(), 3, "all rows must land: {stored:?}");
        for expected in ["run", "build", "--verbose"] {
            assert!(
                stored.iter().any(|d| d.item_value == expected),
                "missing {expected} in {stored:?}"
            );
        }
    }

    #[test]
    fn replace_docs_for_command_rolls_back_on_partial_failure() {
        let db = test_db();
        let original = StoredDoc {
            command: "mycmd".into(),
            item_type: "subcommand".into(),
            item_value: "old".into(),
            description: "original doc".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("mycmd", &[original])
            .expect("seed original doc");

        // Force the second insert of the next replace call to fail, to
        // simulate a mid-batch error after the DELETE and first INSERT have
        // already run within the same transaction.
        db.conn
            .execute_batch(
                "CREATE TRIGGER test_fail_on_sentinel
                 BEFORE INSERT ON command_docs
                 WHEN NEW.item_value = 'FAIL_SENTINEL'
                 BEGIN
                     SELECT RAISE(ABORT, 'simulated mid-batch failure');
                 END;",
            )
            .expect("install failure trigger");

        let replacement = vec![
            StoredDoc {
                command: "mycmd".into(),
                item_type: "subcommand".into(),
                item_value: "one".into(),
                description: "first replacement".into(),
                source: "help".into(),
            },
            StoredDoc {
                command: "mycmd".into(),
                item_type: "subcommand".into(),
                item_value: "FAIL_SENTINEL".into(),
                description: "second replacement".into(),
                source: "help".into(),
            },
        ];

        let result = db.replace_docs_for_command("mycmd", &replacement);
        assert!(
            result.is_err(),
            "expected the seeded failure trigger to fail the second insert"
        );

        let docs = db.docs_for_command("mycmd").expect("read back docs");
        assert_eq!(
            docs.len(),
            1,
            "a failed replace must roll back to the prior state, not a partial one: {docs:?}"
        );
        assert_eq!(docs[0].item_value, "old");
    }

    #[test]
    fn command_matches_completion_requires_token_boundary() {
        assert!(command_matches_completion("git status", "git"));
        assert!(command_matches_completion("git", "git"));
        assert!(
            !command_matches_completion("github-cli status", "git"),
            "a mere string prefix must not be credited as an accepted completion"
        );
    }

    #[test]
    fn command_matches_completion_credits_path_prefix_without_space() {
        // Path-like item_keys (containing '/') legitimately extend without a
        // space token boundary, e.g. `src/foo` completed further into
        // `src/foobar`. This must still be credited as an accepted
        // completion, unlike bare-command prefixes.
        assert!(
            command_matches_completion("src/foobar", "src/foo"),
            "a path item_key must be credited when the executed command extends it without a space"
        );
        assert!(
            command_matches_completion("src/foo", "src/foo"),
            "an exact match must always be credited"
        );
        assert!(
            !command_matches_completion("something", "other/x"),
            "an unrelated executed command must not be credited just because the item_key looks like a path"
        );
        // Command-like (non-path) item_keys still require the space token
        // boundary, so a bare string prefix isn't credited.
        assert!(
            !command_matches_completion("github-cli status", "git"),
            "a mere string prefix must not be credited as an accepted completion"
        );
        assert!(command_matches_completion("git status", "git"));
    }
}
