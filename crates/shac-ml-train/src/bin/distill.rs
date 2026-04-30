use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{read_jsonl, write_jsonl, DistilledExample, SyntheticEvent, SCHEMA_VERSION};
use shac_ml_train::qwen::{Qwen, QwenLike};
use shac_ml_train::tokenizer::{tokenize_command, Vocab};

const CONTEXT_LEN: usize = 16;
const TOP_K_TEACHER: usize = 50;

#[derive(Parser, Debug)]
#[command(about = "Distill Qwen teacher soft targets from scrubbed SyntheticEvent JSONL")]
struct Args {
    #[arg(long)]
    input: PathBuf,

    #[arg(long, default_value = "ml/models/vocab.json")]
    vocab: PathBuf,

    #[arg(long)]
    output: PathBuf,

    #[arg(long, default_value_t = 2000)]
    max_vocab: usize,

    #[arg(long, default_value_t = 8)]
    cwd_buckets: u8,
}

fn bucket_cwd(cwd: &str, n_buckets: u8) -> u8 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    (h.finish() % n_buckets as u64) as u8
}

fn build_example(
    session: &[&SyntheticEvent],
    vocab: &Vocab,
    qwen: &dyn QwenLike,
    cwd_buckets: u8,
) -> Option<Result<DistilledExample>> {
    if session.len() < 2 {
        return None;
    }

    let target = *session.last().unwrap();
    let prev_events = &session[..session.len() - 1];

    // Hard label: first token of the target command
    let target_tokens = tokenize_command(&target.command);
    if target_tokens.is_empty() {
        return None;
    }
    let hard_label = vocab.encode_word(&target_tokens[0]);

    // Build context tokens of length CONTEXT_LEN:
    // [0] = <BOS>
    // [1..CONTEXT_LEN-1] = first tokens of last (CONTEXT_LEN-2) prev commands, oldest-first
    // [CONTEXT_LEN-1] = <PAD> (reserved for runtime prefix)
    let bos_id = vocab.encode_word("<BOS>");
    let pad_id = vocab.encode_word("<PAD>");

    let mut context_tokens = vec![pad_id; CONTEXT_LEN];
    context_tokens[0] = bos_id;
    // slot [CONTEXT_LEN-1] stays PAD

    // Fill slots [1..CONTEXT_LEN-1] with the last (CONTEXT_LEN-2) prev commands
    let context_slots = CONTEXT_LEN - 2; // 14 slots available (indices 1..14 inclusive)
    let prev_count = prev_events.len();
    let start = if prev_count > context_slots {
        prev_count - context_slots
    } else {
        0
    };
    let window = &prev_events[start..]; // oldest of the last context_slots, in order

    for (i, ev) in window.iter().enumerate() {
        let toks = tokenize_command(&ev.command);
        let first_tok = if toks.is_empty() {
            pad_id
        } else {
            vocab.encode_word(&toks[0])
        };
        context_tokens[1 + i] = first_tok;
    }

    // Build prompt with up to last 8 prev commands
    let last_8_start = if prev_events.len() > 8 {
        prev_events.len() - 8
    } else {
        0
    };
    let recent_cmds: Vec<&str> = prev_events[last_8_start..]
        .iter()
        .map(|ev| ev.command.as_str())
        .collect();
    let recent_joined = recent_cmds.join("; ");

    let prompt = format!(
        "User on {} in cwd {}. Recent commands: {}. \
         What is the most likely next command? Answer with just the first word/token of the command.",
        target.os, target.cwd, recent_joined
    );

    // Get soft targets from Qwen
    let dist = match qwen.next_token_distribution(&prompt, TOP_K_TEACHER) {
        Ok(d) => d,
        Err(e) => return Some(Err(e)),
    };

    // Project Qwen tokens → student vocab ids
    let mut accum: HashMap<u32, f32> = HashMap::new();
    let mut total_mass = 0.0f32;
    for (qwen_token, prob) in &dist {
        let first_word = qwen_token.split_whitespace().next().unwrap_or("");
        let student_id = vocab.encode_word(first_word);
        *accum.entry(student_id).or_insert(0.0) += prob;
        total_mass += prob;
    }

    // Renormalize
    if total_mass > 0.0 {
        for v in accum.values_mut() {
            *v /= total_mass;
        }
    }

    // Sort descending by probability, truncate to TOP_K_TEACHER
    let mut soft_targets_top: Vec<(u32, f32)> = accum.into_iter().collect();
    soft_targets_top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    soft_targets_top.truncate(TOP_K_TEACHER);

    let example = DistilledExample {
        schema_version: SCHEMA_VERSION,
        os: target.os.clone(),
        cwd_bucket: bucket_cwd(&target.cwd, cwd_buckets),
        context_tokens,
        hard_label,
        soft_targets_top,
    };

    Some(Ok(example))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Read events
    let events: Vec<SyntheticEvent> =
        read_jsonl(&args.input).with_context(|| format!("read {}", args.input.display()))?;

    // 2. Load or build vocab
    let vocab = if args.vocab.exists() {
        let json = std::fs::read_to_string(&args.vocab)
            .with_context(|| format!("read vocab {}", args.vocab.display()))?;
        Vocab::from_json(&json).with_context(|| format!("parse vocab {}", args.vocab.display()))?
    } else {
        let corpus: Vec<String> = events.iter().map(|e| e.command.clone()).collect();
        let v = Vocab::build_from_corpus(&corpus, args.max_vocab);
        if let Some(parent) = args.vocab.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        std::fs::write(&args.vocab, v.to_json()?)
            .with_context(|| format!("write vocab {}", args.vocab.display()))?;
        v
    };

    // 3. Load Qwen
    let qwen = Qwen::load().await.context("load Qwen model")?;

    // 4. Iterate events, grouped by persona_id
    let mut examples: Vec<DistilledExample> = Vec::new();
    let mut session_buf: Vec<&SyntheticEvent> = Vec::new();
    let mut current_persona: Option<String> = None;

    for ev in &events {
        if Some(&ev.persona_id) != current_persona.as_ref() {
            session_buf.clear();
            current_persona = Some(ev.persona_id.clone());
        }
        session_buf.push(ev);

        if let Some(result) = build_example(&session_buf, &vocab, &qwen, args.cwd_buckets) {
            examples.push(result?);
        }
    }

    // 5. Write output
    write_jsonl(&args.output, &examples)?;

    eprintln!(
        "wrote {} distilled examples to {}",
        examples.len(),
        args.output.display()
    );

    Ok(())
}
