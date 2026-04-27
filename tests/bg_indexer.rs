// Integration tests for 7.11 (background indexer), 7.5b (invalidate-caches), and
// 7.5c (reindex --full / --skip-existing CLI flags).

mod support;

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
