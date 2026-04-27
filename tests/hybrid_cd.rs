mod support;

use std::fs;

use shac::db::AppDb;

#[test]
fn cd_with_seeded_path_jump_returns_global_path() {
    let env = support::TestEnv::new("hybrid-cd-seed");

    // Plant a deep dir to "jump" to.
    let target = env.root.join("projects/deep/repo");
    fs::create_dir_all(&target).expect("create deep target dir");

    // Seed paths_index directly via AppDb at the planned db path.
    {
        let paths = env.app_paths();
        fs::create_dir_all(&paths.data_dir).expect("create data dir");
        let db = AppDb::open(&paths.db_file).expect("open db");
        db.upsert_path_index_with_rank(
            target.to_str().expect("utf8 target"),
            10.0,
            0,
            "test_seed",
            true,
            None,
        )
        .expect("seed paths_index");
    }

    // cwd: separate dir not containing the target as direct child.
    let cwd = env.root.join("workdir");
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

    // shell-tsv-v2 layout per data row: item_key, insert_text, display, kind, source, description.
    let mut found_path_jump = false;
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 5 {
            continue;
        }
        let kind = fields[3];
        let source = fields[4];
        let display = fields[2];
        let insert_text = fields[1];
        if kind == "path_jump"
            && source == "path_jump"
            && display.starts_with("\u{2192} ")
            && (insert_text.ends_with("repo") || insert_text.ends_with("repo/"))
        {
            found_path_jump = true;
            break;
        }
    }

    assert!(
        found_path_jump,
        "expected a path_jump row with arrow display and insert_text ending in repo:\n{output}"
    );
}
