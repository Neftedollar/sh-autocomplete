//! Run a child process with a hard wall-clock timeout, draining stdout
//! concurrently so a child that fills the OS pipe buffer can't deadlock.
//!
//! Every daemon-side shellout (`git for-each-ref`, `kubectl api-resources`,
//! `docker images/ps`, `<cmd> --help`, `man`) previously followed the same
//! shape: spawn with stdout piped, poll `try_wait()` in a sleep loop, kill on
//! timeout, then read stdout *after* the child exited. That ordering
//! deadlocks whenever the child writes more than one pipe buffer (~64 KiB on
//! macOS/Linux) before exiting: the child blocks in `write()` waiting for a
//! reader, we block waiting for it to exit, and the timeout fires and kills a
//! process that was only ever stuck because we refused to read it. `man` and
//! large `--help` outputs routinely exceed 64 KiB, so this was a real
//! truncation/timeout bug, not a theoretical one. Centralizing the pattern
//! here — with a reader thread that drains stdout in parallel — fixes all six
//! sites at once (F10).
//!
//! Draining alone is not enough, because `child.kill()` only signals the
//! *direct* child: a grandchild that inherited the stdout pipe (a backgrounded
//! job, `man`'s groff/cat pipeline, a shim script that forks) keeps the write
//! end open, so `read_to_end` never sees EOF. To bound that, the child runs in
//! its own process group (`process_group(0)`) so a timeout can SIGKILL the
//! whole tree, and the final collect uses `recv_timeout` — a descendant that
//! escapes the group (double-fork) can then leak the reader thread but can
//! never wedge the (single-threaded) daemon's accept loop past the grace
//! window.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the final stdout collect waits before giving up on a descendant
/// that's still holding the pipe. On timeout the reader thread is detached
/// (one leaked thread + its partial buffer is strictly better than stalling
/// every other client); after a group SIGKILL the pipe closes well within it.
const REAP_GRACE: Duration = Duration::from_millis(200);

/// SIGKILL the whole process group led by `pid` (negative pid targets the
/// group). Best-effort: the child may already be dead/reaped, or a descendant
/// may have left the group. Safe: `pid` is a live-or-zombie child we spawned
/// into its own group, so the group id can't have been reused.
fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

/// Captured result of a timed shellout: the full stdout bytes and whether the
/// child exited zero. Returned only when the child exited on its own before
/// the deadline; a timeout, spawn failure, or missing stdout handle yields
/// `None`.
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub success: bool,
}

/// Spawn `command` with stdout piped, drain stdout on a dedicated thread, and
/// enforce a hard `timeout`. The caller configures args/env/stdin/stderr;
/// this helper always overrides stdout to a pipe so it owns the capture.
///
/// Returns `Some(CapturedOutput)` when the child exits before `timeout`,
/// `None` on spawn failure, poll error, or timeout (the child's whole process
/// group is killed and reaped in those cases). Draining on a separate thread
/// plus the process-group handling is what makes this safe for children whose
/// output exceeds the pipe buffer or that leave descendants holding it — see
/// the module docs.
pub fn run_capture(mut command: Command, timeout: Duration) -> Option<CapturedOutput> {
    command.stdout(Stdio::piped());
    // New process group led by the child, so a timeout can signal the whole
    // subtree, not just the direct child (see module docs).
    command.process_group(0);
    let mut child = command.spawn().ok()?;
    let pid = child.id();
    let stdout = child.stdout.take()?;

    // read_to_end blocks until EOF. EOF arrives when every holder of the write
    // end closes it — normally the child, but a descendant can extend that, so
    // the collect below is bounded rather than a bare `recv`.
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    kill_group(pid);
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                kill_group(pid);
                let _ = child.wait();
                break None;
            }
        }
    };

    let Some(status) = status else {
        // Timeout/error: the group was SIGKILLed, so the pipe should close and
        // the reader finish within REAP_GRACE. If a descendant escaped the
        // group and still holds it, DON'T block the daemon — detach the reader
        // (leak one thread + its partial buffer until the pipe finally closes)
        // and give up. Output is discarded on this path regardless.
        let _ = rx.recv_timeout(REAP_GRACE);
        return None;
    };

    // Clean exit. The child is done, but a backgrounded descendant it spawned
    // may still hold the pipe. Wait briefly for a normal drain; if one lingers
    // past the grace window, force the pipe closed by killing the group and
    // take whatever was captured — never block unboundedly.
    let stdout = match rx.recv_timeout(REAP_GRACE) {
        Ok(buf) => buf,
        Err(_) => {
            kill_group(pid);
            rx.recv().unwrap_or_default()
        }
    };
    let _ = reader.join();

    Some(CapturedOutput {
        stdout,
        success: status.success(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `/bin/sh -c <script>` with an explicit PATH. This keeps the tests
    /// hermetic: cargo runs a binary's unit tests as parallel threads in one
    /// process, and other tests (e.g. `list_kubectl_resources`) mutate the
    /// process-global `PATH` via `std::env::set_var("PATH", "/dev/null")` to
    /// exercise their not-on-PATH branch. Resolving `sh`/`yes`/`head` through
    /// the inherited PATH would race those and fail with NotFound. Absolute
    /// `/bin/sh` needs no lookup, and the explicit PATH covers the utilities
    /// the scripts pipe through.
    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]).env("PATH", "/usr/bin:/bin");
        cmd
    }

    #[test]
    fn captures_small_output() {
        let out =
            run_capture(sh("printf hello"), Duration::from_secs(5)).expect("child should exit");
        assert!(out.success);
        assert_eq!(out.stdout, b"hello");
    }

    #[test]
    fn captures_large_output_without_deadlock() {
        // Emit far more than one pipe buffer (~64 KiB). The old "read after
        // exit" pattern would deadlock here; the concurrent reader must not.
        let out = run_capture(sh("yes shac | head -c 1000000"), Duration::from_secs(10))
            .expect("child should exit");
        assert!(out.success);
        assert_eq!(out.stdout.len(), 1_000_000);
    }

    #[test]
    fn times_out_and_returns_none() {
        let out = run_capture(sh("sleep 30"), Duration::from_millis(100));
        assert!(out.is_none(), "a process past its deadline must yield None");
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let out = run_capture(sh("exit 3"), Duration::from_secs(5)).expect("child should exit");
        assert!(!out.success);
    }

    #[test]
    fn timeout_with_backgrounded_child_does_not_hang() {
        // The direct child blocks on a foreground `sleep 30` while a
        // backgrounded `sleep 30` inherits and holds the stdout pipe. The old
        // bare `recv()` blocked until that grandchild exited (~30s). The
        // group-kill + bounded recv must return well inside the bound.
        // (Bound deliberately loose — expected ~0.3s, bug ~30s.)
        let start = Instant::now();
        let out = run_capture(
            sh("sleep 30 & printf hi; sleep 30"),
            Duration::from_millis(100),
        );
        let elapsed = start.elapsed();
        assert!(out.is_none(), "a timed-out capture yields None");
        assert!(
            elapsed < Duration::from_secs(5),
            "must not block on the inherited pipe (took {elapsed:?})"
        );
    }

    #[test]
    fn clean_exit_with_backgrounded_child_returns_promptly() {
        // The direct child exits cleanly but leaves a backgrounded `sleep 30`
        // holding the pipe. We must return the child's own output without
        // waiting out the grandchild's lifetime.
        let start = Instant::now();
        let out = run_capture(sh("sleep 30 & printf hi"), Duration::from_secs(10))
            .expect("child exits cleanly");
        let elapsed = start.elapsed();
        assert!(out.success);
        assert_eq!(out.stdout, b"hi");
        assert!(
            elapsed < Duration::from_secs(5),
            "clean exit must not wait for the backgrounded child (took {elapsed:?})"
        );
    }
}
