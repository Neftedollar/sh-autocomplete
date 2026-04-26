use std::fs;
use std::time::{Duration, Instant};

mod support;

use support::TestEnv;

fn write_zoxide_v3_db(path: &std::path::Path, entries: &[(&str, f64, u64)]) {
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

#[test]
fn install_imports_history_and_zoxide() {
    let env = TestEnv::new("cold-start-imports");

    // Stage ~/.zsh_history
    let history_path = env.home.join(".zsh_history");
    let history_contents = ": 1700000000:0;ls -al\n: 1700000010:0;cd /tmp\n: 1700000020:0;echo hi\n";
    fs::write(&history_path, history_contents).expect("write history");

    // Stage zoxide DB
    let zo_path = env.home.join(".local/share/zoxide/db.zo");
    write_zoxide_v3_db(
        &zo_path,
        &[("/tmp/aaa", 4.0, 100), ("/tmp/bbb", 2.0, 200)],
    );

    let started = Instant::now();
    let mut command = env.shac_cmd();
    command.args(["install", "--shell", "zsh", "--edit-rc", "--yes"]);
    let output = command.output().expect("run install");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "install took too long: {:?}",
        elapsed
    );

    // Open the DB directly and inspect stats.
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
fn install_no_import_flag_skips_imports() {
    let env = TestEnv::new("cold-start-no-import");

    // Stage history + zoxide just like the positive case.
    let history_path = env.home.join(".zsh_history");
    fs::write(
        &history_path,
        ": 1700000000:0;ls\n: 1700000001:0;cd /tmp\n",
    )
    .expect("write history");
    let zo_path = env.home.join(".local/share/zoxide/db.zo");
    write_zoxide_v3_db(&zo_path, &[("/tmp/xxx", 3.0, 50)]);

    let mut command = env.shac_cmd();
    command.args([
        "install",
        "--shell",
        "zsh",
        "--edit-rc",
        "--no-import",
    ]);
    let output = command.output().expect("run install");
    assert!(
        output.status.success(),
        "install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    assert_eq!(db.count_paths_index().expect("count"), 0);
    assert_eq!(db.count_imported_history().expect("count"), 0);
}
