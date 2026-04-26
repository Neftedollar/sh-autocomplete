mod support;

use support::TestEnv;

#[test]
fn base_pr_paths_index_table_exists() {
    let env = TestEnv::new("base-pr-paths-index");
    // Trigger DB init via any shac command
    support::run_ok(&env, ["stats"]);
    let db_path = env.app_paths().db_file;
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='paths_index'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "paths_index table should exist");
}

#[test]
fn base_pr_history_import_hash_column_exists() {
    let env = TestEnv::new("base-pr-history-import-hash");
    support::run_ok(&env, ["stats"]);
    let db_path = env.app_paths().db_file;
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(history_events)").unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        cols.contains(&"import_hash".to_string()),
        "history_events.import_hash column should exist"
    );
    assert!(
        cols.contains(&"imported_at".to_string()),
        "history_events.imported_at column should exist"
    );
}
