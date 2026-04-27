mod support;

use std::fs;

use serde_json::Value;
use shac::db::AppDb;

#[test]
fn cli_daemon_records_exact_accept_and_recent_events() {
    let env = support::TestEnv::new("cli");
    let cwd = support::current_repo();

    support::run_ok(&env, ["config", "set", "daemon_timeout_ms", "750"]);
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["reindex"]);

    let completion = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "pyt",
            "--cursor",
            "3",
            "--cwd",
            &cwd,
            "--format",
            "shell-tsv-v2",
        ],
    );
    let mut lines = completion.lines();
    let header = lines.next().expect("completion header");
    let header_fields = header.split('\t').collect::<Vec<_>>();
    assert_eq!(header_fields.first().copied(), Some("__shac_request_id"));
    let request_id = header_fields
        .get(1)
        .and_then(|value| value.parse::<i64>().ok())
        .expect("numeric request id");
    let has_python = lines.any(|line| {
        let fields = line.split('\t').collect::<Vec<_>>();
        fields.get(1).copied() == Some("python3")
            || fields.get(2).copied() == Some("python3")
            || fields.first().copied() == Some("python3")
    });
    assert!(
        has_python,
        "expected python3 in completion output:\n{completion}"
    );

    let module_completion = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "python3 -m ",
            "--cursor",
            "11",
            "--cwd",
            &cwd,
            "--format",
            "shell-tsv-v2",
        ],
    );
    assert!(
        module_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"pytest")),
        "expected python module candidates after `python3 -m `:\n{module_completion}"
    );
    assert!(
        !module_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"-m")),
        "python module position should not suggest raw python options:\n{module_completion}"
    );

    support::run_ok(
        &env,
        [
            "record-command",
            "--shell",
            "zsh",
            "--cwd",
            &cwd,
            "--command",
            "python3",
            "--trust",
            "interactive",
            "--provenance",
            "accepted_completion",
            "--origin",
            "zsh_precmd",
            "--tty-present",
            "--accepted-request-id",
            &request_id.to_string(),
            "--accepted-item-key",
            "python3",
            "--accepted-rank",
            "0",
        ],
    );

    let stats: Value = serde_json::from_str(&support::run_ok(&env, ["stats"])).expect("stats json");
    assert_eq!(stats["history_events"].as_i64(), Some(1));
    assert_eq!(stats["interactive_history_events"].as_i64(), Some(1));
    assert_eq!(stats["clean_completion_requests"].as_i64(), Some(2));
    assert_eq!(stats["accepted_clean_completions"].as_i64(), Some(1));

    let recent: Value =
        serde_json::from_str(&support::run_ok(&env, ["recent-events", "--limit", "5"]))
            .expect("recent events json");
    let event = recent
        .as_array()
        .and_then(|events| events.first())
        .expect("recent event");
    assert_eq!(event["command"].as_str(), Some("python3"));
    assert_eq!(event["trust"].as_str(), Some("interactive"));
    assert_eq!(event["provenance"].as_str(), Some("accepted_completion"));

    let path_root = env.root.join("path-fixture");
    fs::create_dir_all(path_root.join("alpha/tools")).expect("create path fixture dirs");
    fs::write(path_root.join("README.md"), "").expect("create top-level file");
    fs::write(path_root.join("alpha/README.md"), "").expect("create nested file");

    let cd_completion = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "cd ",
            "--cursor",
            "3",
            "--cwd",
            path_root.to_str().expect("utf8 path root"),
            "--format",
            "shell-tsv-v2",
        ],
    );
    assert!(
        cd_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"alpha/")),
        "cd completion should suggest directories with a trailing slash:\n{cd_completion}"
    );
    assert!(
        !cd_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"README.md")),
        "cd completion should not suggest files:\n{cd_completion}"
    );
    assert!(
        !cd_completion.lines().any(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            fields.get(4) == Some(&"history") || fields.get(4) == Some(&"runtime_history")
        }),
        "empty cd completion should prioritize current-directory paths without history noise:\n{cd_completion}"
    );

    let nested_cd_completion = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "cd alpha/",
            "--cursor",
            "9",
            "--cwd",
            path_root.to_str().expect("utf8 path root"),
            "--format",
            "shell-tsv-v2",
        ],
    );
    assert!(
        nested_cd_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"alpha/tools/")),
        "nested cd completion should preserve the active path prefix:\n{nested_cd_completion}"
    );
    assert!(
        !nested_cd_completion
            .lines()
            .any(|line| line.split('\t').collect::<Vec<_>>().get(1) == Some(&"tools/")),
        "nested cd completion must not drop the active path prefix:\n{nested_cd_completion}"
    );
}

#[test]
fn path_jump_appears_when_paths_index_seeded() {
    let env = support::TestEnv::new("path-jump-seeded");

    // A target dir somewhere not under the cwd.
    let target = env.root.join("elsewhere");
    fs::create_dir_all(&target).expect("create target");

    // Seed paths_index BEFORE spawning daemon (which opens the DB).
    {
        let paths = env.app_paths();
        fs::create_dir_all(&paths.data_dir).expect("data dir");
        let db = AppDb::open(&paths.db_file).expect("open db");
        db.upsert_path_index_with_rank(
            target.to_str().expect("utf8 target"),
            7.0,
            0,
            "test_seed",
            false,
            None,
        )
        .expect("seed paths_index");
    }

    let cwd = env.root.join("cwd-not-elsewhere");
    fs::create_dir_all(&cwd).expect("create cwd");

    let _daemon = env.spawn_daemon();

    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "cd ",
            "--cursor",
            "3",
            "--cwd",
            cwd.to_str().expect("utf8 cwd"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let has_path_jump = output.lines().any(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        fields.get(3) == Some(&"path_jump") && fields.get(4) == Some(&"path_jump")
    });
    assert!(
        has_path_jump,
        "expected a path_jump row when paths_index is seeded:\n{output}"
    );
}

#[test]
fn path_jump_absent_when_paths_index_empty() {
    let env = support::TestEnv::new("path-jump-empty");

    let cwd = env.root.join("emptycwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let _daemon = env.spawn_daemon();

    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "cd ",
            "--cursor",
            "3",
            "--cwd",
            cwd.to_str().expect("utf8 cwd"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let any_path_jump = output.lines().any(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        fields.get(3) == Some(&"path_jump")
    });
    assert!(
        !any_path_jump,
        "expected no path_jump rows when paths_index is empty:\n{output}"
    );
}
