//! `shac-ml-train` — burn training loop for the student model.
//!
//! Reads `DistilledExample` JSONL (T10 output), trains a [`StudentModel`] with
//! a mixed CE+KL distillation loss (Hinton 2015), and writes the final weights
//! as a `.bpk` file via `BurnpackStore`.
//!
//! # Loss function
//!
//! Per-step loss combines a hard cross-entropy term against the gold label
//! and a soft KL term against the teacher distribution at temperature `T`:
//!
//! ```text
//! L = α · CE(student, hard) + (1-α) · T² · KL(soft || softmax(student/T))
//! ```
//!
//! The `T²` rescale keeps the KL gradient magnitude on the same scale as the
//! CE term when `T > 1`, which is the standard Hinton recipe.
//!
//! # Burn 0.21.0-pre.4 API notes
//! - `AdamWConfig::new().with_weight_decay(...).init()` returns an
//!   `OptimizerAdaptor<AdamW, M, B>`. `init()` does NOT take a device; weight
//!   decay is `f32`, not `f64`.
//! - `optim.step(lr, model, grads_params) -> M` consumes and returns `model`.
//! - `GradientsParams::from_grads(grads, &model)` to convert raw grads.
//! - `loss.backward()` is on `Tensor<B, 1>` where `B: AutodiffBackend`.
//! - `model.valid()` returns the inner-backend module (no-autodiff) for
//!   validation/save. `&self`, so we don't need `.clone()`.
//! - `BurnpackStore::from_file(&path)` + `model.save_into(&mut store)` writes
//!   directly to disk; no `into_bytes()` step needed.
//! - `Tensor::one_hot::<2>(num_classes)` on a 1-D Int tensor → 2-D Int tensor;
//!   `.float()` casts to Float.
//! - `tensor.into_scalar()` returns `K::Elem` (e.g., `f32`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use burn::backend::NdArray;
use burn::module::AutodiffModule;
use burn::optim::{AdamWConfig, GradientsParams, Optimizer};
use burn::tensor::activation::log_softmax;
use burn::tensor::backend::{AutodiffBackend, Backend};
use burn::tensor::{Device, Int, Tensor, TensorData};
use burn_autodiff::Autodiff;
use burn_store::{BurnpackStore, ModuleSnapshot};

use shac_ml_train::data::{read_jsonl, DistilledExample};
use shac_ml_train::model::{StudentModel, StudentModelConfig};
use shac_ml_train::tokenizer::Vocab;

type B = Autodiff<NdArray<f32>>;
type InnerB = NdArray<f32>;

#[derive(Parser, Debug)]
#[command(about = "Train the shac student model from distilled JSONL with CE+KL loss")]
struct Args {
    /// Distilled JSONL (output of `shac-ml-distill`).
    #[arg(long)]
    input: PathBuf,

    /// Output `.bpk` file path.
    #[arg(long)]
    output: PathBuf,

    /// Vocab JSON the distilled --input was built against (same file passed
    /// to `shac-ml-distill --vocab`). Sets the model's vocab_size — never
    /// hardcode it independently of the actual vocab (finding #9).
    #[arg(long)]
    vocab: PathBuf,

    /// Declared cwd-bucket count written to feature-spec.json. Must exceed
    /// every `cwd_bucket` actually present in --input (see `shac-ml-distill
    /// --cwd-buckets`, same default).
    #[arg(long, default_value_t = 8)]
    cwd_buckets: u8,

    /// Seed for the deterministic shuffle applied before the internal
    /// train/val split and reshuffled every epoch. This split is for
    /// epoch-logging only — the acceptance signal is the external
    /// `shac-ml-distill --val-output` file, evaluated separately by
    /// `shac-ml-eval`.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    #[arg(long, default_value_t = 64)]
    batch_size: usize,

    #[arg(long, default_value_t = 10)]
    epochs: usize,

    #[arg(long, default_value_t = 3e-4)]
    lr: f64,

    /// Weight on the hard CE term; (1 - alpha) is on the soft KL term.
    #[arg(long, default_value_t = 0.5)]
    alpha: f32,

    /// Distillation temperature `T`. KL is scaled by `T²` per Hinton 2015.
    #[arg(long, default_value_t = 4.0)]
    temperature: f32,
}

/// Minimal deterministic PRNG (xorshift64*) for the internal shuffle below.
/// Not cryptographic; exists only so shuffling is reproducible given --seed
/// without pulling in the `rand` crate, which isn't otherwise a dependency
/// of this crate.
struct Xorshift64Star(u64);

impl Xorshift64Star {
    fn new(seed: u64) -> Self {
        // Avoid the fixed point at 0.
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n`. Panics if `n == 0`.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// In-place Fisher-Yates shuffle driven by `rng`.
fn shuffle<T>(data: &mut [T], rng: &mut Xorshift64Star) {
    for i in (1..data.len()).rev() {
        let j = rng.below(i + 1);
        data.swap(i, j);
    }
}

/// Split into (train, val); `data` must already be shuffled — this just
/// takes a trailing slice. Internal split only, for epoch-logging accuracy;
/// the acceptance signal is the external `--val-output` file from
/// `shac-ml-distill`, checked separately by `shac-ml-eval`.
fn split_train_val(
    mut data: Vec<DistilledExample>,
    val_frac: f32,
) -> (Vec<DistilledExample>, Vec<DistilledExample>) {
    let n_val = ((data.len() as f32) * val_frac) as usize;
    if n_val == 0 || n_val >= data.len() {
        return (data, Vec::new());
    }
    let val = data.split_off(data.len() - n_val);
    (data, val)
}

/// Fail fast if any context/hard/soft token id in `dataset` falls outside
/// `0..vocab_size` — a mismatch between --input and --vocab that would
/// otherwise panic deep inside `one_hot` mid-training or silently corrupt
/// accuracy (finding #9).
fn validate_token_ids(dataset: &[DistilledExample], vocab_size: usize) -> Result<()> {
    for (i, ex) in dataset.iter().enumerate() {
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
        for &(tok, _) in &ex.soft_targets_top {
            if tok as usize >= vocab_size {
                anyhow::bail!(
                    "example {i}: soft target token id {tok} >= vocab_size {vocab_size} \
                     (--input was not built from --vocab)"
                );
            }
        }
    }
    Ok(())
}

/// Fail fast if `dataset` contains a `cwd_bucket` the declared bucket count
/// can't represent — feature-spec.json must describe the actual data, not a
/// hardcoded literal (finding #9).
fn validate_cwd_buckets(dataset: &[DistilledExample], declared: u8) -> Result<()> {
    if let Some(max_bucket) = dataset.iter().map(|ex| ex.cwd_bucket).max() {
        if max_bucket >= declared {
            anyhow::bail!(
                "dataset cwd_bucket {max_bucket} >= declared --cwd-buckets {declared}; \
                 pass --cwd-buckets {} to match --input",
                max_bucket as u32 + 1
            );
        }
    }
    Ok(())
}

/// Encode a slice of examples into batched input/hard/soft tensors.
///
/// Returns:
/// - `input` : `[batch, ctx_len]` Int  — context_tokens
/// - `hard`  : `[batch]` Int            — gold next-token id
/// - `soft`  : `[batch, vocab]` Float   — dense teacher distribution
fn encode_batch<Be: Backend>(
    batch: &[DistilledExample],
    vocab_size: usize,
    ctx_len: usize,
    device: &Be::Device,
) -> (Tensor<Be, 2, Int>, Tensor<Be, 1, Int>, Tensor<Be, 2>) {
    let bs = batch.len();

    // input
    let mut input_data: Vec<i64> = Vec::with_capacity(bs * ctx_len);
    for ex in batch {
        // Truncate / pad to ctx_len (distill should already produce length=ctx_len).
        for i in 0..ctx_len {
            let tok = ex.context_tokens.get(i).copied().unwrap_or(0);
            input_data.push(tok as i64);
        }
    }
    let input = Tensor::<Be, 2, Int>::from_data(TensorData::new(input_data, [bs, ctx_len]), device);

    // hard label
    let hard_data: Vec<i64> = batch.iter().map(|ex| ex.hard_label as i64).collect();
    let hard = Tensor::<Be, 1, Int>::from_data(TensorData::new(hard_data, [bs]), device);

    // dense soft targets
    let mut soft_data: Vec<f32> = vec![0.0; bs * vocab_size];
    for (i, ex) in batch.iter().enumerate() {
        // Renormalize defensively: distill already does this, but be safe.
        let total: f32 = ex.soft_targets_top.iter().map(|(_, p)| *p).sum();
        if total > 0.0 {
            for &(tok, prob) in &ex.soft_targets_top {
                let tok = tok as usize;
                if tok < vocab_size {
                    soft_data[i * vocab_size + tok] += prob / total;
                }
            }
        } else {
            // Fall back to one-hot at hard label.
            let tok = ex.hard_label as usize;
            if tok < vocab_size {
                soft_data[i * vocab_size + tok] = 1.0;
            }
        }
    }
    let soft = Tensor::<Be, 2>::from_data(TensorData::new(soft_data, [bs, vocab_size]), device);

    (input, hard, soft)
}

/// Mixed CE + KL distillation loss.
///
/// `logits`: `[batch, vocab]` (autodiff)
/// `hard`  : `[batch]` Int
/// `soft`  : `[batch, vocab]` Float (teacher distribution)
fn mixed_loss<Be: AutodiffBackend>(
    logits: Tensor<Be, 2>,
    hard: Tensor<Be, 1, Int>,
    soft: Tensor<Be, 2>,
    alpha: f32,
    temperature: f32,
) -> Tensor<Be, 1> {
    let [_batch, vocab] = logits.dims();

    // CE: -mean( sum_k one_hot(hard)_k * log_softmax(logits)_k )
    let log_probs = log_softmax(logits.clone(), 1); // [batch, vocab]
    let hard_one_hot: Tensor<Be, 2> = hard.one_hot::<2>(vocab).float(); // [batch, vocab]
    let ce_per_example = (log_probs * hard_one_hot).sum_dim(1).neg(); // [batch, 1]
    let ce = ce_per_example.mean(); // [1]

    // KL(soft || softmax(logits/T)): mean( sum_k soft_k * (log soft_k - log softmax(logits/T)_k) )
    let scaled = logits.div_scalar(temperature);
    let log_student_t = log_softmax(scaled, 1); // [batch, vocab]
    let log_soft = soft.clone().clamp_min(1e-9_f32).log();
    let kl_per_example = (soft * (log_soft - log_student_t)).sum_dim(1); // [batch, 1]
    let kl = kl_per_example.mean(); // [1]

    let t2 = temperature * temperature;
    ce.mul_scalar(alpha) + kl.mul_scalar((1.0 - alpha) * t2)
}

/// Validation: forward (no autodiff), compute top-1 accuracy.
fn validate(
    model: &StudentModel<InnerB>,
    examples: &[DistilledExample],
    vocab_size: usize,
    ctx_len: usize,
    device: &Device<InnerB>,
    batch_size: usize,
) -> f64 {
    if examples.is_empty() {
        return 0.0;
    }
    let mut correct = 0usize;
    for batch in examples.chunks(batch_size) {
        let (input, hard, _soft) = encode_batch::<InnerB>(batch, vocab_size, ctx_len, device);
        let logits = model.forward(input); // [batch, vocab]
        let pred: Tensor<InnerB, 1, Int> = logits.argmax(1).squeeze_dim::<1>(1);
        let eq = pred.equal(hard).int().sum().into_scalar();
        correct += eq as usize;
    }
    correct as f64 / examples.len() as f64
}

fn main() -> Result<()> {
    let args = Args::parse();

    let device: Device<InnerB> = Default::default();

    // Load the actual vocab used to build --input; vocab_size MUST come from
    // here, never from StudentModelConfig::default() (finding #9).
    let vocab_json = std::fs::read_to_string(&args.vocab)
        .with_context(|| format!("read vocab {}", args.vocab.display()))?;
    let vocab = Vocab::from_json(&vocab_json)
        .with_context(|| format!("parse vocab {}", args.vocab.display()))?;

    let cfg = StudentModelConfig::for_vocab_size(vocab.size());
    let vocab_size = cfg.vocab_size;
    let ctx_len = cfg.context_len;
    let mut model: StudentModel<B> = cfg.init::<B>(&device);

    // AdamW: weight_decay is f32 in burn 0.21.0-pre.4.
    let mut optim = AdamWConfig::new().with_weight_decay(1e-2_f32).init();

    // Load, validate, and split data.
    let mut dataset: Vec<DistilledExample> = read_jsonl::<DistilledExample>(&args.input)
        .with_context(|| format!("read distilled jsonl {}", args.input.display()))?;
    if dataset.is_empty() {
        anyhow::bail!("no examples in {}", args.input.display());
    }
    validate_token_ids(&dataset, vocab_size).with_context(|| {
        format!(
            "{} does not match vocab {}",
            args.input.display(),
            args.vocab.display()
        )
    })?;
    validate_cwd_buckets(&dataset, args.cwd_buckets)?;

    let mut rng = Xorshift64Star::new(args.seed);
    let total = dataset.len();
    shuffle(&mut dataset, &mut rng);
    let (mut train, val) = split_train_val(dataset, 0.1);
    eprintln!(
        "loaded {} examples ({} train / {} val)",
        total,
        train.len(),
        val.len()
    );

    for epoch in 0..args.epochs {
        // Reshuffle every epoch so batch composition isn't the same fixed
        // order each time (adjacent issue B).
        shuffle(&mut train, &mut rng);

        let mut train_loss_sum = 0.0_f64;
        let mut step_count = 0_usize;

        for batch in train.chunks(args.batch_size) {
            let (input, hard, soft) = encode_batch::<B>(batch, vocab_size, ctx_len, &device);
            let logits = model.forward(input);
            let loss = mixed_loss::<B>(logits, hard, soft, args.alpha, args.temperature);

            // Capture the scalar BEFORE consuming `loss` in backward(), since
            // `into_scalar` consumes self.
            let loss_scalar = loss.clone().into_scalar();

            let grads = loss.backward();
            let grads_params = GradientsParams::from_grads(grads, &model);
            model = optim.step(args.lr, model, grads_params);

            train_loss_sum += loss_scalar as f64;
            step_count += 1;
            if step_count.is_multiple_of(100) {
                eprintln!(
                    "epoch {} step {}: loss={:.4}",
                    epoch,
                    step_count,
                    train_loss_sum / step_count as f64
                );
            }
        }

        let inner = model.valid();
        let val_acc = validate(&inner, &val, vocab_size, ctx_len, &device, args.batch_size);
        eprintln!(
            "epoch {} done: avg_train_loss={:.4} val_top1={:.3}",
            epoch,
            train_loss_sum / step_count.max(1) as f64,
            val_acc
        );
    }

    // Save weights as .bpk.
    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
    }

    // Disable auto-extension so the user-specified path is honored verbatim,
    // and allow overwriting any prior run.
    let mut store = BurnpackStore::from_file(&args.output)
        .auto_extension(false)
        .overwrite(true);
    let inner = model.valid();
    inner
        .save_into(&mut store)
        .map_err(|e| anyhow::anyhow!("save .bpk: {e:?}"))?;
    eprintln!("wrote weights to {}", args.output.display());

    // Emit feature-spec.json next to the .bpk so the runtime crate can
    // verify model architecture compatibility at load time.
    let feature_spec_path = args.output.with_file_name("feature-spec.json");
    let spec = serde_json::json!({
        "version": 1,
        "vocab_size": cfg.vocab_size,
        "context_len": cfg.context_len,
        "cwd_buckets": args.cwd_buckets,
        "model_arch": {
            "kind": "mini-transformer",
            "n_layers": cfg.n_layers,
            "n_heads": cfg.n_heads,
            "hidden_dim": cfg.hidden_dim,
            "intermediate_dim": cfg.intermediate_dim,
        }
    });
    std::fs::write(&feature_spec_path, serde_json::to_string_pretty(&spec)?)
        .with_context(|| format!("write {}", feature_spec_path.display()))?;
    eprintln!("wrote feature spec to {}", feature_spec_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(
        context_tokens: Vec<u32>,
        hard_label: u32,
        soft_targets_top: Vec<(u32, f32)>,
        cwd_bucket: u8,
    ) -> DistilledExample {
        DistilledExample {
            schema_version: 1,
            os: "darwin".to_string(),
            cwd_bucket,
            context_tokens,
            hard_label,
            soft_targets_top,
        }
    }

    // ---- finding #9: vocab/dataset contract validation --------------------

    #[test]
    fn validate_token_ids_accepts_in_bounds_dataset() {
        let dataset = vec![example(vec![0, 1, 2], 3, vec![(3, 1.0)], 0)];
        assert!(validate_token_ids(&dataset, 4).is_ok());
    }

    #[test]
    fn validate_token_ids_rejects_out_of_bounds_context_token() {
        let dataset = vec![example(vec![0, 10], 1, vec![(1, 1.0)], 0)];
        assert!(validate_token_ids(&dataset, 4).is_err());
    }

    #[test]
    fn validate_token_ids_rejects_out_of_bounds_hard_label() {
        let dataset = vec![example(vec![0, 1], 10, vec![(1, 1.0)], 0)];
        assert!(validate_token_ids(&dataset, 4).is_err());
    }

    #[test]
    fn validate_token_ids_rejects_out_of_bounds_soft_target() {
        let dataset = vec![example(vec![0, 1], 1, vec![(10, 1.0)], 0)];
        assert!(validate_token_ids(&dataset, 4).is_err());
    }

    #[test]
    fn validate_cwd_buckets_accepts_max_bucket_below_declared() {
        let dataset = vec![example(vec![0], 0, vec![], 7)];
        assert!(validate_cwd_buckets(&dataset, 8).is_ok());
    }

    #[test]
    fn validate_cwd_buckets_rejects_bucket_at_or_above_declared() {
        let dataset = vec![example(vec![0], 0, vec![], 8)];
        assert!(validate_cwd_buckets(&dataset, 8).is_err());
    }

    // ---- adjacent issue B: deterministic, reshuffled internal split --------

    #[test]
    fn shuffle_is_deterministic_given_same_seed() {
        let mut a: Vec<u32> = (0..20).collect();
        let mut b = a.clone();
        shuffle(&mut a, &mut Xorshift64Star::new(42));
        shuffle(&mut b, &mut Xorshift64Star::new(42));
        assert_eq!(a, b);
    }

    #[test]
    fn shuffle_produces_a_permutation_in_a_different_order() {
        let mut data: Vec<u32> = (0..20).collect();
        let original = data.clone();
        shuffle(&mut data, &mut Xorshift64Star::new(42));
        assert_ne!(
            data, original,
            "20 elements should not shuffle back to identity"
        );
        let mut sorted = data.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted, original,
            "shuffle must not lose or duplicate elements"
        );
    }

    #[test]
    fn repeated_shuffle_calls_on_same_rng_differ_across_epochs() {
        let mut data: Vec<u32> = (0..20).collect();
        let mut rng = Xorshift64Star::new(1);
        shuffle(&mut data, &mut rng);
        let epoch1 = data.clone();
        shuffle(&mut data, &mut rng);
        let epoch2 = data.clone();
        assert_ne!(
            epoch1, epoch2,
            "reshuffling with the continuing rng each epoch must not repeat the same order"
        );
    }
}
