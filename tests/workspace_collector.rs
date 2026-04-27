//! Integration tests for `Engine::collect_workspace_candidates` —
//! the `ArgType::Workspace` collector that reads VS Code's recent-workspaces
//! store and surfaces them as candidates with `kind=workspace`. PLAN §7.9.

mod support;

use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a synthetic VS Code `state.vscdb` SQLite DB inside the test env's
/// HOME at `Library/Application Support/Code/User/globalStorage/state.vscdb`
/// (macOS path). This is the path the engine tries first.
fn write_vscdb(env: &support::TestEnv, entries_json: &str) {
    let dir = env
        .home
        .join("Library/Application Support/Code/User/globalStorage");
    fs::create_dir_all(&dir).expect("create vscode storage dir");
    let db_path = dir.join("state.vscdb");

    let conn = rusqlite::Connection::open(&db_path).expect("open test vscdb");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE, value TEXT);",
    )
    .expect("create table");
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES ('history.recentlyOpenedPathsList', ?1)",
        rusqlite::params![entries_json],
    )
    .expect("insert history");
}

/// Build the JSON payload for `history.recentlyOpenedPathsList`.
///
/// `entries` is a slice of raw JSON object strings, each representing one
/// entry in the `entries` array (e.g. `r#"{"folderUri":"file:///tmp/foo"}"#`).
fn history_json(entries: &[&str]) -> String {
    format!(r#"{{"entries":[{}]}}"#, entries.join(","))
}

/// Write a synthetic `storage.json` file in the macOS VS Code path.
#[allow(dead_code)]
fn write_storage_json(env: &support::TestEnv, content: &str) {
    let dir = env
        .home
        .join("Library/Application Support/Code/User/globalStorage");
    fs::create_dir_all(&dir).expect("create vscode storage dir");
    fs::write(dir.join("storage.json"), content).expect("write storage.json");
}

/// Parse shell-tsv-v2 output from a `complete` invocation, returning
/// `(insert_text, display, kind, source)` quads for every non-meta data line.
fn parse_candidates(output: &str) -> Vec<(String, String, String, String)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 5 {
            continue;
        }
        out.push((
            fields[1].to_string(), // insert_text
            fields[2].to_string(), // display
            fields[3].to_string(), // kind
            fields[4].to_string(), // source
        ));
    }
    out
}

/// Run `shac complete` with `--line LINE --cursor <len> --cwd /tmp` and return
/// the full parsed candidate list.
fn complete(env: &support::TestEnv, line: &str) -> Vec<(String, String, String, String)> {
    let cursor = line.len().to_string();
    let output = support::run_ok(
        env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ],
    );
    parse_candidates(&output)
}

fn workspace_candidates(
    candidates: &[(String, String, String, String)],
) -> Vec<(&str, &str, &str)> {
    candidates
        .iter()
        .filter(|(_, _, kind, source)| kind == "workspace" && source == "workspace")
        .map(|(insert, _, kind, _)| (insert.as_str(), kind.as_str(), "workspace"))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn code_completion_lists_recent_workspaces() {
    let env = support::TestEnv::new("ws-list");

    // Create the real directories so they exist on disk.
    let ws1 = env.home.join("projects/alpha");
    let ws2 = env.home.join("projects/beta");
    let ws3 = env.home.join("projects/gamma");
    for ws in [&ws1, &ws2, &ws3] {
        fs::create_dir_all(ws).expect("create workspace dir");
    }

    let json = history_json(&[
        &format!(r#"{{"folderUri":"file://{}"}}"#, ws1.display()),
        &format!(r#"{{"folderUri":"file://{}"}}"#, ws2.display()),
        &format!(r#"{{"folderUri":"file://{}"}}"#, ws3.display()),
    ]);
    write_vscdb(&env, &json);

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code ");
    let ws = workspace_candidates(&candidates);

    assert_eq!(ws.len(), 3, "expected 3 workspace candidates, got: {ws:?}");
    let inserts: Vec<&str> = ws.iter().map(|(i, _, _)| *i).collect();
    // All three workspace basenames should appear.
    assert!(
        inserts.iter().any(|i| i.ends_with("alpha")),
        "alpha missing: {inserts:?}"
    );
    assert!(
        inserts.iter().any(|i| i.ends_with("beta")),
        "beta missing: {inserts:?}"
    );
    assert!(
        inserts.iter().any(|i| i.ends_with("gamma")),
        "gamma missing: {inserts:?}"
    );
}

#[test]
fn code_completion_filters_by_active_prefix() {
    let env = support::TestEnv::new("ws-prefix");

    let myproj = env.home.join("projects/myproject");
    let other = env.home.join("projects/other");
    fs::create_dir_all(&myproj).expect("mkdir");
    fs::create_dir_all(&other).expect("mkdir");

    let json = history_json(&[
        &format!(r#"{{"folderUri":"file://{}"}}"#, myproj.display()),
        &format!(r#"{{"folderUri":"file://{}"}}"#, other.display()),
    ]);
    write_vscdb(&env, &json);

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code my");
    let ws = workspace_candidates(&candidates);

    assert_eq!(ws.len(), 1, "expected only 'myproject', got: {ws:?}");
    assert!(
        ws[0].0.ends_with("myproject"),
        "unexpected candidate: {:?}",
        ws[0]
    );
}

#[test]
fn code_completion_skips_remote_workspaces() {
    let env = support::TestEnv::new("ws-remote");

    let local = env.home.join("projects/local");
    fs::create_dir_all(&local).expect("mkdir");

    let json = history_json(&[
        &format!(r#"{{"folderUri":"file://{}"}}"#, local.display()),
        // Remote workspace — must be skipped.
        r#"{"folderUri":"vscode-remote://ssh-remote%2Bserver.example.com/home/user/proj","remoteAuthority":"ssh-remote+server.example.com"}"#,
    ]);
    write_vscdb(&env, &json);

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code ");
    let ws = workspace_candidates(&candidates);

    assert_eq!(ws.len(), 1, "remote entry must not surface; got: {ws:?}");
    assert!(
        ws[0].0.ends_with("local"),
        "unexpected candidate: {:?}",
        ws[0]
    );
}

#[test]
fn code_completion_skips_missing_paths() {
    let env = support::TestEnv::new("ws-missing");

    let exists = env.home.join("projects/exists");
    fs::create_dir_all(&exists).expect("mkdir");
    // "gone" is intentionally NOT created on disk.
    let gone = env.home.join("projects/gone");

    let json = history_json(&[
        &format!(r#"{{"folderUri":"file://{}"}}"#, exists.display()),
        &format!(r#"{{"folderUri":"file://{}"}}"#, gone.display()),
    ]);
    write_vscdb(&env, &json);

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code ");
    let ws = workspace_candidates(&candidates);

    assert_eq!(ws.len(), 1, "stale entry must not surface; got: {ws:?}");
    assert!(
        ws[0].0.ends_with("exists"),
        "unexpected candidate: {:?}",
        ws[0]
    );
}

#[test]
fn code_completion_handles_missing_storage() {
    // No VS Code dir at all — must produce no workspace candidates, no panic.
    let env = support::TestEnv::new("ws-no-storage");

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code ");
    let ws = workspace_candidates(&candidates);

    assert!(
        ws.is_empty(),
        "no storage should yield no workspace candidates; got: {ws:?}"
    );
}

#[test]
fn code_completion_handles_malformed_json() {
    let env = support::TestEnv::new("ws-bad-json");

    let dir = env
        .home
        .join("Library/Application Support/Code/User/globalStorage");
    fs::create_dir_all(&dir).expect("create dir");

    // Write a vscdb with broken JSON payload — must not panic.
    let conn = rusqlite::Connection::open(dir.join("state.vscdb")).expect("open db");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE, value TEXT);",
    )
    .expect("create table");
    conn.execute(
        "INSERT INTO ItemTable (key, value) VALUES ('history.recentlyOpenedPathsList', ?1)",
        rusqlite::params!["{ this is not valid json "],
    )
    .expect("insert");

    let _daemon = env.spawn_daemon();
    let candidates = complete(&env, "code ");
    let ws = workspace_candidates(&candidates);

    assert!(
        ws.is_empty(),
        "malformed JSON must yield empty candidates; got: {ws:?}"
    );
}

#[test]
fn subl_uses_workspace_completion() {
    let env = support::TestEnv::new("ws-subl");

    let proj = env.home.join("projects/sublproj");
    fs::create_dir_all(&proj).expect("mkdir");

    let json = history_json(&[&format!(
        r#"{{"folderUri":"file://{}"}}"#,
        proj.display()
    )]);
    write_vscdb(&env, &json);

    let _daemon = env.spawn_daemon();

    // subl should also produce workspace candidates (same ArgType::Workspace mapping).
    let candidates = complete(&env, "subl ");
    let ws = workspace_candidates(&candidates);

    assert!(
        ws.iter().any(|(i, _, _)| i.ends_with("sublproj")),
        "subl must produce workspace candidates; got: {ws:?}"
    );
}
