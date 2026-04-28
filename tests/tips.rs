mod support;

use shac::tips::{catalog, TipCategory};

#[test]
fn catalog_has_expected_ids() {
    let ids: Vec<&str> = catalog().iter().map(|t| t.id).collect();
    let expected = [
        "hybrid_cd",
        "git_branches",
        "ssh_hosts",
        "npm_scripts",
        "kubectl_resources",
        "docker_images",
        "make_targets",
        "transitions",
        "path_jump_cyan",
        "unknown_command",
        "menu_detail_verbose",
        "tips_off",
    ];
    for id in expected {
        assert!(ids.contains(&id), "missing tip id: {id}");
    }
}

#[test]
fn catalog_categories_match_spec() {
    let by_id: std::collections::HashMap<&str, TipCategory> =
        catalog().iter().map(|t| (t.id, t.category)).collect();
    assert_eq!(by_id.get("git_branches").copied(), Some(TipCategory::Capability));
    assert_eq!(by_id.get("transitions").copied(), Some(TipCategory::Explanation));
    assert_eq!(by_id.get("tips_off").copied(), Some(TipCategory::Config));
}

#[test]
fn tips_state_table_is_created() {
    let env = support::TestEnv::new("tips-schema");
    let _daemon = env.spawn_daemon();
    // Force a `complete` to ensure the daemon opened the DB and ran init.
    support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "ls",
            "--cursor",
            "2",
            "--cwd",
            env.root.to_string_lossy().as_ref(),
            "--format",
            "json",
        ],
    );

    let db_path = env.app_paths().db_file;
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tips_state'",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(exists, 1, "tips_state table should exist");
}

use rusqlite::Connection;
use shac::tips::storage;

fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE tips_state (
            tip_id          TEXT PRIMARY KEY,
            shows_count     INTEGER NOT NULL DEFAULT 0,
            last_shown_at   INTEGER,
            muted           INTEGER NOT NULL DEFAULT 0,
            muted_at        INTEGER,
            first_shown_at  INTEGER
        );",
    ).unwrap();
    conn
}

#[test]
fn record_show_inserts_and_increments() {
    let conn = test_db();
    storage::record_show(&conn, "git_branches", 1000).unwrap();
    storage::record_show(&conn, "git_branches", 2000).unwrap();
    let state = storage::load_all(&conn).unwrap();
    let entry = state.get("git_branches").expect("entry exists");
    assert_eq!(entry.shows_count, 2);
    assert_eq!(entry.last_shown_at, Some(2000));
    assert_eq!(entry.first_shown_at, Some(1000));
    assert!(!entry.muted);
}

#[test]
fn mute_and_unmute() {
    let conn = test_db();
    storage::record_show(&conn, "git_branches", 1000).unwrap();
    storage::mute(&conn, "git_branches", 5000).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert!(state.get("git_branches").unwrap().muted);

    storage::unmute(&conn, "git_branches").unwrap();
    let state = storage::load_all(&conn).unwrap();
    let e = state.get("git_branches").unwrap();
    assert!(!e.muted);
    assert_eq!(e.shows_count, 0, "unmute resets shows_count for a second chance");
}

#[test]
fn reset_clears_counts_but_preserves_mutes() {
    let conn = test_db();
    storage::record_show(&conn, "a", 1).unwrap();
    storage::record_show(&conn, "b", 1).unwrap();
    storage::mute(&conn, "b", 2).unwrap();
    storage::reset(&conn, false).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert_eq!(state.get("a").unwrap().shows_count, 0);
    assert!(state.get("b").unwrap().muted, "soft reset preserves mute");
}

#[test]
fn first_shown_at_set_when_mute_precedes_record_show() {
    let conn = test_db();
    storage::mute(&conn, "x", 100).unwrap();
    storage::record_show(&conn, "x", 500).unwrap();
    let state = storage::load_all(&conn).unwrap();
    let entry = state.get("x").expect("entry");
    assert_eq!(entry.first_shown_at, Some(500), "first_shown_at must be set on first show even if a row already existed");
    assert_eq!(entry.shows_count, 1);
}

#[test]
fn reset_hard_clears_everything() {
    let conn = test_db();
    storage::record_show(&conn, "a", 1).unwrap();
    storage::mute(&conn, "a", 2).unwrap();
    storage::reset(&conn, true).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert!(state.is_empty(), "hard reset deletes all rows");
}
