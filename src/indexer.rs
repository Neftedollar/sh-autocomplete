use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;

use crate::db::{AppDb, StoredDoc};

const HELP_TIMEOUT: Duration = Duration::from_millis(350);
const MAN_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_DOCS_PER_COMMAND: usize = 80;
const MAX_DESCRIPTION_LEN: usize = 160;

/// Commands known to open GUI windows (Tk/Aqua/X11) instead of writing help
/// to stdout. Spawning these from the indexer pops up windows and stresses
/// the system, especially on macOS where the bg indexer can spawn many at
/// once. Skip them entirely — extracting docs requires a TTY anyway.
const GUI_APP_DENYLIST: &[&str] = &[
    "wish",
    "tkcon",
    "tclsh",
    "tkdiff",
    "gitk",
    "git-gui",
    "git-citool",
    "idle",
    "idle3",
    "idle3.10",
    "idle3.11",
    "idle3.12",
    "idle3.13",
    "osascript",
    "open",
    "Wish",
    "Wish.app",
];

/// Index PATH binaries into the `commands` table by name + path + mtime.
/// Also upserts bundled `static_docs` for the curated tool list (git, docker,
/// kubectl, ...). Never shells out to `<cmd> --help` — that path is reserved
/// for the explicit, single-command `shac index add-command <name>` flow.
pub fn reindex_path_commands(
    db: &AppDb,
    path_env: Option<&str>,
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
                    upsert_static_docs(db, &name)?;
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

/// Explicit per-command indexing: shells out `<cmd> --help` to extract
/// flags/options. The only entry point that actually invokes a subprocess.
/// Refuses GUI binaries (Tk/Aqua/AppleScript wrappers) to prevent window
/// popups; falls back to bundled `static_docs` if shellout is unsafe.
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
    match path.as_deref() {
        Some(p) if !is_gui_app(command, p) => upsert_docs_with_help_shellout(db, command)?,
        _ => upsert_static_docs(db, command)?,
    }
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

/// Returns true if the binary is a known GUI app or a shell script whose
/// shebang/exec line points at a GUI interpreter (wish, osascript, etc.).
/// Such commands open native windows when invoked with `--help`, so we
/// must skip them in the indexer to avoid window spam and system load.
fn is_gui_app(name: &str, path: &Path) -> bool {
    if GUI_APP_DENYLIST.contains(&name) {
        return true;
    }
    // Skip anything that lives inside a macOS .app bundle.
    if path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().ends_with(".app"))
    {
        return true;
    }
    // Read first 256 bytes and check for known GUI exec patterns.
    if let Ok(mut f) = fs::File::open(path) {
        let mut head = [0u8; 256];
        if let Ok(n) = f.read(&mut head) {
            let prefix = String::from_utf8_lossy(&head[..n]);
            for needle in &[
                "exec wish",
                "exec tclsh",
                "exec expect",
                "osascript",
                "/Wish.app",
            ] {
                if prefix.contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}

/// Upsert only bundled docs from `static_docs()`. No subprocess shellout;
/// safe to call for any PATH binary. Returns silently when no static docs
/// exist for the given command.
fn upsert_static_docs(db: &AppDb, command: &str) -> Result<()> {
    if let Some(docs) = static_docs(command) {
        if !docs.is_empty() {
            db.replace_docs_for_command(command, &docs)?;
        }
    }
    Ok(())
}

/// Upsert docs for `command`, preferring the cleanest source for flag/subcommand
/// completion. Caller MUST verify that `command` is not a GUI app via
/// `is_gui_app` first — this function will happily shell out to anything.
///
/// Priority: bundled `static_docs` (curated, highest quality) → `<cmd> --help`
/// (concise, structured flag list — ideal for completion) → `man` (fallback for
/// tools whose `--help` is empty or prints only to a man page). `--help` is
/// primary because its terse per-flag rows parse far more cleanly than man's
/// prose; man is the safety net, not the default.
///
/// NB: before F10 fixed the shellout pipe-buffer deadlock, `man` silently
/// timed out on any page over ~64 KiB, so `--help` was the de-facto primary
/// source regardless of ordering. Making the order explicit here keeps that
/// observed behavior stable now that `man` actually works.
fn upsert_docs_with_help_shellout(db: &AppDb, command: &str) -> Result<()> {
    let docs = static_docs(command)
        .or_else(|| parse_help_output(command))
        .or_else(|| parse_man_output(command))
        .unwrap_or_default();
    if !docs.is_empty() {
        db.replace_docs_for_command(command, &docs)?;
    }
    Ok(())
}

/// Split a help/man flags column into its individual flag tokens. Handles the
/// common shapes: `-h, --help`, `-m, --message <MSG>`, `--color=<WHEN>`,
/// `--color[=WHEN]`, `-i[SUFFIX]`, and metavars interleaved between flags
/// (`-o FILE, --output FILE`). Each flag becomes its own candidate so a user
/// typing either the short or the long form gets a prefix match.
fn flag_tokens(column: &str) -> Vec<String> {
    let mut flags = Vec::new();
    for token in column
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // A metavar (FILE, <MSG>, WHEN) between or after flags is skipped, not a
        // terminator — otherwise `-o FILE, --output FILE` would drop `--output`.
        if !token.starts_with('-') || token.len() <= 1 {
            continue;
        }
        // Strip an attached value/metavar: `--color=<WHEN>`, `--color[=WHEN]`,
        // `-i[SUFFIX]` -> `--color` / `-i`.
        let flag = token.split(['=', '[']).next().unwrap_or(token).trim();
        if flag.starts_with('-') && flag.len() > 1 {
            flags.push(flag.to_string());
        }
    }
    flags
}

/// A help line's first column names a subcommand only when it is a single
/// lowercase token (letters, digits, hyphens, underscores — e.g. openssl's
/// `s_client`). This rejects prose, `Usage:` lines, section headers and wrapped
/// descriptions that are not real subcommands.
fn is_subcommand_name(token: &str) -> bool {
    token.len() >= 2
        && token.starts_with(|c: char| c.is_ascii_lowercase())
        && token
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn parse_help_output(command: &str) -> Option<Vec<StoredDoc>> {
    let output = run_help_with_timeout(command, HELP_TIMEOUT)?;
    if !output.success && output.stdout.is_empty() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Some(parse_help_text(command, &stdout))
}

/// Pure parser for `<command> --help` output. Split from the shellout so it can
/// be unit-tested against real help text. Extracts option rows (one candidate
/// per flag) and subcommand rows (indented single-token names under a group
/// header).
fn parse_help_text(command: &str, stdout: &str) -> Vec<StoredDoc> {
    let mut docs = Vec::new();
    for line in stdout.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, "  ");
        let head = parts.next().unwrap_or("").trim();
        let description = parts.next().unwrap_or("").trim();
        if description.is_empty() {
            continue;
        }
        if head.starts_with('-') {
            // Option row: "-h, --help  Print help" -> one candidate per flag.
            for flag in flag_tokens(head) {
                docs.push(doc(command, "option", &flag, description, "help"));
                if docs.len() >= MAX_DOCS_PER_COMMAND {
                    return docs;
                }
            }
        } else if indented {
            // Subcommand row under a group header ("  install   Add ...").
            // clap groups subcommands under custom headers (Setup:, Index:, ...)
            // rather than a single "Commands:" section, so key off the row
            // shape, not the header name. Take the primary name (first token) so
            // aliased rows like cargo's "build, b   Compile ..." index as `build`.
            let name = head.split([',', ' ', '\t']).next().unwrap_or(head);
            if !is_subcommand_name(name) {
                continue;
            }
            docs.push(doc(command, "subcommand", name, description, "help"));
            if docs.len() >= MAX_DOCS_PER_COMMAND {
                return docs;
            }
        }
    }
    docs
}

fn run_help_with_timeout(command: &str, timeout: Duration) -> Option<crate::proc::CapturedOutput> {
    let mut cmd = Command::new(command);
    cmd.arg("--help")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    crate::proc::run_capture(cmd, timeout)
}

fn parse_man_output(command: &str) -> Option<Vec<StoredDoc>> {
    let mut cmd = Command::new("man");
    cmd.args(["-P", "cat", command])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("MANWIDTH", "200")
        .env("MANPAGER", "cat");
    let output = crate::proc::run_capture(cmd, MAN_TIMEOUT)?;
    if !output.success || output.stdout.is_empty() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let text = strip_man_formatting(&raw);
    parse_man_sections(&text, command)
}

fn strip_man_formatting(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i + 1] == b'\x08' {
            // overstrike bold: char, backspace, char — keep the second copy
            out.push(bytes[i + 2]);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_man_sections(text: &str, command: &str) -> Option<Vec<StoredDoc>> {
    const SECTION_HEADERS: &[&str] = &[
        "OPTIONS",
        "COMMANDS",
        "SUBCOMMANDS",
        "VERBS",
        "ACTIONS",
        "AVAILABLE COMMANDS",
        "GLOBAL OPTIONS",
        "GLOBAL FLAGS",
    ];
    let mut docs = Vec::new();
    let mut in_section = false;
    'lines: for line in text.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
            let upper = line.trim().to_uppercase();
            in_section = SECTION_HEADERS.iter().any(|&h| upper.starts_with(h));
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('-') {
            let mut parts = trimmed.splitn(2, "  ");
            if let Some(column) = parts.next().map(str::trim).filter(|s| !s.is_empty()) {
                let description = parts.next().unwrap_or("").trim();
                if !description.is_empty() {
                    for flag in flag_tokens(column) {
                        docs.push(doc(command, "option", &flag, description, "man"));
                        // Break the LINE loop (not just the flag loop) so the cap
                        // actually bounds total docs.
                        if docs.len() >= MAX_DOCS_PER_COMMAND {
                            break 'lines;
                        }
                    }
                }
            }
        }
    }
    if docs.is_empty() {
        None
    } else {
        Some(docs)
    }
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
    upsert_static_docs(db, name)?;
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

    #[test]
    fn flag_tokens_extracts_individual_flags_without_metavars() {
        assert_eq!(flag_tokens("-h, --help"), vec!["-h", "--help"]);
        assert_eq!(flag_tokens("-V, --version"), vec!["-V", "--version"]);
        // Trailing metavar never leaks into an inserted flag.
        assert_eq!(flag_tokens("-m, --message <MSG>"), vec!["-m", "--message"]);
        // A metavar BETWEEN flags (old argparse) is skipped, not a terminator:
        // the long form must survive.
        assert_eq!(
            flag_tokens("-o FILE, --output FILE"),
            vec!["-o", "--output"]
        );
        // Attached =VALUE and bracketed optional values are stripped.
        assert_eq!(flag_tokens("--color=<WHEN>"), vec!["--color"]);
        assert_eq!(flag_tokens("--color[=WHEN]"), vec!["--color"]);
        assert_eq!(flag_tokens("-i[SUFFIX]"), vec!["-i"]);
        assert_eq!(flag_tokens("--long-only"), vec!["--long-only"]);
    }

    #[test]
    fn is_subcommand_name_accepts_names_rejects_prose() {
        assert!(is_subcommand_name("install"));
        assert!(is_subcommand_name("reset-personalization"));
        assert!(is_subcommand_name("net0"));
        assert!(!is_subcommand_name("Usage:")); // header / colon / capital
        assert!(!is_subcommand_name("Print")); // capital (a wrapped desc word)
        assert!(!is_subcommand_name("--help")); // an option, not a subcommand
        assert!(!is_subcommand_name("a")); // too short
        assert!(!is_subcommand_name(""));
    }

    #[test]
    fn parse_help_text_extracts_subcommands_and_single_flags() {
        // A realistic clap grouped-help layout (as `shac --help` emits): custom
        // group headers instead of a single "Commands:" section, plus Options.
        let help = "\
Shell autocomplete engine CLI

Usage: shac <COMMAND>

Setup:
  install                Add shac integration to your shell rc file
  daemon                 Manage the background daemon

Diagnostics:
  doctor                 Check that the daemon is healthy
  reset-personalization  Clear all learned preferences
  build, b               Aliased row (cargo-style): index the primary name

Options:
  -h, --help     Print help
  -V, --version  Print version

Run 'shac help <COMMAND>' for more information on a specific command.
";
        let docs = parse_help_text("shac", help);

        let subs: Vec<&str> = docs
            .iter()
            .filter(|d| d.item_type == "subcommand")
            .map(|d| d.item_value.as_str())
            .collect();
        assert!(subs.contains(&"install"), "subcommands: {subs:?}");
        assert!(subs.contains(&"daemon"));
        assert!(subs.contains(&"doctor"));
        assert!(subs.contains(&"reset-personalization"));
        // Aliased row indexes the primary name, not the comma-joined column.
        assert!(subs.contains(&"build"), "cargo-style alias: {subs:?}");
        assert!(!subs.iter().any(|s| s.contains(',')));

        let opts: Vec<&str> = docs
            .iter()
            .filter(|d| d.item_type == "option")
            .map(|d| d.item_value.as_str())
            .collect();
        // Individual flags, never the comma-joined column.
        assert!(
            opts.contains(&"-h") && opts.contains(&"--help"),
            "opts: {opts:?}"
        );
        assert!(opts.contains(&"-V") && opts.contains(&"--version"));
        assert!(
            docs.iter().all(|d| !d.item_value.contains(", ")),
            "no candidate may carry the raw '-h, --help' column"
        );

        // Prose, usage and the footer must not become subcommands.
        assert!(!subs.contains(&"usage"));
        assert!(!subs.contains(&"run"));
        assert!(!subs.contains(&"shell"));
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
        reindex_path_commands(&db, Some(""), false).unwrap();
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
        reindex_path_commands(&db, Some(""), true).unwrap();
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
        let count = reindex_path_commands(&db, Some(&path_env), false).unwrap();
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
        reindex_path_commands(&db, Some(&path_env), true).unwrap();
        assert!(
            db.command_has_docs("mybin"),
            "pre-indexed doc should survive skip_existing reindex"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
