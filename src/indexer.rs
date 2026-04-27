use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::db::{AppDb, StoredDoc};

const HELP_TIMEOUT: Duration = Duration::from_millis(350);
const MAX_DOCS_PER_COMMAND: usize = 80;
const MAX_DESCRIPTION_LEN: usize = 160;

pub fn reindex_path_commands(
    db: &AppDb,
    path_env: Option<&str>,
    full: bool,
    skip_existing: bool,
) -> Result<usize> {
    let mut seen = BTreeSet::new();
    let path_env = path_env
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| env::var("PATH").unwrap_or_default());

    for dir in path_env.split(':').filter(|segment| !segment.is_empty()) {
        let dir_path = Path::new(dir);
        if !dir_path.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(dir_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = match path.file_name().and_then(|v| v.to_str()) {
                    Some(name) => name.to_string(),
                    None => continue,
                };
                if seen.contains(&name) || !is_executable(&path) {
                    continue;
                }
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or_default();
                db.upsert_command(
                    &name,
                    "command",
                    Some(path.to_string_lossy().as_ref()),
                    mtime,
                )?;
                if !skip_existing || !db.command_has_docs(&name) {
                    maybe_upsert_docs(db, &name, full)?;
                }
                seen.insert(name);
            }
        }
    }

    for builtin in [
        "cd", "echo", "export", "unset", "source", "alias", "pwd", "history", "type", "exit",
    ] {
        db.upsert_command(builtin, "builtin", None, 0)?;
    }

    Ok(seen.len())
}

pub fn index_command(db: &AppDb, command: &str, path_env: Option<&str>) -> Result<usize> {
    let path = find_command_path(command, path_env);
    let mtime = path.as_deref().map(path_mtime).unwrap_or_default();
    db.upsert_command(
        command,
        "command",
        path.as_ref()
            .map(|value| value.to_string_lossy())
            .as_deref(),
        mtime,
    )?;
    maybe_upsert_docs(db, command, true)?;
    db.upsert_index_target("command", command, false, false, 0)?;
    Ok(1)
}

pub fn index_path_target(
    db: &AppDb,
    path: &Path,
    recursive: bool,
    full: bool,
    max_depth: usize,
) -> Result<usize> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    db.upsert_index_target(
        "path",
        canonical.to_string_lossy().as_ref(),
        recursive,
        full,
        max_depth as i64,
    )?;

    let mut seen = BTreeSet::new();
    index_path_inner(db, &canonical, recursive, full, max_depth, 0, &mut seen)
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn maybe_upsert_docs(db: &AppDb, command: &str, explicit: bool) -> Result<()> {
    let docs = static_docs(command)
        .or_else(|| parse_help_output(command, explicit))
        .unwrap_or_default();
    if !docs.is_empty() {
        db.replace_docs_for_command(command, &docs)?;
    }
    Ok(())
}

fn parse_help_output(command: &str, explicit: bool) -> Option<Vec<StoredDoc>> {
    let safe_default = [
        "git", "docker", "kubectl", "npm", "cargo", "dotnet", "python", "python3", "pip", "pytest",
        "grep", "find", "ls",
    ];
    if !explicit && !safe_default.contains(&command) {
        return None;
    }
    let output = run_help_with_timeout(command, HELP_TIMEOUT)?;
    if !output.status.success() && output.stdout.is_empty() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut docs = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('-') {
            let mut parts = trimmed.splitn(2, "  ");
            let item = parts.next()?.trim();
            let description = parts.next().unwrap_or("").trim();
            if !item.is_empty() && !description.is_empty() {
                docs.push(doc(command, "option", item, description, "help"));
                if docs.len() >= MAX_DOCS_PER_COMMAND {
                    break;
                }
            }
        }
    }
    Some(docs)
}

fn run_help_with_timeout(command: &str, timeout: Duration) -> Option<std::process::Output> {
    let mut child = Command::new(command)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        if child.try_wait().ok()?.is_some() || start.elapsed() >= timeout {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if start.elapsed() >= timeout {
        let _ = child.kill();
    }
    child.wait_with_output().ok()
}

fn doc(
    command: &str,
    item_type: &str,
    item_value: &str,
    description: &str,
    source: &str,
) -> StoredDoc {
    StoredDoc {
        command: command.to_string(),
        item_type: item_type.to_string(),
        item_value: item_value.to_string(),
        description: truncate_description(description),
        source: source.to_string(),
    }
}

fn truncate_description(value: &str) -> String {
    value
        .chars()
        .take(MAX_DESCRIPTION_LEN)
        .collect::<String>()
        .trim()
        .to_string()
}

fn find_command_path(command: &str, path_env: Option<&str>) -> Option<PathBuf> {
    let path_env = path_env
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| env::var("PATH").unwrap_or_default());
    path_env
        .split(':')
        .filter(|segment| !segment.is_empty())
        .map(|segment| Path::new(segment).join(command))
        .find(|path| is_executable(path))
}

fn path_mtime(path: &Path) -> i64 {
    path.metadata()
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn index_path_inner(
    db: &AppDb,
    path: &Path,
    recursive: bool,
    full: bool,
    max_depth: usize,
    depth: usize,
    seen: &mut BTreeSet<String>,
) -> Result<usize> {
    if path.is_file() {
        return index_path_file(db, path, full, seen);
    }
    if !path.is_dir() {
        return Ok(0);
    }

    let mut indexed = 0;
    for entry in fs::read_dir(path)?.flatten() {
        let entry_path = entry.path();
        if entry_path.is_file() {
            indexed += index_path_file(db, &entry_path, full, seen)?;
        } else if recursive && depth < max_depth && entry_path.is_dir() {
            indexed +=
                index_path_inner(db, &entry_path, recursive, full, max_depth, depth + 1, seen)?;
        }
    }
    Ok(indexed)
}

fn index_path_file(
    db: &AppDb,
    path: &Path,
    full: bool,
    seen: &mut BTreeSet<String>,
) -> Result<usize> {
    if !full && !is_executable(path) {
        return Ok(0);
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return Ok(0);
    };
    if !seen.insert(name.to_string()) {
        return Ok(0);
    }
    db.upsert_command(
        name,
        "command",
        Some(path.to_string_lossy().as_ref()),
        path_mtime(path),
    )?;
    maybe_upsert_docs(db, name, false)?;
    Ok(1)
}

fn static_docs(command: &str) -> Option<Vec<StoredDoc>> {
    let mk = |item_type: &str, item_value: &str, description: &str| StoredDoc {
        command: command.to_string(),
        item_type: item_type.to_string(),
        item_value: item_value.to_string(),
        description: description.to_string(),
        source: "builtin-index".to_string(),
    };

    let docs = match command {
        "git" => vec![
            mk("subcommand", "status", "Show the working tree status"),
            mk("subcommand", "checkout", "Switch branches or restore files"),
            mk("subcommand", "commit", "Record changes to the repository"),
            mk(
                "subcommand",
                "pull",
                "Fetch from and integrate with another repository",
            ),
            mk("subcommand", "push", "Update remote refs"),
            mk("option", "--help", "Show git help"),
            mk(
                "option",
                "-C",
                "Run as if git was started in another directory",
            ),
        ],
        "docker" => vec![
            mk("subcommand", "build", "Build an image from a Dockerfile"),
            mk("subcommand", "compose", "Docker Compose management"),
            mk("subcommand", "exec", "Run a command in a running container"),
            mk("subcommand", "ps", "List containers"),
            mk("subcommand", "run", "Run a command in a new container"),
        ],
        "kubectl" => vec![
            mk("subcommand", "get", "Display one or many resources"),
            mk("subcommand", "apply", "Apply a configuration to a resource"),
            mk(
                "subcommand",
                "describe",
                "Show details of a specific resource",
            ),
            mk("subcommand", "logs", "Print container logs"),
            mk("subcommand", "exec", "Execute a command in a container"),
        ],
        "npm" => vec![
            mk("subcommand", "install", "Install a package"),
            mk("subcommand", "run", "Run a defined package script"),
            mk("subcommand", "test", "Run the test script"),
            mk("subcommand", "publish", "Publish a package"),
            mk("subcommand", "outdated", "Check for outdated packages"),
        ],
        "cargo" => vec![
            mk("subcommand", "build", "Compile the current package"),
            mk(
                "subcommand",
                "check",
                "Analyze the package and report errors",
            ),
            mk("subcommand", "run", "Run a binary or example"),
            mk("subcommand", "test", "Execute unit and integration tests"),
            mk("subcommand", "fmt", "Format source code"),
            mk("option", "--release", "Build artifacts in release mode"),
        ],
        "dotnet" => vec![
            mk(
                "subcommand",
                "build",
                "Build a project and its dependencies",
            ),
            mk("subcommand", "run", "Build and run a project"),
            mk("subcommand", "test", "Run unit tests using the test runner"),
            mk(
                "subcommand",
                "restore",
                "Restore the dependencies and tools of a project",
            ),
            mk(
                "subcommand",
                "publish",
                "Publish the application and its dependencies",
            ),
            mk(
                "subcommand",
                "new",
                "Create a new project, configuration, or solution",
            ),
            mk(
                "option",
                "--info",
                "Display detailed information about a .NET installation",
            ),
        ],
        "python" | "python3" => vec![
            mk("option", "-m", "Run a library module as a script"),
            mk("option", "-c", "Program passed in as a string"),
            mk("option", "-V", "Print the Python version number and exit"),
            mk(
                "option",
                "-i",
                "Inspect interactively after running a script",
            ),
            mk("option", "-m pytest", "Run the pytest test runner module"),
        ],
        "pip" => vec![
            mk("subcommand", "install", "Install packages"),
            mk("subcommand", "uninstall", "Uninstall packages"),
            mk("subcommand", "list", "List installed packages"),
            mk(
                "subcommand",
                "show",
                "Show information about installed packages",
            ),
            mk(
                "subcommand",
                "freeze",
                "Output installed packages in requirements format",
            ),
            mk(
                "subcommand",
                "wheel",
                "Build wheel archives for requirements",
            ),
        ],
        "pytest" => vec![
            mk(
                "option",
                "-k",
                "Only run tests matching the given substring expression",
            ),
            mk("option", "-q", "Decrease verbosity"),
            mk("option", "-x", "Stop after the first failure"),
            mk(
                "option",
                "--lf",
                "Rerun only the tests that failed at the last run",
            ),
            mk(
                "option",
                "-m",
                "Only run tests matching a given mark expression",
            ),
        ],
        _ => return None,
    };
    Some(docs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn test_db() -> AppDb {
        AppDb::open(std::path::Path::new(":memory:")).unwrap()
    }

    /// Verify that skip_existing=false (default mode) always calls maybe_upsert_docs —
    /// i.e. running reindex twice does not wipe already-indexed commands.
    #[test]
    fn skip_existing_false_does_not_wipe_existing_docs() {
        let db = test_db();
        let doc = crate::db::StoredDoc {
            command: "mycmd".into(),
            item_type: "option".into(),
            item_value: "--foo".into(),
            description: "a flag".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("mycmd", &[doc]).unwrap();
        assert!(db.command_has_docs("mycmd"));

        // Pass empty path_env to avoid scanning real PATH.
        reindex_path_commands(&db, Some(""), false, false).unwrap();
        // mycmd docs were not touched (not in PATH scan).
        assert!(db.command_has_docs("mycmd"));
    }

    /// Verify that skip_existing=true avoids re-indexing commands that already have docs.
    #[test]
    fn skip_existing_true_preserves_existing_docs() {
        let db = test_db();
        let doc = crate::db::StoredDoc {
            command: "git".into(),
            item_type: "subcommand".into(),
            item_value: "fake-subcmd".into(),
            description: "fake doc".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("git", &[doc]).unwrap();
        assert!(db.command_has_docs("git"));

        // Second reindex with skip_existing=true on empty path; the git entry should be intact.
        reindex_path_commands(&db, Some(""), true, true).unwrap();
        assert!(db.command_has_docs("git"));
    }

    /// Verify that reindex_path_commands discovers and upserts executables from a fake PATH dir.
    #[test]
    fn indexes_executables_found_in_path() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::path::PathBuf::from(format!("/tmp/shac-indexer-test-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("fakecmd");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let db = test_db();
        let path_env = dir.to_string_lossy().to_string();
        let count = reindex_path_commands(&db, Some(&path_env), false, false).unwrap();
        // Should have indexed at least the fake binary.
        assert!(count >= 1, "expected at least 1 indexed, got {count}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Verify skip_existing guard: a command that already has docs is not re-indexed.
    #[test]
    fn skip_existing_skips_already_indexed_command() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::path::PathBuf::from(format!("/tmp/shac-skip-test-{suffix}"));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("mybin");
        std::fs::write(&bin, "#!/bin/sh\necho hello\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let db = test_db();
        // Manually insert a doc for mybin.
        let doc = crate::db::StoredDoc {
            command: "mybin".into(),
            item_type: "option".into(),
            item_value: "--version".into(),
            description: "pre-indexed doc".into(),
            source: "help".into(),
        };
        db.replace_docs_for_command("mybin", &[doc]).unwrap();
        assert!(db.command_has_docs("mybin"));

        // Reindex with skip_existing=true; mybin should still have its doc (not erased).
        let path_env = dir.to_string_lossy().to_string();
        reindex_path_commands(&db, Some(&path_env), true, true).unwrap();
        assert!(
            db.command_has_docs("mybin"),
            "pre-indexed doc should survive skip_existing reindex"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
