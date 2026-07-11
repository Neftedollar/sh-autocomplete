use std::fs;
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use shac::config::{AppConfig, AppPaths};
use shac::db::{AppDb, COMPLETION_TELEMETRY_RETENTION_DAYS, HISTORY_RETENTION_DAYS};
use shac::engine::Engine;
use shac::indexer;
use shac::protocol::RecordCommandRequest;

const BG_REINDEX_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// Caps a single client request line so a client that never sends a newline
/// can't grow daemon memory without bound. A completion request is a few KB.
const MAX_REQUEST_BYTES: u64 = 1024 * 1024; // 1 MiB

/// The accept loop is single-threaded, so a silent/stalled client must not be
/// allowed to block completions for every other shell forever. Real clients
/// give up and fall back to native completion after their `daemon_timeout_ms`
/// budget (150ms by default — see `AppConfig::default`), so a multi-second
/// server-side timeout stalls every other client for far longer than any
/// client actually waits. 500ms comfortably covers a legitimate local
/// unix-socket round trip (the request is written immediately after
/// connect()) while bounding the serial-loop stall to a fraction of a
/// second. Note this only shrinks the stall window — it does not eliminate
/// it; a fully robust fix would make the accept loop handle connections
/// concurrently instead of serially, which is out of scope here.
const CLIENT_READ_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Parser)]
struct Args {
    #[arg(long)]
    socket: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut paths = AppPaths::discover()?;
    if let Some(socket) = args.socket {
        paths.socket_file = socket.into();
    }
    paths.ensure()?;
    if paths.socket_file.exists() {
        if UnixStream::connect(&paths.socket_file).is_ok() {
            // A live daemon is still listening on this socket — do not
            // unlink it out from under it, and do not start a second one.
            anyhow::bail!(
                "another shacd instance is already listening on {}",
                paths.socket_file.display()
            );
        }
        // Nothing answered: a stale socket left behind by a crashed/killed
        // daemon. Safe to unlink and rebind.
        fs::remove_file(&paths.socket_file).ok();
    }
    let listener = UnixListener::bind(&paths.socket_file).context("bind unix socket")?;
    // The control socket is unauthenticated (any local peer can `complete`,
    // `stats`, or poison learning via `record-command`), so restrict it to the
    // owner (F5). Best-effort chmod right after bind, then verify the ACTUAL
    // mode once at startup: if the chmod silently failed (exotic FS) and the
    // 0700 state dir didn't shield it either, warn rather than leaving the
    // channel quietly reachable by other local users.
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&paths.socket_file, fs::Permissions::from_mode(0o600));
        if let Ok(meta) = fs::metadata(&paths.socket_file) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                eprintln!(
                    "shac: warning: control socket {} is group/other-accessible (mode {mode:o}); \
                     other local users may be able to reach the daemon",
                    paths.socket_file.display()
                );
            }
        }
    }
    // Only claim the pid-file after a successful bind, so a bind failure
    // never leaves an orphaned pid-file pointing at a process that's about
    // to exit.
    fs::write(&paths.pid_file, std::process::id().to_string()).context("write pid file")?;
    let _state_guard = StateGuard::new(paths.socket_file.clone(), paths.pid_file.clone());
    // SHAC_CLIENT_READ_TIMEOUT_MS overrides the per-client read timeout —
    // intended for integration tests only (the 5s default would make a
    // stalled-client test slow).
    let client_read_timeout = std::env::var("SHAC_CLIENT_READ_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(CLIENT_READ_TIMEOUT);
    let engine = Engine::new(&paths)?;

    // Background indexer: opens its own DB connection (WAL-safe) and
    // incrementally indexes --help output for all PATH executables.
    // Waits 2s after daemon start to avoid competing with first completions,
    // then loops every BG_REINDEX_INTERVAL.  Uses skip_existing=true so it
    // never overwrites manually-indexed docs or reindexes commands already in DB.
    // On transient errors, retries with exponential backoff (60s → 300s → cap).
    //
    // SHAC_BG_REINDEX_INTERVAL_SECS and SHAC_BG_SETTLE_SECS override the intervals
    // at runtime — intended for integration tests only.
    // Set SHAC_BG_DISABLED=1 to skip spawning the thread entirely (used by tests).
    if std::env::var("SHAC_BG_DISABLED")
        .ok()
        .filter(|v| !v.is_empty() && v != "0")
        .is_some()
    {
        eprintln!("shacd: bg indexer disabled via SHAC_BG_DISABLED");
    } else {
        let db_path = paths.db_file.clone();
        let config_paths = paths.clone();
        let path_env = std::env::var("PATH").ok();
        let reindex_interval = std::env::var("SHAC_BG_REINDEX_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(BG_REINDEX_INTERVAL);
        let settle_secs = std::env::var("SHAC_BG_SETTLE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(2);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(settle_secs));
            // Backoff schedule for consecutive failures: 60s, 300s, then cap at reindex_interval.
            let backoff = [
                Duration::from_secs(60).min(reindex_interval),
                Duration::from_secs(300).min(reindex_interval),
                reindex_interval,
            ];
            let mut fail_count: usize = 0;
            loop {
                match AppDb::open(&db_path).and_then(|db| {
                    // Prune completion telemetry once per daemon start (this
                    // loop's first pass) and then on every periodic tick, so
                    // completion_requests/completion_items don't grow
                    // unbounded. Best-effort: a prune failure never blocks
                    // reindexing. Reloaded from config on every tick (cheap:
                    // a small TOML file) rather than once at daemon startup,
                    // so `shac config set telemetry_retention_days ...`
                    // takes effect without restarting the daemon — this is
                    // the user-facing privacy control, it should be
                    // responsive.
                    let config = AppConfig::load(&config_paths).ok();
                    let telemetry_days = config
                        .as_ref()
                        .map(|c| c.telemetry_retention_days as i64)
                        .unwrap_or(COMPLETION_TELEMETRY_RETENTION_DAYS);
                    if let Err(e) = db.prune_completion_telemetry(telemetry_days) {
                        eprintln!("shac: telemetry prune error: {e}");
                    }
                    // Also cap the recorded shell-command history so the DB
                    // can't grow without bound over months of use. Same
                    // reload-per-tick rationale as telemetry: `shac config set
                    // history_retention_days ...` takes effect without a
                    // daemon restart.
                    let history_days = config
                        .as_ref()
                        .map(|c| c.history_retention_days as i64)
                        .unwrap_or(HISTORY_RETENTION_DAYS);
                    if let Err(e) = db.prune_history_events(history_days) {
                        eprintln!("shac: history prune error: {e}");
                    }
                    // bg indexer never shells out to `<cmd> --help`; only
                    // records names + paths and seeds bundled static_docs.
                    // Per-command --help extraction is opt-in via
                    // `shac index add-command <name>` only.
                    indexer::reindex_path_commands(&db, path_env.as_deref(), true)
                }) {
                    Ok(n) => {
                        eprintln!("shac: background indexed {} commands", n);
                        fail_count = 0;
                        thread::sleep(reindex_interval);
                    }
                    Err(e) => {
                        eprintln!("shac: background index error: {e}");
                        let idx = fail_count.min(backoff.len() - 1);
                        thread::sleep(backoff[idx]);
                        fail_count = fail_count.saturating_add(1);
                    }
                }
            }
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_client(&engine, stream, client_read_timeout) {
                    if !is_broken_pipe(&err) {
                        eprintln!("client error: {err:#}");
                    }
                }
            }
            Err(err) => eprintln!("accept error: {err:#}"),
        }
    }
    Ok(())
}

struct StateGuard {
    socket_file: PathBuf,
    pid_file: PathBuf,
}

impl StateGuard {
    fn new(socket_file: PathBuf, pid_file: PathBuf) -> Self {
        Self {
            socket_file,
            pid_file,
        }
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        fs::remove_file(&self.socket_file).ok();
        fs::remove_file(&self.pid_file).ok();
    }
}

fn handle_client(engine: &Engine, mut stream: UnixStream, read_timeout: Duration) -> Result<()> {
    stream
        .set_read_timeout(Some(read_timeout))
        .context("set client read timeout")?;
    let mut reader = BufReader::new(stream.try_clone()?.take(MAX_REQUEST_BYTES));
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(_) => {}
        Err(err) if is_timeout(&err) => return Ok(()),
        Err(err) => return Err(err).context("read client request"),
    }
    if line.trim().is_empty() {
        return Ok(());
    }
    if !line.ends_with('\n') {
        // Either the request exceeded MAX_REQUEST_BYTES with no newline in
        // sight, or the peer closed the connection mid-line. Drop it
        // cleanly instead of parsing a truncated/oversized request.
        anyhow::bail!("request exceeded {MAX_REQUEST_BYTES}-byte limit or was truncated");
    }
    let request: serde_json::Value = serde_json::from_str(&line).context("parse request json")?;
    let action = request
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("complete");

    let payload = match action {
        "complete" => {
            let mut resp = serde_json::to_value(
                &engine.complete(serde_json::from_value(request["payload"].clone())?)?,
            )?;
            resp["daemon_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
            serde_json::to_vec(&resp)?
        }
        "explain" => serde_json::to_vec(
            &engine.explain(serde_json::from_value(request["payload"].clone())?)?,
        )?,
        "record-command" => {
            let payload: RecordCommandRequest = serde_json::from_value(request["payload"].clone())?;
            engine.record_command(payload)?;
            br#"{"ok":true}"#.to_vec()
        }
        "reindex" => {
            let path_env = request
                .get("payload")
                .and_then(|payload| payload.get("path_env"))
                .and_then(|value| value.as_str());
            let skip_existing = request
                .get("payload")
                .and_then(|payload| payload.get("skip_existing"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let indexed = engine.reindex(path_env, skip_existing)?;
            serde_json::to_vec(&serde_json::json!({ "indexed": indexed }))?
        }
        "invalidate-caches" => {
            engine.invalidate_caches();
            br#"{"ok":true}"#.to_vec()
        }
        "stats" => serde_json::to_vec(&engine.stats()?)?,
        _ => serde_json::to_vec(
            &serde_json::json!({ "error": format!("unknown action: {action}") }),
        )?,
    };
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    Ok(())
}

fn is_timeout(err: &std::io::Error) -> bool {
    matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io_err| {
                matches!(
                    io_err.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                )
            })
            .unwrap_or(false)
    })
}
