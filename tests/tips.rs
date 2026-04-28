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
