//! `shac-ml-eval` — evaluate a trained `.bpk` on held-out distilled JSONL.
//!
//! Loads a trained student model, runs inference on held-out examples, and
//! prints top-1, top-3, and top-5 accuracy.

use std::path::PathBuf;

use anyhow::{Context, Result};
use burn::backend::NdArray;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};
use clap::Parser;
use shac_ml_train::data::{read_jsonl, DistilledExample};
use shac_ml_train::model::{StudentModel, StudentModelConfig};
use shac_ml_train::tokenizer::Vocab;

type B = NdArray<f32>;

#[derive(Parser, Debug)]
#[command(about = "Evaluate a trained .bpk on a held-out distilled JSONL")]
struct Args {
    /// Trained .bpk weights
    #[arg(long)]
    model: PathBuf,

    /// Held-out distilled JSONL. For the acceptance gate this MUST be the
    /// `shac-ml-distill --val-output` file, never the file --model was
    /// trained on (finding #8).
    #[arg(long)]
    input: PathBuf,

    /// Vocab JSON --input was built against (same file passed to
    /// `shac-ml-train --vocab`). Sets the model's vocab_size — never
    /// hardcode it independently of the actual vocab (finding #9).
    #[arg(long)]
    vocab: PathBuf,
}

/// Fail fast if any context/hard token id in `examples` falls outside
/// `0..vocab_size` — otherwise top-k accuracy is silently wrong instead of
/// erroring (finding #9).
fn validate_token_ids(examples: &[DistilledExample], vocab_size: usize) -> Result<()> {
    for (i, ex) in examples.iter().enumerate() {
        for &tok in &ex.context_tokens {
            if tok as usize >= vocab_size {
                anyhow::bail!(
                    "example {i}: context token id {tok} >= vocab_size {vocab_size} \
                     (--input was not built from --vocab)"
                );
            }
        }
        if ex.hard_label as usize >= vocab_size {
            anyhow::bail!(
                "example {i}: hard_label {} >= vocab_size {vocab_size} \
                 (--input was not built from --vocab)",
                ex.hard_label
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let device = Default::default();

    // Load the actual vocab used to build --input; vocab_size MUST come from
    // here, never from StudentModelConfig::default() (finding #9).
    let vocab_json = std::fs::read_to_string(&args.vocab)
        .with_context(|| format!("read vocab {}", args.vocab.display()))?;
    let vocab = Vocab::from_json(&vocab_json)
        .with_context(|| format!("parse vocab {}", args.vocab.display()))?;
    let cfg = StudentModelConfig::for_vocab_size(vocab.size());

    let mut model: StudentModel<B> = cfg.init(&device);
    let mut store = BurnpackStore::from_file(&args.model).auto_extension(false);
    model
        .load_from(&mut store)
        .with_context(|| format!("load .bpk from {}", args.model.display()))?;

    let examples: Vec<DistilledExample> = read_jsonl(&args.input)?;
    if examples.is_empty() {
        eprintln!("warning: no examples in {}", args.input.display());
        return Ok(());
    }
    validate_token_ids(&examples, cfg.vocab_size).with_context(|| {
        format!(
            "{} does not match vocab {}",
            args.input.display(),
            args.vocab.display()
        )
    })?;

    let mut top1 = 0usize;
    let mut top3 = 0usize;
    let mut top5 = 0usize;

    let ctx_len = cfg.context_len;
    let vocab_size = cfg.vocab_size;

    for batch in examples.chunks(64) {
        let mut ctx_flat: Vec<i64> = Vec::with_capacity(batch.len() * ctx_len);
        for ex in batch {
            for i in 0..ctx_len {
                ctx_flat.push(ex.context_tokens.get(i).copied().unwrap_or(0) as i64);
            }
        }
        let input: Tensor<B, 2, Int> =
            Tensor::from_data(TensorData::new(ctx_flat, [batch.len(), ctx_len]), &device);
        let logits = model.forward(input);
        for (i, ex) in batch.iter().enumerate() {
            let row: Vec<f32> = logits
                .clone()
                .slice([i..i + 1, 0..vocab_size])
                .into_data()
                .to_vec()
                .map_err(|e| anyhow::anyhow!("decode logits row: {:?}", e))?;
            let mut scored: Vec<(usize, f32)> = row.into_iter().enumerate().collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let target = ex.hard_label as usize;
            if scored.first().map(|&(t, _)| t) == Some(target) {
                top1 += 1;
            }
            if scored.iter().take(3).any(|&(t, _)| t == target) {
                top3 += 1;
            }
            if scored.iter().take(5).any(|&(t, _)| t == target) {
                top5 += 1;
            }
        }
    }
    let n = examples.len() as f64;
    println!("Held-out evaluation ({} examples):", n);
    println!("  top-1: {:.3}", top1 as f64 / n);
    println!("  top-3: {:.3}", top3 as f64 / n);
    println!("  top-5: {:.3}", top5 as f64 / n);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(context_tokens: Vec<u32>, hard_label: u32) -> DistilledExample {
        DistilledExample {
            schema_version: 1,
            os: "darwin".to_string(),
            cwd_bucket: 0,
            context_tokens,
            hard_label,
            soft_targets_top: vec![],
        }
    }

    // ---- finding #9: vocab/dataset contract validation --------------------

    #[test]
    fn validate_token_ids_accepts_in_bounds_dataset() {
        let examples = vec![example(vec![0, 1, 2], 3)];
        assert!(validate_token_ids(&examples, 4).is_ok());
    }

    #[test]
    fn validate_token_ids_rejects_out_of_bounds_context_token() {
        let examples = vec![example(vec![0, 10], 1)];
        assert!(validate_token_ids(&examples, 4).is_err());
    }

    #[test]
    fn validate_token_ids_rejects_out_of_bounds_hard_label() {
        let examples = vec![example(vec![0, 1], 10)];
        assert!(validate_token_ids(&examples, 4).is_err());
    }
}
