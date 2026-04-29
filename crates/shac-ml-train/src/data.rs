//! JSONL records exchanged between pipeline stages.
//!
//! Stage outputs:
//!   gen_synthetic → SyntheticEvent (one per line)
//!   scrub        → SyntheticEvent (still, but PII replaced)
//!   distill      → DistilledExample (with soft targets)
//!   train        → reads DistilledExample
//!
//! All records are versioned by `schema_version` so a new pipeline run
//! catches any drift early.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticEvent {
    pub schema_version: u32,
    pub persona_id: String,
    pub os: String,                 // "darwin" | "linux"
    pub cwd: String,                // already-scrubbed path template
    pub command: String,
    pub prev_command: Option<String>,
    pub ts_offset_secs: i64,        // synthetic monotonic clock within session
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistilledExample {
    pub schema_version: u32,
    pub os: String,
    pub cwd_bucket: u8,
    pub context_tokens: Vec<u32>,   // length = context_len, padded with <PAD>
    pub hard_label: u32,            // ground-truth next-token id
    pub soft_targets_top: Vec<(u32, f32)>, // top-k from teacher; rest = uniform residual
}

pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {} of {}", lineno + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: T = serde_json::from_str(&line)
            .with_context(|| format!("parse line {} of {}", lineno + 1, path.display()))?;
        out.push(rec);
    }
    Ok(out)
}

pub fn write_jsonl<T: Serialize>(path: &Path, records: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for rec in records {
        let line = serde_json::to_string(rec).context("serialize record")?;
        writeln!(writer, "{}", line)?;
    }
    writer.flush()?;
    Ok(())
}
