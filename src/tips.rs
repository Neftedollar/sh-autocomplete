//! Discoverability hints — catalog, selection, persistence, runtime.

use std::path::Path;

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

    // All return false until Task 6 fills them in.
    pub fn hybrid_cd(_: &Context) -> bool { false }
    pub fn git_branches(_: &Context) -> bool { false }
    pub fn ssh_hosts(_: &Context) -> bool { false }
    pub fn npm_scripts(_: &Context) -> bool { false }
    pub fn kubectl_resources(_: &Context) -> bool { false }
    pub fn docker_images(_: &Context) -> bool { false }
    pub fn make_targets(_: &Context) -> bool { false }
    pub fn transitions(_: &Context) -> bool { false }
    pub fn path_jump_cyan(_: &Context) -> bool { false }
    pub fn unknown_command(_: &Context) -> bool { false }
    pub fn menu_detail_verbose(_: &Context) -> bool { false }
    pub fn tips_off(_: &Context) -> bool { false }
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
                    shows_count: r.get::<_, i64>(1)? as u32,
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
                 last_shown_at = excluded.last_shown_at",
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
