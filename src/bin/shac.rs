use std::fs;
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Command as ClapCmd, FromArgMatches, Subcommand, ValueEnum};
use shac::config::{AppConfig, AppPaths};
use shac::context;
use shac::engine::Engine;
use shac::indexer;
use shac::protocol::{CompletionRequest, ExplainResponse, RecordCommandRequest, SessionInfo};
use shac::quote::{quote_token, TokenContext};
use shac::shell::{Shell, BASH_COMPLETION, FISH_COMPLETION, ZSH_COMPLETION};
use shac::wire::encode_field;

const GROUPED_HELP: &str = "\
Shell autocomplete engine CLI

Usage: shac <COMMAND>

Setup:
  install                Add shac integration to your shell rc file
  uninstall              Remove shac integration from your shell rc file
  daemon                 Manage the background daemon (start / stop / restart / status)

Index:
  import                 Import command history from zsh history or zoxide
  scan-projects          Scan directories and index project paths for path completions
  reindex                Re-scan PATH commands and rebuild documentation index
  index                  Add a specific command or directory path to the index
  invalidate-caches      Clear all cached completion results

Diagnostics:
  doctor                 Check that the daemon, shell integration, and index are healthy
  explain                Explain why candidates ranked the way they did for a query
  stats                  Show usage statistics (completions accepted, telemetry retention, etc.)
  recent-events          Show recent completion and acceptance events
  debug                  Low-level debug tools (show raw completion results)

Personalization:
  reset-personalization  Clear all learned preferences and start personalization from scratch

Settings:
  config                 View or edit configuration settings
  locale                 View or change the UI language / locale
  tips                   Manage inline usage tips (list / mute / unmute)

Options:
  -h, --help     Print help
  -V, --version  Print version

Run 'shac help <COMMAND>' for more information on a specific command.";

fn build_app() -> ClapCmd {
    ClapCmd::new("shac")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Shell autocomplete engine CLI")
        .override_help(GROUPED_HELP)
        .arg_required_else_help(true)
        // ── Setup ────────────────────────────────────────────────────────────
        .next_help_heading("Setup")
        .subcommand(InstallArgs::augment_args(
            ClapCmd::new("install").about("Add shac integration to your shell rc file"),
        ))
        .subcommand(InstallArgs::augment_args(
            ClapCmd::new("uninstall").about("Remove shac integration from your shell rc file"),
        ))
        .subcommand(DaemonArgs::augment_args(ClapCmd::new("daemon").about(
            "Manage the background daemon (start / stop / restart / status)",
        )))
        // ── Index ────────────────────────────────────────────────────────────
        .next_help_heading("Index")
        .subcommand(ImportArgs::augment_args(
            ClapCmd::new("import").about("Import command history from zsh history or zoxide"),
        ))
        .subcommand(ScanProjectsArgs::augment_args(
            ClapCmd::new("scan-projects")
                .about("Scan directories and index project paths for path completions"),
        ))
        .subcommand(ReindexArgs::augment_args(
            ClapCmd::new("reindex").about("Re-scan PATH commands and rebuild documentation index"),
        ))
        .subcommand(IndexArgs::augment_args(
            ClapCmd::new("index").about("Add a specific command or directory path to the index"),
        ))
        .subcommand(ClapCmd::new("invalidate-caches").about("Clear all cached completion results"))
        // ── Diagnostics ──────────────────────────────────────────────────────
        .next_help_heading("Diagnostics")
        .subcommand(DoctorArgs::augment_args(ClapCmd::new("doctor").about(
            "Check that the daemon, shell integration, and index are healthy",
        )))
        .subcommand(CompletionArgs::augment_args(ClapCmd::new("explain").about(
            "Explain why candidates ranked the way they did for a query",
        )))
        .subcommand(
            ClapCmd::new("stats")
                .about("Show usage statistics (completions accepted, telemetry retention, etc.)"),
        )
        .subcommand(RecentEventsArgs::augment_args(
            ClapCmd::new("recent-events").about("Show recent completion and acceptance events"),
        ))
        .subcommand(DebugArgs::augment_args(
            ClapCmd::new("debug").about("Low-level debug tools (show raw completion results)"),
        ))
        // ── Personalization ───────────────────────────────────────────────────
        .next_help_heading("Personalization")
        .subcommand(
            ClapCmd::new("reset-personalization")
                .about("Clear all learned preferences and start personalization from scratch"),
        )
        // ── Settings ─────────────────────────────────────────────────────────
        .next_help_heading("Settings")
        .subcommand(ConfigArgs::augment_args(
            ClapCmd::new("config").about("View or edit configuration settings"),
        ))
        .subcommand(LocaleArgs::augment_args(
            ClapCmd::new("locale").about("View or change the UI language / locale"),
        ))
        .subcommand(TipsArgs::augment_args(
            ClapCmd::new("tips").about("Manage inline usage tips (list / mute / unmute)"),
        ))
        // ── Internal (shell scripts only) ─────────────────────────────────────
        .subcommand(CompletionArgs::augment_args(
            ClapCmd::new("complete").hide(true),
        ))
        .subcommand(RecordArgs::augment_args(
            ClapCmd::new("record-command").hide(true),
        ))
        .subcommand(ShellEnvArgs::augment_args(
            ClapCmd::new("shell-env").hide(true),
        ))
        .subcommand(SuggestArgs::augment_args(
            ClapCmd::new("suggest").hide(true),
        ))
        .subcommand(ClapCmd::new("migration-status").hide(true))
}

#[derive(Debug, Args)]
struct LocaleArgs {
    #[command(subcommand)]
    action: LocaleAction,
}

#[derive(Debug, Subcommand)]
enum LocaleAction {
    List,
    Current,
    Set {
        #[arg(value_name = "LANG")]
        lang: Option<String>,
        #[arg(long)]
        unset: bool,
    },
    DumpKeys {
        #[arg(long)]
        missing: Option<String>,
    },
}

#[derive(Debug, Args)]
struct TipsArgs {
    #[command(subcommand)]
    action: TipsAction,
}

#[derive(Debug, Subcommand)]
enum TipsAction {
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        muted: bool,
    },
    Mute {
        id: String,
    },
    Unmute {
        id: String,
    },
    Reset {
        #[arg(long)]
        hard: bool,
    },
}

#[derive(Debug, Args)]
struct ReindexArgs {
    /// Re-process every PATH command, including those already indexed.
    /// Default is to skip commands that already have docs.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(long, value_enum)]
    shell: ShellKind,
    #[arg(long)]
    edit_rc: bool,
    #[arg(long)]
    no_import: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[command(subcommand)]
    action: ImportAction,
}

#[derive(Debug, Subcommand)]
enum ImportAction {
    ZshHistory {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    Zoxide {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    All {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
struct ScanProjectsArgs {
    #[arg(long)]
    root: Vec<String>,
    #[arg(long, default_value_t = 3)]
    depth: usize,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, value_enum)]
    shell: Option<ShellKind>,
}

#[derive(Debug, Args)]
struct DebugArgs {
    #[command(subcommand)]
    action: DebugAction,
}

#[derive(Debug, Subcommand)]
enum DebugAction {
    Completion(CompletionArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ShellKind {
    Bash,
    Fish,
    Zsh,
}

const SHAC_RC_BEGIN: &str = "# >>> shac initialize >>>";
const SHAC_RC_END: &str = "# <<< shac initialize <<<";

#[derive(Debug, Args)]
struct DaemonArgs {
    #[command(subcommand)]
    action: DaemonAction,
}

#[derive(Debug, Args)]
struct IndexArgs {
    #[command(subcommand)]
    action: IndexAction,
}

#[derive(Debug, Subcommand)]
enum IndexAction {
    /// Index a command's flags and subcommands from its `--help` output
    AddCommand {
        /// Command name to index (e.g. git, cargo, docker)
        cmd: String,
    },
    /// Index a directory's paths so they complete for `cd` and friends
    AddPath {
        /// Directory to index
        path: String,
        /// Also index one level of subdirectories
        #[arg(long)]
        subpath: bool,
        /// Index the full recursive tree (may be slow for large trees)
        #[arg(long)]
        full: bool,
        /// Recursion depth when indexing subdirectories (0 = unlimited)
        #[arg(long, default_value_t = 0)]
        deep: usize,
    },
    /// Show what has been indexed (commands, paths, docs)
    Status,
}

#[derive(Debug, Subcommand)]
enum DaemonAction {
    /// Start the background daemon
    Start,
    /// Stop the running daemon
    Stop,
    /// Restart the daemon (reload the current binary)
    Restart,
    /// Show whether the daemon is running
    Status,
}

#[derive(Debug, Args)]
struct CompletionArgs {
    #[arg(long)]
    shell: String,
    #[arg(long)]
    line: String,
    #[arg(long)]
    cursor: usize,
    #[arg(long, default_value = ".")]
    cwd: String,
    #[arg(long)]
    prev_command: Option<String>,
    #[arg(long = "history-command")]
    history_commands: Vec<String>,
    #[arg(long, default_value = "shell-words")]
    format: String,
}

#[derive(Debug, Args)]
struct RecordArgs {
    #[arg(long)]
    command: String,
    #[arg(long, default_value = ".")]
    cwd: String,
    #[arg(long)]
    shell: Option<String>,
    #[arg(long)]
    trust: Option<String>,
    #[arg(long)]
    provenance: Option<String>,
    #[arg(long)]
    provenance_source: Option<String>,
    #[arg(long)]
    provenance_confidence: Option<String>,
    #[arg(long)]
    origin: Option<String>,
    #[arg(long)]
    tty_present: bool,
    #[arg(long)]
    exit_status: Option<i32>,
    #[arg(long)]
    accepted_request_id: Option<i64>,
    #[arg(long)]
    accepted_item_key: Option<String>,
    #[arg(long)]
    accepted_rank: Option<i64>,
}

#[derive(Debug, Args)]
struct RecentEventsArgs {
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Debug, Args)]
struct ShellEnvArgs {
    #[arg(long, value_enum)]
    shell: ShellKind,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    action: ConfigAction,
}

#[derive(Debug, Subcommand)]
enum ConfigAction {
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Debug, Args)]
struct SuggestArgs {
    #[arg(long, default_value = ".")]
    cwd: String,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}

fn main() -> Result<()> {
    let matches = build_app().get_matches();
    let paths = AppPaths::discover()?;
    paths.ensure()?;

    match matches.subcommand() {
        Some(("install", sub)) => install(
            &paths,
            InstallArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("uninstall", sub)) => {
            let a = InstallArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit());
            uninstall(&paths, a.shell, a.edit_rc)
        }
        Some(("daemon", sub)) => daemon_action(
            &paths,
            DaemonArgs::from_arg_matches(sub)
                .unwrap_or_else(|e| e.exit())
                .action,
        ),
        Some(("import", sub)) => import_action(
            &paths,
            ImportArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("scan-projects", sub)) => scan_projects_action(
            &paths,
            ScanProjectsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("reindex", sub)) => {
            let a = ReindexArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit());
            ensure_daemon(&paths)?;
            let value = send_request(
                &paths,
                "reindex",
                serde_json::json!({ "path_env": std::env::var("PATH").ok(), "skip_existing": !a.all }),
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Some(("index", sub)) => index_action(
            &paths,
            IndexArgs::from_arg_matches(sub)
                .unwrap_or_else(|e| e.exit())
                .action,
        ),
        Some(("invalidate-caches", _)) => {
            ensure_daemon(&paths)?;
            let resp = send_request(&paths, "invalidate-caches", serde_json::json!({}))?;
            if let Some(err) = resp.get("error").and_then(|e| e.as_str()) {
                bail!("daemon error: {err}");
            }
            println!("caches invalidated");
            Ok(())
        }
        Some(("doctor", sub)) => doctor(
            &paths,
            DoctorArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("explain", sub)) => explain(
            &paths,
            CompletionArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("stats", _)) => {
            ensure_daemon(&paths)?;
            let value = send_request(&paths, "stats", serde_json::json!({}))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Some(("recent-events", sub)) => recent_events(
            &paths,
            RecentEventsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("debug", sub)) => debug_action(
            &paths,
            DebugArgs::from_arg_matches(sub)
                .unwrap_or_else(|e| e.exit())
                .action,
        ),
        Some(("reset-personalization", _)) => reset_personalization(&paths),
        Some(("config", sub)) => config_action(
            &paths,
            ConfigArgs::from_arg_matches(sub)
                .unwrap_or_else(|e| e.exit())
                .action,
        ),
        Some(("locale", sub)) => run_locale(
            &paths,
            LocaleArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("tips", sub)) => run_tips(
            &paths,
            TipsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("complete", sub)) => complete(
            &paths,
            CompletionArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("record-command", sub)) => {
            // Honor the kill-switch client-side: when shac is disabled, record
            // nothing and don't even `ensure_daemon` (starting a daemon just to
            // drop the record). The daemon caches `enabled` at startup, so
            // relying on its gate alone left `config set enabled false`
            // ineffective on a running daemon until restart — while completions
            // went quiet client-side, so the user reasonably believed shac was
            // off yet recording continued (review P1).
            if shac_disabled(&paths)? {
                return Ok(());
            }
            ensure_daemon(&paths)?;
            let a = RecordArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit());
            send_request(
                &paths,
                "record-command",
                serde_json::to_value(RecordCommandRequest {
                    command: a.command,
                    cwd: canonicalize_lossy(&a.cwd),
                    shell: a.shell,
                    trust: a.trust,
                    provenance: a.provenance,
                    provenance_source: a.provenance_source,
                    provenance_confidence: a.provenance_confidence,
                    origin: a.origin,
                    tty_present: Some(a.tty_present || std::io::stdin().is_terminal()),
                    exit_status: a.exit_status,
                    accepted_request_id: a.accepted_request_id,
                    accepted_item_key: a.accepted_item_key,
                    accepted_rank: a.accepted_rank,
                })?,
            )?;
            Ok(())
        }
        Some(("shell-env", sub)) => shell_env(
            &paths,
            ShellEnvArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("suggest", sub)) => run_suggest(
            &paths,
            SuggestArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()),
        ),
        Some(("migration-status", _)) => migration_status(&paths),
        _ => {
            build_app().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn run_suggest(paths: &AppPaths, args: SuggestArgs) -> Result<()> {
    let cwd = std::path::PathBuf::from(&args.cwd)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&args.cwd));
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let cfg = AppConfig::load(paths).unwrap_or_default();

    let input = shac::suggest::SuggestInput {
        cwd: &cwd,
        home: &home,
        config_dir: &paths.config_dir,
        config: &cfg,
        all: args.all,
        accepted_sources_recent: std::collections::HashSet::new(),
    };
    let output = shac::suggest::run(&input)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print!("{}", shac::suggest::render_text(&output));
    }
    Ok(())
}

fn run_locale(paths: &AppPaths, args: LocaleArgs) -> Result<()> {
    use shac::i18n::{resolve_locale, Catalog};
    match args.action {
        LocaleAction::List => {
            println!("en  (bundled)");
            for lang in Catalog::user_locale_files(&paths.config_dir) {
                println!("{lang}  (user)");
            }
            Ok(())
        }
        LocaleAction::Current => {
            let cfg = AppConfig::load(paths)?;
            let resolved = resolve_locale(
                std::env::var("SHAC_LOCALE").ok(),
                Some(cfg.ui.locale),
                std::env::var("LC_MESSAGES").ok(),
                std::env::var("LANG").ok(),
            );
            let source_label = match resolved.source {
                shac::i18n::LocaleSource::Env => "SHAC_LOCALE env",
                shac::i18n::LocaleSource::Config => "ui.locale config",
                shac::i18n::LocaleSource::AutoLcMessages => "LC_MESSAGES env",
                shac::i18n::LocaleSource::AutoLang => "LANG env",
                shac::i18n::LocaleSource::Default => "default (en)",
            };
            println!("{} (source: {source_label})", resolved.lang);
            Ok(())
        }
        LocaleAction::Set { lang, unset } => {
            let mut cfg = AppConfig::load(paths)?;
            if unset {
                cfg.ui.locale = String::new();
                cfg.save(paths)?;
                println!("ui.locale unset (back to auto-detect)");
            } else {
                let lang = lang.context("locale required unless --unset")?;
                cfg.ui.locale = lang.clone();
                cfg.save(paths)?;
                println!("ui.locale = {lang}");
            }
            Ok(())
        }
        LocaleAction::DumpKeys { missing } => {
            // For --missing <target>, build the catalog around <target> so the
            // user's <target>.toml is merged. Otherwise resolve the active
            // locale (only matters for the no-missing path which lists en keys).
            let cfg = AppConfig::load(paths)?;
            let lang = if let Some(target) = &missing {
                target.clone()
            } else {
                resolve_locale(
                    std::env::var("SHAC_LOCALE").ok(),
                    Some(cfg.ui.locale),
                    std::env::var("LC_MESSAGES").ok(),
                    std::env::var("LANG").ok(),
                )
                .lang
            };
            let catalog = Catalog::build(&paths.config_dir, &lang);
            if let Some(target) = missing {
                for k in catalog.missing_keys(&target) {
                    println!("{k}");
                }
            } else {
                for k in catalog.known_keys() {
                    println!("{k}");
                }
            }
            Ok(())
        }
    }
}

fn run_tips(paths: &AppPaths, args: TipsArgs) -> Result<()> {
    let db = shac::db::AppDb::open(&paths.db_file)
        .with_context(|| format!("open db at {:?}", paths.db_file))?;
    let conn = db.connection();
    match args.action {
        TipsAction::List { all, muted } => {
            let state = shac::tips::storage::load_all(conn)?;
            let catalog = shac::tips::catalog();
            for tip in catalog {
                let s = state.get(tip.id);
                let is_muted = s.map(|x| x.muted).unwrap_or(false);
                let count = s.map(|x| x.shows_count).unwrap_or(0);
                if muted && !is_muted {
                    continue;
                }
                if !all && !muted && count == 0 && !is_muted {
                    continue;
                }
                let status = if is_muted { "muted" } else { "active" };
                println!(
                    "{:30} {:11} shows={}/{}",
                    tip.id, status, count, tip.max_shows
                );
            }
            Ok(())
        }
        TipsAction::Mute { id } => {
            let now = unix_now_secs();
            shac::tips::storage::mute(conn, &id, now)?;
            println!("muted: {id}");
            Ok(())
        }
        TipsAction::Unmute { id } => {
            shac::tips::storage::unmute(conn, &id)?;
            println!("unmuted: {id}");
            Ok(())
        }
        TipsAction::Reset { hard } => {
            shac::tips::storage::reset(conn, hard)?;
            println!(
                "{}",
                if hard {
                    "tips state reset (hard)"
                } else {
                    "tips state reset (soft)"
                }
            );
            Ok(())
        }
    }
}

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn index_action(paths: &AppPaths, action: IndexAction) -> Result<()> {
    let engine = Engine::new(paths)?;
    match action {
        IndexAction::AddCommand { cmd } => {
            let indexed =
                indexer::index_command(engine.db(), &cmd, std::env::var("PATH").ok().as_deref())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "indexed_commands": indexed,
                    "target": {
                        "type": "command",
                        "value": cmd
                    }
                }))?
            );
            Ok(())
        }
        IndexAction::AddPath {
            path,
            subpath,
            full,
            deep,
        } => {
            let indexed = indexer::index_path_target(
                engine.db(),
                &PathBuf::from(&path),
                subpath,
                full,
                deep,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "indexed_commands": indexed,
                    "target": {
                        "type": "path",
                        "value": path,
                        "subpath": subpath,
                        "full": full,
                        "deep": deep
                    }
                }))?
            );
            Ok(())
        }
        IndexAction::Status => {
            let targets = engine
                .db()
                .list_index_targets()?
                .into_iter()
                .map(|target| {
                    serde_json::json!({
                        "id": target.id,
                        "type": target.target_type,
                        "value": target.value,
                        "subpath": target.recursive,
                        "full": target.full,
                        "deep": target.max_depth,
                        "created_ts": target.created_ts,
                        "last_indexed_ts": target.last_indexed_ts
                    })
                })
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string_pretty(&targets)?);
            Ok(())
        }
    }
}

fn learning_status_check(paths: &AppPaths) -> serde_json::Value {
    let accepted = shac::db::AppDb::open(&paths.db_file)
        .and_then(|db| db.stats())
        .map(|s| s.accepted_clean_completions)
        .unwrap_or(0);
    let (ok, detail) = if accepted == 0 {
        (
            false,
            "no accepted completions yet — press Tab a few times to start learning".to_string(),
        )
    } else {
        (true, format!("{accepted} accepted completions recorded"))
    };
    doctor_check("learning_status", ok, detail)
}

/// Cold-start checks (PLAN §7.12) surface the telemetry collected during
/// `shac install` so users can confirm their first-run import paid off:
///
/// - `cold_start_paths`: how many rows are in `paths_index` (zsh history
///   replay + zoxide + project scan combined). Zero is a red flag — likely
///   the user ran `--no-import` or all sources were missing.
/// - `cold_start_history`: imported zsh history events count + import
///   coverage percent (imported / total history rows).
/// - `time_to_first_accept`: seconds between `install` and the first
///   accepted completion. Surfaced as informational once available.
fn cold_start_checks(paths: &AppPaths) -> Vec<serde_json::Value> {
    let stats = match shac::db::AppDb::open(&paths.db_file).and_then(|db| db.stats()) {
        Ok(s) => s,
        Err(err) => {
            return vec![doctor_check(
                "cold_start_telemetry",
                false,
                format!("could not open db: {err:#}"),
            )];
        }
    };

    let mut checks = Vec::with_capacity(3);

    let paths_ok = stats.paths_index_rows > 0;
    let paths_detail = format!(
        "{} entries (cwd_event + zoxide + project_scan)",
        stats.paths_index_rows
    );
    checks.push(doctor_check("cold_start_paths", paths_ok, paths_detail));

    let history_ok = stats.imported_history_events > 0;
    let history_detail = format!(
        "{} imported events ({:.1}% of history)",
        stats.imported_history_events, stats.import_coverage_pct
    );
    checks.push(doctor_check(
        "cold_start_history",
        history_ok,
        history_detail,
    ));

    let (ttfa_ok, ttfa_detail) = match stats.time_to_first_accept_seconds {
        Some(secs) if secs >= 0 => (true, format!("{secs}s")),
        Some(_) => (true, "negative — clock skew?".to_string()),
        None => (
            false,
            "not yet — press Tab to accept a completion".to_string(),
        ),
    };
    checks.push(doctor_check("time_to_first_accept", ttfa_ok, ttfa_detail));

    checks
}

fn doctor(paths: &AppPaths, args: DoctorArgs) -> Result<()> {
    cleanup_stale_daemon_state(paths);
    let config = AppConfig::load(paths).unwrap_or_default();
    let mut checks = vec![
        doctor_check(
            "config_file",
            paths.config_file.exists(),
            paths.config_file.display().to_string(),
        ),
        doctor_check(
            "db_file",
            paths.db_file.exists(),
            paths.db_file.display().to_string(),
        ),
        doctor_check(
            "socket",
            paths.socket_file.exists(),
            paths.socket_file.display().to_string(),
        ),
        doctor_check("pid_file", paths.pid_file.exists(), pid_file_detail(paths)),
        doctor_check(
            "daemon_running",
            daemon_is_running(paths),
            daemon_detail(paths),
        ),
        daemon_version_check(paths),
        doctor_check(
            "zsh_adapter",
            paths.shell_dir.join("shac.zsh").exists(),
            paths.shell_dir.join("shac.zsh").display().to_string(),
        ),
        adapter_currency_check(paths),
        doctor_check(
            "bash_adapter",
            paths.shell_dir.join("shac.bash").exists(),
            paths.shell_dir.join("shac.bash").display().to_string(),
        ),
        doctor_check(
            "enabled_config",
            config.enabled,
            "config enabled".to_string(),
        ),
        doctor_check(
            "enabled_env",
            std::env::var_os("SHAC_DISABLE").is_none(),
            "SHAC_DISABLE unset".to_string(),
        ),
        doctor_check(
            "active_shac",
            true,
            std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|err| err.to_string()),
        ),
        doctor_check(
            "shacd_binary",
            daemon_binary_path().is_ok(),
            daemon_binary_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|err| err.to_string()),
        ),
        doctor_check(
            "daemon_timeout_ms",
            config.daemon_timeout_ms >= 20,
            config.daemon_timeout_ms.to_string(),
        ),
        doctor_check("zsh_menu_detail", true, config.ui.zsh.menu_detail.clone()),
        doctor_check(
            "zsh_menu_metadata",
            true,
            format!(
                "kind={} source={} description={} max_items={} width={}",
                config.ui.zsh.show_kind,
                config.ui.zsh.show_source,
                config.ui.zsh.show_description,
                config.ui.zsh.max_items,
                config.ui.zsh.max_description_width
            ),
        ),
    ];
    checks.push(learning_status_check(paths));
    checks.extend(cold_start_checks(paths));
    if matches!(args.shell, Some(ShellKind::Zsh)) {
        checks.extend(zsh_doctor_checks(paths)?);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in checks {
            println!(
                "{:<22} {:<4} {}",
                check["name"].as_str().unwrap_or_default(),
                if check["ok"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "fail"
                },
                check["detail"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(())
}

fn pid_file_detail(paths: &AppPaths) -> String {
    match fs::read_to_string(&paths.pid_file) {
        Ok(pid) => format!("{} pid={}", paths.pid_file.display(), pid.trim()),
        Err(_) => paths.pid_file.display().to_string(),
    }
}

fn daemon_detail(paths: &AppPaths) -> String {
    let pid = fs::read_to_string(&paths.pid_file)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    format!("socket={} pid={pid}", paths.socket_file.display())
}

/// Doctor check: the running daemon must be the same version as this client
/// binary. A `brew upgrade` swaps the on-disk binary but leaves the old
/// long-running daemon in memory, so completions are served by stale code with
/// no visible symptom until behavior drifts. The daemon reports its version in
/// every `complete` response (since the earliest released daemon), so a live
/// probe catches the skew regardless of shell state.
fn daemon_version_check(paths: &AppPaths) -> serde_json::Value {
    let client = env!("CARGO_PKG_VERSION");
    if !daemon_is_running(paths) {
        return doctor_check(
            "daemon_version",
            true,
            format!("client v{client}; daemon not running"),
        );
    }
    let daemon = probe_daemon_version(paths);
    let (ok, detail) = version_skew_detail(client, &daemon);
    doctor_check("daemon_version", ok, detail)
}

/// Compare the client binary version to the running daemon's reported version.
/// Split from I/O so the messaging is unit-testable. Only a *confirmed*
/// mismatch is a failure; an unreadable version (probe error/timeout) stays OK
/// so a transient hiccup doesn't cry wolf — `daemon_running` already covers
/// connectivity.
/// Outcome of probing the running daemon for its version.
enum DaemonVersionProbe {
    /// The daemon reported its version.
    Version(String),
    /// The daemon answered but the response carried no `daemon_version` field —
    /// definitionally a pre-0.5.2 daemon (the field has shipped since v0.5.2),
    /// i.e. exactly the stale daemon this check exists to catch.
    MissingField,
    /// Could not reach or decode the daemon (connect, timeout, or parse error).
    Unreachable,
}

fn version_skew_detail(client: &str, daemon: &DaemonVersionProbe) -> (bool, String) {
    match daemon {
        DaemonVersionProbe::Version(d) if d == client => {
            (true, format!("client and daemon both v{client}"))
        }
        DaemonVersionProbe::Version(d) => (
            false,
            format!(
                "daemon v{d} still running but client is v{client} — run `shac daemon restart` \
                 (`brew upgrade` leaves the old daemon in memory)"
            ),
        ),
        DaemonVersionProbe::MissingField => (
            false,
            format!(
                "a pre-0.5.2 daemon is still running (client is v{client}) — run \
                 `shac daemon restart`"
            ),
        ),
        DaemonVersionProbe::Unreachable => (
            true,
            format!("client v{client}; could not read running daemon version"),
        ),
    }
}

/// Ask the running daemon for its version via a minimal read-only `complete`
/// probe (the only action whose response carries `daemon_version`). Distinguishes
/// a genuinely absent version field (old daemon) from an unreachable daemon.
fn probe_daemon_version(paths: &AppPaths) -> DaemonVersionProbe {
    let mut env = std::collections::HashMap::new();
    // Keep the probe read-only: SHAC_NO_TIPS stops the daemon claiming the
    // one-shot first-run greeter or advancing tip-cooldown state for this
    // throwaway request (maybe_pick_tip gates on it).
    env.insert("SHAC_NO_TIPS".to_string(), "1".to_string());
    let req = CompletionRequest {
        shell: "zsh".to_string(),
        line: String::new(),
        cursor: 0,
        cwd: ".".to_string(),
        env,
        session: SessionInfo {
            tty: None,
            pid: None,
        },
        history_hint: shac::protocol::HistoryHint {
            prev_command: None,
            runtime_commands: Vec::new(),
        },
    };
    let payload = match serde_json::to_value(req) {
        Ok(payload) => payload,
        Err(_) => return DaemonVersionProbe::Unreachable,
    };
    // A generous one-shot timeout: unlike a live completion this is a manual
    // diagnostic, and the production `daemon_timeout_ms` (tens of ms) is too
    // tight for the empty-line probe, which triggers a full candidate compute.
    let response =
        match send_request_with_timeout(paths, "complete", payload, Duration::from_millis(1000)) {
            Ok(response) => response,
            Err(_) => return DaemonVersionProbe::Unreachable,
        };
    match response
        .get("daemon_version")
        .and_then(|value| value.as_str())
    {
        Some(version) => DaemonVersionProbe::Version(version.to_string()),
        None => DaemonVersionProbe::MissingField,
    }
}

fn zsh_doctor_checks(paths: &AppPaths) -> Result<Vec<serde_json::Value>> {
    let script = paths.shell_dir.join("shac.zsh");
    let mut checks = Vec::new();
    checks.push(doctor_check(
        "zsh_adapter_version",
        adapter_contains_owned_widget(&script),
        "owned-widget-v1".to_string(),
    ));

    if !command_available("zsh") {
        checks.push(doctor_check(
            "zsh_binding_smoke",
            false,
            "zsh not found".to_string(),
        ));
        return Ok(checks);
    }

    let smoke = Command::new("zsh")
        .arg("-fic")
        .arg(format!(
            "source {}; print -r -- \"tab=$(bindkey '^I') space=$(bindkey ' ') ctrl_f=$(bindkey '^F') fn_tab=${{+functions[_shac_tab_widget]}} fn_space=${{+functions[_shac_space_widget]}} detail=${{_shac_ui_menu_detail:-}}\"",
            shell_escape(&script.to_string_lossy())
        ))
        .output()
        .context("run zsh binding smoke")?;
    let stdout = String::from_utf8_lossy(&smoke.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&smoke.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        stdout.clone()
    } else {
        format!("{stdout} stderr={stderr}")
    };
    checks.push(doctor_check(
        "zsh_binding_smoke",
        smoke.status.success()
            && stdout.contains("_shac_tab_widget")
            && stdout.contains("_shac_space_widget")
            && stdout.contains("_shac_forward_char_widget")
            && stdout.contains("fn_tab=1")
            && stdout.contains("fn_space=1"),
        detail,
    ));
    Ok(checks)
}

fn adapter_contains_owned_widget(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| {
            content.contains("_shac_tab_widget") && content.contains("_shac_space_widget")
        })
        .unwrap_or(false)
}

/// Doctor check: the installed zsh adapter must match the one embedded in this
/// binary. `brew upgrade` refreshes the binary but not the adapter that
/// `shac install` wrote to the config dir, so a stale adapter can mis-parse a
/// newer binary's output (e.g. render an unrecognized control line as a blank
/// phantom candidate). Only a present-but-outdated adapter fails; a missing one
/// is left to the `zsh_adapter` existence check.
fn adapter_currency_check(paths: &AppPaths) -> serde_json::Value {
    let path = paths.shell_dir.join("shac.zsh");
    if !path.exists() {
        return doctor_check(
            "zsh_adapter_current",
            true,
            "no zsh adapter installed".to_string(),
        );
    }
    let current = fs::read_to_string(&path)
        .map(|content| content == ZSH_COMPLETION)
        .unwrap_or(false);
    let detail = if current {
        "matches installed binary".to_string()
    } else {
        "stale — run `shac install --shell zsh` (a `brew upgrade` does not refresh the adapter)"
            .to_string()
    };
    doctor_check("zsh_adapter_current", current, detail)
}

fn command_available(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!(
            "command -v {} >/dev/null 2>&1",
            shell_escape(command)
        ))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn doctor_check(name: &str, ok: bool, detail: String) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "ok": ok,
        "detail": detail
    })
}

fn debug_action(paths: &AppPaths, action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Completion(args) => debug_completion(paths, args),
    }
}

fn debug_completion(paths: &AppPaths, args: CompletionArgs) -> Result<()> {
    let request = completion_request(&args);
    let completion = if shac_disabled(paths)? {
        disabled_completion_response()
    } else {
        ensure_daemon(paths)?;
        send_request(paths, "complete", serde_json::to_value(&request)?)?
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "request": request,
            "response": completion,
            "daemon_running": daemon_is_running(paths),
            "disabled": shac_disabled(paths)?
        }))?
    );
    Ok(())
}

fn install(paths: &AppPaths, args: InstallArgs) -> Result<()> {
    let shell = args.shell;
    let edit_rc = args.edit_rc;
    let (file_name, content, snippet) = match shell {
        ShellKind::Bash => (
            "shac.bash",
            BASH_COMPLETION,
            format!("source {}", paths.shell_dir.join("shac.bash").display()),
        ),
        ShellKind::Fish => (
            "shac.fish",
            FISH_COMPLETION,
            format!("source {}", paths.shell_dir.join("shac.fish").display()),
        ),
        ShellKind::Zsh => (
            "shac.zsh",
            ZSH_COMPLETION,
            format!("source {}", paths.shell_dir.join("shac.zsh").display()),
        ),
    };
    let shell_file = paths.shell_dir.join(file_name);
    fs::write(&shell_file, content)?;
    if edit_rc {
        let rc_file = rc_file_for_shell(shell)?;
        let shell_label = shell_kind_to_import(shell).label();
        let rc_display = rc_file.display().to_string();

        // Attempt the rc-block edit and capture a serialised error string so
        // it can be surfaced through print_step's UX *and* propagated to the
        // caller.  A failed rc write means the shell is NOT hooked, so we
        // must exit non-zero and skip the success-style next-steps banner.
        let rc_err: Option<String> = match install_rc_block(shell, &shell_file) {
            Ok(()) => None,
            Err(e) => Some(format!("{e:#}")),
        };
        print_step(
            &format!("Hooking shac into {shell_label}"),
            || -> Result<String> {
                match rc_err {
                    None => Ok(rc_display.clone()),
                    Some(ref msg) => Err(anyhow::anyhow!("{msg}")),
                }
            },
        );
        // Propagate the failure so `shac install` exits non-zero and the
        // caller does not see the success-style next-steps.
        if let Some(msg) = rc_err {
            anyhow::bail!(
                "failed to update rc file {rc_display}: {msg}\n\
                 Add the following line to {rc_display} manually:\n  \
                 source {shell_file}",
                shell_file = shell_file.display()
            );
        }

        // Open the DB once for both the import flow and the prior seeder.
        // We seed priors regardless of `--no-import` because they're a
        // bundled corpus, not a per-user import — without them the
        // cold-start menu collapses to alphabetical command names.
        let db = shac::db::AppDb::open(&paths.db_file)?;

        if !args.no_import {
            let opts = shac::import::ImportOpts {
                yes: args.yes,
                roots: shac::import::default_project_roots(),
                depth: 3,
                shell: shell_kind_to_import(shell),
                history_path: None,
                zoxide_path: None,
            };
            match shac::import::run_full_import(&db, opts) {
                Ok(summaries) => print_first_run_summary(&summaries),
                Err(err) => eprintln!("shac: import failed: {err:#}"),
            }
        }

        // Detect installed CLIs so we only seed priors for tools the user can
        // actually run. Commands not found on PATH (kubectl, docker, dotnet…)
        // produce noise in completion menus on machines that don't have them.
        let detection = shac::tools::detect_tools();
        let n_detected = detection.installed.len();
        match shac::priors::seed_priors_into_docs_filtered(&db, &detection) {
            Ok(seeded) => print_priors_seeded_line(n_detected, seeded),
            Err(err) => eprintln!("shac: priors seeding failed: {err:#}"),
        }

        println!();
        println!("Try: cd <Tab>");
        println!("  Run `shac doctor` if Tab feels off.");
        println!("  Run `shac stats` to see what was learned.");
        println!("  (Open a new shell or run `source {rc_display}` to activate.)");
    } else {
        println!("{snippet}");
    }
    Ok(())
}

fn shell_kind_to_import(shell: ShellKind) -> shac::import::ShellKind {
    match shell {
        ShellKind::Bash => shac::import::ShellKind::Bash,
        ShellKind::Fish => shac::import::ShellKind::Fish,
        ShellKind::Zsh => shac::import::ShellKind::Zsh,
    }
}

/// First-run UX printer: render polished per-source output for the install
/// flow's import results, mirroring the spec in PLAN §7.1.
///
/// Each summary maps to one line:
///
/// `✓ Importing zsh history... (12,847 entries)     [1.8s]`
///
/// When stdout is not a TTY (CI logs, redirected output), we fall back to a
/// plain colorless render and skip ANSI escape sequences.
fn print_first_run_summary(summaries: &[shac::import::ImportSummary]) {
    let tty = std::io::stdout().is_terminal();
    let check = if tty {
        "\x1b[32m\u{2713}\x1b[0m"
    } else {
        "\u{2713}"
    };
    let dim_open = if tty { "\x1b[2m" } else { "" };
    let dim_close = if tty { "\x1b[0m" } else { "" };

    for s in summaries {
        let (label, detail) = first_run_line(s);
        println!(
            "{check} {label:<46} {dim_open}{detail}  [{elapsed}]{dim_close}",
            label = label,
            detail = detail,
            elapsed = format_elapsed(s.elapsed),
        );
    }
}

/// First-run UX line for the bundled command priors. Renders a single
/// `Loaded N command priors` row that follows the same visual style as
/// [`print_first_run_summary`] (green check on TTY, plain on non-TTY).
/// Decoupled from `ImportSummary` because priors are not a per-user import —
/// they're a static corpus shipped in the binary.
///
/// `n_detected` is the number of installed CLIs detected; `seeded` is the
/// number of prior rows actually written (filtered to those CLIs).
fn print_priors_seeded_line(n_detected: usize, seeded: usize) {
    let tty = std::io::stdout().is_terminal();
    let check = if tty {
        "\x1b[32m\u{2713}\x1b[0m"
    } else {
        "\u{2713}"
    };
    let dim_open = if tty { "\x1b[2m" } else { "" };
    let dim_close = if tty { "\x1b[0m" } else { "" };
    let label = "Loaded command priors";
    let detail = format!(
        "Detected {} installed CLIs · seeded {} command priors",
        fmt_count(n_detected),
        fmt_count(seeded),
    );
    println!("{check} {label:<46} {dim_open}{detail}{dim_close}");
}

/// Compact human label and detail for one [`ImportSummary`], used both by the
/// first-run printer and the standalone `shac import` subcommand.
fn first_run_line(s: &shac::import::ImportSummary) -> (String, String) {
    match s.source {
        "zsh_history" => (
            "Importing zsh history".into(),
            format!(
                "{} entries, {} dup, {} redacted",
                fmt_count(s.inserted),
                fmt_count(s.skipped_dup),
                fmt_count(s.skipped_redacted)
            ),
        ),
        "zoxide" => (
            "Importing zoxide".into(),
            format!("{} destinations", fmt_count(s.inserted)),
        ),
        "project_scan" => (
            "Scanning project roots for git repos".into(),
            format!("{} found", fmt_count(s.inserted)),
        ),
        other => (
            format!("Importing {other}"),
            format!("{} inserted", fmt_count(s.inserted)),
        ),
    }
}

/// Render an integer with simple thousands separators (e.g. `12,847`).
fn fmt_count(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Render a [`Duration`] as `0.4s` / `1.8s` (>= 100ms), or `45ms` (< 100ms).
fn format_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms >= 100 {
        let secs = ms as f64 / 1000.0;
        format!("{secs:.1}s")
    } else {
        format!("{ms}ms")
    }
}

/// Print a labelled step. On a TTY, write `label...` and overwrite with
/// `\r✓ label  detail` once the closure resolves. On a non-TTY, just print
/// the result line directly.
///
/// On error, prints `✗ label  detail` and returns the error string. Errors
/// are not propagated — `print_step` is for UX only and never fails the
/// surrounding flow.
fn print_step<F>(label: &str, op: F)
where
    F: FnOnce() -> Result<String>,
{
    let tty = std::io::stdout().is_terminal();
    if tty {
        // In-progress line — we deliberately don't terminate with \n so we
        // can overwrite with \r below.
        print!("{label}...");
        let _ = std::io::stdout().flush();
    }
    let started = Instant::now();
    let outcome = op();
    let elapsed = format_elapsed(started.elapsed());
    match outcome {
        Ok(detail) => {
            if tty {
                let check = "\x1b[32m\u{2713}\x1b[0m";
                let dim_open = "\x1b[2m";
                let dim_close = "\x1b[0m";
                let detail_part = if detail.is_empty() {
                    String::new()
                } else {
                    format!(" {dim_open}{detail}{dim_close}")
                };
                // \r + clear-to-EOL ("\x1b[2K") to fully replace the prior line.
                println!("\r\x1b[2K{check} {label:<46}{detail_part} \x1b[2m[{elapsed}]\x1b[0m");
            } else {
                let detail_part = if detail.is_empty() {
                    String::new()
                } else {
                    format!(" {detail}")
                };
                println!("\u{2713} {label}{detail_part}  [{elapsed}]");
            }
        }
        Err(err) => {
            if tty {
                let cross = "\x1b[31m\u{2717}\x1b[0m";
                println!("\r\x1b[2K{cross} {label:<46} \x1b[31m{err:#}\x1b[0m  [{elapsed}]");
            } else {
                println!("\u{2717} {label}  {err:#}  [{elapsed}]");
            }
        }
    }
}

/// Simple summary used by the `shac import` / `shac scan-projects`
/// subcommands (one summary at a time, no first-run framing).
fn print_import_summary(summaries: &[shac::import::ImportSummary]) {
    let tty = std::io::stdout().is_terminal();
    let check = if tty {
        "\x1b[32m\u{2713}\x1b[0m"
    } else {
        "\u{2713}"
    };
    for s in summaries {
        println!(
            "{check} {}: {} inserted, {} dup, {} redacted ({}ms)",
            s.source,
            s.inserted,
            s.skipped_dup,
            s.skipped_redacted,
            s.elapsed.as_millis()
        );
    }
}

fn import_action(paths: &AppPaths, args: ImportArgs) -> Result<()> {
    let db = shac::db::AppDb::open(&paths.db_file)?;
    match args.action {
        ImportAction::ZshHistory { path, dry_run } => {
            let resolved = path
                .map(PathBuf::from)
                .or_else(shac::import::default_zsh_history_path)
                .ok_or_else(|| anyhow::anyhow!("could not resolve zsh history path"))?;
            if dry_run {
                println!("would import zsh history from {}", resolved.display());
                return Ok(());
            }
            let red = shac::import::Redactor::new();
            let summary = shac::import::import_zsh_history(&db, &resolved, &red)?;
            print_import_summary(std::slice::from_ref(&summary));
        }
        ImportAction::Zoxide { path, dry_run } => {
            let resolved = path
                .map(PathBuf::from)
                .or_else(shac::import::default_zoxide_path)
                .ok_or_else(|| anyhow::anyhow!("could not resolve zoxide path"))?;
            if dry_run {
                println!("would import zoxide DB from {}", resolved.display());
                return Ok(());
            }
            let summary = shac::import::import_zoxide(&db, &resolved)?;
            print_import_summary(std::slice::from_ref(&summary));
        }
        ImportAction::All { yes } => {
            let opts = shac::import::ImportOpts {
                yes,
                roots: shac::import::default_project_roots(),
                depth: 3,
                shell: shac::import::ShellKind::Zsh,
                history_path: None,
                zoxide_path: None,
            };
            let summaries = shac::import::run_full_import(&db, opts)?;
            print_import_summary(&summaries);
        }
    }
    Ok(())
}

fn scan_projects_action(paths: &AppPaths, args: ScanProjectsArgs) -> Result<()> {
    let db = shac::db::AppDb::open(&paths.db_file)?;
    let roots: Vec<PathBuf> = if args.root.is_empty() {
        shac::import::default_project_roots()
    } else {
        args.root.into_iter().map(PathBuf::from).collect()
    };
    let summary = shac::import::scan_projects(&db, &roots, args.depth)?;
    print_import_summary(std::slice::from_ref(&summary));
    Ok(())
}

fn uninstall(paths: &AppPaths, shell: ShellKind, edit_rc: bool) -> Result<()> {
    let file_name = match shell {
        ShellKind::Bash => "shac.bash",
        ShellKind::Fish => "shac.fish",
        ShellKind::Zsh => "shac.zsh",
    };
    fs::remove_file(paths.shell_dir.join(file_name)).ok();
    if edit_rc {
        uninstall_rc_block(shell)?;
    }
    println!("uninstalled");
    Ok(())
}

fn install_rc_block(shell: ShellKind, shell_file: &Path) -> Result<()> {
    let rc_file = rc_file_for_shell(shell)?;
    let mut content = fs::read_to_string(&rc_file).unwrap_or_default();
    let block = managed_rc_block(shell, shell_file);
    if !content.contains(SHAC_RC_BEGIN) {
        if !content.ends_with('\n') && !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&block);
        fs::write(&rc_file, content).with_context(|| format!("write {}", rc_file.display()))?;
    }
    Ok(())
}

fn uninstall_rc_block(shell: ShellKind) -> Result<()> {
    let rc_file = rc_file_for_shell(shell)?;
    let content = fs::read_to_string(&rc_file).unwrap_or_default();
    let updated = remove_managed_rc_block(&content);
    if updated != content {
        fs::write(&rc_file, updated).with_context(|| format!("write {}", rc_file.display()))?;
    }
    Ok(())
}

fn managed_rc_block(shell: ShellKind, shell_file: &Path) -> String {
    let path = shell_escape(&shell_file.to_string_lossy());
    match shell {
        ShellKind::Fish => {
            format!("{SHAC_RC_BEGIN}\nif test -f {path}\n  source {path}\nend\n{SHAC_RC_END}\n")
        }
        _ => {
            format!("{SHAC_RC_BEGIN}\nif [ -f {path} ]; then\n  source {path}\nfi\n{SHAC_RC_END}\n")
        }
    }
}

fn remove_managed_rc_block(content: &str) -> String {
    let mut out = Vec::new();
    let mut skip = false;
    for line in content.lines() {
        if line == SHAC_RC_BEGIN {
            skip = true;
            continue;
        }
        if line == SHAC_RC_END {
            skip = false;
            continue;
        }
        if !skip {
            out.push(line);
        }
    }
    let mut result = out.join("\n");
    if content.ends_with('\n') && !result.is_empty() {
        result.push('\n');
    }
    result
}

fn rc_file_for_shell(shell: ShellKind) -> Result<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    Ok(match shell {
        ShellKind::Bash => home.join(".bashrc"),
        ShellKind::Fish => dirs::config_dir()
            .unwrap_or_else(|| home.join(".config"))
            .join("fish/config.fish"),
        ShellKind::Zsh => home.join(".zshrc"),
    })
}

/// Start the daemon if it is not already running, printing NOTHING. Safe to
/// call from the completion path, whose stdout is a protocol stream the shell
/// widgets parse — a stray "started"/"running" line there becomes a phantom
/// completion candidate that erases the user's typed token (F1). Returns true
/// if the daemon was already up, false if it was freshly started.
fn start_daemon_quiet(paths: &AppPaths) -> Result<bool> {
    cleanup_stale_daemon_state(paths);
    if daemon_is_running(paths) {
        return Ok(true);
    }
    let daemon_bin = daemon_binary_path()?;
    let command = format!(
        "if command -v setsid >/dev/null 2>&1; then nohup setsid {} >/dev/null 2>&1 </dev/null & else nohup {} >/dev/null 2>&1 </dev/null & fi",
        shell_escape(&daemon_bin.to_string_lossy()),
        shell_escape(&daemon_bin.to_string_lossy())
    );
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .status()
        .context("start shacd with detached shell")?;
    if !status.success() {
        bail!("failed to launch daemon process");
    }
    if wait_for_socket(&paths.socket_file, Duration::from_secs(2)) {
        Ok(false)
    } else {
        bail!("daemon did not create socket in time")
    }
}

fn daemon_action(paths: &AppPaths, action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start => {
            if start_daemon_quiet(paths)? {
                println!("running");
            } else {
                println!("started");
            }
            Ok(())
        }
        DaemonAction::Stop => {
            if !paths.pid_file.exists() {
                cleanup_stale_daemon_state(paths);
                println!("stopped");
                return Ok(());
            }
            let pid = fs::read_to_string(&paths.pid_file)?.trim().to_string();
            if !pid_is_shacd(&pid) {
                // The pid-file is stale: either the daemon already exited and
                // the OS recycled this pid for an unrelated (still-running)
                // process, or it never started. Don't signal a process we
                // don't own — just clear our own state. (Not delegated to
                // `cleanup_stale_daemon_state`: that treats any live pid as
                // "daemon running" and would leave this stale pid-file in
                // place.)
                fs::remove_file(&paths.pid_file).ok();
                if paths.socket_file.exists() && UnixStream::connect(&paths.socket_file).is_err() {
                    fs::remove_file(&paths.socket_file).ok();
                }
                println!("stopped");
                return Ok(());
            }
            let status = Command::new("kill")
                .arg(&pid)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("stop shacd")?;
            if !status.success() && process_exists(&pid).unwrap_or(false) {
                bail!("failed to stop daemon process {pid}");
            }
            wait_for_shutdown(paths, Duration::from_secs(2));
            cleanup_stale_daemon_state(paths);
            println!("stopped");
            Ok(())
        }
        DaemonAction::Restart => {
            // Prefer brew services so launchd keeps auto-restart on login.
            // Fall back to manual stop+start for non-brew installs.
            let via_brew = Command::new("brew")
                .args(["services", "restart", "shac"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !via_brew {
                daemon_action(paths, DaemonAction::Stop)?;
                daemon_action(paths, DaemonAction::Start)?;
            }
            println!("restarted");
            Ok(())
        }
        DaemonAction::Status => {
            cleanup_stale_daemon_state(paths);
            let running = daemon_is_running(paths);
            println!("{}", if running { "running" } else { "stopped" });
            Ok(())
        }
    }
}

fn complete(paths: &AppPaths, args: CompletionArgs) -> Result<()> {
    let shell = Shell::parse(Some(&args.shell));
    let (base_ctx, typed_home_user) = active_token_base_context(&args);
    if shac_disabled(paths)? {
        print_completion_response(
            disabled_completion_response(),
            shell,
            base_ctx,
            typed_home_user.as_deref(),
            &args.format,
        )?;
        return Ok(());
    }
    ensure_daemon(paths)?;
    let request = completion_request(&args);
    let response = send_request(paths, "complete", serde_json::to_value(request)?)?;
    print_completion_response(
        response,
        shell,
        base_ctx,
        typed_home_user.as_deref(),
        &args.format,
    )
}

/// The shared `TokenContext` fields that come from the CLI-supplied line
/// itself rather than any individual candidate: the unterminated quote char
/// of the active token (§4.3) and whether the active token as the user
/// *actually typed it* already begins with a home-reference prefix (the
/// other half of F3/F4 — see [`tilde_user_part`] and [`item_token_context`]).
/// Recomputed here (rather than round-tripped through the daemon response)
/// because the daemon only ever tells us a candidate's `kind`, not the raw
/// line the user typed.
fn active_token_base_context(args: &CompletionArgs) -> (TokenContext, Option<String>) {
    let cwd = canonicalize_lossy(&args.cwd);
    let parsed = context::parse(&args.line, args.cursor, Path::new(&cwd));
    let base = TokenContext {
        open_quote: parsed.open_quote,
        ..TokenContext::default()
    };
    (base, tilde_user_part(&parsed.active_token))
}

/// The "user part" of a home-reference prefix -- WHICH home a leading
/// `~`/`$HOME` denotes: `Some("")` for the current user's own home (`~`,
/// `~/...`, `$HOME`, `$HOME/...`), `Some("<name>")` for `~name`/`~name/...`,
/// and `None` when the token is not a home reference. Two home references
/// target the same directory iff their user parts are equal -- the comparison
/// `item_token_context` uses so a raw fs candidate keeps a bare tilde only when
/// it continues the SAME home the user typed, never a different `~otheruser`
/// the collector introduced (an attacker-planted `~root` dir must not hijack
/// `cd ~<Tab>`).
fn tilde_user_part(token: &str) -> Option<String> {
    if let Some(rest) = token.strip_prefix("$HOME") {
        return (rest.is_empty() || rest.starts_with('/')).then(String::new);
    }
    token
        .strip_prefix('~')
        .map(|rest| rest.chars().take_while(|c| *c != '/').collect())
}

/// The daemon `kind` values that only ever come from home-shortened
/// `insert_text` (built via `shorten_with_home` in engine.rs), never from a
/// raw filesystem entry. Used by [`item_token_context`] to derive signal (a)
/// of `TokenContext::home_ref` (F3/F4).
const HOME_SHORTENED_KINDS: &[&str] = &["path_jump", "workspace"];

/// Completion `source` values that carry the user's own command text — their
/// real shell history and learned transitions. A leading `~`/`$HOME` in one of
/// these is a genuine home reference the user themselves wrote, never a raw
/// filesystem entry that merely happens to start with `~` (the F3/F4 hazard,
/// which only arises for filesystem-scanned `path_cache` candidates). They are
/// therefore trusted to keep a bare home prefix (signal (c) of `home_ref`) even
/// though their `kind` (e.g. `subcommand`) is not one of HOME_SHORTENED_KINDS —
/// otherwise a learned `cd ~/proj/` transition would insert as `cd \~/proj/`,
/// which cd's into a literal `~` directory instead of expanding home.
const HOME_AUTHORED_SOURCES: &[&str] = &["transition", "history", "runtime_history"];

/// Build the per-item `TokenContext` from the shared base context (see
/// [`active_token_base_context`]) plus the item's own `kind`. Split out from
/// `print_completion_response` (which is I/O — println! — and awkward to
/// assert against) so the wiring is unit-testable on its own.
fn item_token_context(
    ctx: &TokenContext,
    typed_home_user: Option<&str>,
    item: &serde_json::Value,
) -> TokenContext {
    let kind = item
        .get("kind")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    // A candidate whose `kind` is a literal flag (e.g. python's `-V`) must
    // stay a bare flag: the leading-dash guard in `quote_token` is only for
    // path-like values that happen to start with `-` (spec §4.2), never for
    // a candidate that IS a flag.
    let is_option = kind == "option";
    // F3/F4/F5 + bare-tilde hole: a candidate keeps a bare home prefix only when
    // (a) the daemon assigned a home-shortened kind (path_jump/workspace, always
    // the user's OWN home, unforgeable by a filename), or (b) the candidate's
    // home reference denotes the SAME home target the user actually typed
    // (equal user parts) -- comparing user parts, not a coarse "typed some
    // tilde" bool, stops a bare typed `~` from licensing a different
    // `~otheruser` the fs collector introduced -- or (c) the candidate comes
    // from a HOME_AUTHORED_SOURCE (the user's own history/transitions) and
    // carries a genuine home prefix. Signal (c) is what a raw `path_cache`
    // filesystem entry can never satisfy, preserving the F3/F4 guard.
    let candidate_home_user = item
        .get("insert_text")
        .and_then(|value| value.as_str())
        .and_then(tilde_user_part);
    let source = item
        .get("source")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    let home_ref = HOME_SHORTENED_KINDS.contains(&kind)
        || (typed_home_user.is_some() && candidate_home_user.as_deref() == typed_home_user)
        || (HOME_AUTHORED_SOURCES.contains(&source) && candidate_home_user.is_some());
    TokenContext {
        is_option,
        home_ref,
        ..ctx.clone()
    }
}

fn print_completion_response(
    response: serde_json::Value,
    shell: Shell,
    ctx: TokenContext,
    typed_home_user: Option<&str>,
    format: &str,
) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if format == "shell-tsv-v3" {
        // Never emit an empty field: zsh's `${(ps:\t:)}` elides empty elements
        // and bash's `IFS=$'\t' read -a` collapses consecutive tabs, so an empty
        // request_id would shift every following field (F2) — in bash that fed a
        // non-numeric `--accepted-request-id` and silently dropped the recorded
        // command. `0` is a safe non-id sentinel (real ids are positive).
        let request_id = response
            .get("request_id")
            .and_then(|value| value.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_else(|| "0".to_string());
        let mode = response
            .get("mode")
            .and_then(|value| value.as_str())
            .unwrap_or("replace_token");
        let items = response
            .get("items")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        println!(
            "__shac_request_id\t{}\t{}\t{}",
            encode_field(&request_id),
            encode_field(mode),
            items.len()
        );
        for item in items {
            let item_key = item
                .get("item_key")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let insert_text = item
                .get("insert_text")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let display = item
                .get("display")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let kind = item
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let source = item
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let description = item
                .get("meta")
                .and_then(|value| value.get("description"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let item_ctx = item_token_context(&ctx, typed_home_user, &item);
            // Two daemon-computed bits, kept separate to avoid the drift F7/F8
            // set out to kill. `verbatim`: the candidate is already a whole
            // valid shell line (a multi-word history/transition entry at any
            // command position — buffer start OR after `&&`/`|`/`;`), so
            // per-token escaping (which turns `cd ..` into `cd\ ..`, one broken
            // word) must be skipped. `full_line`: Enter may run it — only when
            // it replaces the whole buffer — and is emitted as field 7 so every
            // widget keys Enter/insert off the same bit. Single-token candidates
            // (paths, options, subcommands) get neither and are escaped.
            let verbatim = item
                .get("verbatim")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let full_line = item
                .get("full_line")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let quoted_insert = if verbatim {
                insert_text.to_string()
            } else {
                quote_token(shell, &item_ctx, insert_text)
            };
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                encode_field(item_key),
                encode_field(&quoted_insert),
                encode_field(display),
                encode_field(kind),
                encode_field(source),
                encode_field(description),
                if full_line { "1" } else { "0" }
            );
        }
        if let Some(tip) = response.get("tip").and_then(|v| v.as_object()) {
            let id = tip.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let text = tip.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !id.is_empty() && !text.is_empty() {
                println!("__shac_tip\t{}\t{}", encode_field(id), encode_field(text));
            }
        }
        if let Some(dv) = response.get("daemon_version").and_then(|v| v.as_str()) {
            println!("__shac_daemon_version\t{}", encode_field(dv));
        }
    } else {
        let items = response
            .get("items")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        for item in items {
            if let Some(display) = item.get("display").and_then(|value| value.as_str()) {
                println!("{display}");
            }
        }
    }
    Ok(())
}

fn completion_request(args: &CompletionArgs) -> CompletionRequest {
    let mut env = std::collections::HashMap::new();
    for key in [
        "SHAC_NO_TIPS",
        "SHAC_LOCALE",
        "SHAC_TIPS_DEBUG",
        "LC_MESSAGES",
        "LANG",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    CompletionRequest {
        shell: args.shell.clone(),
        line: args.line.clone(),
        cursor: args.cursor,
        cwd: canonicalize_lossy(&args.cwd),
        env,
        session: current_session_info(),
        history_hint: shac::protocol::HistoryHint {
            prev_command: args.prev_command.clone(),
            runtime_commands: args.history_commands.clone(),
        },
    }
}

fn disabled_completion_response() -> serde_json::Value {
    serde_json::json!({
        "request_id": null,
        "items": [],
        "mode": "replace_token",
        "fallback": true
    })
}

fn explain(paths: &AppPaths, args: CompletionArgs) -> Result<()> {
    if shac_disabled(paths)? {
        println!("shac is disabled");
        return Ok(());
    }
    ensure_daemon(paths)?;
    let request = completion_request(&args);
    let response = send_request(paths, "explain", serde_json::to_value(request)?)?;
    let explain: ExplainResponse = serde_json::from_value(response)?;
    println!("query: {}", explain.query);
    for item in explain.items {
        println!("{} [{:.3}] via {}", item.display, item.score, item.source);
        for feature in item.features {
            println!(
                "  {:<24} value={:.3} weight={:.3} contribution={:.3}",
                feature.name, feature.value, feature.weight, feature.contribution
            );
        }
    }
    Ok(())
}

fn config_action(paths: &AppPaths, action: ConfigAction) -> Result<()> {
    let mut config = AppConfig::load(paths)?;
    match action {
        ConfigAction::Get { key } => {
            if let Some(value) = config.get_key(&key) {
                println!("{value}");
                Ok(())
            } else {
                bail!("unknown config key: {key}")
            }
        }
        ConfigAction::Set { key, value } => {
            config.set_key(&key, &value)?;
            config.save(paths)?;
            Ok(())
        }
    }
}

fn migration_status(paths: &AppPaths) -> Result<()> {
    let engine = Engine::new(paths)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&engine.migration_status()?)?
    );
    Ok(())
}

fn shell_env(paths: &AppPaths, args: ShellEnvArgs) -> Result<()> {
    let config = AppConfig::load(paths)?;
    match args.shell {
        ShellKind::Fish => {}
        ShellKind::Zsh => {
            let zsh = config.ui.zsh;
            println!(
                "typeset -g _shac_ui_menu_detail={}",
                shell_escape(&zsh.menu_detail)
            );
            println!(
                "typeset -gi _shac_ui_show_kind={}",
                if zsh.show_kind { 1 } else { 0 }
            );
            println!(
                "typeset -gi _shac_ui_show_source={}",
                if zsh.show_source { 1 } else { 0 }
            );
            println!(
                "typeset -gi _shac_ui_show_description={}",
                if zsh.show_description { 1 } else { 0 }
            );
            println!(
                "typeset -gi _shac_ui_max_description_width={}",
                zsh.max_description_width
            );
            println!("typeset -gi _shac_ui_max_items={}", zsh.max_items);
            println!(
                "typeset -gi _shac_ui_inline_zsh={}",
                if config.features.inline_zsh { 1 } else { 0 }
            );
            println!(
                "typeset -g _shac_client_version={}",
                shell_escape(env!("CARGO_PKG_VERSION"))
            );
        }
        ShellKind::Bash => {}
    }
    Ok(())
}

fn recent_events(paths: &AppPaths, args: RecentEventsArgs) -> Result<()> {
    let engine = Engine::new(paths)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&engine.recent_events(args.limit)?)?
    );
    Ok(())
}

fn reset_personalization(paths: &AppPaths) -> Result<()> {
    let engine = Engine::new(paths)?;
    engine.reset_personalization()?;
    Ok(())
}

fn ensure_daemon(paths: &AppPaths) -> Result<()> {
    // Quiet start: this runs inside `complete`, whose stdout is the protocol
    // stream (F1). Never print a status line here.
    start_daemon_quiet(paths)?;
    Ok(())
}

fn shac_disabled(paths: &AppPaths) -> Result<bool> {
    if std::env::var_os("SHAC_DISABLE").is_some() {
        return Ok(true);
    }
    Ok(!AppConfig::load(paths)?.enabled)
}

fn request_timeout_for_action(action: &str, base_timeout_ms: u64) -> Duration {
    let timeout_ms = match action {
        // `reindex` rescans every PATH command and rebuilds the doc index
        // synchronously in the daemon; on a machine with a large PATH — or a
        // slow CI runner — that legitimately takes several seconds. It is an
        // explicit, occasional command, not a keystroke-latency path, so give
        // the client a generous ceiling. A too-tight one makes the client
        // report a "read daemon response" failure while the daemon is still
        // working and actually finishing the reindex (the recurring CI flake
        // in reindex_default_flags_succeeds / cli_daemon_records_*).
        "reindex" => base_timeout_ms.max(30_000),
        "stats" | "migration-status" => base_timeout_ms.max(500),
        _ => base_timeout_ms.max(1),
    };
    Duration::from_millis(timeout_ms)
}

fn send_request(
    paths: &AppPaths,
    action: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value> {
    let timeout_ms = AppConfig::load(paths)
        .map(|config| config.daemon_timeout_ms)
        .unwrap_or_else(|_| AppConfig::default().daemon_timeout_ms);
    let timeout = request_timeout_for_action(action, timeout_ms);
    send_request_with_timeout(paths, action, payload, timeout)
}

fn send_request_with_timeout(
    paths: &AppPaths,
    action: &str,
    payload: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let mut stream = connect_with_retry(&paths.socket_file, timeout)?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();
    let request = serde_json::json!({ "action": action, "payload": payload });
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reader = BufReader::new(stream);
    let response = read_response_with_retry(&mut reader, timeout)?;
    if response.trim().is_empty() {
        bail!("empty response from daemon");
    }
    Ok(serde_json::from_str(&response)?)
}

fn read_response_with_retry(
    reader: &mut BufReader<UnixStream>,
    timeout: Duration,
) -> Result<String> {
    let started = Instant::now();
    let mut response = String::new();
    loop {
        match reader.read_line(&mut response) {
            Ok(_) => return Ok(response),
            Err(err)
                if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && started.elapsed() < timeout =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => return Err(err).context("read daemon response"),
        }
    }
}

fn connect_with_retry(path: &Path, timeout: Duration) -> Result<UnixStream> {
    let started = Instant::now();
    let mut last_error = None;
    while started.elapsed() < timeout {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    match last_error {
        Some(err) => Err(err).context("connect to daemon socket"),
        None => bail!("connect to daemon socket timed out"),
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn daemon_binary_path() -> Result<PathBuf> {
    let current = std::env::current_exe()?;
    let sibling = current
        .parent()
        .map(|dir| dir.join("shacd"))
        .context("locate executable directory")?;
    if sibling.exists() {
        return Ok(sibling);
    }

    let build_dir = current
        .parent()
        .and_then(|dir| dir.parent())
        .context("locate cargo target dir")?;
    for candidate in [
        build_dir.join("debug/shacd"),
        build_dir.join("release/shacd"),
    ] {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(path) = std::env::var("SHAC_DAEMON_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "unable to locate shacd binary; build it first with `cargo build --bins` or set SHAC_DAEMON_BIN"
    )
}

fn canonicalize_lossy(path: &str) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .to_string()
}

fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn current_session_info() -> SessionInfo {
    let tty = std::env::var("TTY").ok().or_else(|| {
        if std::io::stdin().is_terminal() || std::io::stderr().is_terminal() {
            Some("interactive".to_string())
        } else {
            None
        }
    });
    SessionInfo {
        tty,
        pid: Some(std::process::id()),
    }
}

fn daemon_is_running(paths: &AppPaths) -> bool {
    if !paths.socket_file.exists() {
        return false;
    }

    if let Ok(pid) = fs::read_to_string(&paths.pid_file) {
        if process_exists(pid.trim()).unwrap_or(false) {
            return true;
        }
    }

    UnixStream::connect(&paths.socket_file).is_ok()
}

fn cleanup_stale_daemon_state(paths: &AppPaths) {
    let mut live_pid = false;
    if paths.pid_file.exists() {
        match fs::read_to_string(&paths.pid_file) {
            Ok(pid) => {
                if process_exists(pid.trim()).unwrap_or(false) {
                    live_pid = true;
                } else {
                    fs::remove_file(&paths.pid_file).ok();
                }
            }
            Err(_) => {
                fs::remove_file(&paths.pid_file).ok();
            }
        }
    }

    if live_pid {
        return;
    }

    if paths.socket_file.exists() && UnixStream::connect(&paths.socket_file).is_err() {
        fs::remove_file(&paths.socket_file).ok();
    }
}

fn process_exists(pid: &str) -> Result<bool> {
    let status = Command::new("kill")
        .arg("-0")
        .arg(pid)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("probe daemon process")?;
    Ok(status.success())
}

/// Verifies `pid` is actually running as `shacd` before we `kill` it. PIDs
/// get recycled by the OS (e.g. after a reboot), so a pid-file alone is not
/// proof the process it names is still our daemon — signaling an unverified
/// PID could kill an unrelated, innocent process.
fn pid_is_shacd(pid: &str) -> bool {
    let output = match Command::new("ps")
        .arg("-p")
        .arg(pid)
        .arg("-o")
        .arg("comm=")
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    comm_is_shacd(String::from_utf8_lossy(&output.stdout).trim())
}

/// `ps -p PID -o comm=` reports the full executable path on macOS (and often
/// just the bare process name on Linux) — matching with `.contains("shacd")`
/// would misidentify any recycled-pid process whose path merely *contains*
/// "shacd" as a substring (e.g. `/tmp/team-shacd-tools/backup-agent`) as our
/// daemon and kill it. Compare the basename exactly instead.
fn comm_is_shacd(comm: &str) -> bool {
    Path::new(comm.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "shacd")
        .unwrap_or(false)
}

fn wait_for_shutdown(paths: &AppPaths, timeout: Duration) {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if !paths.socket_file.exists() && !paths.pid_file.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod first_run_ux_tests {
    use super::*;
    use shac::import::ImportSummary;

    #[test]
    fn fmt_count_inserts_thousands_separators() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(7), "7");
        assert_eq!(fmt_count(847), "847");
        assert_eq!(fmt_count(1_000), "1,000");
        assert_eq!(fmt_count(12_847), "12,847");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    #[test]
    fn reindex_gets_a_generous_timeout_floor() {
        // A tiny daemon_timeout_ms (the keystroke-latency default) must not
        // bound a full-PATH reindex: the client would give up while the daemon
        // is still working. reindex floors at 30s regardless.
        assert_eq!(
            request_timeout_for_action("reindex", 150),
            Duration::from_millis(30_000)
        );
        // A larger configured timeout still wins if it exceeds the floor.
        assert_eq!(
            request_timeout_for_action("reindex", 45_000),
            Duration::from_millis(45_000)
        );
        // Latency-sensitive actions keep their small ceilings.
        assert_eq!(
            request_timeout_for_action("complete", 150),
            Duration::from_millis(150)
        );
    }

    #[test]
    fn format_elapsed_seconds_above_threshold() {
        assert_eq!(format_elapsed(Duration::from_millis(100)), "0.1s");
        assert_eq!(format_elapsed(Duration::from_millis(1_800)), "1.8s");
        assert_eq!(format_elapsed(Duration::from_millis(12_000)), "12.0s");
    }

    #[test]
    fn format_elapsed_milliseconds_below_threshold() {
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0ms");
        assert_eq!(format_elapsed(Duration::from_millis(45)), "45ms");
        assert_eq!(format_elapsed(Duration::from_millis(99)), "99ms");
    }

    #[test]
    fn first_run_line_labels_match_spec() {
        let s = ImportSummary {
            source: "zsh_history",
            seen: 12_847,
            inserted: 12_847,
            skipped_dup: 3,
            skipped_redacted: 1,
            elapsed: Duration::from_millis(1_800),
        };
        let (label, detail) = first_run_line(&s);
        assert_eq!(label, "Importing zsh history");
        assert!(detail.contains("12,847 entries"));
        assert!(detail.contains("3 dup"));
        assert!(detail.contains("1 redacted"));
    }

    #[test]
    fn first_run_line_handles_zoxide_and_project_scan() {
        let zox = ImportSummary {
            source: "zoxide",
            seen: 156,
            inserted: 156,
            skipped_dup: 0,
            skipped_redacted: 0,
            elapsed: Duration::from_millis(100),
        };
        let (label, detail) = first_run_line(&zox);
        assert_eq!(label, "Importing zoxide");
        assert_eq!(detail, "156 destinations");

        let scan = ImportSummary {
            source: "project_scan",
            seen: 23,
            inserted: 23,
            skipped_dup: 0,
            skipped_redacted: 0,
            elapsed: Duration::from_millis(600),
        };
        let (label, detail) = first_run_line(&scan);
        assert_eq!(label, "Scanning project roots for git repos");
        assert_eq!(detail, "23 found");
    }

    /// `install_rc_block` must return an error when the rc file is read-only.
    ///
    /// This covers the fix for the codex P1 finding: the rc-hook step was
    /// silently swallowed by `print_step`, so a permission-denied write would
    /// let `shac install --edit-rc` exit 0 while the shell was never hooked.
    ///
    /// The test calls `install_rc_block` directly with a temporary HOME that
    /// contains a read-only `.zshrc`, asserts an `Err` is returned, and
    /// verifies the error message mentions the file path.
    ///
    /// Skipped when running as root (root can write to read-only files).
    #[test]
    fn install_rc_block_fails_on_readonly_rc_file() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!(
            "shac-rc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).expect("create tmp dir");
        let rc_file = tmp.join(".zshrc");
        // Create an existing rc file so read_to_string succeeds, then make it
        // read-only so the subsequent fs::write fails.
        fs::write(&rc_file, "# existing rc\n").expect("write initial rc");
        let mut perms = fs::metadata(&rc_file).unwrap().permissions();
        perms.set_mode(0o444); // read-only
        fs::set_permissions(&rc_file, perms.clone()).expect("chmod rc");

        // Build a dummy shell file path (need not exist for the error path).
        let shell_file = tmp.join("shac.zsh");

        // Temporarily redirect HOME so rc_file_for_shell resolves to our dir.
        let orig_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &tmp) };
        let result = install_rc_block(ShellKind::Zsh, &shell_file);

        // Restore HOME and permissions before any assertion.
        match orig_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        perms.set_mode(0o644);
        let _ = fs::set_permissions(&rc_file, perms);
        let _ = fs::remove_dir_all(&tmp);

        // Root can bypass read-only permissions — skip the assertion in that case.
        // Check by whether we got an error (if root, the write succeeds → Ok).
        if result.is_ok() {
            // Running as root: the write succeeded despite read-only perms. Skip.
            return;
        }

        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(".zshrc") || msg.contains("write"),
            "error should mention the rc file or write failure, got: {msg}"
        );
    }
}

#[cfg(test)]
mod completion_response_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn item_token_context_marks_home_ref_for_home_shortened_kinds() {
        // F3/F4: the shac.rs wiring must derive home_ref from the trusted
        // `kind` field (never from insert_text content) — a path_jump or
        // workspace candidate opts a bare tilde in, regardless of whether
        // the user typed a home reference at all.
        let ctx = TokenContext::default();
        assert!(item_token_context(&ctx, None, &json!({"kind": "path_jump"})).home_ref);
        assert!(item_token_context(&ctx, None, &json!({"kind": "workspace"})).home_ref);
    }

    #[test]
    fn item_token_context_defaults_home_ref_false_for_raw_fs_and_other_kinds() {
        // A raw filesystem entry (kind == "path") — even one literally named
        // `~evil` — must not be treated as a home reference when the user
        // typed no tilde at all (typed_home_user None, e.g. active token ""
        // or "-r"), or it would tilde-expand on insertion instead of
        // inserting literally.
        let ctx = TokenContext::default();
        assert!(
            !item_token_context(&ctx, None, &json!({"kind": "path", "insert_text": "~evil"}))
                .home_ref
        );
        assert!(!item_token_context(&ctx, None, &json!({"kind": "command"})).home_ref);
        assert!(!item_token_context(&ctx, None, &json!({})).home_ref);
    }

    #[test]
    fn item_token_context_ors_typed_home_prefix_into_home_ref() {
        // The fs collector returns `kind == "path"` (not a HOME_SHORTENED_KINDS
        // member) for `cd ~/Doc<Tab>`, echoing the user's own `~/` prefix
        // back in `insert_text` ("~/Documents/"). `typed_home_user` carries
        // the "user typed a home prefix" signal computed once by
        // `active_token_base_context`; item_token_context must recognize
        // that the candidate continues the SAME home (equal user parts,
        // both `Some("")`) and set home_ref true rather than letting `kind`
        // alone decide.
        let ctx = TokenContext::default();
        let item = json!({"kind": "path", "insert_text": "~/Documents/"});
        assert!(item_token_context(&ctx, Some(""), &item).home_ref);
    }

    #[test]
    fn item_token_context_marks_home_ref_for_home_authored_source_tilde() {
        // A learned transition (`source == "transition"`) for `cd ~/Korat/`
        // arrives with `kind == "subcommand"` (not a HOME_SHORTENED_KIND) and
        // an empty typed token, so signals (a) and (b) both miss. Signal (c)
        // must recognize it as the user's own home reference and keep the
        // tilde bare -- otherwise quote_token escapes it to `\~/Korat/`, which
        // cd's into a literal `~` directory. Covers history/runtime_history too.
        let ctx = TokenContext::default();
        for source in ["transition", "history", "runtime_history"] {
            let item = json!({
                "kind": "subcommand",
                "source": source,
                "insert_text": "~/Korat/",
            });
            assert!(
                item_token_context(&ctx, None, &item).home_ref,
                "home-authored source {source} with a ~/ value must be home_ref"
            );
        }
    }

    #[test]
    fn item_token_context_keeps_path_cache_tilde_guarded() {
        // The F3/F4 guard must survive signal (c): a raw filesystem entry
        // (`source == "path_cache"`) that merely happens to start with `~`
        // must NOT be treated as a home reference, or a file literally named
        // `~evil` would tilde-expand to another user's home on insertion.
        let ctx = TokenContext::default();
        let item = json!({
            "kind": "path",
            "source": "path_cache",
            "insert_text": "~evil",
        });
        assert!(!item_token_context(&ctx, None, &item).home_ref);
    }

    #[test]
    fn version_skew_detail_flags_only_confirmed_mismatch() {
        // Matching versions: ok, no alarm.
        let (ok, _) =
            version_skew_detail("0.6.4", &DaemonVersionProbe::Version("0.6.4".to_string()));
        assert!(ok);

        // Confirmed mismatch (stale daemon after brew upgrade): fail, and the
        // detail must tell the user exactly how to fix it.
        let (ok, detail) =
            version_skew_detail("0.6.4", &DaemonVersionProbe::Version("0.5.3".to_string()));
        assert!(!ok);
        assert!(detail.contains("0.5.3"));
        assert!(detail.contains("0.6.4"));
        assert!(detail.contains("shac daemon restart"));

        // A daemon that answers but omits the version field is a confirmed
        // pre-0.5.2 daemon — fail, don't wave it through.
        let (ok, detail) = version_skew_detail("0.6.4", &DaemonVersionProbe::MissingField);
        assert!(!ok);
        assert!(detail.contains("shac daemon restart"));

        // Unreachable (probe timeout/error): stays OK so a transient hiccup
        // doesn't cry wolf.
        let (ok, _) = version_skew_detail("0.6.4", &DaemonVersionProbe::Unreachable);
        assert!(ok);
    }

    #[test]
    fn tilde_user_part_parses_home_reference_user_parts() {
        assert_eq!(tilde_user_part("~"), Some(String::new()));
        assert_eq!(tilde_user_part("~/"), Some(String::new()));
        assert_eq!(tilde_user_part("~/D"), Some(String::new()));
        assert_eq!(tilde_user_part("~root/"), Some("root".to_string()));
        assert_eq!(tilde_user_part("~alice"), Some("alice".to_string()));
        assert_eq!(tilde_user_part("$HOME"), Some(String::new()));
        assert_eq!(tilde_user_part("$HOME/x"), Some(String::new()));

        // Not a home reference at all.
        assert_eq!(tilde_user_part("$HOMEx"), None);
        assert_eq!(tilde_user_part("My"), None);
        assert_eq!(tilde_user_part("-r"), None);
    }

    #[test]
    fn active_token_base_context_derives_home_ref_from_typed_line() {
        // The wiring function itself: `cd ~/D<Tab>` types a home prefix, so
        // the shared base context must carry the typed home user (the
        // current user's OWN home, `Some("")`) even before any candidate's
        // `kind` is known.
        let with_tilde = CompletionArgs {
            shell: "zsh".to_string(),
            line: "cd ~/D".to_string(),
            cursor: 6,
            cwd: "/".to_string(),
            prev_command: None,
            history_commands: Vec::new(),
            format: "shell-tsv-v3".to_string(),
        };
        assert_eq!(
            active_token_base_context(&with_tilde).1,
            Some(String::new())
        );

        let without_tilde = CompletionArgs {
            line: "cd -r".to_string(),
            cursor: 5,
            ..with_tilde
        };
        assert_eq!(active_token_base_context(&without_tilde).1, None);
    }

    #[test]
    fn cd_tilde_completion_insert_stays_bare_end_to_end() {
        // Reproduces the BLOCKER end to end: given the fs collector's real
        // output shape for `cd ~/D<Tab>` (kind "path", insert_text echoing
        // the typed `~/` prefix), the formatter's full chain —
        // active_token_base_context -> item_token_context -> quote_token —
        // must leave the `~/` bare rather than escaping it to `\~/`.
        let args = CompletionArgs {
            shell: "zsh".to_string(),
            line: "cd ~/D".to_string(),
            cursor: 6,
            cwd: "/".to_string(),
            prev_command: None,
            history_commands: Vec::new(),
            format: "shell-tsv-v3".to_string(),
        };
        let (base_ctx, typed_home_user) = active_token_base_context(&args);
        let item = json!({"kind": "path", "insert_text": "~/Documents/"});
        let item_ctx = item_token_context(&base_ctx, typed_home_user.as_deref(), &item);
        let quoted = quote_token(Shell::Zsh, &item_ctx, "~/Documents/");
        assert_eq!(quoted, "~/Documents/");
    }

    #[test]
    fn bare_tilde_typed_does_not_bare_a_different_user_home() {
        // THE BUG this closes: an attacker-planted directory literally
        // named `~root` returned by the raw fs collector must not keep a
        // bare tilde just because the user typed a bare `~` (their OWN
        // home) — that would let `cd ~<Tab>` insert an unescaped `~root/`,
        // which the shell expands to a DIFFERENT user's home (/var/root)
        // instead of the local `./~root` directory the fs entry denotes.
        let ctx = TokenContext::default();
        let evil = json!({"kind": "path", "insert_text": "~root/"});
        assert!(!item_token_context(&ctx, Some(""), &evil).home_ref);

        // The legitimate case `cd ~<Tab>` exists to serve — the fs
        // collector's own home listing — must still keep its bare tilde.
        let legit = json!({"kind": "path", "insert_text": "~/Documents/"});
        assert!(item_token_context(&ctx, Some(""), &legit).home_ref);
    }

    #[test]
    fn explicit_other_user_home_is_honored() {
        // When the user explicitly types `~root<Tab>` themselves, a raw fs
        // candidate continuing that SAME home (`~root/x`) is a genuine home
        // reference and keeps its bare tilde.
        let ctx = TokenContext::default();
        let item = json!({"kind": "path", "insert_text": "~root/x"});
        assert!(item_token_context(&ctx, Some("root"), &item).home_ref);
    }

    #[test]
    fn item_token_context_marks_option_kind() {
        let ctx = TokenContext::default();
        let option_item = json!({"kind": "option"});
        assert!(item_token_context(&ctx, None, &option_item).is_option);

        let path_item = json!({"kind": "path"});
        assert!(!item_token_context(&ctx, None, &path_item).is_option);
    }

    #[test]
    fn item_token_context_preserves_shared_open_quote() {
        let ctx = TokenContext {
            open_quote: Some('"'),
            ..Default::default()
        };
        let item = json!({"kind": "path"});
        assert_eq!(item_token_context(&ctx, None, &item).open_quote, Some('"'));
    }
}

#[cfg(test)]
mod pid_is_shacd_tests {
    use super::*;

    /// A full path ending in `/shacd` (what `ps -o comm=` reports on macOS
    /// for the real daemon) must match.
    #[test]
    fn full_path_ending_in_shacd_matches() {
        assert!(comm_is_shacd("/Users/me/dev/shac/target/debug/shacd"));
    }

    /// A path that merely *contains* "shacd" in a parent directory, but whose
    /// basename is unrelated, must NOT match — this is the exact
    /// innocent-process-kill hole a substring check reopens.
    #[test]
    fn path_containing_shacd_in_parent_dir_does_not_match() {
        assert!(!comm_is_shacd("/tmp/team-shacd-tools/backup-agent"));
    }

    /// A bare basename with no path separators still matches (Linux
    /// typically reports just the process name).
    #[test]
    fn bare_basename_shacd_matches() {
        assert!(comm_is_shacd("shacd"));
    }

    /// An unrelated bare name does not match.
    #[test]
    fn unrelated_bare_name_does_not_match() {
        assert!(!comm_is_shacd("backup-agent"));
    }
}
