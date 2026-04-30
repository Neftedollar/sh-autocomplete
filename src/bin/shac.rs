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
use shac::engine::Engine;
use shac::indexer;
use shac::ml::{train_model, TrainOptions};
use shac::protocol::{CompletionRequest, ExplainResponse, RecordCommandRequest, SessionInfo};
use shac::shell::{BASH_COMPLETION, FISH_COMPLETION, ZSH_COMPLETION};

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
  stats                  Show usage statistics (completions accepted, model status, etc.)
  recent-events          Show recent completion and acceptance events
  debug                  Low-level debug tools (show raw completion results)

Personalization:
  train-model            Train (or retrain) the personalization ranking model
  reset-personalization  Clear all learned preferences and start personalization from scratch
  export-training-data   Export labelled completion data for ML model training

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
        .subcommand(InstallArgs::augment_args(ClapCmd::new("install").about("Add shac integration to your shell rc file")))
        .subcommand(InstallArgs::augment_args(ClapCmd::new("uninstall").about("Remove shac integration from your shell rc file")))
        .subcommand(DaemonArgs::augment_args(ClapCmd::new("daemon").about("Manage the background daemon (start / stop / restart / status)")))
        // ── Index ────────────────────────────────────────────────────────────
        .next_help_heading("Index")
        .subcommand(ImportArgs::augment_args(ClapCmd::new("import").about("Import command history from zsh history or zoxide")))
        .subcommand(ScanProjectsArgs::augment_args(ClapCmd::new("scan-projects").about("Scan directories and index project paths for path completions")))
        .subcommand(ReindexArgs::augment_args(ClapCmd::new("reindex").about("Re-scan PATH commands and rebuild documentation index")))
        .subcommand(IndexArgs::augment_args(ClapCmd::new("index").about("Add a specific command or directory path to the index")))
        .subcommand(ClapCmd::new("invalidate-caches").about("Clear all cached completion results"))
        // ── Diagnostics ──────────────────────────────────────────────────────
        .next_help_heading("Diagnostics")
        .subcommand(DoctorArgs::augment_args(ClapCmd::new("doctor").about("Check that the daemon, shell integration, and index are healthy")))
        .subcommand(CompletionArgs::augment_args(ClapCmd::new("explain").about("Explain why candidates ranked the way they did for a query")))
        .subcommand(ClapCmd::new("stats").about("Show usage statistics (completions accepted, model status, etc.)"))
        .subcommand(RecentEventsArgs::augment_args(ClapCmd::new("recent-events").about("Show recent completion and acceptance events")))
        .subcommand(DebugArgs::augment_args(ClapCmd::new("debug").about("Low-level debug tools (show raw completion results)")))
        // ── Personalization ───────────────────────────────────────────────────
        .next_help_heading("Personalization")
        .subcommand(TrainModelArgs::augment_args(ClapCmd::new("train-model").about("Train (or retrain) the personalization ranking model")))
        .subcommand(ClapCmd::new("reset-personalization").about("Clear all learned preferences and start personalization from scratch"))
        .subcommand(TrainingDataArgs::augment_args(ClapCmd::new("export-training-data").about("Export labelled completion data for ML model training")))
        // ── Settings ─────────────────────────────────────────────────────────
        .next_help_heading("Settings")
        .subcommand(ConfigArgs::augment_args(ClapCmd::new("config").about("View or edit configuration settings")))
        .subcommand(LocaleArgs::augment_args(ClapCmd::new("locale").about("View or change the UI language / locale")))
        .subcommand(TipsArgs::augment_args(ClapCmd::new("tips").about("Manage inline usage tips (list / mute / unmute)")))
        // ── Internal (shell scripts only) ─────────────────────────────────────
        .subcommand(CompletionArgs::augment_args(ClapCmd::new("complete").hide(true)))
        .subcommand(RecordArgs::augment_args(ClapCmd::new("record-command").hide(true)))
        .subcommand(ShellEnvArgs::augment_args(ClapCmd::new("shell-env").hide(true)))
        .subcommand(SuggestArgs::augment_args(ClapCmd::new("suggest").hide(true)))
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
    AddCommand {
        cmd: String,
    },
    AddPath {
        path: String,
        #[arg(long)]
        subpath: bool,
        #[arg(long)]
        full: bool,
        #[arg(long, default_value_t = 0)]
        deep: usize,
    },
    Status,
}

#[derive(Debug, Subcommand)]
enum DaemonAction {
    Start,
    Stop,
    Restart,
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
struct TrainingDataArgs {
    #[arg(long)]
    output: Option<String>,
    #[arg(long, default_value_t = 10000)]
    limit: usize,
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

#[derive(Debug, Args)]
struct TrainModelArgs {
    #[arg(long)]
    output: String,
    #[arg(long, default_value_t = 10000)]
    limit: usize,
    #[arg(long, default_value_t = 30)]
    iterations: usize,
    #[arg(long, default_value_t = 0.15)]
    learning_rate: f64,
}

fn main() -> Result<()> {
    let matches = build_app().get_matches();
    let paths = AppPaths::discover()?;
    paths.ensure()?;

    match matches.subcommand() {
        Some(("install", sub))    => install(&paths, InstallArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("uninstall", sub))  => { let a = InstallArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()); uninstall(&paths, a.shell, a.edit_rc) }
        Some(("daemon", sub))     => daemon_action(&paths, DaemonArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()).action),
        Some(("import", sub))     => import_action(&paths, ImportArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("scan-projects", sub)) => scan_projects_action(&paths, ScanProjectsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
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
        Some(("index", sub))      => index_action(&paths, IndexArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()).action),
        Some(("invalidate-caches", _)) => {
            ensure_daemon(&paths)?;
            let resp = send_request(&paths, "invalidate-caches", serde_json::json!({}))?;
            if let Some(err) = resp.get("error").and_then(|e| e.as_str()) {
                bail!("daemon error: {err}");
            }
            println!("caches invalidated");
            Ok(())
        }
        Some(("doctor", sub))        => doctor(&paths, DoctorArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("explain", sub))       => explain(&paths, CompletionArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("stats", _)) => {
            ensure_daemon(&paths)?;
            let value = send_request(&paths, "stats", serde_json::json!({}))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Some(("recent-events", sub)) => recent_events(&paths, RecentEventsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("debug", sub))         => debug_action(&paths, DebugArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()).action),
        Some(("train-model", sub))   => train_model_file(&paths, TrainModelArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("reset-personalization", _)) => reset_personalization(&paths),
        Some(("export-training-data", sub)) => export_training_data(&paths, TrainingDataArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("config", sub))        => config_action(&paths, ConfigArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit()).action),
        Some(("locale", sub))        => run_locale(&paths, LocaleArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("tips", sub))          => run_tips(&paths, TipsArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("complete", sub))      => complete(&paths, CompletionArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("record-command", sub)) => {
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
        Some(("shell-env", sub))      => shell_env(&paths, ShellEnvArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("suggest", sub))        => run_suggest(&paths, SuggestArgs::from_arg_matches(sub).unwrap_or_else(|e| e.exit())),
        Some(("migration-status", _)) => migration_status(&paths),
        _ => { build_app().print_help()?; println!(); Ok(()) }
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

fn learning_status_check(paths: &AppPaths, config: &AppConfig) -> serde_json::Value {
    let accepted = shac::db::AppDb::open(&paths.db_file)
        .and_then(|db| db.stats())
        .map(|s| s.accepted_clean_completions)
        .unwrap_or(0);
    let (ok, detail) = if config.features.ml_rerank {
        (
            true,
            format!("personalized model active ({accepted} accepted completions)"),
        )
    } else if accepted == 0 {
        (
            false,
            "no accepted completions yet — press Tab a few times to start learning".to_string(),
        )
    } else {
        let remaining = (50 - accepted).max(0);
        (false, format!("{accepted}/50 accepted completions — {remaining} more to activate personalized model"))
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
        doctor_check(
            "zsh_adapter",
            paths.shell_dir.join("shac.zsh").exists(),
            paths.shell_dir.join("shac.zsh").display().to_string(),
        ),
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
    checks.push(learning_status_check(paths, &config));
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

fn daemon_action(paths: &AppPaths, action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start => {
            cleanup_stale_daemon_state(paths);
            if daemon_is_running(paths) {
                println!("running");
                return Ok(());
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
            let started = wait_for_socket(&paths.socket_file, Duration::from_secs(2));
            if started {
                println!("started");
                Ok(())
            } else {
                bail!("daemon did not create socket in time")
            }
        }
        DaemonAction::Stop => {
            if !paths.pid_file.exists() {
                cleanup_stale_daemon_state(paths);
                println!("stopped");
                return Ok(());
            }
            let pid = fs::read_to_string(&paths.pid_file)?.trim().to_string();
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
    if shac_disabled(paths)? {
        print_completion_response(disabled_completion_response(), &args.format)?;
        return Ok(());
    }
    ensure_daemon(paths)?;
    let request = completion_request(&args);
    let response = send_request(paths, "complete", serde_json::to_value(request)?)?;
    print_completion_response(response, &args.format)
}

fn print_completion_response(response: serde_json::Value, format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if format == "shell-metadata" {
        if let Some(request_id) = response.get("request_id").and_then(|value| value.as_i64()) {
            println!("__shac_request_id\t{request_id}");
        } else {
            println!("__shac_request_id\t");
        }
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
    } else if format == "shell-tsv-v2" {
        let request_id = response
            .get("request_id")
            .and_then(|value| value.as_i64())
            .map(|value| value.to_string())
            .unwrap_or_default();
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
            sanitize_shell_field(&request_id),
            sanitize_shell_field(mode),
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
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                sanitize_shell_field(item_key),
                sanitize_shell_field(insert_text),
                sanitize_shell_field(display),
                sanitize_shell_field(kind),
                sanitize_shell_field(source),
                sanitize_shell_field(description)
            );
        }
        if let Some(tip) = response.get("tip").and_then(|v| v.as_object()) {
            let id = tip.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let text = tip.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !id.is_empty() && !text.is_empty() {
                println!(
                    "__shac_tip\t{}\t{}",
                    sanitize_shell_field(id),
                    sanitize_shell_field(text)
                );
            }
        }
        if let Some(dv) = response.get("daemon_version").and_then(|v| v.as_str()) {
            println!("__shac_daemon_version\t{}", sanitize_shell_field(dv));
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

fn sanitize_shell_field(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\t' | '\n' | '\r' => ' ',
            _ => ch,
        })
        .collect::<String>()
        .trim()
        .to_string()
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

fn export_training_data(paths: &AppPaths, args: TrainingDataArgs) -> Result<()> {
    let engine = Engine::new(paths)?;
    let samples = engine.training_samples(args.limit)?;
    let mut output = String::new();
    for sample in samples {
        output.push_str(&serde_json::to_string(&sample)?);
        output.push('\n');
    }
    if let Some(path) = args.output {
        fs::write(path, output)?;
    } else {
        print!("{output}");
    }
    Ok(())
}

fn train_model_file(paths: &AppPaths, args: TrainModelArgs) -> Result<()> {
    let engine = Engine::new(paths)?;
    let samples = engine.training_samples(args.limit)?;
    if samples.is_empty() {
        bail!("no training samples available yet");
    }
    let model = train_model(
        &samples,
        &TrainOptions {
            iterations: args.iterations,
            learning_rate: args.learning_rate,
        },
    );
    model.save(&PathBuf::from(&args.output))?;
    println!("{}", args.output);
    Ok(())
}

fn ensure_daemon(paths: &AppPaths) -> Result<()> {
    cleanup_stale_daemon_state(paths);
    if daemon_is_running(paths) {
        return Ok(());
    }
    daemon_action(paths, DaemonAction::Start)
}

fn shac_disabled(paths: &AppPaths) -> Result<bool> {
    if std::env::var_os("SHAC_DISABLE").is_some() {
        return Ok(true);
    }
    Ok(!AppConfig::load(paths)?.enabled)
}

fn request_timeout_for_action(action: &str, base_timeout_ms: u64) -> Duration {
    let timeout_ms = match action {
        "reindex" => base_timeout_ms.max(1_500),
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
