//! Integration tests for the standalone `shac import` and `shac scan-projects`
//! subcommands. The existing `cold_start_import.rs` only exercises `shac
//! install --yes`, which invokes the import pipeline indirectly. These tests
//! pin behaviour of the user-facing subcommands so future changes to import
//! semantics surface here.

use std::fs;
use std::path::Path;

mod support;

use support::{run_ok, TestEnv};

/// Mirror of the helper in `cold_start_import.rs` — writes a v3 zoxide binary
/// database. Inlined rather than shared so the two test files stay
/// independent.
fn write_zoxide_v3_db(path: &Path, entries: &[(&str, f64, u64)]) {
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (p, rank, last) in entries {
        buf.extend_from_slice(&(p.len() as u64).to_le_bytes());
        buf.extend_from_slice(p.as_bytes());
        buf.extend_from_slice(&rank.to_le_bytes());
        buf.extend_from_slice(&last.to_le_bytes());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, &buf).expect("write zoxide");
}

/// Mix of extended-format (`: <ts>:<dur>;cmd`) and plain-format zsh history
/// lines. Roughly 12 entries — comfortably above the >0 assertion threshold.
const HISTORY_FIXTURE: &str = "\
: 1700000000:0;ls -al\n\
: 1700000010:0;cd /tmp\n\
: 1700000020:0;echo hi\n\
: 1700000030:5;cargo build\n\
: 1700000040:0;git status\n\
: 1700000050:1;grep -n foo bar.rs\n\
plain-line-one\n\
: 1700000060:0;mkdir -p /tmp/shac-test\n\
plain-line-two\n\
: 1700000070:0;rm -rf /tmp/shac-test\n\
: 1700000080:0;cargo test\n\
: 1700000090:0;exit\n\
";

#[test]
fn shac_import_zsh_history_with_explicit_path() {
    let env = TestEnv::new("import-zsh-history-path");

    // Stage a fixture at a non-default path (NOT ~/.zsh_history).
    let fixture_path = env.root.join("fixtures").join("zsh_history.txt");
    fs::create_dir_all(fixture_path.parent().unwrap()).expect("create fixture dir");
    fs::write(&fixture_path, HISTORY_FIXTURE).expect("write history fixture");

    let _ = run_ok(
        &env,
        [
            "import",
            "zsh-history",
            "--path",
            &fixture_path.to_string_lossy(),
        ],
    );

    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let stats = db.stats().expect("stats");
    assert!(
        stats.imported_history_events > 0,
        "expected imported_history_events > 0, got {}",
        stats.imported_history_events
    );
}

#[test]
fn shac_import_zsh_history_dry_run_does_not_write() {
    let env = TestEnv::new("import-zsh-history-dry-run");

    let fixture_path = env.root.join("fixtures").join("zsh_history.txt");
    fs::create_dir_all(fixture_path.parent().unwrap()).expect("create fixture dir");
    fs::write(&fixture_path, HISTORY_FIXTURE).expect("write history fixture");

    let _ = run_ok(
        &env,
        [
            "import",
            "zsh-history",
            "--path",
            &fixture_path.to_string_lossy(),
            "--dry-run",
        ],
    );

    let paths = env.app_paths();
    // The dry run should not have created the database file. If it does
    // exist (e.g. due to other init paths), assert zero imported events.
    if paths.db_file.exists() {
        let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
        let stats = db.stats().expect("stats");
        assert_eq!(
            stats.imported_history_events, 0,
            "dry-run should not import any events"
        );
    }
}

#[test]
fn shac_import_zoxide_with_explicit_path() {
    let env = TestEnv::new("import-zoxide-path");

    let fixture_path = env.root.join("fixtures").join("db.zo");
    write_zoxide_v3_db(
        &fixture_path,
        &[
            ("/tmp/aaa", 4.0, 100),
            ("/tmp/bbb", 2.0, 200),
            ("/tmp/ccc", 1.0, 300),
        ],
    );

    let _ = run_ok(
        &env,
        [
            "import",
            "zoxide",
            "--path",
            &fixture_path.to_string_lossy(),
        ],
    );

    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let stats = db.stats().expect("stats");
    assert!(
        stats.imported_zoxide_paths > 0,
        "expected imported_zoxide_paths > 0, got {}",
        stats.imported_zoxide_paths
    );
}

#[test]
fn shac_import_all_runs_both() {
    let env = TestEnv::new("import-all");

    // `shac import all --yes` uses the default lookup paths (HOME-based).
    // TestEnv overrides HOME, so stage fixtures at the defaults.
    let history_path = env.home.join(".zsh_history");
    fs::write(&history_path, HISTORY_FIXTURE).expect("write history");

    let zo_path = env.home.join(".local/share/zoxide/db.zo");
    write_zoxide_v3_db(
        &zo_path,
        &[("/tmp/aaa", 4.0, 100), ("/tmp/bbb", 2.0, 200)],
    );

    let _ = run_ok(&env, ["import", "all", "--yes"]);

    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let stats = db.stats().expect("stats");
    assert!(
        stats.imported_history_events > 0,
        "expected imported_history_events > 0, got {}",
        stats.imported_history_events
    );
    assert!(
        stats.imported_zoxide_paths > 0,
        "expected imported_zoxide_paths > 0, got {}",
        stats.imported_zoxide_paths
    );
}

#[test]
fn shac_scan_projects_with_explicit_root() {
    let env = TestEnv::new("scan-projects-root");

    // Two repos, each detected via a `.git` directory.
    let projects_root = env.root.join("projects");
    let repo_a_git = projects_root.join("repo-a").join(".git");
    let repo_b_git = projects_root.join("repo-b").join(".git");
    fs::create_dir_all(&repo_a_git).expect("create repo-a/.git");
    fs::create_dir_all(&repo_b_git).expect("create repo-b/.git");

    let _ = run_ok(
        &env,
        [
            "scan-projects",
            "--root",
            &projects_root.to_string_lossy(),
            "--depth",
            "3",
        ],
    );

    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let stats = db.stats().expect("stats");
    assert!(
        stats.scanned_project_paths >= 2,
        "expected scanned_project_paths >= 2, got {}",
        stats.scanned_project_paths
    );
}

#[test]
fn shac_scan_projects_respects_depth_limit() {
    let env = TestEnv::new("scan-projects-depth");

    // Deep tree: <root>/a/b/c/d/e/.git — five levels below the scan root.
    let scan_root = env.root.join("deep_root");
    let deep_git = scan_root
        .join("a")
        .join("b")
        .join("c")
        .join("d")
        .join("e")
        .join(".git");
    fs::create_dir_all(&deep_git).expect("create deep .git");

    // Shallow run: depth=2 should not reach `e/.git`.
    let _ = run_ok(
        &env,
        [
            "scan-projects",
            "--root",
            &scan_root.to_string_lossy(),
            "--depth",
            "2",
        ],
    );

    let paths = env.app_paths();
    {
        let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
        let stats = db.stats().expect("stats");
        assert_eq!(
            stats.scanned_project_paths, 0,
            "depth=2 should prune before reaching e/.git, got {}",
            stats.scanned_project_paths
        );
    }

    // Deep run: depth=6 reaches `e/.git` and inserts exactly one project.
    let _ = run_ok(
        &env,
        [
            "scan-projects",
            "--root",
            &scan_root.to_string_lossy(),
            "--depth",
            "6",
        ],
    );

    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let stats = db.stats().expect("stats");
    assert_eq!(
        stats.scanned_project_paths, 1,
        "depth=6 should find exactly one project at e/.git, got {}",
        stats.scanned_project_paths
    );
}
