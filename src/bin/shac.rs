use std::fs;
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use shac::config::{AppConfig, AppPaths};
use shac::engine::Engine;
use shac::indexer;
use shac::ml::{train_model, TrainOptions};
use shac::protocol::{CompletionRequest, ExplainResponse, RecordCommandRequest, SessionInfo};
use shac::shell::{BASH_COMPLETION, ZSH_COMPLETION};

#[derive(Debug, Parser)]
#[command(version, about = "Shell autocomplete engine CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Install(InstallArgs),
    Uninstall(InstallArgs),
    Daemon(DaemonArgs),
    Index(IndexArgs),
    Doctor(DoctorArgs),
    Debug(DebugArgs),
    Reindex,
    Explain(CompletionArgs),
    Complete(CompletionArgs),
    RecordCommand(RecordArgs),
    Config(ConfigArgs),
    Stats,
    MigrationStatus,
    RecentEvents(RecentEventsArgs),
    ShellEnv(ShellEnvArgs),
    ResetPersonalization,
    ExportTrainingData(TrainingDataArgs),
    TrainModel(TrainModelArgs),
}

#[derive(Debug, Args)]
struct InstallArgs {
    #[arg(long, value_enum)]
    shell: ShellKind,
    #[arg(long)]
    edit_rc: bool,
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
    let cli = Cli::parse();
    let paths = AppPaths::discover()?;
    paths.ensure()?;

    match cli.command {
        Commands::Install(args) => install(&paths, args.shell, args.edit_rc),
        Commands::Uninstall(args) => uninstall(&paths, args.shell, args.edit_rc),
        Commands::Daemon(args) => daemon_action(&paths, args.action),
        Commands::Index(args) => index_action(&paths, args.action),
        Commands::Doctor(args) => doctor(&paths, args),
        Commands::Debug(args) => debug_action(&paths, args.action),
        Commands::Reindex => {
            ensure_daemon(&paths)?;
            let value = send_request(
                &paths,
                "reindex",
                serde_json::json!({ "path_env": std::env::var("PATH").ok() }),
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Commands::Explain(args) => explain(&paths, args),
        Commands::Complete(args) => complete(&paths, args),
        Commands::RecordCommand(args) => {
            ensure_daemon(&paths)?;
            send_request(
                &paths,
                "record-command",
                serde_json::to_value(RecordCommandRequest {
                    command: args.command,
                    cwd: canonicalize_lossy(&args.cwd),
                    shell: args.shell,
                    trust: args.trust,
                    provenance: args.provenance,
                    provenance_source: args.provenance_source,
                    provenance_confidence: args.provenance_confidence,
                    origin: args.origin,
                    tty_present: Some(args.tty_present || std::io::stdin().is_terminal()),
                    exit_status: args.exit_status,
                    accepted_request_id: args.accepted_request_id,
                    accepted_item_key: args.accepted_item_key,
                    accepted_rank: args.accepted_rank,
                })?,
            )?;
            Ok(())
        }
        Commands::Config(args) => config_action(&paths, args.action),
        Commands::Stats => {
            ensure_daemon(&paths)?;
            let value = send_request(&paths, "stats", serde_json::json!({}))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(())
        }
        Commands::MigrationStatus => migration_status(&paths),
        Commands::RecentEvents(args) => recent_events(&paths, args),
        Commands::ShellEnv(args) => shell_env(&paths, args),
        Commands::ResetPersonalization => reset_personalization(&paths),
        Commands::ExportTrainingData(args) => export_training_data(&paths, args),
        Commands::TrainModel(args) => train_model_file(&paths, args),
    }
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
        (true, format!("personalized model active ({accepted} accepted completions)"))
    } else if accepted == 0 {
        (false, "no accepted completions yet — press Tab a few times to start learning".to_string())
    } else {
        let remaining = (50 - accepted).max(0);
        (false, format!("{accepted}/50 accepted completions — {remaining} more to activate personalized model"))
    };
    doctor_check("learning_status", ok, detail)
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
    if matches!(args.shell, Some(ShellKind::Zsh)) {
        checks.extend(zsh_doctor_checks(paths)?);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in checks {
            println!(
                "{:<18} {:<4} {}",
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

fn install(paths: &AppPaths, shell: ShellKind, edit_rc: bool) -> Result<()> {
    let (file_name, content, snippet) = match shell {
        ShellKind::Bash => (
            "shac.bash",
            BASH_COMPLETION,
            format!("source {}", paths.shell_dir.join("shac.bash").display()),
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
        install_rc_block(shell, &shell_file)?;
        println!("installed  {}", rc_file.display());
        println!();
        println!("next steps:");
        println!("  1. open a new terminal (or run: source {})", rc_file.display());
        println!("  2. try pressing Tab after: git <Tab>  or  cargo <Tab>");
        println!("  3. run `shac doctor` if something looks off");
    } else {
        println!("{snippet}");
    }
    Ok(())
}

fn uninstall(paths: &AppPaths, shell: ShellKind, edit_rc: bool) -> Result<()> {
    let file_name = match shell {
        ShellKind::Bash => "shac.bash",
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
    let block = managed_rc_block(shell_file);
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

fn managed_rc_block(shell_file: &Path) -> String {
    format!(
        "{SHAC_RC_BEGIN}\nif [ -f {} ]; then\n  source {}\nfi\n{SHAC_RC_END}\n",
        shell_escape(&shell_file.to_string_lossy()),
        shell_escape(&shell_file.to_string_lossy())
    )
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
    CompletionRequest {
        shell: args.shell.clone(),
        line: args.line.clone(),
        cursor: args.cursor,
        cwd: canonicalize_lossy(&args.cwd),
        env: std::env::vars().collect(),
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
