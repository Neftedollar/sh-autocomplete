use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{
    read_jsonl, write_jsonl, DistilledExample, SyntheticEvent, SCHEMA_VERSION,
};
use shac_ml_train::qwen::{Qwen, QwenLike};
use shac_ml_train::tokenizer::{tokenize_command, Vocab};

const CONTEXT_LEN: usize = 16;
const TOP_K_TEACHER: usize = 50;

/// Abort threshold for the hard-label <UNK> fraction when reusing an
/// existing --vocab file (see the cross-OS vocab reuse guard in `main`).
const UNK_FRACTION_ABORT_THRESHOLD: f32 = 0.05;

#[derive(Parser, Debug)]
#[command(about = "Distill Qwen teacher soft targets from scrubbed SyntheticEvent JSONL")]
struct Args {
    #[arg(long)]
    input: PathBuf,

    #[arg(long, default_value = "ml/models/vocab.json")]
    vocab: PathBuf,

    #[arg(long)]
    output: PathBuf,

    /// Optional held-out split, taken chronologically per persona (see
    /// --val-fraction). Val examples are always excluded from --output.
    #[arg(long)]
    val_output: Option<PathBuf>,

    /// Fraction of each persona's examples (the trailing, most recent ones)
    /// held out into --val-output. Ignored unless --val-output is set.
    #[arg(long, default_value_t = 0.2)]
    val_fraction: f32,

    #[arg(long, default_value_t = 2000)]
    max_vocab: usize,

    #[arg(long, default_value_t = 8)]
    cwd_buckets: u8,

    /// Skip the abort-on-high-<UNK> guard that fires when --vocab is loaded
    /// from an existing file and can't represent this corpus (e.g. reusing
    /// one OS's vocab for another).
    #[arg(long, default_value_t = false)]
    allow_vocab_reuse: bool,
}

/// FNV-1a. `DefaultHasher` (SipHash) is explicitly unspecified by std and
/// free to change between Rust releases, which would silently reshuffle cwd
/// bucket assignments across builds; FNV-1a is a fixed algorithm so dataset
/// generation stays reproducible.
fn bucket_cwd(cwd: &str, n_buckets: u8) -> u8 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in cwd.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash % n_buckets as u64) as u8
}

async fn build_example(
    session: &[&SyntheticEvent],
    vocab: &Vocab,
    qwen: &impl QwenLike,
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
    let start = prev_count.saturating_sub(context_slots);
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
    let dist = match qwen.next_token_distribution(&prompt, TOP_K_TEACHER).await {
        Ok(d) => d,
        Err(e) => return Some(Err(e)),
    };

    // Project Qwen tokens → student vocab ids. Whitespace-only teacher
    // tokens and words the vocab doesn't have are dropped, not redirected to
    // <UNK> — collapsing OOV mass onto <UNK> would teach the student to
    // predict it. <PAD> is likewise never a valid soft-target destination.
    let unk_id = vocab.id_of("<UNK>").expect("UNK is a fixed special token");
    let pad_special_id = vocab.id_of("<PAD>").expect("PAD is a fixed special token");

    let mut accum: HashMap<u32, f32> = HashMap::new();
    let mut total_mass = 0.0f32;
    for (qwen_token, prob) in &dist {
        let first_word = qwen_token.split_whitespace().next().unwrap_or("");
        if first_word.is_empty() {
            continue;
        }
        let student_id = match vocab.id_of(first_word) {
            Some(id) if id != unk_id && id != pad_special_id => id,
            _ => continue,
        };
        *accum.entry(student_id).or_insert(0.0) += prob;
        total_mass += prob;
    }

    // Renormalize surviving mass
    if total_mass > 0.0 {
        for v in accum.values_mut() {
            *v /= total_mass;
        }
    }

    // Sort descending by probability, truncate to TOP_K_TEACHER. If every
    // teacher token was filtered out above, fall back to one-hot on the
    // hard label rather than shipping an empty/all-<UNK> distribution.
    let mut soft_targets_top: Vec<(u32, f32)> = if accum.is_empty() {
        vec![(hard_label, 1.0)]
    } else {
        accum.into_iter().collect()
    };
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

/// True if `ev` starts a new session relative to `current`: either the
/// (persona_id, session_id) key changed, or `prev_command` is `None` — a
/// defensive fallback for malformed/pre-session-id input, where the first
/// event of a genuinely new session might not have bumped session_id.
fn is_session_boundary(current: &Option<(String, u32)>, ev: &SyntheticEvent) -> bool {
    ev.prev_command.is_none()
        || match current {
            Some((persona, session)) => *persona != ev.persona_id || *session != ev.session_id,
            None => true,
        }
}

/// Split `examples` into (train, val) chronologically per persona: the
/// trailing `val_fraction` of each persona's own examples (in build order,
/// which follows session/event order) go to val, the rest to train. Val
/// examples are never included in the train half.
fn split_train_val_per_persona(
    examples: Vec<DistilledExample>,
    personas: &[String],
    val_fraction: f32,
) -> (Vec<DistilledExample>, Vec<DistilledExample>) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in personas {
        *counts.entry(p.as_str()).or_insert(0) += 1;
    }
    // Index within each persona's own sequence at which the trailing
    // val_fraction begins; examples before it stay in train.
    let val_start: HashMap<&str, usize> = counts
        .into_iter()
        .map(|(persona, n)| {
            let n_val = ((n as f32) * val_fraction) as usize;
            (persona, n - n_val.min(n))
        })
        .collect();

    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut train = Vec::with_capacity(examples.len());
    let mut val = Vec::new();
    for (example, persona) in examples.into_iter().zip(personas.iter()) {
        let idx = seen.entry(persona.as_str()).or_insert(0);
        if *idx >= val_start[persona.as_str()] {
            val.push(example);
        } else {
            train.push(example);
        }
        *idx += 1;
    }
    (train, val)
}

/// Drop examples whose hard label is `<UNK>`, keeping `personas` aligned by
/// index with the returned examples. Training the student to predict `<UNK>`
/// is pure noise, and `build_example`'s all-OOV soft-target fallback
/// one-hots onto the hard label — including `<UNK>` itself when the target
/// command's first token isn't in the vocab — so this is the only place that
/// hazard is actually filtered out of the emitted dataset.
fn filter_unk_hard_label(
    examples: Vec<DistilledExample>,
    personas: Vec<String>,
    unk_id: u32,
) -> (Vec<DistilledExample>, Vec<String>, usize) {
    let mut kept_examples = Vec::with_capacity(examples.len());
    let mut kept_personas = Vec::with_capacity(personas.len());
    let mut excluded = 0usize;
    for (example, persona) in examples.into_iter().zip(personas) {
        if example.hard_label == unk_id {
            excluded += 1;
        } else {
            kept_examples.push(example);
            kept_personas.push(persona);
        }
    }
    (kept_examples, kept_personas, excluded)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Read events
    let events: Vec<SyntheticEvent> =
        read_jsonl(&args.input).with_context(|| format!("read {}", args.input.display()))?;

    // 2. Load or build vocab
    let vocab_reused = args.vocab.exists();
    let vocab = if vocab_reused {
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

    // 4. Iterate events, grouped by (persona_id, session_id) — a session
    // boundary also fires on prev_command == None as a defensive fallback
    // for malformed or pre-session-id input. Without this, gen_synthetic's
    // ~50 independent sessions per persona would run together into one
    // context window.
    let mut examples: Vec<DistilledExample> = Vec::new();
    let mut example_personas: Vec<String> = Vec::new(); // parallel to examples, for the val split below
    let mut session_buf: Vec<&SyntheticEvent> = Vec::new();
    let mut current_session: Option<(String, u32)> = None;

    for ev in &events {
        if is_session_boundary(&current_session, ev) {
            session_buf.clear();
            current_session = Some((ev.persona_id.clone(), ev.session_id));
        }
        session_buf.push(ev);

        if let Some(result) = build_example(&session_buf, &vocab, &qwen, args.cwd_buckets).await {
            examples.push(result?);
            example_personas.push(ev.persona_id.clone());
        }
    }

    // 5. Guard against silent cross-OS vocab reuse: a reused vocab that
    // can't represent this corpus collapses every unrecognized hard label
    // onto <UNK>, silently starving the student on the new OS's vocabulary.
    let unk_id = vocab.id_of("<UNK>").expect("UNK is a fixed special token");
    let unk_count = examples.iter().filter(|e| e.hard_label == unk_id).count();
    let unk_fraction = if examples.is_empty() {
        0.0
    } else {
        unk_count as f32 / examples.len() as f32
    };
    eprintln!(
        "hard-label <UNK> fraction: {:.1}% ({unk_count}/{})",
        unk_fraction * 100.0,
        examples.len()
    );
    if vocab_reused && unk_fraction > UNK_FRACTION_ABORT_THRESHOLD && !args.allow_vocab_reuse {
        anyhow::bail!(
            "reused vocab {} produced {:.1}% <UNK> hard labels (> {:.0}%) — likely cross-OS \
             vocab reuse. Build a per-OS vocab instead (convention: ml/models/vocab-<os>.json), \
             or pass --allow-vocab-reuse to proceed anyway.",
            args.vocab.display(),
            unk_fraction * 100.0,
            UNK_FRACTION_ABORT_THRESHOLD * 100.0,
        );
    }

    // 6. Exclude examples with hard label <UNK> from everything we emit.
    // Filtered *after* the guard above so the guard's fraction still
    // reflects the full, unfiltered corpus.
    let (examples, example_personas, excluded_unk) =
        filter_unk_hard_label(examples, example_personas, unk_id);
    eprintln!("excluded {excluded_unk} example(s) with hard-label <UNK> from output");

    // 7. Split (if requested) and write output. Val examples never appear
    // in --output.
    let (train_examples, val_examples) = match &args.val_output {
        Some(_) => split_train_val_per_persona(examples, &example_personas, args.val_fraction),
        None => (examples, Vec::new()),
    };

    write_jsonl(&args.output, &train_examples)?;
    eprintln!(
        "wrote {} train examples to {}",
        train_examples.len(),
        args.output.display()
    );
    if let Some(val_output) = &args.val_output {
        write_jsonl(val_output, &val_examples)?;
        eprintln!(
            "wrote {} val examples to {}",
            val_examples.len(),
            val_output.display()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shac_ml_train::qwen::MockQwen;
    use std::collections::HashSet;

    fn ev(persona: &str, session: u32, cmd: &str, prev: Option<&str>) -> SyntheticEvent {
        SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: persona.to_string(),
            session_id: session,
            os: "darwin".to_string(),
            cwd: "/Users/x/proj".to_string(),
            command: cmd.to_string(),
            prev_command: prev.map(str::to_string),
            ts_offset_secs: 0,
        }
    }

    // ---- finding #5: session-boundary reset --------------------------------

    #[test]
    fn session_boundary_on_persona_change() {
        let current = Some(("alice".to_string(), 0));
        let next = ev("bob", 0, "ls", Some("cd /tmp"));
        assert!(is_session_boundary(&current, &next));
    }

    #[test]
    fn session_boundary_on_session_id_change() {
        let current = Some(("alice".to_string(), 0));
        let next = ev("alice", 1, "ls", Some("cd /tmp"));
        assert!(is_session_boundary(&current, &next));
    }

    #[test]
    fn no_boundary_within_same_session() {
        let current = Some(("alice".to_string(), 0));
        let next = ev("alice", 0, "ls", Some("cd /tmp"));
        assert!(!is_session_boundary(&current, &next));
    }

    #[test]
    fn session_boundary_on_missing_prev_command_even_if_key_unchanged() {
        // Defensive fallback: malformed/pre-session-id data where the first
        // event of a genuinely new session forgot to bump session_id.
        let current = Some(("alice".to_string(), 0));
        let next = ev("alice", 0, "ls", None);
        assert!(is_session_boundary(&current, &next));
    }

    // ---- finding #7: soft-target mass never lands on <UNK>/<PAD> -----------

    #[tokio::test]
    async fn soft_targets_drop_oov_and_renormalize() {
        let vocab =
            Vocab::build_from_corpus(&["git commit".to_string(), "cargo build".to_string()], 50);
        let prev = ev("alice", 0, "git commit", None);
        let target = ev("alice", 0, "cargo build", Some("git commit"));
        let session = vec![&prev, &target];

        let qwen = MockQwen {
            canned_distribution: vec![
                ("git".to_string(), 0.5),              // in vocab -> survives
                (" ".to_string(), 0.3),                // whitespace-only -> dropped
                ("xyz-not-in-vocab".to_string(), 0.2), // OOV -> dropped, not <UNK>
            ],
            ..MockQwen::default()
        };

        let example = build_example(&session, &vocab, &qwen, 8)
            .await
            .expect("session long enough to produce an example")
            .expect("build_example should not error");

        let unk_id = vocab.id_of("<UNK>").unwrap();
        let pad_id = vocab.id_of("<PAD>").unwrap();
        assert!(
            example
                .soft_targets_top
                .iter()
                .all(|&(id, _)| id != unk_id && id != pad_id),
            "no soft mass should land on <UNK>/<PAD>: {:?}",
            example.soft_targets_top
        );
        assert_eq!(example.soft_targets_top.len(), 1);
        let (id, prob) = example.soft_targets_top[0];
        assert_eq!(id, vocab.encode_word("git"));
        assert!(
            (prob - 1.0).abs() < 1e-6,
            "surviving mass should renormalize to 1.0, got {prob}"
        );
    }

    #[tokio::test]
    async fn soft_targets_fall_back_to_one_hot_when_all_teacher_tokens_are_oov() {
        let vocab =
            Vocab::build_from_corpus(&["git commit".to_string(), "cargo build".to_string()], 50);
        let prev = ev("alice", 0, "git commit", None);
        let target = ev("alice", 0, "cargo build", Some("git commit"));
        let session = vec![&prev, &target];

        let qwen = MockQwen {
            canned_distribution: vec![
                ("   ".to_string(), 0.6),
                ("totally-unknown-word".to_string(), 0.4),
            ],
            ..MockQwen::default()
        };

        // Here the target's hard label ("cargo") is in-vocab; the case where
        // it *isn't* is covered separately below, since that's the one
        // main()'s post-guard filter has to catch.
        let example = build_example(&session, &vocab, &qwen, 8)
            .await
            .unwrap()
            .unwrap();

        let hard_label = vocab.encode_word("cargo");
        assert_eq!(example.hard_label, hard_label);
        assert_eq!(example.soft_targets_top, vec![(hard_label, 1.0)]);
    }

    #[tokio::test]
    async fn soft_targets_fallback_can_land_on_unk_hard_label_when_target_is_oov() {
        // build_example's contract is unchanged: when the target command's
        // first token isn't in the vocab, the hard label is <UNK>, and if
        // every teacher token is also OOV, the one-hot fallback lands on
        // that same <UNK> hard label. This is exactly the hazard
        // `filter_unk_hard_label` (exercised below) removes before any
        // example reaches --output/--val-output.
        let vocab =
            Vocab::build_from_corpus(&["git commit".to_string(), "cargo build".to_string()], 50);
        let prev = ev("alice", 0, "git commit", None);
        let target = ev("alice", 0, "zzz-totally-unseen-command", Some("git commit"));
        let session = vec![&prev, &target];

        let qwen = MockQwen {
            canned_distribution: vec![
                ("   ".to_string(), 0.6),
                ("totally-unknown-word".to_string(), 0.4),
            ],
            ..MockQwen::default()
        };

        let example = build_example(&session, &vocab, &qwen, 8)
            .await
            .unwrap()
            .unwrap();

        let unk_id = vocab.id_of("<UNK>").unwrap();
        assert_eq!(example.hard_label, unk_id);
        assert_eq!(example.soft_targets_top, vec![(unk_id, 1.0)]);
    }

    // ---- distill main(): emitted examples never have hard label <UNK> -----

    #[test]
    fn filter_unk_hard_label_drops_unk_and_counts_them() {
        let vocab =
            Vocab::build_from_corpus(&["git commit".to_string(), "cargo build".to_string()], 50);
        let unk_id = vocab.id_of("<UNK>").unwrap();
        let git_id = vocab.encode_word("git");
        let cargo_id = vocab.encode_word("cargo");

        let example_with = |hard_label: u32| DistilledExample {
            schema_version: SCHEMA_VERSION,
            os: "darwin".to_string(),
            cwd_bucket: 0,
            context_tokens: vec![0; 16],
            hard_label,
            soft_targets_top: vec![(hard_label, 1.0)],
        };
        let examples = vec![
            example_with(git_id),
            example_with(unk_id),
            example_with(cargo_id),
        ];
        let personas = vec!["alice".to_string(), "bob".to_string(), "alice".to_string()];

        let (kept, kept_personas, excluded) = filter_unk_hard_label(examples, personas, unk_id);

        assert_eq!(excluded, 1);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|e| e.hard_label != unk_id));
        // Personas must stay aligned with the surviving examples by index.
        assert_eq!(
            kept_personas,
            vec!["alice".to_string(), "alice".to_string()]
        );
    }

    // ---- adjacent issue B: per-persona chronological val split -------------

    #[test]
    fn split_train_val_per_persona_takes_trailing_fraction_per_persona() {
        // alice: 10 examples, bob: 5 examples, in chronological (build) order.
        let personas: Vec<String> = std::iter::repeat_n("alice".to_string(), 10)
            .chain(std::iter::repeat_n("bob".to_string(), 5))
            .collect();
        let examples: Vec<DistilledExample> = (0..personas.len())
            .map(|i| DistilledExample {
                schema_version: SCHEMA_VERSION,
                os: "darwin".to_string(),
                cwd_bucket: 0,
                context_tokens: vec![0; 16],
                hard_label: i as u32,
                soft_targets_top: vec![(i as u32, 1.0)],
            })
            .collect();

        let (train, val) = split_train_val_per_persona(examples, &personas, 0.2);

        // alice: 10 * 0.2 = 2 val, 8 train; bob: 5 * 0.2 = 1 val, 4 train.
        assert_eq!(train.len(), 12);
        assert_eq!(val.len(), 3);

        // Val must hold each persona's chronologically last examples.
        let alice_val: Vec<u32> = val
            .iter()
            .map(|e| e.hard_label)
            .filter(|&h| h < 10)
            .collect();
        assert_eq!(alice_val, vec![8, 9]);
        let bob_val: Vec<u32> = val
            .iter()
            .map(|e| e.hard_label)
            .filter(|&h| h >= 10)
            .collect();
        assert_eq!(bob_val, vec![14]);

        // Val examples must never also appear in train.
        let train_labels: HashSet<u32> = train.iter().map(|e| e.hard_label).collect();
        let val_labels: HashSet<u32> = val.iter().map(|e| e.hard_label).collect();
        assert!(train_labels.is_disjoint(&val_labels));
    }
}
