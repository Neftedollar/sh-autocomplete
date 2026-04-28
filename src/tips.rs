//! Discoverability hints — catalog, selection, persistence, runtime.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipCategory {
    Capability = 0,
    Explanation = 1,
    Config = 2,
}

/// Read-only context passed to trigger predicates.
pub struct Context<'a> {
    pub line: &'a str,
    pub cursor: usize,
    pub cwd: &'a Path,
    pub tty: &'a str,
    pub home: &'a Path,
    pub response_sources: &'a [String],
    pub has_path_jump: bool,
    pub n_candidates: usize,
    pub unknown_bin: Option<&'a str>,
}

pub struct Tip {
    pub id: &'static str,
    pub category: TipCategory,
    pub text_key: &'static str,
    pub max_shows: u32,
    pub source_hint: Option<&'static str>,
    pub trigger: fn(&Context) -> bool,
}

pub fn catalog() -> &'static [Tip] {
    &CATALOG
}

const CATALOG: &[Tip] = &[
    Tip { id: "hybrid_cd",          category: TipCategory::Capability,  text_key: "tips.hybrid_cd",          max_shows: 3, source_hint: Some("path_jump"),     trigger: triggers::hybrid_cd },
    Tip { id: "git_branches",       category: TipCategory::Capability,  text_key: "tips.git_branches",       max_shows: 3, source_hint: Some("git_branches"),  trigger: triggers::git_branches },
    Tip { id: "ssh_hosts",          category: TipCategory::Capability,  text_key: "tips.ssh_hosts",          max_shows: 3, source_hint: Some("ssh_hosts"),     trigger: triggers::ssh_hosts },
    Tip { id: "npm_scripts",        category: TipCategory::Capability,  text_key: "tips.npm_scripts",        max_shows: 3, source_hint: Some("npm_scripts"),   trigger: triggers::npm_scripts },
    Tip { id: "kubectl_resources",  category: TipCategory::Capability,  text_key: "tips.kubectl_resources",  max_shows: 3, source_hint: Some("kubectl_resources"), trigger: triggers::kubectl_resources },
    Tip { id: "docker_images",      category: TipCategory::Capability,  text_key: "tips.docker_images",      max_shows: 3, source_hint: Some("docker_images"), trigger: triggers::docker_images },
    Tip { id: "make_targets",       category: TipCategory::Capability,  text_key: "tips.make_targets",       max_shows: 3, source_hint: Some("make_targets"),  trigger: triggers::make_targets },
    Tip { id: "transitions",        category: TipCategory::Explanation, text_key: "tips.transitions",        max_shows: 5, source_hint: Some("transitions"),   trigger: triggers::transitions },
    Tip { id: "path_jump_cyan",     category: TipCategory::Explanation, text_key: "tips.path_jump_cyan",     max_shows: 5, source_hint: Some("path_jump"),     trigger: triggers::path_jump_cyan },
    Tip { id: "unknown_command",    category: TipCategory::Capability,  text_key: "tips.unknown_command",    max_shows: 3, source_hint: None,                  trigger: triggers::unknown_command },
    Tip { id: "menu_detail_verbose",category: TipCategory::Config,      text_key: "tips.menu_detail_verbose",max_shows: 2, source_hint: None,                  trigger: triggers::menu_detail_verbose },
    Tip { id: "tips_off",           category: TipCategory::Config,      text_key: "tips.tips_off",           max_shows: 2, source_hint: None,                  trigger: triggers::tips_off },
];

mod triggers {
    use super::Context;

    pub fn hybrid_cd(c: &Context) -> bool {
        starts_with_token(c.line, "cd") && c.has_path_jump
    }

    pub fn git_branches(c: &Context) -> bool {
        if !matches_subcommand(c.line, "git", &["checkout", "switch", "merge", "rebase"]) {
            return false;
        }
        let mut path = c.cwd.to_path_buf();
        loop {
            if path.join(".git").exists() {
                return true;
            }
            if !path.pop() {
                return false;
            }
        }
    }

    pub fn ssh_hosts(c: &Context) -> bool {
        if !starts_with_token(c.line, "ssh") {
            return false;
        }
        let cfg = c.home.join(".ssh").join("config");
        std::fs::metadata(&cfg).map(|m| m.len() > 0).unwrap_or(false)
    }

    pub fn npm_scripts(c: &Context) -> bool {
        if !matches_subcommand(c.line, "npm", &["run"])
            && !matches_subcommand(c.line, "pnpm", &["run"])
            && !matches_subcommand(c.line, "yarn", &["run"]) {
            return false;
        }
        c.cwd.join("package.json").exists()
    }

    pub fn kubectl_resources(c: &Context) -> bool {
        if !starts_with_token(c.line, "kubectl") { return false; }
        let rest = c.line.strip_prefix("kubectl ").unwrap_or("");
        if rest.split_whitespace().next().is_none() { return false; }
        if std::env::var_os("KUBECONFIG").is_some() { return true; }
        c.home.join(".kube").join("config").exists()
    }

    pub fn docker_images(c: &Context) -> bool {
        matches_subcommand(c.line, "docker", &["run", "exec", "rmi"])
    }

    pub fn make_targets(c: &Context) -> bool {
        if !starts_with_token(c.line, "make") && !starts_with_token(c.line, "just") {
            return false;
        }
        c.cwd.join("Makefile").exists()
            || c.cwd.join("makefile").exists()
            || c.cwd.join("Justfile").exists()
            || c.cwd.join("justfile").exists()
    }

    pub fn transitions(c: &Context) -> bool {
        c.response_sources.iter().any(|s| s == "transitions")
    }

    pub fn path_jump_cyan(c: &Context) -> bool {
        c.has_path_jump
    }

    pub fn unknown_command(c: &Context) -> bool {
        c.unknown_bin.is_some() && c.n_candidates == 0
    }

    pub fn menu_detail_verbose(_: &Context) -> bool {
        // Depends on user's historical menu open count — applied at the selection layer when wired.
        false
    }

    pub fn tips_off(_: &Context) -> bool {
        // Depends on overall tips-shown count — applied at the selection layer when wired.
        false
    }

    fn starts_with_token(line: &str, token: &str) -> bool {
        let mut parts = line.split_whitespace();
        parts.next() == Some(token)
    }

    fn matches_subcommand(line: &str, prog: &str, subs: &[&str]) -> bool {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(prog) { return false; }
        match parts.next() {
            Some(sub) => subs.contains(&sub),
            None => false,
        }
    }
}

/// Public test surface for the private `triggers` module.
pub mod triggers_for_test {
    use super::Context;
    pub fn git_branches(c: &Context) -> bool { super::triggers::git_branches(c) }
    pub fn ssh_hosts(c: &Context) -> bool { super::triggers::ssh_hosts(c) }
    pub fn npm_scripts(c: &Context) -> bool { super::triggers::npm_scripts(c) }
    pub fn make_targets(c: &Context) -> bool { super::triggers::make_targets(c) }
    pub fn docker_images(c: &Context) -> bool { super::triggers::docker_images(c) }
    pub fn transitions(c: &Context) -> bool { super::triggers::transitions(c) }
    pub fn unknown_command(c: &Context) -> bool { super::triggers::unknown_command(c) }
    pub fn hybrid_cd(c: &Context) -> bool { super::triggers::hybrid_cd(c) }
    pub fn kubectl_resources(c: &Context) -> bool { super::triggers::kubectl_resources(c) }
    pub fn path_jump_cyan(c: &Context) -> bool { super::triggers::path_jump_cyan(c) }
}

pub mod storage {
    use std::collections::HashMap;
    use anyhow::{Context, Result};
    use rusqlite::{params, Connection};

    #[derive(Debug, Clone, Default)]
    pub struct TipState {
        pub shows_count: u32,
        pub last_shown_at: Option<i64>,
        pub first_shown_at: Option<i64>,
        pub muted: bool,
        pub muted_at: Option<i64>,
    }

    pub type StateMap = HashMap<String, TipState>;

    pub fn load_all(conn: &Connection) -> Result<StateMap> {
        let mut stmt = conn.prepare(
            "SELECT tip_id, shows_count, last_shown_at, first_shown_at, muted, muted_at FROM tips_state",
        ).context("prepare load_all")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TipState {
                    shows_count: r.get::<_, i64>(1)?.try_into().unwrap_or(0),
                    last_shown_at: r.get(2)?,
                    first_shown_at: r.get(3)?,
                    muted: r.get::<_, i64>(4)? != 0,
                    muted_at: r.get(5)?,
                },
            ))
        }).context("query tips_state")?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, state) = row.context("read row")?;
            out.insert(id, state);
        }
        Ok(out)
    }

    pub fn record_show(conn: &Connection, tip_id: &str, now: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO tips_state(tip_id, shows_count, last_shown_at, first_shown_at)
             VALUES (?1, 1, ?2, ?2)
             ON CONFLICT(tip_id) DO UPDATE SET
                 shows_count = shows_count + 1,
                 last_shown_at = excluded.last_shown_at,
                 first_shown_at = COALESCE(tips_state.first_shown_at, excluded.first_shown_at)",
            params![tip_id, now],
        ).context("upsert record_show")?;
        Ok(())
    }

    pub fn mute(conn: &Connection, tip_id: &str, now: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO tips_state(tip_id, shows_count, muted, muted_at)
             VALUES (?1, 0, 1, ?2)
             ON CONFLICT(tip_id) DO UPDATE SET muted = 1, muted_at = excluded.muted_at",
            params![tip_id, now],
        ).context("mute")?;
        Ok(())
    }

    pub fn unmute(conn: &Connection, tip_id: &str) -> Result<()> {
        conn.execute(
            "UPDATE tips_state SET muted = 0, muted_at = NULL, shows_count = 0 WHERE tip_id = ?1",
            params![tip_id],
        ).context("unmute")?;
        Ok(())
    }

    pub fn reset(conn: &Connection, hard: bool) -> Result<()> {
        if hard {
            conn.execute("DELETE FROM tips_state", []).context("reset --hard")?;
        } else {
            conn.execute(
                "UPDATE tips_state SET shows_count = 0, last_shown_at = NULL, first_shown_at = NULL",
                [],
            ).context("reset")?;
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone)]
pub struct SessionState {
    pub shown_this_session: HashSet<String>,
    pub last_tab_at: Option<Instant>,
}

pub struct SelectInput<'a> {
    pub context: &'a Context<'a>,
    pub state: &'a HashMap<String, storage::TipState>,
    pub session: &'a SessionState,
    pub zero_acceptance_sources: &'a HashSet<String>,
    pub tips_per_session_max: usize,
}

pub fn select<'a>(input: &'a SelectInput<'a>) -> Option<&'static Tip> {
    if input.session.shown_this_session.len() >= input.tips_per_session_max {
        return None;
    }
    let mut candidates: Vec<&'static Tip> = catalog().iter().filter(|t| {
        if !(t.trigger)(input.context) { return false; }
        if input.session.shown_this_session.contains(t.id) { return false; }
        if let Some(s) = input.state.get(t.id) {
            if s.muted { return false; }
            if s.shows_count >= t.max_shows { return false; }
        }
        true
    }).collect();

    // Sort ascending: smallest category rank, then zero-acceptance first, then
    // least-recently-shown first. unwrap_or(0) means never-shown ties with
    // epoch-0 — never-shown wins because the priority is "rotate fairly".
    candidates.sort_by_key(|t| {
        let category_rank = t.category as u8;
        let zero_acc_priority = match t.source_hint {
            Some(src) if input.zero_acceptance_sources.contains(src) => 0u8,
            _ => 1u8,
        };
        let last_shown = input.state.get(t.id).and_then(|s| s.last_shown_at).unwrap_or(0);
        (category_rank, zero_acc_priority, last_shown)
    });

    candidates.into_iter().next()
}

#[derive(Default)]
pub struct Runtime {
    sessions: std::sync::Mutex<HashMap<String, SessionState>>,
}

impl Runtime {
    pub fn session_for(&self, tty: &str) -> SessionState {
        let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        map.get(tty).cloned().unwrap_or_default()
    }

    pub fn record_show(&self, tty: &str, tip_id: &str) {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(tty.to_string()).or_default();
        entry.shown_this_session.insert(tip_id.to_string());
        entry.last_tab_at = Some(Instant::now());
    }
}
