mod support;

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
