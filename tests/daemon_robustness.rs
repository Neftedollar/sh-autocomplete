// Integration tests for the runtime-audit daemon-robustness fixes (B1, B2,
// B3, B6). Each test spawns a real shacd (or drives `shac daemon ...`)
// against an isolated TestEnv sandbox — see tests/support/mod.rs.

mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

// ── B1: bounded request read ────────────────────────────────────────────────

/// A client that sends more than MAX_REQUEST_BYTES (1 MiB) with no newline
/// must be dropped cleanly — not grow daemon memory unboundedly, and not
/// wedge the daemon for subsequent clients.
#[test]
fn oversized_request_is_dropped_without_crashing_daemon() {
    let env = support::TestEnv::new("oversized-request");
    let _daemon = env.spawn_daemon();
    let paths = env.app_paths();

    {
        let mut stream = UnixStream::connect(&paths.socket_file).expect("connect oversized client");
        // Bound both directions so a buffer-full stall can't hang the test
        // itself; a real bug here should surface as a timeout error, not a
        // hang.
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .expect("set write timeout");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set read timeout");

        // 20 * 64 KiB = 1.25 MiB, comfortably over the 1 MiB cap, with no
        // trailing newline anywhere in the stream.
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..20 {
            if stream.write_all(&chunk).is_err() {
                // The daemon already closed its end after hitting the cap —
                // also an acceptable outcome, just stop sending.
                break;
            }
        }
        let _ = stream.flush();

        // The daemon must close the connection rather than hang waiting for
        // a newline that will never come.
        let mut buf = [0u8; 16];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(
            n, 0,
            "expected the daemon to close the oversized connection (EOF), got {n} bytes"
        );
    }

    // The daemon must still be alive and responsive to a normal request.
    let out = support::run_ok(&env, ["invalidate-caches"]);
    assert!(
        out.contains("caches invalidated"),
        "daemon did not survive an oversized request; got: {out}"
    );
}

// ── B2: read timeout on a silent client ─────────────────────────────────────

/// A client that connects and never sends anything must not freeze
/// completions for every other shell forever — it should be dropped after
/// the read timeout, after which the (single-threaded) accept loop resumes
/// serving other clients.
#[test]
fn silent_client_is_dropped_and_daemon_stays_responsive() {
    let env = support::TestEnv::new("silent-client");
    // Override the 5s production default so the test doesn't need to wait
    // that long — SHAC_CLIENT_READ_TIMEOUT_MS is test-only, like the
    // existing SHAC_BG_* overrides.
    let _daemon = env.spawn_daemon_with_extra_env(&[("SHAC_CLIENT_READ_TIMEOUT_MS", "300")]);
    let paths = env.app_paths();

    let stream = UnixStream::connect(&paths.socket_file).expect("connect silent client");
    // Send nothing. Hold the connection open well past the read timeout.
    std::thread::sleep(Duration::from_millis(700));

    // A normal request must complete promptly — proof the accept loop is not
    // still blocked inside the silent client's handle_client call.
    let out = support::run_ok(&env, ["invalidate-caches"]);
    assert!(
        out.contains("caches invalidated"),
        "daemon did not stay responsive past a silent client; got: {out}"
    );

    drop(stream);
}

// ── B3: verify pid identity before kill ─────────────────────────────────────

/// `shac daemon stop` must not signal a pid that is not actually shacd — a
/// stale pid-file whose pid the OS has recycled for an unrelated, live
/// process must be treated as "already dead" (clean up state, don't kill).
#[test]
fn daemon_stop_does_not_kill_unrelated_process_with_recycled_pid() {
    let env = support::TestEnv::new("recycled-pid-stop");
    let paths = env.app_paths();
    // `shac`'s own main() calls paths.ensure() before dispatching, but we
    // write the pid-file ourselves ahead of invoking the CLI, so make sure
    // the directory exists first.
    fs::create_dir_all(&paths.state_dir).expect("create state dir");

    // Stand in for "an unrelated process that now happens to own this
    // recycled pid" with a real, live, non-shacd process.
    let mut innocent = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn innocent process");
    fs::write(&paths.pid_file, innocent.id().to_string()).expect("write fake pid file");
    assert!(
        !paths.socket_file.exists(),
        "test setup should not have a socket file"
    );

    let out = support::run_ok(&env, ["daemon", "stop"]);
    assert!(out.contains("stopped"), "expected 'stopped', got: {out}");

    assert!(
        innocent.try_wait().expect("try_wait").is_none(),
        "daemon stop must not have signaled the unrelated process"
    );
    assert!(
        !paths.pid_file.exists(),
        "stale pid-file should have been cleaned up"
    );

    innocent.kill().ok();
    innocent.wait().ok();
}

/// Regression for a substring-vs-basename pid-verification bug: on macOS
/// `ps -p PID -o comm=` reports the process's full executable path, not just
/// its basename. A process whose path merely *contains* "shacd" as a
/// substring somewhere in a parent directory (e.g.
/// `/tmp/team-shacd-tools/backup-agent`) must NOT be misidentified as the
/// shacd daemon and killed — only an exact basename match of `shacd` counts.
// macOS-only: `ps -o comm=` reports the full executable path there, so a path
// containing "shacd" as a substring is the exact mis-kill vector this guards
// against. On Linux `ps -o comm=` reports only the (basename) command name, so
// the scenario is not reproducible; the basename fix itself is correct on both.
#[cfg(target_os = "macos")]
#[test]
fn daemon_stop_does_not_kill_process_whose_path_contains_shacd_substring() {
    let env = support::TestEnv::new("shacd-substring-path");
    let paths = env.app_paths();
    fs::create_dir_all(&paths.state_dir).expect("create state dir");

    // Executable lives at .../team-shacd-tools/backup-agent: the parent
    // directory name contains "shacd" as a substring, but the basename
    // ("backup-agent") does not.
    let fake_bin_dir = env.root.join("team-shacd-tools");
    fs::create_dir_all(&fake_bin_dir).expect("create fake bin dir");
    let fake_bin = fake_bin_dir.join("backup-agent");
    fs::copy("/bin/sleep", &fake_bin).expect("copy sleep binary");

    let mut innocent = Command::new(&fake_bin)
        .arg("30")
        .spawn()
        .expect("spawn innocent process with shacd-substring path");
    fs::write(&paths.pid_file, innocent.id().to_string()).expect("write fake pid file");
    assert!(
        !paths.socket_file.exists(),
        "test setup should not have a socket file"
    );

    let out = support::run_ok(&env, ["daemon", "stop"]);
    assert!(out.contains("stopped"), "expected 'stopped', got: {out}");

    assert!(
        innocent.try_wait().expect("try_wait").is_none(),
        "daemon stop must not have signaled a process whose path merely contains \
         'shacd' as a substring but whose basename does not match"
    );
    assert!(
        !paths.pid_file.exists(),
        "stale pid-file should have been cleaned up"
    );

    innocent.kill().ok();
    innocent.wait().ok();
}

/// Regression: when the pid-file really does name a running shacd, `daemon
/// stop` must still stop it (identical behavior to before the B3 fix).
///
/// Uses `shac daemon start` (rather than `TestEnv::spawn_daemon`) so shacd is
/// fully detached (via `setsid`/`nohup`, same as real usage) instead of
/// staying an unreaped child of this test process — an unreaped child stays
/// a zombie after being killed, and `kill -0`/`ps` (used by `pid_is_shacd`
/// and `process_exists`) still report zombies as present, which would make
/// this test assert on a harness artifact rather than shacd's real behavior.
#[test]
fn daemon_stop_still_kills_a_real_shacd_process() {
    let env = support::TestEnv::new("real-pid-stop");
    let paths = env.app_paths();

    let started = support::run_ok(&env, ["daemon", "start"]);
    assert!(
        started.contains("started"),
        "expected 'started', got: {started}"
    );
    assert!(paths.pid_file.exists());

    let out = support::run_ok(&env, ["daemon", "stop"]);
    assert!(out.contains("stopped"), "expected 'stopped', got: {out}");
    assert!(
        !paths.pid_file.exists(),
        "pid-file should be removed after a real stop"
    );
    assert!(
        !paths.socket_file.exists(),
        "socket file should be removed after a real stop"
    );
}

// ── B6: pid/socket startup lifecycle ────────────────────────────────────────

/// Starting a second shacd against a socket a live daemon already owns must
/// fail cleanly instead of unlinking the live socket out from under it.
#[test]
fn second_daemon_start_is_rejected_while_first_is_alive() {
    let env = support::TestEnv::new("dup-daemon-start");
    // The failed second-start attempt below leaves an established-but-never-
    // accepted connection in the first daemon's listen backlog (connect()
    // succeeds at the kernel level as soon as the socket exists, regardless
    // of whether shacd has called accept() yet). Use a short read timeout so
    // the first daemon clears that abandoned connection quickly instead of
    // holding up the `invalidate-caches` check below for the 5s production
    // default.
    let _daemon = env.spawn_daemon_with_extra_env(&[("SHAC_CLIENT_READ_TIMEOUT_MS", "300")]);
    let paths = env.app_paths();
    assert!(paths.socket_file.exists());

    let mut command = Command::new(&env.shacd);
    env.apply_env(&mut command);
    command.env("SHAC_BG_DISABLED", "1");
    let output = command.output().expect("run second shacd");
    assert!(
        !output.status.success(),
        "a second shacd instance must not start while the first is alive"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already listening"),
        "expected an 'already listening' error, got stderr: {stderr}"
    );

    // The first daemon's socket must be untouched and still answering.
    let out = support::run_ok(&env, ["invalidate-caches"]);
    assert!(
        out.contains("caches invalidated"),
        "first daemon should still be responsive; got: {out}"
    );
}

/// A stale socket left behind by a crashed daemon (no one listening) must
/// still be cleaned up so a fresh daemon can start normally.
#[test]
fn daemon_restarts_cleanly_after_a_crash_leaves_a_stale_socket() {
    let env = support::TestEnv::new("stale-socket-restart");
    let daemon = env.spawn_daemon();
    let paths = env.app_paths();
    assert!(paths.socket_file.exists());

    // Simulate a crash: SIGKILL so StateGuard's Drop never runs, leaving a
    // stale socket (and pid-file) on disk with nothing listening.
    drop(daemon);
    assert!(
        paths.socket_file.exists(),
        "expected the crashed daemon's socket file to remain on disk"
    );
    assert!(
        UnixStream::connect(&paths.socket_file).is_err(),
        "nothing should be listening on the stale socket"
    );

    // spawn_daemon polls for the socket to reappear and panics on failure,
    // which is exactly the assertion we want here: a fresh daemon must bind
    // successfully despite the stale socket file.
    let _daemon2 = env.spawn_daemon();
    let out = support::run_ok(&env, ["invalidate-caches"]);
    assert!(out.contains("caches invalidated"));
}
