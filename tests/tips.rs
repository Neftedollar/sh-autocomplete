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
    assert_eq!(
        by_id.get("git_branches").copied(),
        Some(TipCategory::Capability)
    );
    assert_eq!(
        by_id.get("transitions").copied(),
        Some(TipCategory::Explanation)
    );
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
    )
    .unwrap();
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
    assert_eq!(
        e.shows_count, 0,
        "unmute resets shows_count for a second chance"
    );
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
    assert_eq!(
        entry.first_shown_at,
        Some(500),
        "first_shown_at must be set on first show even if a row already existed"
    );
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

use shac::tips::{triggers_for_test, Context};
use std::path::{Path, PathBuf};

fn ctx<'a>(line: &'a str, cwd: &'a Path, home: &'a Path, sources: &'a [String]) -> Context<'a> {
    Context {
        line,
        cursor: line.len(),
        cwd,
        tty: "test-tty",
        home,
        response_sources: sources,
        has_path_jump: sources.iter().any(|s| s == "path_jump"),
        n_candidates: sources.len(),
        unknown_bin: None,
    }
}

#[test]
fn git_branches_trigger_inside_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let c = ctx("git checkout ", &cwd, &home, &sources);
    assert!(triggers_for_test::git_branches(&c));
}

#[test]
fn git_branches_trigger_outside_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let c = ctx("git checkout ", &cwd, &home, &sources);
    assert!(!triggers_for_test::git_branches(&c));
}

#[test]
fn ssh_hosts_requires_ssh_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let cwd = home.clone();
    let sources = vec![];

    let c = ctx("ssh ", &cwd, &home, &sources);
    assert!(!triggers_for_test::ssh_hosts(&c));

    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(
        home.join(".ssh").join("config"),
        "Host foo\n  HostName 1.2.3.4\n",
    )
    .unwrap();
    let c = ctx("ssh ", &cwd, &home, &sources);
    assert!(triggers_for_test::ssh_hosts(&c));
}

#[test]
fn npm_scripts_requires_package_json_and_command() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];

    let c = ctx("npm run ", &cwd, &home, &sources);
    assert!(!triggers_for_test::npm_scripts(&c));

    std::fs::write(cwd.join("package.json"), "{\"scripts\":{\"x\":\"y\"}}").unwrap();
    let c = ctx("npm run ", &cwd, &home, &sources);
    assert!(triggers_for_test::npm_scripts(&c));

    let c = ctx("npm install ", &cwd, &home, &sources);
    assert!(!triggers_for_test::npm_scripts(&c), "only `run` triggers");
}

#[test]
fn make_targets_requires_makefile_or_justfile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];

    let c = ctx("make ", &cwd, &home, &sources);
    assert!(!triggers_for_test::make_targets(&c));

    std::fs::write(cwd.join("Makefile"), "all:\n").unwrap();
    let c = ctx("make ", &cwd, &home, &sources);
    assert!(triggers_for_test::make_targets(&c));
}

#[test]
fn docker_trigger_matches_run_exec_rmi() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    for ok in ["docker run ", "docker exec ", "docker rmi "] {
        let c = ctx(ok, &cwd, &home, &sources);
        assert!(triggers_for_test::docker_images(&c), "{ok} should trigger");
    }
    for nope in ["docker ps ", "docker logs "] {
        let c = ctx(nope, &cwd, &home, &sources);
        assert!(
            !triggers_for_test::docker_images(&c),
            "{nope} should not trigger"
        );
    }
}

#[test]
fn transitions_trigger_uses_response_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let with = vec!["transitions".to_string()];
    let without = vec!["history".to_string()];
    assert!(triggers_for_test::transitions(&ctx(
        "foo", &cwd, &home, &with
    )));
    assert!(!triggers_for_test::transitions(&ctx(
        "foo", &cwd, &home, &without
    )));
    let _ = tmp;
}

#[test]
fn unknown_command_uses_unknown_bin_field() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let mut c = ctx("kubectx ", &cwd, &home, &sources);
    c.unknown_bin = Some("kubectx");
    c.n_candidates = 0;
    assert!(triggers_for_test::unknown_command(&c));

    let mut c = ctx("kubectx ", &cwd, &home, &sources);
    c.unknown_bin = None;
    assert!(!triggers_for_test::unknown_command(&c));
}

use shac::tips::storage::TipState;
use shac::tips::{select, SelectInput, SessionState};
use std::collections::{HashMap, HashSet};

fn empty_state() -> HashMap<String, TipState> {
    HashMap::new()
}
fn empty_session() -> SessionState {
    SessionState::default()
}

#[test]
fn select_returns_none_when_no_trigger_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let context = ctx("ls -la", &cwd, &home, &sources);
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_skips_muted() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut state = empty_state();
    state.insert(
        "git_branches".into(),
        TipState {
            muted: true,
            ..Default::default()
        },
    );
    let input = SelectInput {
        context: &context,
        state: &state,
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_skips_when_session_already_saw_tip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut session = empty_session();
    session.shown_this_session.insert("git_branches".into());
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &session,
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_caps_per_session_max() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut session = empty_session();
    session.shown_this_session.insert("a".into());
    session.shown_this_session.insert("b".into());
    session.shown_this_session.insert("c".into());
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &session,
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(
        select(&input).is_none(),
        "session at max should suppress further tips"
    );
}

#[test]
fn select_capability_beats_explanation() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    let sources = vec!["transitions".to_string()];
    let context = ctx("git checkout main", &cwd, &home, &sources);
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    let picked = select(&input).expect("a tip");
    assert_eq!(picked.id, "git_branches");
}

#[test]
fn select_prefers_zero_acceptance_within_category() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("config"), "Host x\n").unwrap();
    let cwd = home.clone();
    std::fs::write(cwd.join("package.json"), "{\"scripts\":{}}").unwrap();
    let sources = vec![];
    let context = ctx("ssh ", &cwd, &home, &sources);
    let mut zero = HashSet::new();
    zero.insert("ssh_hosts".to_string());

    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &zero,
        tips_per_session_max: 3,
    };
    let picked = select(&input).expect("a tip");
    assert_eq!(picked.id, "ssh_hosts");
}

#[test]
fn config_set_show_tips_persists() {
    let env = support::TestEnv::new("config-show-tips");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["config", "set", "ui.show_tips", "false"]);
    let out = support::run_ok(&env, ["config", "get", "ui.show_tips"]);
    assert!(out.trim().ends_with("false"), "got: {out}");
}

#[test]
fn complete_in_git_repo_emits_tip_line() {
    let env = support::TestEnv::new("tip-line");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git checkout main",
            "--cursor",
            "20",
            "--cwd",
            cwd.to_string_lossy().as_ref(),
            "--format",
            "shell-tsv-v2",
        ],
    );
    // First call may emit greeter; either greeter or git_branches counts.
    assert!(
        out.contains("__shac_tip"),
        "expected __shac_tip line, got:\n{out}"
    );
}

#[test]
fn tips_mute_then_list_shows_muted() {
    let env = support::TestEnv::new("tips-cli");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["tips", "mute", "git_branches"]);
    let out = support::run_ok(&env, ["tips", "list", "--muted"]);
    assert!(out.contains("git_branches"));
    assert!(out.contains("muted"));
}

#[test]
fn tips_reset_hard_clears_state() {
    let env = support::TestEnv::new("tips-reset");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["tips", "mute", "git_branches"]);
    support::run_ok(&env, ["tips", "reset", "--hard"]);
    let out = support::run_ok(&env, ["tips", "list", "--all"]);
    assert!(
        !out.contains("muted"),
        "expected no muted after hard reset, got:\n{out}"
    );
}

#[test]
fn shac_no_tips_env_suppresses_tip_line() {
    let env = support::TestEnv::new("no-tip-env");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let mut cmd = env.shac_cmd();
    cmd.env("SHAC_NO_TIPS", "1");
    cmd.args([
        "complete",
        "--shell",
        "zsh",
        "--line",
        "git checkout main",
        "--cursor",
        "20",
        "--cwd",
        cwd.to_string_lossy().as_ref(),
        "--format",
        "shell-tsv-v2",
    ]);
    let out = cmd.output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("__shac_tip"),
        "SHAC_NO_TIPS should suppress: {stdout}"
    );
}
