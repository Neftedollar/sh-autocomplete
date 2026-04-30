use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{write_jsonl, SyntheticEvent, SCHEMA_VERSION};
use shac_ml_train::personas::{self, Persona};
use shac_ml_train::qwen::{GenerationConfig, Qwen, QwenLike};

#[derive(Parser, Debug)]
#[command(about = "Generate synthetic shell history JSONL via local Qwen 0.5B")]
struct Args {
    /// Path to personas.toml
    #[arg(long, default_value = "ml/data/personas.toml")]
    personas: PathBuf,

    /// Output directory; writes synthetic-{os}.jsonl per OS
    #[arg(long, default_value = "ml/data")]
    out_dir: PathBuf,

    /// Restrict to a single OS (darwin/linux). If unset, generates both.
    #[arg(long)]
    os: Option<String>,

    /// Restrict to a single persona by id (for spot-checking).
    #[arg(long)]
    persona: Option<String>,

    /// Don't write JSONL, just print first 10 generated commands.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let personas = personas::load(&args.personas)?;
    let qwen = Qwen::load().await?;

    let by_os = group_by_os(&personas, args.os.as_deref(), args.persona.as_deref());
    for (os, persona_list) in by_os {
        let mut events: Vec<SyntheticEvent> = Vec::new();
        for persona in persona_list {
            for session_idx in 0..persona.sessions_to_generate {
                let session = generate_session(&qwen, persona, session_idx)?;
                if args.dry_run {
                    for ev in session.iter().take(10) {
                        println!("[dry] {} | {} | {}", ev.persona_id, ev.cwd, ev.command);
                    }
                    return Ok(());
                }
                events.extend(session);
            }
        }
        let out_path = args.out_dir.join(format!("synthetic-{os}.jsonl"));
        write_jsonl(&out_path, &events)?;
        eprintln!("wrote {} events to {}", events.len(), out_path.display());
    }
    Ok(())
}

fn group_by_os<'a>(
    personas: &'a [Persona],
    os_filter: Option<&str>,
    persona_filter: Option<&str>,
) -> Vec<(String, Vec<&'a Persona>)> {
    let mut darwin: Vec<&Persona> = Vec::new();
    let mut linux: Vec<&Persona> = Vec::new();
    for p in personas {
        if let Some(f) = os_filter {
            if f != p.os {
                continue;
            }
        }
        if let Some(f) = persona_filter {
            if f != p.id {
                continue;
            }
        }
        match p.os.as_str() {
            "darwin" => darwin.push(p),
            "linux" => linux.push(p),
            _ => {}
        }
    }
    let mut out = Vec::new();
    if !darwin.is_empty() {
        out.push(("darwin".to_string(), darwin));
    }
    if !linux.is_empty() {
        out.push(("linux".to_string(), linux));
    }
    out
}

fn generate_session(
    qwen: &dyn QwenLike,
    persona: &Persona,
    session_idx: usize,
) -> Result<Vec<SyntheticEvent>> {
    let system = format!(
        "You are simulating a developer's shell history. Output ONE shell command per line, \
         no comments, no shell prompts, no explanations. Do not emit lines that contain \
         multiple commands separated by `;`. Stay in character. Tools available: {}.",
        persona.tools_installed.join(", ")
    );
    let user = format!(
        "Persona description:\n{}\n\nWorking directory pattern: {}\n\n\
         Generate {} realistic shell commands for one session by this user, one per line.",
        persona.style_prompt.trim(),
        persona.cwd_pattern,
        persona.typical_session_length,
    );
    let raw = qwen
        .generate(&system, &user, &GenerationConfig::default())
        .with_context(|| format!("generate session {} for {}", session_idx, persona.id))?;

    let mut events = Vec::new();
    let mut prev: Option<String> = None;
    for (line_idx, line) in raw.lines().enumerate() {
        let cmd = clean_command(line);
        if cmd.is_empty() || !is_plausible_command(&cmd) {
            continue;
        }
        events.push(SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: persona.id.clone(),
            os: persona.os.clone(),
            cwd: persona.cwd_pattern.clone(),
            command: cmd.clone(),
            prev_command: prev.clone(),
            ts_offset_secs: line_idx as i64 * 30,
        });
        prev = Some(cmd);
    }
    // Drop sessions that came back as prompt-loops or empty
    if events.len() < 3 {
        return Ok(Vec::new());
    }
    let unique = events
        .iter()
        .map(|e| e.command.split_whitespace().next().unwrap_or(""))
        .collect::<std::collections::HashSet<_>>()
        .len();
    if unique < 3 {
        return Ok(Vec::new());
    }
    Ok(events)
}

fn clean_command(line: &str) -> String {
    let line = line.trim();
    // Strip leading prompts the model sometimes invents
    for prefix in ["$ ", "% ", "> ", "# "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    line.to_string()
}

fn is_plausible_command(cmd: &str) -> bool {
    if cmd.is_empty() || cmd.len() > 240 {
        return false;
    }
    if cmd.contains('\n') || cmd.contains('\r') {
        return false;
    }
    // Reject lines that look like Markdown bullets or prose
    if cmd.starts_with('-') && cmd.contains(' ') && !cmd.starts_with("--") {
        // probably "- some bullet" not a flag
        return false;
    }
    // First word must look tool-ish
    let first = cmd.split_whitespace().next().unwrap_or("");
    !first.is_empty()
        && first
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/')
}
