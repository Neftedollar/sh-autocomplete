// Integration tests for 7.11 (background indexer), 7.5b (invalidate-caches), and
// 7.5c (reindex --full / --skip-existing CLI flags).

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

// ── 7.5b: invalidate-caches ─────────────────────────────────────────────────

#[test]
fn invalidate_caches_returns_ok_when_daemon_running() {
    let env = support::TestEnv::new("invalidate-caches");
    let _daemon = env.spawn_daemon();
    let out = support::run_ok(&env, ["invalidate-caches"]);
    assert!(
        out.contains("caches invalidated"),
        "expected 'caches invalidated' in output, got: {out}"
    );
}

// ── 7.5c: reindex --full / --skip-existing ───────────────────────────────────

#[test]
fn reindex_default_flags_succeeds() {
    let env = support::TestEnv::new("reindex-default");
    let _daemon = env.spawn_daemon();
    // Plain `reindex` (no flags) should succeed and return a JSON response.
    let out = support::run_ok(&env, ["reindex"]);
    assert!(
        out.contains("indexed"),
        "expected 'indexed' in output, got: {out}"
    );
}

#[test]
fn reindex_skip_existing_flag_succeeds() {
    let env = support::TestEnv::new("reindex-skip");
    let _daemon = env.spawn_daemon();
    let out = support::run_ok(&env, ["reindex", "--skip-existing"]);
    assert!(
        out.contains("indexed"),
        "expected 'indexed' in output, got: {out}"
    );
}

/// --full runs a full reindex; verify it sends full=true to daemon and gets a valid response.
/// We restrict PATH to a single small directory so the daemon reindex completes quickly.
#[test]
fn reindex_full_flag_succeeds_with_restricted_path() {
    let env = support::TestEnv::new("reindex-full");
    let _daemon = env.spawn_daemon();
    // Use a long enough timeout for the reindex RPC (default 1500ms can be tight on CI).
    support::run_ok(&env, ["config", "set", "daemon_timeout_ms", "5000"]);
    // Restrict PATH so the full reindex only scans the shac binary dir — fast.
    let mut cmd = env.shac_cmd();
    cmd.env("PATH", env.bin_dir.to_string_lossy().as_ref());
    cmd.args(["reindex", "--full"]);
    let out = cmd.output().expect("run shac reindex --full");
    assert!(
        out.status.success(),
        "reindex --full failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("indexed"),
        "expected 'indexed' in output, got: {stdout}"
    );
}

/// --full and --skip-existing are mutually exclusive (enforced by clap).
#[test]
fn reindex_full_and_skip_existing_are_mutually_exclusive() {
    let env = support::TestEnv::new("reindex-conflict");
    let _daemon = env.spawn_daemon();
    let mut cmd = env.shac_cmd();
    cmd.args(["reindex", "--full", "--skip-existing"]);
    let out = cmd.output().expect("run shac");
    assert!(
        !out.status.success(),
        "expected failure when both --full and --skip-existing are passed"
    );
}

// ── 7.11: background indexer E2E ────────────────────────────────────────────

/// Proves the background indexer thread actually does work.
///
/// Strategy:
/// 1. Create a stub binary `shac-bg-test-stub` in a controlled stub dir.
/// 2. Launch the daemon with SHAC_BG_SETTLE_SECS=0 and
///    SHAC_BG_REINDEX_INTERVAL_SECS=1 so the background thread runs almost
///    immediately without waiting the real 6-hour interval.
///    PATH is restricted to only the stub dir + the test bin dir so the indexer
///    finishes quickly.
/// 3. Poll for up to 5 seconds for the stub binary to appear in `shac complete`
///    results (it is added to the `commands` table by reindex_path_commands and
///    surfaces in command-position completions).
/// 4. Assert the stub is present; fail with a descriptive message if it is not.
///
/// Note: the background thread runs `reindex_path_commands` with `full=false`,
/// which means `parse_help_output` is not called for unknown binaries (they are
/// not in the safe_default allow-list).  The verifiable side-effect is therefore
/// the command being added to the `commands` table, not having its docs parsed.
/// The completion check below relies on the command-position branch in
/// `Engine::complete` which lists all commands from `list_commands()`.
#[test]
fn bg_indexer_populates_commands_table() {
    let env = support::TestEnv::new("bg-indexer-e2e");

    // Create a stub binary directory with a unique binary name.
    let stub_dir = env.root.join("stub-bin");
    fs::create_dir_all(&stub_dir).expect("create stub dir");
    let stub_name = "shac-bg-test-stub";
    let stub_path = stub_dir.join(stub_name);
    fs::write(&stub_path, "#!/bin/sh\necho stub\n").expect("write stub binary");
    let mut perms = fs::metadata(&stub_path).expect("stub metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub_path, perms).expect("chmod stub binary");

    // Build a PATH that includes the stub dir first, plus the test bin dir.
    let stub_path_env = env.path_with_prefix(&stub_dir);

    // Spawn daemon with fast BG intervals so the indexer fires within ~1 second.
    // SHAC_BG_DISABLED=0 opts back in (the default is disabled in test harness).
    // SHAC_BG_SETTLE_SECS=0 removes the 2s startup delay.
    // SHAC_BG_REINDEX_INTERVAL_SECS=1 makes the loop repeat every second.
    let _daemon = env.spawn_daemon_with_extra_env(&[
        ("SHAC_BG_DISABLED", "0"),
        ("PATH", stub_path_env.as_str()),
        ("SHAC_BG_SETTLE_SECS", "0"),
        ("SHAC_BG_REINDEX_INTERVAL_SECS", "1"),
    ]);

    // Poll for up to 5 seconds for the stub to appear in completions.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let stub_prefix = &stub_name[..6]; // "shac-b" — enough to be distinctive
    let found = loop {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", &stub_path_env);
        cmd.args([
            "complete",
            "--shell", "zsh",
            "--line", stub_prefix,
            "--cursor", &stub_prefix.len().to_string(),
            "--cwd", env.root.to_string_lossy().as_ref(),
            "--format", "json",
        ]);
        if let Ok(out) = cmd.output() {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if stdout.contains(stub_name) {
                    break true;
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };

    assert!(
        found,
        "background indexer did not add '{stub_name}' to the commands table within 5s. \
         Check that SHAC_BG_SETTLE_SECS and SHAC_BG_REINDEX_INTERVAL_SECS env vars \
         are read by shacd correctly."
    );
}
