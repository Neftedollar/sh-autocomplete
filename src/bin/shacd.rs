use std::fs;
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use shac::config::AppPaths;
use shac::db::AppDb;
use shac::engine::{self, Engine};
use shac::indexer;
use shac::protocol::RecordCommandRequest;

const BG_REINDEX_INTERVAL: Duration = Duration::from_secs(6 * 3600);

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
        fs::remove_file(&paths.socket_file).ok();
    }
    fs::write(&paths.pid_file, std::process::id().to_string()).context("write pid file")?;
    let listener = UnixListener::bind(&paths.socket_file).context("bind unix socket")?;
    let _state_guard = StateGuard::new(paths.socket_file.clone(), paths.pid_file.clone());
    if engine::maybe_auto_train(&paths).unwrap_or(false) {
        eprintln!("shac: personalized model activated");
    }
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
                match AppDb::open(&db_path)
                    .and_then(|db| indexer::reindex_path_commands(&db, path_env.as_deref(), true, true))
                {
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
                if let Err(err) = handle_client(&engine, stream) {
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

fn handle_client(engine: &Engine, mut stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let request: serde_json::Value = serde_json::from_str(&line).context("parse request json")?;
    let action = request
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("complete");

    let payload = match action {
        "complete" => serde_json::to_vec(
            &engine.complete(serde_json::from_value(request["payload"].clone())?)?,
        )?,
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
            let full = request
                .get("payload")
                .and_then(|payload| payload.get("full"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Default skip_existing=true (safer/incremental); --full overrides it to false.
            let skip_existing = request
                .get("payload")
                .and_then(|payload| payload.get("skip_existing"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let indexed = engine.reindex(path_env, full, skip_existing)?;
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
