//! Cold-start imports: zsh history, zoxide DB, and project scanner.
//!
//! See `PLAN-cold-start-and-hybrid-cd.md` § 2 (Workstream B) for the design
//! decisions baked into this module.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use regex::{Regex, RegexSet};
use sha2::{Digest, Sha256};

use crate::db::AppDb;
use crate::protocol::{PROVENANCE_LEGACY, TRUST_LEGACY};

/// Summary of one import source's outcome.
#[derive(Debug, Clone)]
pub struct ImportSummary {
    pub source: &'static str,
    pub seen: usize,
    pub inserted: usize,
    pub skipped_dup: usize,
    pub skipped_redacted: usize,
    pub elapsed: Duration,
}

impl ImportSummary {
    fn empty(source: &'static str) -> Self {
        Self {
            source,
            seen: 0,
            inserted: 0,
            skipped_dup: 0,
            skipped_redacted: 0,
            elapsed: Duration::ZERO,
        }
    }
}

/// Options bundling all configurable knobs for `run_full_import`.
#[derive(Debug, Clone)]
pub struct ImportOpts {
    pub yes: bool,
    pub roots: Vec<PathBuf>,
    pub depth: usize,
    pub shell: ShellKind,
    pub history_path: Option<PathBuf>,
    pub zoxide_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub enum ShellKind {
    Bash,
    Fish,
    Zsh,
}

impl ShellKind {
    pub fn label(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::Fish => "fish",
            ShellKind::Zsh => "zsh",
        }
    }
}

/// Compute the standard project-scan roots (filtered to existing directories).
pub fn default_project_roots() -> Vec<PathBuf> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let candidates = [
        home.join("dev"),
        home.join("Documents/dev"),
        home.join("code"),
        home.join("src"),
        home.join("projects"),
    ];
    candidates.into_iter().filter(|p| p.is_dir()).collect()
}

/// Default zsh history location (`~/.zsh_history`).
pub fn default_zsh_history_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zsh_history"))
}

/// Default zoxide DB location (`~/.local/share/zoxide/db.zo`).
pub fn default_zoxide_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".local/share/zoxide/db.zo"))
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Redactor compiles a single `RegexSet` for fast secret detection.
pub struct Redactor {
    set: RegexSet,
}

impl Redactor {
    pub fn new() -> Self {
        let patterns = [
            r"\bAKIA[0-9A-Z]{16}\b",
            r"\bASIA[0-9A-Z]{16}\b",
            r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
            r"\beyJ[A-Za-z0-9_-]{20,}\.eyJ",
            r"\bgithub_pat_[A-Za-z0-9_]{82}\b",
            r"\bghp_[A-Za-z0-9]{36}\b",
            r"\bsk-[A-Za-z0-9]{32,}\b",
            r"postgres(?:ql)?://[^@]+@",
        ];
        let set = RegexSet::new(patterns).expect("redaction patterns must compile");
        Self { set }
    }

    pub fn matches(&self, cmd: &str) -> bool {
        self.set.is_match(cmd)
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Zsh history parser
// ---------------------------------------------------------------------------

/// Parses one logical zsh-history command from a raw line.
///
/// Returns `(timestamp, command)`. Timestamp falls back to `0` when the
/// line uses zsh's plain (non-extended) format.
fn parse_history_line(line: &str, extended_re: &Regex) -> Option<(i64, String)> {
    if let Some(caps) = extended_re.captures(line) {
        let ts: i64 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let cmd = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
        Some((ts, cmd))
    } else if !line.is_empty() {
        Some((0, line.to_string()))
    } else {
        None
    }
}

/// Strip zsh's metafication byte (0x83) and return a UTF-8 string.
fn decode_zsh_bytes(bytes: &[u8]) -> String {
    let cleaned: Vec<u8> = bytes.iter().copied().filter(|b| *b != 0x83).collect();
    String::from_utf8_lossy(&cleaned).into_owned()
}

/// Pull the cd target out of a `cd <arg>` line. Honors `~`, `~/...`, `$HOME`,
/// `$HOME/...`, absolute paths, and `cd -`. Returns:
/// - `Some(Some(path))` for a resolvable update.
/// - `Some(None)` for `cd -` (resets `last_cd_target`).
/// - `None` to mean "do not change last_cd_target" (relative cd, etc.).
fn parse_cd_command(cmd: &str) -> Option<Option<String>> {
    let trimmed = cmd.trim_start();
    let rest = trimmed.strip_prefix("cd")?;
    // Must be followed by whitespace or end-of-string for a bare `cd`.
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let arg = rest.trim();
    if arg.is_empty() {
        // `cd` with no args -> $HOME
        return dirs::home_dir().map(|h| Some(h.to_string_lossy().into_owned()));
    }
    let first = arg.split_whitespace().next().unwrap_or("");
    if first == "-" {
        return Some(None);
    }
    if first == "~" {
        return dirs::home_dir().map(|h| Some(h.to_string_lossy().into_owned()));
    }
    if let Some(rest) = first.strip_prefix("~/") {
        return dirs::home_dir().map(|h| Some(h.join(rest).to_string_lossy().into_owned()));
    }
    if first == "$HOME" {
        return std::env::var("HOME")
            .ok()
            .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))
            .map(Some);
    }
    if let Some(rest) = first.strip_prefix("$HOME/") {
        let home = std::env::var("HOME")
            .ok()
            .or_else(|| dirs::home_dir().map(|h| h.to_string_lossy().into_owned()))?;
        return Some(Some(format!("{home}/{rest}")));
    }
    if first.starts_with('/') {
        return Some(Some(first.to_string()));
    }
    // Relative path: skip.
    None
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

/// Read raw history lines, joining `\\\n` continuations conservatively.
fn read_history_lines(path: &Path) -> Result<Vec<String>> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let decoded = decode_zsh_bytes(&bytes);
    let mut out = Vec::new();
    let mut current = String::new();
    for raw in decoded.lines() {
        if let Some(stripped) = raw.strip_suffix('\\') {
            current.push_str(stripped);
            current.push('\n');
            continue;
        }
        if !current.is_empty() {
            current.push_str(raw);
            out.push(std::mem::take(&mut current));
        } else {
            out.push(raw.to_string());
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    Ok(out)
}

/// Import zsh-history file into `history_events` and `paths_index`.
pub fn import_zsh_history(
    db: &AppDb,
    path: &Path,
    redactor: &Redactor,
) -> Result<ImportSummary> {
    let started = Instant::now();
    let mut summary = ImportSummary::empty("zsh_history");

    if !path.exists() {
        summary.elapsed = started.elapsed();
        return Ok(summary);
    }

    let extended_re = Regex::new(r"^: (\d+):\d+;(.*)$").expect("extended-history regex compiles");
    let lines = read_history_lines(path)?;

    let mut last_cd_target: Option<String> = None;
    let mut seen_hashes: HashSet<[u8; 32]> = HashSet::with_capacity(lines.len());
    let mut path_targets: Vec<String> = Vec::new();

    db.begin_txn()?;
    let result = (|| -> Result<()> {
        for raw_line in lines {
            let line = raw_line.trim_end_matches('\n');
            if line.is_empty() {
                continue;
            }
            let parsed = match parse_history_line(line, &extended_re) {
                Some(p) => p,
                None => continue,
            };
            let (ts, cmd) = parsed;
            let cmd = cmd.trim();
            if cmd.is_empty() {
                continue;
            }
            summary.seen += 1;

            // Replay cd state BEFORE redaction filtering so cd targets still
            // populate `paths_index` even if a future event would be redacted.
            if let Some(update) = parse_cd_command(cmd) {
                last_cd_target = update.clone();
                if let Some(path) = update {
                    path_targets.push(path);
                }
            }

            if redactor.matches(cmd) {
                summary.skipped_redacted += 1;
                continue;
            }

            // Idempotency: in-memory hash + DB partial unique index.
            let hash_hex = sha256_hex(&format!("{ts}|{cmd}"));
            let mut hash_bytes = [0u8; 32];
            for (i, chunk) in hash_hex.as_bytes().chunks(2).enumerate().take(32) {
                hash_bytes[i] = u8::from_str_radix(
                    std::str::from_utf8(chunk).unwrap_or("00"),
                    16,
                )
                .unwrap_or(0);
            }
            if !seen_hashes.insert(hash_bytes) {
                summary.skipped_dup += 1;
                continue;
            }

            let cwd = last_cd_target.clone().unwrap_or_default();
            let inserted = db.insert_imported_history(
                ts,
                &cwd,
                cmd,
                Some("zsh"),
                &hash_hex,
                TRUST_LEGACY,
                PROVENANCE_LEGACY,
            )?;
            if inserted {
                summary.inserted += 1;
            } else {
                summary.skipped_dup += 1;
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => db.commit_txn()?,
        Err(err) => {
            let _ = db.rollback_txn();
            return Err(err);
        }
    }

    // After commit, populate paths_index with all derived cd targets so the
    // hybrid-cd source has data even on a fresh install. We do these one-by-one
    // (visit_count semantics depend on per-call upserts).
    for target in path_targets {
        let is_git_repo = Path::new(&target).join(".git").exists();
        let _ = db.upsert_path_index(&target, "cwd_event", is_git_repo, None);
    }

    summary.elapsed = started.elapsed();
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Zoxide reader
// ---------------------------------------------------------------------------

fn read_u32_le<R: Read>(r: &mut R) -> std::io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_le<R: Read>(r: &mut R) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_f64_le<R: Read>(r: &mut R) -> std::io::Result<f64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(f64::from_le_bytes(buf))
}

fn read_bincode_string<R: Read>(r: &mut R) -> std::io::Result<String> {
    let len = read_u64_le(r)? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Import a zoxide v3 database into `paths_index`.
pub fn import_zoxide(db: &AppDb, path: &Path) -> Result<ImportSummary> {
    let started = Instant::now();
    let mut summary = ImportSummary::empty("zoxide");

    if !path.exists() {
        summary.elapsed = started.elapsed();
        return Ok(summary);
    }

    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::new(file);

    let version = match read_u32_le(&mut reader) {
        Ok(v) => v,
        Err(_) => {
            summary.elapsed = started.elapsed();
            return Ok(summary);
        }
    };
    if version != 3 {
        eprintln!(
            "shac: skipping zoxide DB at {} (version {} != 3)",
            path.display(),
            version
        );
        summary.elapsed = started.elapsed();
        return Ok(summary);
    }

    let count = match read_u64_le(&mut reader) {
        Ok(c) => c,
        Err(_) => {
            summary.elapsed = started.elapsed();
            return Ok(summary);
        }
    };

    for _ in 0..count {
        let entry_path = match read_bincode_string(&mut reader) {
            Ok(p) => p,
            Err(_) => break,
        };
        let rank = match read_f64_le(&mut reader) {
            Ok(r) => r,
            Err(_) => break,
        };
        let last_accessed = match read_u64_le(&mut reader) {
            Ok(t) => t,
            Err(_) => break,
        };
        summary.seen += 1;
        let is_git_repo = Path::new(&entry_path).join(".git").exists();
        if db
            .upsert_path_index_with_rank(
                &entry_path,
                rank,
                last_accessed as i64,
                "zoxide_import",
                is_git_repo,
                None,
            )
            .is_ok()
        {
            summary.inserted += 1;
        }
    }

    summary.elapsed = started.elapsed();
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Project scanner
// ---------------------------------------------------------------------------

const PRUNE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    ".next",
    "vendor",
    ".cache",
];

fn detect_project_marker(dir: &Path) -> Option<String> {
    let exact = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "Gemfile",
        "composer.json",
    ];
    for marker in exact {
        if dir.join(marker).exists() {
            return Some(marker.to_string());
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.ends_with(".fsproj") || s.ends_with(".csproj") {
                return Some(s.into_owned());
            }
        }
    }
    None
}

/// Walk `roots` looking for `.git`-bearing directories up to `max_depth`.
pub fn scan_projects(
    db: &AppDb,
    roots: &[PathBuf],
    max_depth: usize,
) -> Result<ImportSummary> {
    let started = Instant::now();
    let mut summary = ImportSummary::empty("project_scan");

    let mut stack: Vec<(PathBuf, usize)> = roots
        .iter()
        .filter(|p| p.is_dir())
        .map(|p| (p.clone(), 0usize))
        .collect();

    while let Some((dir, depth)) = stack.pop() {
        summary.seen += 1;

        // If `.git` (file or dir) is here, treat as project and stop descent.
        let git_marker = dir.join(".git");
        if git_marker.exists() {
            let marker = detect_project_marker(&dir);
            let path_str = dir.to_string_lossy().into_owned();
            let mtime = dir
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if db
                .upsert_path_index_with_rank(
                    &path_str,
                    0.5,
                    mtime,
                    "project_scan",
                    true,
                    marker.as_deref(),
                )
                .is_ok()
            {
                summary.inserted += 1;
            }
            continue;
        }

        if depth >= max_depth {
            continue;
        }

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if PRUNE_DIRS.iter().any(|p| *p == name_str) {
                    continue;
                }
                if name_str.starts_with('.') {
                    // Skip hidden dirs other than what we already handled.
                    continue;
                }
                stack.push((path, depth + 1));
            }
        }
    }

    summary.elapsed = started.elapsed();
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

fn prompt_yes_default(prompt: &str) -> bool {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "{prompt}");
    let _ = stdout.flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return true;
    }
    let answer = buf.trim().to_lowercase();
    answer.is_empty() || answer == "y" || answer == "yes"
}

/// Run all importers and return per-source summaries. Records `install_ts`
/// in `app_meta` if not already present.
pub fn run_full_import(db: &AppDb, opts: ImportOpts) -> Result<Vec<ImportSummary>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    db.meta_set_if_unset("install_ts", &now.to_string())?;

    let mut summaries = Vec::new();
    let redactor = Redactor::new();

    // Zsh history (only on zsh by default; other shells skip).
    if matches!(opts.shell, ShellKind::Zsh) {
        let history_path = opts.history_path.clone().or_else(default_zsh_history_path);
        if let Some(path) = history_path {
            if path.exists() {
                let approved = if opts.yes {
                    true
                } else {
                    let entry_count = read_history_lines(&path)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    prompt_yes_default(&format!(
                        "Import {} zsh history entries? [Y/n] ",
                        entry_count
                    ))
                };
                if approved {
                    summaries.push(import_zsh_history(db, &path, &redactor)?);
                } else {
                    let mut s = ImportSummary::empty("zsh_history");
                    s.elapsed = Duration::ZERO;
                    summaries.push(s);
                }
            } else {
                summaries.push(ImportSummary::empty("zsh_history"));
            }
        }
    } else {
        let _ = opts.shell.label(); // currently bash/fish history not imported
    }

    // Zoxide.
    let zoxide_path = opts.zoxide_path.clone().or_else(default_zoxide_path);
    if let Some(path) = zoxide_path {
        summaries.push(import_zoxide(db, &path)?);
    }

    // Project scanner.
    summaries.push(scan_projects(db, &opts.roots, opts.depth)?);

    Ok(summaries)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn open_test_db() -> (tempdir::TempDir, AppDb) {
        let dir = tempdir::TempDir::new("shac-import-test").expect("tempdir");
        let db_path = dir.path().join("shac.db");
        let db = AppDb::open(&db_path).expect("open db");
        (dir, db)
    }

    fn write_history_file(dir: &Path, contents: &[u8]) -> PathBuf {
        let path = dir.join(".zsh_history");
        let mut f = File::create(&path).expect("create");
        f.write_all(contents).expect("write");
        path
    }

    #[test]
    fn parses_extended_history_format() {
        let re = Regex::new(r"^: (\d+):\d+;(.*)$").unwrap();
        let parsed = parse_history_line(": 1700000000:0;git status", &re).unwrap();
        assert_eq!(parsed.0, 1_700_000_000);
        assert_eq!(parsed.1, "git status");
    }

    #[test]
    fn parses_plain_history_format() {
        let re = Regex::new(r"^: (\d+):\d+;(.*)$").unwrap();
        let parsed = parse_history_line("ls -al", &re).unwrap();
        assert_eq!(parsed.0, 0);
        assert_eq!(parsed.1, "ls -al");
    }

    #[test]
    fn idempotent_double_import() {
        let (dir, db) = open_test_db();
        let history = write_history_file(
            dir.path(),
            b": 1700000000:0;ls\n: 1700000001:0;cd /tmp\n: 1700000002:0;echo hi\n",
        );
        let red = Redactor::new();
        let first = import_zsh_history(&db, &history, &red).unwrap();
        assert!(first.inserted > 0, "first import should insert rows");
        let second = import_zsh_history(&db, &history, &red).unwrap();
        assert_eq!(second.inserted, 0);
        assert_eq!(second.skipped_dup, first.inserted);
    }

    #[test]
    fn cd_replay_populates_paths_index() {
        let (dir, db) = open_test_db();
        let history = write_history_file(
            dir.path(),
            b": 1700000000:0;cd /tmp/foo\n: 1700000001:0;echo hi\n",
        );
        let red = Redactor::new();
        import_zsh_history(&db, &history, &red).unwrap();
        let rows = db.top_paths(None, 50).unwrap();
        assert!(
            rows.iter().any(|r| r.path == "/tmp/foo"),
            "expected /tmp/foo in paths_index, got: {:?}",
            rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cd_with_tilde_expansion() {
        let parsed = parse_cd_command("cd ~/proj").unwrap().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(parsed, home.join("proj").to_string_lossy());
    }

    #[test]
    fn cd_relative_skipped() {
        assert!(parse_cd_command("cd ../foo").is_none());
        assert!(parse_cd_command("cd subdir").is_none());
    }

    #[test]
    fn redactor_drops_aws_key() {
        let red = Redactor::new();
        assert!(red.matches("aws s3 ls AKIA1234567890ABCDEF"));
        assert!(!red.matches("ls /tmp"));
    }

    #[test]
    fn zoxide_v3_parser() {
        let (dir, db) = open_test_db();
        let zo_path = dir.path().join("db.zo");
        let mut buf: Vec<u8> = Vec::new();
        // version 3
        buf.extend_from_slice(&3u32.to_le_bytes());
        // 2 entries
        buf.extend_from_slice(&2u64.to_le_bytes());
        for (path, rank, last) in [("/tmp/aaa", 4.0_f64, 100u64), ("/tmp/bbb", 2.0, 200)] {
            buf.extend_from_slice(&(path.len() as u64).to_le_bytes());
            buf.extend_from_slice(path.as_bytes());
            buf.extend_from_slice(&rank.to_le_bytes());
            buf.extend_from_slice(&last.to_le_bytes());
        }
        std::fs::write(&zo_path, &buf).unwrap();
        let summary = import_zoxide(&db, &zo_path).unwrap();
        assert_eq!(summary.seen, 2);
        assert_eq!(summary.inserted, 2);
        assert_eq!(db.count_paths_index_by_source("zoxide_import").unwrap(), 2);
    }

    #[test]
    fn zoxide_wrong_version_skipped() {
        let (dir, db) = open_test_db();
        let zo_path = dir.path().join("db.zo");
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());
        std::fs::write(&zo_path, &buf).unwrap();
        let summary = import_zoxide(&db, &zo_path).unwrap();
        assert_eq!(summary.seen, 0);
        assert_eq!(summary.inserted, 0);
        assert_eq!(db.count_paths_index().unwrap(), 0);
    }

    #[test]
    fn project_scanner_finds_git_repos() {
        let (dir, db) = open_test_db();
        let root = dir.path().join("dev");
        for sub in ["alpha", "beta", "gamma"] {
            let r = root.join(sub);
            std::fs::create_dir_all(r.join(".git")).unwrap();
            std::fs::write(r.join("Cargo.toml"), "[package]\n").unwrap();
        }
        // Add a non-repo too
        std::fs::create_dir_all(root.join("delta")).unwrap();
        let summary = scan_projects(&db, std::slice::from_ref(&root), 3).unwrap();
        assert!(summary.inserted >= 3, "summary={:?}", summary);
        assert_eq!(db.count_paths_index_by_source("project_scan").unwrap(), 3);
        let rows = db.top_paths(None, 50).unwrap();
        for sub in ["alpha", "beta", "gamma"] {
            assert!(
                rows.iter()
                    .any(|r| r.path.ends_with(sub) && r.is_git_repo),
                "missing repo for {sub}"
            );
        }
    }

    #[test]
    fn perf_10k_history_lines_under_250ms() {
        let (dir, db) = open_test_db();
        let mut content = String::with_capacity(10_000 * 40);
        for i in 0..10_000 {
            content.push_str(&format!(": 17000{:05}:0;echo line{i}\n", i));
        }
        let history_path = dir.path().join(".zsh_history");
        std::fs::write(&history_path, content.as_bytes()).unwrap();
        let red = Redactor::new();
        let started = Instant::now();
        let summary = import_zsh_history(&db, &history_path, &red).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(summary.inserted, 10_000);
        // Generous budget; CI hosts can be slow.
        assert!(
            elapsed < Duration::from_millis(2_500),
            "10k zsh-history lines took {:?}",
            elapsed
        );
        eprintln!("perf_10k_history_lines: {:?}", elapsed);
    }
}

// `tempdir` shim — the project doesn't currently depend on `tempdir`, so we
// implement a tiny in-tree replacement for tests only. Production code does
// not use this.
#[cfg(test)]
mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        pub fn new(prefix: &str) -> std::io::Result<Self> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = std::env::temp_dir().join(format!(
                "{prefix}-{}-{}-{}",
                std::process::id(),
                nanos,
                n
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
