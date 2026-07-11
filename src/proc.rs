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

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
/// `None` on spawn failure, poll error, or timeout (the child is killed and
/// reaped in those cases). Draining on a separate thread is what makes this
/// safe for children whose output exceeds the pipe buffer — see the module
/// docs.
pub fn run_capture(mut command: Command, timeout: Duration) -> Option<CapturedOutput> {
    command.stdout(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;

    // read_to_end blocks until EOF, which arrives either when the child exits
    // normally or when we kill it below — so the reader thread always finishes
    // promptly and never leaks.
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
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    // Killing/exiting the child closes the pipe's write end, so read_to_end
    // returns and the reader thread sends its buffer. Collect it (partial
    // output on timeout is discarded via the `status?` below) and join.
    let stdout = rx.recv().unwrap_or_default();
    let _ = reader.join();

    let status = status?;
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
}
