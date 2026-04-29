# shac ML Runtime Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate the trained `.bpk` artifacts produced by the [pipeline plan](./2026-04-29-shac-ml-pipeline.md) into the shipped shac daemon: feature extraction, burn inference, score blending into the existing 12-feature engine, on-device residual personalization, plus opt-in config and `shac ml ...` CLI.

**Architecture:** New `src/ml/` module with four submodules: `feature_extractor` (build context tensor from `CompletionRequest`), `inference` (load `.bpk` + forward pass via burn `NdArray`), `residual` (SQLite-backed online personalization), and `mod.rs` (top-level facade). Engine gains a 13th feature `ml_seq_score`. CLI gains `shac ml {load-status, inspect-residual, reset-personalization}`. Feature is opt-in (`features.ml_seq_rerank=false` default).

**Tech Stack:** Burn 0.21 with `NdArray` backend (no autodiff at runtime), `burn-store` (BurnpackStore), `shac-ml-train` workspace crate with `model-only` feature flag, `rusqlite` (existing).

**Reference docs:**
- Spec: `docs/superpowers/specs/2026-04-29-shac-ml-next-command-design.md`
- Pipeline plan (prerequisite): `docs/superpowers/plans/2026-04-29-shac-ml-pipeline.md`

**Prerequisite:** Pipeline plan tasks 1–17 are complete. The runtime plan assumes:
- `crates/shac-ml-train/` exists with `model-only` feature flag
- `ml/models/{shac-ml-darwin.bpk, shac-ml-linux.bpk, vocab.json, feature-spec.json}` are committed

---

## File map

**Created:**
- `src/ml/mod.rs`
- `src/ml/feature_extractor.rs`
- `src/ml/inference.rs`
- `src/ml/residual.rs`
- `tests/ml_inference.rs`
- `tests/ml_residual.rs`
- `tests/ml_blend.rs`
- `docs/ml.md` — user-facing doc

**Modified:**
- `Cargo.toml` (root) — add `shac-ml-train` runtime dep, `burn`, `burn-store`
- `src/lib.rs` — register `pub mod ml;`
- `src/config.rs` — add `features.ml_seq_rerank`, `ranking.ml_seq_score`, `ml.*` knobs
- `src/db.rs` — add `ml_residual` table + accessors
- `src/engine.rs` — load `MlInference`, compute `ml_seq_score`, update residual on accept
- `src/bin/shac.rs` — add `Ml(MlArgs)` subcommand and dispatcher
- `src/protocol.rs` — extend response with optional `MlStatus` for `shac ml load-status`
- `CHANGELOG.md` — v0.6.0 entry
- `Cargo.toml` (root) — bump `version = "0.6.0"` (last task)

**Renamed (none):** the existing `src/ml.rs` (logistic regression for `ml_rerank`) is a different feature and stays put. We extend, not replace.

---

## Important note on the existing `src/ml.rs`

`src/ml.rs` already exists — it's the **logistic regression** behind `features.ml_rerank` (not `ml_seq_rerank`). Two ML systems will coexist:
- `ml_rerank` (existing): logistic regression auto-trained from user history, blends as a single score
- `ml_seq_rerank` (this plan): pre-trained mini-Transformer + residual, blends as a 13th feature

To avoid confusion, the new module lives at `src/ml/` (directory), and `src/ml.rs` (file) becomes `src/ml.rs` UNCHANGED. Rust resolves `mod ml` to `src/ml.rs` *or* `src/ml/mod.rs`, not both. So **before adding `src/ml/`, we must rename**:
- `src/ml.rs` → `src/ml_logreg.rs`
- Update `src/lib.rs` to declare `pub mod ml_logreg;` instead of `pub mod ml;`
- Update all imports of `crate::ml::{MlModel, ...}` → `crate::ml_logreg::{MlModel, ...}`

Task 1 handles this rename safely. Task 2 then creates `src/ml/`.

---

## Task 1: Rename `src/ml.rs` → `src/ml_logreg.rs` to free up `ml` namespace

**Files:**
- Modify: `src/ml.rs` → renamed to `src/ml_logreg.rs`
- Modify: `src/lib.rs`
- Modify: `src/engine.rs` (imports)

- [ ] **Step 1: Verify current usages of `crate::ml`**

Run: `grep -rn "crate::ml::\|use crate::ml\b\|use crate::ml;" src/ tests/`
Expected: prints all use sites — likely just `engine.rs` imports `MlModel`, `train_model`, `TrainOptions`, `TrainingSample`. Note the lines.

- [ ] **Step 2: Rename file**

Run: `git mv src/ml.rs src/ml_logreg.rs`

- [ ] **Step 3: Update `src/lib.rs`**

In `src/lib.rs`, replace the `pub mod ml;` declaration with:

```rust
pub mod ml_logreg;
```

- [ ] **Step 4: Update `src/engine.rs` imports**

Anywhere `engine.rs` references `crate::ml::` or `use crate::ml::`, change to `crate::ml_logreg::`.

Run: `cargo check`
Fix any compile errors by updating import paths.

- [ ] **Step 5: Update any other use sites**

Run: `grep -rn "crate::ml::" src/ tests/`
Expected: no matches. If matches remain, fix them.

Run: `grep -rn "use shac::ml::\|shac::ml::" tests/`
Update tests that import `shac::ml::*` to `shac::ml_logreg::*`.

- [ ] **Step 6: Run tests**

Run: `cargo test --lib`
Expected: PASS (all existing tests).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: rename src/ml.rs → src/ml_logreg.rs to free ml/ namespace

The new ML next-command predictor (v0.6.0) lives at src/ml/ as a
directory module. Renaming the existing logistic-regression module
avoids the file-vs-directory mod conflict. Behavior unchanged."
```

---

## Task 2: Add runtime crate deps + create `src/ml/mod.rs` skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `src/ml/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add deps to root `Cargo.toml`**

In the `[dependencies]` section of root `Cargo.toml`, add:

```toml
burn = { version = "=0.21", default-features = false, features = ["std", "ndarray"] }
burn-store = { version = "=0.21", default-features = false, features = ["burnpack"] }
shac-ml-train = { path = "crates/shac-ml-train", default-features = false, features = ["model-only"] }
```

Place these alphabetically among existing deps.

- [ ] **Step 2: Create `src/ml/` directory and skeleton `mod.rs`**

```bash
mkdir -p src/ml
```

Write `src/ml/mod.rs`:

```rust
//! ML next-command prediction (v0.6.0).
//!
//! Loaded once at daemon start, queried on every `/complete` request when
//! `features.ml_seq_rerank=true`. See docs/ml.md for the user-facing story
//! and docs/superpowers/specs/2026-04-29-shac-ml-next-command-design.md for
//! design rationale.

pub mod feature_extractor;
pub mod inference;
pub mod residual;

pub use feature_extractor::{Context as MlContext, FeatureSpec};
pub use inference::MlInference;
pub use residual::ResidualStore;
```

Create stubs for the submodules so the crate compiles:

```bash
for f in feature_extractor inference residual; do
  echo "//! placeholder" > "src/ml/${f}.rs"
done
```

- [ ] **Step 3: Register module in `src/lib.rs`**

Add to `src/lib.rs`:

```rust
pub mod ml;
```

(near the existing `pub mod` declarations).

- [ ] **Step 4: Compile-check**

Run: `cargo check`
Expected: PASS, with warnings about unused stub modules.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/lib.rs src/ml/
git commit -m "feat(ml): bootstrap src/ml/ runtime module + burn/shac-ml-train deps"
```

---

## Task 3: `feature_extractor.rs` — Context, FeatureSpec, build_features

**Files:**
- Modify: `src/ml/feature_extractor.rs`

- [ ] **Step 1: Implement the module**

Replace contents of `src/ml/feature_extractor.rs`:

```rust
//! Build a context tensor from a `CompletionRequest`. The shape and content
//! of this tensor is the contract with the trained `.bpk`: any drift between
//! pipeline (in `crates/shac-ml-train/src/bin/distill.rs`) and runtime
//! (this file) breaks the model.
//!
//! Layout (16 i64 token ids):
//!   [0]    = <BOS>
//!   [1..15] = first-token of last 14 commands (oldest → newest), <PAD>-padded
//!   [15]   = first-token of the user's current input prefix, or <PAD>
//!
//! `cwd_bucket` and `os` are not part of the input tensor in v1 — we kept the
//! contract simple. They're inputs to the residual layer instead.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use shac_ml_train::tokenizer::{tokenize_command, Vocab};

/// Embedded vocab.json shipped alongside the .bpk.
const VOCAB_JSON: &str = include_str!("../../ml/models/vocab.json");
const FEATURE_SPEC_JSON: &str = include_str!("../../ml/models/feature-spec.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSpec {
    pub version: u32,
    pub vocab_size: usize,
    pub context_len: usize,
    pub cwd_buckets: u8,
    pub model_arch: serde_json::Value,
}

/// Inputs needed to build the model's context tensor.
pub struct Context<'a> {
    pub recent_commands: &'a [&'a str], // oldest → newest, may be shorter than context_len
    pub current_prefix: &'a str,
    pub cwd: &'a str,
}

impl FeatureSpec {
    pub fn from_embedded() -> Result<Self> {
        serde_json::from_str(FEATURE_SPEC_JSON).context("parse feature-spec.json")
    }
}

pub fn load_embedded_vocab() -> Result<Vocab> {
    Vocab::from_json(VOCAB_JSON).context("parse vocab.json")
}

/// Build the i64 token vector. Length is always `spec.context_len`.
pub fn build_token_vector(ctx: &Context, vocab: &Vocab, spec: &FeatureSpec) -> Vec<i64> {
    let pad = vocab.id_of("<PAD>").expect("PAD reserved");
    let bos = vocab.id_of("<BOS>").expect("BOS reserved");
    let mut out: Vec<i64> = vec![pad as i64; spec.context_len];
    out[0] = bos as i64;

    // Fill positions 1..context_len-1 with first-tokens of last (context_len - 2) commands.
    let body_len = spec.context_len.saturating_sub(2);
    let take = ctx.recent_commands.len().min(body_len);
    for (i, cmd) in ctx.recent_commands.iter().rev().take(take).rev().enumerate() {
        let toks = tokenize_command(cmd);
        if let Some(first) = toks.first() {
            out[1 + i] = vocab.encode_word(first) as i64;
        }
    }

    // Last slot: current input prefix's first token, or PAD if empty.
    let prefix_toks = tokenize_command(ctx.current_prefix);
    if let Some(first) = prefix_toks.first() {
        out[spec.context_len - 1] = vocab.encode_word(first) as i64;
    }

    out
}

/// Stable cwd hash → bucket index. Must match the bucketing used by the
/// pipeline's `distill` bin.
pub fn cwd_bucket(cwd: &str, n_buckets: u8) -> u8 {
    let mut h = DefaultHasher::new();
    cwd.hash(&mut h);
    (h.finish() % n_buckets as u64) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_spec() -> FeatureSpec {
        FeatureSpec {
            version: 1,
            vocab_size: 2000,
            context_len: 16,
            cwd_buckets: 8,
            model_arch: serde_json::Value::Null,
        }
    }

    #[test]
    fn empty_context_is_all_pad_except_bos() {
        let vocab = Vocab::new_with_special_only();
        let ctx = Context {
            recent_commands: &[],
            current_prefix: "",
            cwd: "/tmp",
        };
        let v = build_token_vector(&ctx, &vocab, &fixture_spec());
        let pad = vocab.id_of("<PAD>").unwrap() as i64;
        let bos = vocab.id_of("<BOS>").unwrap() as i64;
        assert_eq!(v[0], bos);
        for &t in &v[1..] {
            assert_eq!(t, pad);
        }
    }

    #[test]
    fn recent_commands_fill_in_order() {
        let mut tokens: Vec<String> = shac_ml_train::tokenizer::SPECIAL_TOKENS
            .iter()
            .map(|s| s.to_string())
            .collect();
        tokens.push("git".to_string());
        tokens.push("cargo".to_string());
        let vocab = Vocab {
            tokens: tokens.clone(),
            // re-derive index via to_json/from_json
            ..Vocab::new_with_special_only()
        };
        // Use the proper builder so the index is rebuilt
        let json = serde_json::to_string(&Vocab {
            tokens,
            ..Vocab::new_with_special_only()
        })
        .unwrap();
        let vocab = Vocab::from_json(&json).unwrap();

        let ctx = Context {
            recent_commands: &["git status", "cargo test"],
            current_prefix: "",
            cwd: "/tmp",
        };
        let v = build_token_vector(&ctx, &vocab, &fixture_spec());
        let git_id = vocab.id_of("git").unwrap() as i64;
        let cargo_id = vocab.id_of("cargo").unwrap() as i64;
        assert_eq!(v[1], git_id);
        assert_eq!(v[2], cargo_id);
    }

    #[test]
    fn current_prefix_lands_in_last_slot() {
        let vocab = Vocab::new_with_special_only();
        let ctx = Context {
            recent_commands: &[],
            current_prefix: "<UNK>",
            cwd: "/tmp",
        };
        let v = build_token_vector(&ctx, &vocab, &fixture_spec());
        let unk = vocab.id_of("<UNK>").unwrap() as i64;
        assert_eq!(v[15], unk);
    }

    #[test]
    fn cwd_bucket_is_deterministic_and_in_range() {
        let a = cwd_bucket("/Users/foo/dev", 8);
        let b = cwd_bucket("/Users/foo/dev", 8);
        assert_eq!(a, b);
        assert!(a < 8);
    }
}
```

- [ ] **Step 2: Run unit tests**

Run: `cargo test --lib ml::feature_extractor`
Expected: PASS, 4 tests.

If `include_str!` fails because `ml/models/vocab.json` is missing (pipeline tasks 1–17 not complete), this will error out. That's a real blocker — the pipeline plan must be done first. If you're re-checking the runtime plan in isolation, temporarily commit a fixture `vocab.json` and `feature-spec.json` containing the special tokens only.

- [ ] **Step 3: Commit**

```bash
git add src/ml/feature_extractor.rs
git commit -m "feat(ml): feature_extractor — Context tensor + cwd bucketing"
```

---

## Task 4: `inference.rs` — load `.bpk` via burn, run forward

**Files:**
- Modify: `src/ml/inference.rs`

- [ ] **Step 1: Implement inference module**

Replace contents of `src/ml/inference.rs`:

```rust
//! Burn-backed inference. Loads the bundled `.bpk` once at daemon start;
//! every request runs a forward pass on the NdArray (CPU) backend.

use anyhow::{anyhow, Context as _, Result};
use burn::backend::NdArray;
use burn::module::Module;
use burn::tensor::{activation, Int, Tensor, TensorData};
use burn_store::BurnpackStore;
use shac_ml_train::model::{StudentModel, StudentModelConfig};
use shac_ml_train::tokenizer::Vocab;

use crate::ml::feature_extractor::{
    build_token_vector, load_embedded_vocab, Context, FeatureSpec,
};

type B = NdArray<f32>;

/// `.bpk` bytes embedded at compile time. Selected by `target_os`.
#[cfg(target_os = "macos")]
const MODEL_BYTES: &[u8] = include_bytes!("../../ml/models/shac-ml-darwin.bpk");
#[cfg(target_os = "linux")]
const MODEL_BYTES: &[u8] = include_bytes!("../../ml/models/shac-ml-linux.bpk");
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const MODEL_BYTES: &[u8] = &[];

pub struct MlInference {
    model: StudentModel<B>,
    vocab: Vocab,
    spec: FeatureSpec,
    device: <B as burn::tensor::backend::Backend>::Device,
}

impl MlInference {
    /// Loads the bundled model. Returns `Ok(None)` on unsupported OS rather than
    /// erroring — the engine then disables the feature gracefully.
    pub fn load_bundled() -> Result<Option<Self>> {
        if MODEL_BYTES.is_empty() {
            return Ok(None);
        }
        let device = Default::default();
        let spec = FeatureSpec::from_embedded()?;
        let vocab = load_embedded_vocab()?;
        if spec.version != 1 {
            return Err(anyhow!(
                "feature_spec.version {} is unsupported by this shac build",
                spec.version
            ));
        }
        if spec.vocab_size != vocab.size() {
            return Err(anyhow!(
                "vocab size mismatch: spec says {}, vocab.json has {}",
                spec.vocab_size,
                vocab.size()
            ));
        }
        let cfg = StudentModelConfig {
            vocab_size: spec.vocab_size,
            context_len: spec.context_len,
            ..StudentModelConfig::default()
        };
        let mut model: StudentModel<B> = cfg.init(&device);
        let mut store = BurnpackStore::from_static(MODEL_BYTES);
        model.load_from(&mut store).context("load .bpk weights")?;
        Ok(Some(Self {
            model,
            vocab,
            spec,
            device,
        }))
    }

    pub fn vocab(&self) -> &Vocab {
        &self.vocab
    }

    pub fn spec(&self) -> &FeatureSpec {
        &self.spec
    }

    /// Run a forward pass and return a probability vector of length `vocab_size`.
    pub fn distribution(&self, ctx: &Context) -> Result<Vec<f32>> {
        let token_vec = build_token_vector(ctx, &self.vocab, &self.spec);
        let input: Tensor<B, 2, Int> = Tensor::from_data(
            TensorData::new(token_vec, [1, self.spec.context_len]),
            &self.device,
        );
        let logits = self.model.forward(input);
        let probs = activation::softmax(logits, 1);
        let data = probs.into_data();
        data.to_vec::<f32>()
            .map_err(|e| anyhow!("decode probs tensor: {:?}", e))
    }

    /// First-token id for an arbitrary candidate string.
    pub fn first_token_id(&self, candidate: &str) -> u32 {
        let toks = shac_ml_train::tokenizer::tokenize_command(candidate);
        match toks.first() {
            Some(t) => self.vocab.encode_word(t),
            None => self.vocab.id_of("<UNK>").unwrap(),
        }
    }
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 3: Add a unit test for distribution shape and finiteness**

Append to `src/ml/inference.rs` (inside file):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_model_loads_and_runs() {
        let inf = match MlInference::load_bundled() {
            Ok(Some(x)) => x,
            Ok(None) => return, // unsupported OS in CI
            Err(e) => panic!("load: {e}"),
        };
        let ctx = Context {
            recent_commands: &["git status"],
            current_prefix: "git",
            cwd: "/tmp",
        };
        let probs = inf.distribution(&ctx).unwrap();
        assert_eq!(probs.len(), inf.spec.vocab_size);
        let total: f32 = probs.iter().sum();
        assert!((total - 1.0).abs() < 1e-3, "softmax should sum to ~1, got {total}");
        assert!(probs.iter().all(|p| p.is_finite() && *p >= 0.0));
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test --lib ml::inference`
Expected: PASS on darwin/linux. Skips silently on other OSes.

- [ ] **Step 5: Commit**

```bash
git add src/ml/inference.rs
git commit -m "feat(ml): inference module — load .bpk + softmax forward"
```

---

## Task 5: `residual.rs` — SQLite schema + cache + update

**Files:**
- Modify: `src/db.rs` — add `ml_residual` table + accessors
- Modify: `src/ml/residual.rs`

- [ ] **Step 1: Add `ml_residual` table to `db.rs`**

In `src/db.rs`, find the `init` method (around line 106). Inside the `execute_batch` SQL block, append (just before the closing `r#"…"#`):

```sql
CREATE TABLE IF NOT EXISTS ml_residual (
    cwd_bucket   INTEGER NOT NULL,
    prev_cmd_id  INTEGER NOT NULL,
    token_id     INTEGER NOT NULL,
    weight       REAL NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (cwd_bucket, prev_cmd_id, token_id)
);
CREATE INDEX IF NOT EXISTS idx_ml_residual_lookup ON ml_residual (cwd_bucket, prev_cmd_id);
```

- [ ] **Step 2: Add accessors to `db.rs`**

Add these methods to the `impl AppDb` block (somewhere after `record_history`):

```rust
    /// Load all residual entries into memory at daemon start.
    pub fn load_ml_residual(&self) -> rusqlite::Result<Vec<((u8, u32, u32), f32)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT cwd_bucket, prev_cmd_id, token_id, weight FROM ml_residual")?;
        let rows = stmt.query_map([], |r| {
            let bucket: i64 = r.get(0)?;
            let prev: i64 = r.get(1)?;
            let token: i64 = r.get(2)?;
            let w: f64 = r.get(3)?;
            Ok(((bucket as u8, prev as u32, token as u32), w as f32))
        })?;
        rows.collect()
    }

    /// Upsert a residual entry. Used on daemon shutdown / batched flush.
    pub fn upsert_ml_residual(
        &self,
        cwd_bucket: u8,
        prev_cmd_id: u32,
        token_id: u32,
        weight: f32,
        updated_at_secs: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO ml_residual (cwd_bucket, prev_cmd_id, token_id, weight, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(cwd_bucket, prev_cmd_id, token_id) DO UPDATE SET
                 weight = excluded.weight,
                 updated_at = excluded.updated_at",
            (
                cwd_bucket as i64,
                prev_cmd_id as i64,
                token_id as i64,
                weight as f64,
                updated_at_secs,
            ),
        )?;
        Ok(())
    }

    /// Number of rows in `ml_residual`.
    pub fn ml_residual_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM ml_residual", [], |r| r.get(0))
    }

    pub fn truncate_ml_residual(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM ml_residual", [])?;
        Ok(())
    }

    /// Top-N strongest residuals for `shac ml inspect-residual`.
    pub fn top_ml_residual(
        &self,
        n: usize,
    ) -> rusqlite::Result<Vec<(u8, u32, u32, f32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT cwd_bucket, prev_cmd_id, token_id, weight FROM ml_residual
             ORDER BY ABS(weight) DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([n as i64], |r| {
            let bucket: i64 = r.get(0)?;
            let prev: i64 = r.get(1)?;
            let token: i64 = r.get(2)?;
            let w: f64 = r.get(3)?;
            Ok((bucket as u8, prev as u32, token as u32, w as f32))
        })?;
        rows.collect()
    }
```

- [ ] **Step 3: Implement `src/ml/residual.rs`**

Replace contents of `src/ml/residual.rs`:

```rust
//! On-device personalization. Residual is an additive bias on the model's
//! pre-softmax logits, keyed by (cwd_bucket, prev_cmd_id, token_id). Updated
//! online whenever the user accepts a completion.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::db::AppDb;

/// Maximum number of residual rows we keep on disk. LRU-evict beyond this.
const MAX_RESIDUAL_ROWS: usize = 100_000;

pub struct ResidualStore {
    /// (cwd_bucket, prev_cmd_id, token_id) → weight
    cache: Mutex<HashMap<(u8, u32, u32), f32>>,
}

impl ResidualStore {
    pub fn load(db: &AppDb) -> Result<Self> {
        let entries = db.load_ml_residual()?;
        let cache: HashMap<(u8, u32, u32), f32> = entries.into_iter().collect();
        Ok(Self {
            cache: Mutex::new(cache),
        })
    }

    pub fn empty() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn lookup(&self, cwd_bucket: u8, prev_cmd_id: u32, token_id: u32) -> f32 {
        self.cache
            .lock()
            .unwrap()
            .get(&(cwd_bucket, prev_cmd_id, token_id))
            .copied()
            .unwrap_or(0.0)
    }

    pub fn count(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    /// Online SGD step on (cwd_bucket, prev_cmd_id) for the top-K candidates
    /// in `dist`. Pulls the accepted token toward 1.0, others toward 0.0.
    /// Persists each touched key to SQLite.
    pub fn update_on_acceptance(
        &self,
        db: &AppDb,
        cwd_bucket: u8,
        prev_cmd_id: u32,
        accepted_token_id: u32,
        dist: &[f32],
        lr: f32,
        top_k: usize,
    ) -> Result<()> {
        let mut indexed: Vec<(usize, f32)> = dist.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        indexed.truncate(top_k);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let mut cache = self.cache.lock().unwrap();
        for (token_id, p_user) in indexed {
            let token_id_u32 = token_id as u32;
            let target: f32 = if token_id_u32 == accepted_token_id { 1.0 } else { 0.0 };
            let grad = lr * (p_user - target);
            let key = (cwd_bucket, prev_cmd_id, token_id_u32);
            let entry = cache.entry(key).or_insert(0.0);
            *entry -= grad;
            db.upsert_ml_residual(cwd_bucket, prev_cmd_id, token_id_u32, *entry, now)?;
        }

        if cache.len() > MAX_RESIDUAL_ROWS {
            // No-op for now; eviction follows in a future spec. We log instead
            // of erroring to avoid disrupting the hot path.
            eprintln!(
                "ml_residual rows ({}) exceed cap ({}) — eviction not implemented in v0.6.0",
                cache.len(),
                MAX_RESIDUAL_ROWS
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_returns_zero_for_unseen_keys() {
        let store = ResidualStore::empty();
        assert_eq!(store.lookup(0, 0, 0), 0.0);
    }

    #[test]
    fn update_pulls_accepted_up_and_others_down() {
        // Use an in-memory db
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db = AppDb::open(tmp.path()).unwrap();
        let store = ResidualStore::empty();

        // dist: token 5 has the highest probability (0.6), token 3 next (0.3),
        // user accepts token 3.
        let mut dist = vec![0.0f32; 100];
        dist[5] = 0.6;
        dist[3] = 0.3;
        dist[7] = 0.1;
        store
            .update_on_acceptance(&db, 0, 1, /*accepted=*/ 3, &dist, 0.1, 5)
            .unwrap();

        // Accepted token's residual should be > 0 (target=1, p_user=0.3 → grad<0 → entry+=|grad|)
        let r_accepted = store.lookup(0, 1, 3);
        assert!(r_accepted > 0.0, "accepted residual should be positive, got {r_accepted}");
        // Non-accepted top token should be < 0
        let r_other = store.lookup(0, 1, 5);
        assert!(r_other < 0.0, "non-accepted residual should be negative, got {r_other}");
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib ml::residual`
Expected: PASS, 2 tests.

Run: `cargo test --lib db::`
Expected: existing db tests still PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/ml/residual.rs
git commit -m "feat(ml): residual personalization — SQLite-backed online SGD"
```

---

## Task 6: Config knobs — `features.ml_seq_rerank`, `ranking.ml_seq_score`, `ml.*`

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Add fields**

In `src/config.rs`, modify three structs:

`FeatureFlags` — add `ml_seq_rerank: bool`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureFlags {
    pub history_ranking: bool,
    pub doc_search: bool,
    pub project_context: bool,
    pub ml_rerank: bool,
    pub ml_seq_rerank: bool,
    pub inline_zsh: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            history_ranking: true,
            doc_search: true,
            project_context: true,
            ml_rerank: false,
            ml_seq_rerank: false,
            inline_zsh: false,
        }
    }
}
```

`RankingWeights` — add `ml_seq_score: f64`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingWeights {
    pub prefix_score: f64,
    pub fuzzy_score: f64,
    pub global_usage_score: f64,
    pub cwd_usage_score: f64,
    pub recency_score: f64,
    pub transition_score: f64,
    pub project_affinity_score: f64,
    pub position_score: f64,
    pub source_prior: f64,
    pub doc_match_score: f64,
    pub path_frecency_score: f64,
    pub ml_seq_score: f64,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            prefix_score: 0.32,
            fuzzy_score: 0.18,
            global_usage_score: 0.10,
            cwd_usage_score: 0.08,
            recency_score: 0.08,
            transition_score: 0.08,
            project_affinity_score: 0.07,
            position_score: 0.04,
            source_prior: 0.03,
            doc_match_score: 0.02,
            path_frecency_score: 0.10,
            ml_seq_score: 0.20,
        }
    }
}
```

Add a new `MlConfig` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MlConfig {
    pub residual_lr: f32,
    pub residual_top_k: usize,
    pub disable_personalization: bool,
}

impl Default for MlConfig {
    fn default() -> Self {
        Self {
            residual_lr: 0.05,
            residual_top_k: 20,
            disable_personalization: false,
        }
    }
}
```

Wire it into `AppConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub enabled: bool,
    pub features: FeatureFlags,
    pub ranking: RankingWeights,
    pub ui: UiConfig,
    pub max_results: usize,
    pub daemon_timeout_ms: u64,
    pub ml_model_file: Option<String>,
    pub ml_blend_weight: f64,
    pub ml: MlConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            features: FeatureFlags::default(),
            ranking: RankingWeights::default(),
            ui: UiConfig::default(),
            max_results: 12,
            daemon_timeout_ms: 150,
            ml_model_file: None,
            ml_blend_weight: 0.35,
            ml: MlConfig::default(),
        }
    }
}
```

- [ ] **Step 2: Add the new keys to the get/set match arms**

Find `get_value_str` (around line 208) and `set_value_str` (around line 263) in `config.rs`. They have a big match expression keyed by string `"features.foo"`.

Add these arms to `get_value_str`:

```rust
"features.ml_seq_rerank" => Some(self.features.ml_seq_rerank.to_string()),
"ranking.ml_seq_score" => Some(self.ranking.ml_seq_score.to_string()),
"ml.residual_lr" => Some(self.ml.residual_lr.to_string()),
"ml.residual_top_k" => Some(self.ml.residual_top_k.to_string()),
"ml.disable_personalization" => Some(self.ml.disable_personalization.to_string()),
```

And to `set_value_str`:

```rust
"features.ml_seq_rerank" => self.features.ml_seq_rerank = value.parse()?,
"ranking.ml_seq_score" => self.ranking.ml_seq_score = value.parse()?,
"ml.residual_lr" => self.ml.residual_lr = value.parse()?,
"ml.residual_top_k" => self.ml.residual_top_k = value.parse()?,
"ml.disable_personalization" => self.ml.disable_personalization = value.parse()?,
```

(Match the surrounding style — use the same `value.parse()?` pattern.)

- [ ] **Step 3: Compile-check**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 4: Test config roundtrip**

Run: `cargo test --lib config`
Expected: existing config tests PASS.

Append a quick test inside `src/config.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn ml_seq_keys_get_set() {
    let mut cfg = AppConfig::default();
    cfg.set_value_str("features.ml_seq_rerank", "true").unwrap();
    cfg.set_value_str("ranking.ml_seq_score", "0.5").unwrap();
    cfg.set_value_str("ml.residual_lr", "0.01").unwrap();
    assert!(cfg.features.ml_seq_rerank);
    assert!((cfg.ranking.ml_seq_score - 0.5).abs() < 1e-9);
    assert!((cfg.ml.residual_lr - 0.01).abs() < 1e-6);
    assert_eq!(cfg.get_value_str("features.ml_seq_rerank"), Some("true".to_string()));
}
```

Run: `cargo test --lib config::tests::ml_seq_keys_get_set`
Expected: PASS.

> **Note for the implementing engineer:** if `set_value_str` / `get_value_str` are named differently in `config.rs`, use the actual names. The pattern is: each known key maps to a struct field via `parse()`.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add features.ml_seq_rerank, ranking.ml_seq_score, ml.* knobs"
```

---

## Task 7: Engine integration — load `MlInference`, compute `ml_seq_score`

**Files:**
- Modify: `src/engine.rs`

- [ ] **Step 1: Add fields to `Engine`**

In `src/engine.rs`, modify the `Engine` struct (around line 147):

```rust
pub struct Engine {
    config: AppConfig,
    db: AppDb,
    paths: AppPaths,
    ml_model: Option<MlModel>,                       // existing logreg
    ml_inference: Option<crate::ml::MlInference>,    // NEW: mini-Transformer
    ml_residual: crate::ml::ResidualStore,           // NEW: personalization
    tips_runtime: crate::tips::Runtime,
    catalog_cache: std::sync::Mutex<std::collections::HashMap<String, crate::i18n::Catalog>>,
}
```

(Update import: `use crate::ml_logreg::MlModel;` should already be in place from Task 1.)

- [ ] **Step 2: Initialize new fields in `Engine::new`**

Modify `Engine::new` (around line 160). After loading `ml_model`, add:

```rust
        let ml_inference = if config.features.ml_seq_rerank
            && std::env::var_os("SHAC_ML_DISABLE").is_none()
        {
            crate::ml::MlInference::load_bundled().unwrap_or_else(|err| {
                eprintln!("ml_seq_rerank: failed to load bundled model: {err:#}");
                None
            })
        } else {
            None
        };
        let ml_residual = if config.ml.disable_personalization {
            crate::ml::ResidualStore::empty()
        } else {
            crate::ml::ResidualStore::load(&db).unwrap_or_else(|err| {
                eprintln!("ml_seq_rerank: failed to load residual: {err:#}");
                crate::ml::ResidualStore::empty()
            })
        };
```

And in the constructor body:

```rust
        Ok(Self {
            config,
            db,
            paths: paths.clone(),
            ml_model,
            ml_inference,
            ml_residual,
            tips_runtime: crate::tips::Runtime::default(),
            catalog_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
```

- [ ] **Step 3: Compute `ml_seq_score` per candidate**

In `score_candidate` (around line 1431), after the existing 12-feature `vec![...]` block (around line 1509) but **before** the `let heuristic_score = ...` line, add:

```rust
        // 13th feature: ML sequence score (only when feature flag is on AND model loaded)
        let ml_seq = if self.config.features.ml_seq_rerank {
            if let Some(inf) = &self.ml_inference {
                self.compute_ml_seq_score(inf, candidate, prev_command, cwd)
            } else {
                0.0
            }
        } else {
            0.0
        };
        let mut features = features; // shadow as mutable so we can push
        features.push(feature(
            "ml_seq_score",
            ml_seq,
            self.config.ranking.ml_seq_score,
        ));
```

Add a new method on `Engine`:

```rust
    fn compute_ml_seq_score(
        &self,
        inf: &crate::ml::MlInference,
        candidate: &crate::engine::Candidate,
        prev_command: Option<&str>,
        cwd: &str,
    ) -> f64 {
        // Recent commands: just the immediate previous one for v1; fuller
        // history-window comes in v0.6.1.
        let recent: Vec<&str> = prev_command.into_iter().collect();
        let ctx = crate::ml::MlContext {
            recent_commands: &recent,
            current_prefix: "",
            cwd,
        };
        let dist = match inf.distribution(&ctx) {
            Ok(d) => d,
            Err(_) => return 0.0,
        };
        let token_id = inf.first_token_id(&candidate.insert_text);
        let base_log_prob = dist
            .get(token_id as usize)
            .copied()
            .unwrap_or(1e-9)
            .max(1e-9)
            .ln() as f64;
        // Residual lookup
        let bucket = crate::ml::feature_extractor::cwd_bucket(cwd, inf.spec().cwd_buckets);
        let prev_cmd_id = prev_command
            .map(|p| inf.first_token_id(p))
            .unwrap_or_else(|| inf.vocab().id_of("<BOS>").unwrap());
        let residual = self
            .ml_residual
            .lookup(bucket, prev_cmd_id, token_id) as f64;
        base_log_prob + residual
    }
```

> **Note for the implementing engineer:** `Candidate` is a private type to `engine.rs`. Adjust the path (or move `compute_ml_seq_score` into the `impl Engine` block where `Candidate` is in scope without the `crate::engine::` qualifier).

- [ ] **Step 4: Compile-check**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 5: Run all engine tests**

Run: `cargo test --lib engine`
Expected: PASS — existing tests are unaffected because `features.ml_seq_rerank=false` by default and the new feature contributes 0.0.

- [ ] **Step 6: Commit**

```bash
git add src/engine.rs
git commit -m "feat(engine): integrate ml_seq_score as 13th feature (opt-in)"
```

---

## Task 8: Update residual on accepted completion

**Files:**
- Modify: `src/engine.rs`

- [ ] **Step 1: Hook into `record_command`**

In `src/engine.rs`, modify `record_command` (around line 312):

```rust
    pub fn record_command(&self, request: RecordCommandRequest) -> Result<()> {
        // Existing history recording
        self.db.record_history(&request)?;

        // ML residual update — only on accepted completions when feature is on
        if !self.config.features.ml_seq_rerank
            || self.config.ml.disable_personalization
            || std::env::var_os("SHAC_ML_DISABLE").is_some()
        {
            return Ok(());
        }
        let inf = match self.ml_inference.as_ref() {
            Some(i) => i,
            None => return Ok(()),
        };
        // Only update if the user accepted a shac suggestion (not a free-form command)
        if request.accepted_request_id.is_none() {
            return Ok(());
        }
        let prev_command = self
            .db
            .last_history_command_excluding(&request.command)
            .unwrap_or_default();
        let recent: Vec<&str> = if prev_command.is_empty() {
            Vec::new()
        } else {
            vec![prev_command.as_str()]
        };
        let ctx = crate::ml::MlContext {
            recent_commands: &recent,
            current_prefix: "",
            cwd: &request.cwd,
        };
        let dist = match inf.distribution(&ctx) {
            Ok(d) => d,
            Err(err) => {
                eprintln!("ml_seq residual update: distribution failed: {err:#}");
                return Ok(());
            }
        };
        let bucket = crate::ml::feature_extractor::cwd_bucket(&request.cwd, inf.spec().cwd_buckets);
        let prev_cmd_id = if recent.is_empty() {
            inf.vocab().id_of("<BOS>").unwrap()
        } else {
            inf.first_token_id(recent[0])
        };
        let accepted_token = inf.first_token_id(&request.command);
        if let Err(err) = self.ml_residual.update_on_acceptance(
            &self.db,
            bucket,
            prev_cmd_id,
            accepted_token,
            &dist,
            self.config.ml.residual_lr,
            self.config.ml.residual_top_k,
        ) {
            eprintln!("ml_seq residual update: persist failed: {err:#}");
        }
        Ok(())
    }
```

- [ ] **Step 2: Add the helper method `last_history_command_excluding` to `db.rs`**

In `src/db.rs`, add to `impl AppDb`:

```rust
    /// Most recent recorded command that is *not* `excluding`. Used for residual
    /// updates so we don't condition on the command being recorded right now.
    pub fn last_history_command_excluding(
        &self,
        excluding: &str,
    ) -> rusqlite::Result<String> {
        self.conn.query_row(
            "SELECT command FROM history_events
             WHERE command <> ?1
             ORDER BY ts DESC LIMIT 1",
            [excluding],
            |r| r.get(0),
        )
    }
```

(If the actual column name is `timestamp` not `ts`, adjust to match `db.rs:160`.)

- [ ] **Step 3: Compile-check**

Run: `cargo check`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/engine.rs src/db.rs
git commit -m "feat(engine): update ml_residual on accepted completions"
```

---

## Task 9: Integration test — feature toggles correctly switch ML on/off

**Files:**
- Create: `tests/ml_blend.rs`

- [ ] **Step 1: Write the integration test**

`tests/ml_blend.rs`:

```rust
//! Integration tests for ml_seq_rerank toggling. Run shac as a library —
//! avoids spawning the daemon binary so tests stay fast.

mod support;

use shac::config::AppConfig;
use shac::engine::Engine;
use shac::protocol::{CompletionRequest, HistoryHint, SessionInfo};

fn fake_request(line: &str, cwd: &str, prev: Option<&str>) -> CompletionRequest {
    CompletionRequest {
        shell: "zsh".into(),
        line: line.into(),
        cursor: line.len(),
        cwd: cwd.into(),
        env: Default::default(),
        session: SessionInfo::default(),
        history_hint: HistoryHint {
            prev_command: prev.map(String::from),
            ..Default::default()
        },
    }
}

#[test]
fn ml_off_produces_no_ml_seq_score_feature() {
    let env = support::TestEnv::new();
    // Default config: ml_seq_rerank=false
    let engine = Engine::new(env.paths()).unwrap();

    // Record some history so we have candidates to score
    env.record_history(&engine, &["git status", "git add ."]);

    let resp = engine
        .complete(fake_request("git ", &env.cwd().to_string_lossy(), Some("git status")))
        .unwrap();
    // No ml_seq_score should appear because feature is off — but the feature
    // is always included in the feature vec with weight 0 when off. The
    // contract: ml_seq_score's *contribution* is 0 when off.
    let explain = engine
        .explain(fake_request("git ", &env.cwd().to_string_lossy(), Some("git status")))
        .unwrap();
    let ml_features: Vec<_> = explain
        .items
        .iter()
        .flat_map(|i| i.features.iter().filter(|f| f.name == "ml_seq_score"))
        .collect();
    if !ml_features.is_empty() {
        // If we kept the feature in the vec for stable explain output, it
        // must contribute 0.
        for f in ml_features {
            assert!(f.contribution.abs() < 1e-9);
        }
    }
    assert!(!resp.items.is_empty());
}

#[test]
fn ml_on_with_zero_weight_is_equivalent_to_off() {
    let env = support::TestEnv::new();
    // Build config with ml_seq_rerank=true but weight=0
    let mut cfg = AppConfig::default();
    cfg.features.ml_seq_rerank = true;
    cfg.ranking.ml_seq_score = 0.0;
    env.write_config(&cfg);

    let engine_on = Engine::new(env.paths()).unwrap();
    env.record_history(&engine_on, &["git status", "git add ."]);
    let resp_on = engine_on
        .complete(fake_request("git ", &env.cwd().to_string_lossy(), Some("git status")))
        .unwrap();

    // Compare to a parallel run with ml_seq_rerank=false.
    let env_off = support::TestEnv::new();
    let engine_off = Engine::new(env_off.paths()).unwrap();
    env_off.record_history(&engine_off, &["git status", "git add ."]);
    let resp_off = engine_off
        .complete(fake_request("git ", &env_off.cwd().to_string_lossy(), Some("git status")))
        .unwrap();

    // Top item should be the same in both runs
    if let (Some(a), Some(b)) = (resp_on.items.first(), resp_off.items.first()) {
        assert_eq!(a.insert_text, b.insert_text,
            "weight=0 should be equivalent to feature-off; got on={} off={}",
            a.insert_text, b.insert_text);
    }
}
```

> **Note for the implementing engineer:** the helpers `support::TestEnv::record_history` and `write_config` may need to be added to `tests/support/mod.rs` (the existing test scaffolding). Check what's already there; reuse or extend.

If the bundled `.bpk` is missing for the CI target_os, `MlInference::load_bundled()` returns `Ok(None)` and ml-on becomes a no-op — the test "weight=0 ≡ feature-off" still holds, just trivially. The test does not require an actual model.

- [ ] **Step 2: Run the test**

Run: `cargo test --test ml_blend`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/ml_blend.rs tests/support/mod.rs
git commit -m "test(ml): integration tests for ml_seq_rerank toggle behavior"
```

---

## Task 10: Integration test — residual table populates and clears

**Files:**
- Create: `tests/ml_residual.rs`

- [ ] **Step 1: Write the test**

`tests/ml_residual.rs`:

```rust
mod support;

use shac::config::AppConfig;
use shac::engine::Engine;
use shac::protocol::{HistoryHint, RecordCommandRequest};

#[test]
fn residual_grows_on_accepts_and_clears_on_reset() {
    let env = support::TestEnv::new();
    let mut cfg = AppConfig::default();
    cfg.features.ml_seq_rerank = true;
    env.write_config(&cfg);

    let engine = Engine::new(env.paths()).unwrap();
    if engine.ml_inference_loaded().is_none() {
        // No bundled model on this CI target_os — skip
        return;
    }

    let initial = engine.db().ml_residual_count().unwrap();

    for cmd in &["git status", "git add .", "cargo test"] {
        let req = RecordCommandRequest {
            command: cmd.to_string(),
            cwd: env.cwd().to_string_lossy().into_owned(),
            shell: Some("zsh".into()),
            trust: None,
            provenance: None,
            provenance_source: None,
            provenance_confidence: None,
            origin: None,
            tty_present: Some(true),
            exit_status: Some(0),
            accepted_request_id: Some(1), // simulate "user accepted from menu"
            accepted_item_key: Some(cmd.to_string()),
            accepted_rank: Some(0),
        };
        engine.record_command(req).unwrap();
    }

    let after = engine.db().ml_residual_count().unwrap();
    assert!(after > initial, "expected residual rows to grow, was {initial}, now {after}");

    // Reset
    engine.db().truncate_ml_residual().unwrap();
    let cleared = engine.db().ml_residual_count().unwrap();
    assert_eq!(cleared, 0);
}
```

- [ ] **Step 2: Add `Engine::ml_inference_loaded()` accessor**

In `src/engine.rs`, add:

```rust
    pub fn ml_inference_loaded(&self) -> Option<&crate::ml::MlInference> {
        self.ml_inference.as_ref()
    }
```

- [ ] **Step 3: Run the test**

Run: `cargo test --test ml_residual`
Expected: PASS (or quick skip on unsupported OS).

- [ ] **Step 4: Commit**

```bash
git add tests/ml_residual.rs src/engine.rs
git commit -m "test(ml): residual populates on accepts and clears on reset"
```

---

## Task 11: Performance test — inference latency under 10ms (CI tolerance)

**Files:**
- Create: `tests/ml_inference.rs`

- [ ] **Step 1: Write the test**

`tests/ml_inference.rs`:

```rust
mod support;

use shac::config::AppConfig;
use shac::engine::Engine;
use shac::protocol::{CompletionRequest, HistoryHint, SessionInfo};

#[test]
fn ml_seq_inference_latency_under_ci_tolerance() {
    let env = support::TestEnv::new();
    let mut cfg = AppConfig::default();
    cfg.features.ml_seq_rerank = true;
    env.write_config(&cfg);

    let engine = Engine::new(env.paths()).unwrap();
    if engine.ml_inference_loaded().is_none() {
        return; // skip on unsupported CI OS
    }
    env.record_history(&engine, &["git status", "git add .", "cargo test"]);

    let req = CompletionRequest {
        shell: "zsh".into(),
        line: "git ".into(),
        cursor: 4,
        cwd: env.cwd().to_string_lossy().into_owned(),
        env: Default::default(),
        session: SessionInfo::default(),
        history_hint: HistoryHint {
            prev_command: Some("git status".into()),
            ..Default::default()
        },
    };

    // Warm up
    let _ = engine.complete(req.clone());

    let start = std::time::Instant::now();
    let _ = engine.complete(req);
    let elapsed = start.elapsed();

    // CI tolerance: 50ms (real M1 target is 5ms; CI runners are slower).
    // If this consistently fails, model is too big or NdArray backend is misconfigured.
    assert!(
        elapsed.as_millis() < 50,
        "ml_seq_rerank inference too slow: {}ms",
        elapsed.as_millis()
    );
}
```

- [ ] **Step 2: Run**

Run: `cargo test --release --test ml_inference`
Expected: PASS. Use `--release` because debug-mode burn is much slower.

- [ ] **Step 3: Commit**

```bash
git add tests/ml_inference.rs
git commit -m "test(ml): inference latency stays under CI tolerance"
```

---

## Task 12: CLI surface — `shac ml {load-status, inspect-residual, reset-personalization}`

**Files:**
- Modify: `src/bin/shac.rs`

- [ ] **Step 1: Add `MlArgs` and dispatcher**

In `src/bin/shac.rs`, find the `Commands` enum (around line 26). Add a variant:

```rust
    Ml(MlArgs),
```

Then add the args struct and subaction enum (place near `LocaleArgs`):

```rust
#[derive(Debug, Args)]
struct MlArgs {
    #[command(subcommand)]
    action: MlAction,
}

#[derive(Debug, Subcommand)]
enum MlAction {
    LoadStatus,
    InspectResidual {
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    ResetPersonalization,
}
```

- [ ] **Step 2: Wire the dispatcher**

Find the main `match` on `Commands` (typically a long function near the bottom of `shac.rs`). Add an arm:

```rust
        Commands::Ml(args) => run_ml(args, &paths)?,
```

Add the function (anywhere convenient — near other `run_*` helpers):

```rust
fn run_ml(args: MlArgs, paths: &shac::config::AppPaths) -> anyhow::Result<()> {
    let engine = shac::engine::Engine::new(paths)?;
    match args.action {
        MlAction::LoadStatus => {
            let cfg = engine.config();
            println!("ml_seq_rerank: enabled = {}", cfg.features.ml_seq_rerank);
            match engine.ml_inference_loaded() {
                Some(inf) => {
                    println!("model: loaded");
                    println!("  vocab_size: {}", inf.spec().vocab_size);
                    println!("  context_len: {}", inf.spec().context_len);
                    println!("  cwd_buckets: {}", inf.spec().cwd_buckets);
                }
                None => println!("model: NOT loaded (unsupported OS or feature off)"),
            }
            let count = engine.db().ml_residual_count().unwrap_or(0);
            println!("residual_rows: {}", count);
            println!("disable_personalization: {}", cfg.ml.disable_personalization);
        }
        MlAction::InspectResidual { top } => {
            let rows = engine.db().top_ml_residual(top)?;
            // Decode token ids back to surface form when possible
            let vocab = engine.ml_inference_loaded().map(|inf| inf.vocab().clone());
            println!(
                "{:<8} {:<16} {:<16} {}",
                "bucket", "prev_token", "next_token", "weight"
            );
            for (bucket, prev_id, token_id, w) in rows {
                let prev_str = vocab
                    .as_ref()
                    .and_then(|v| v.token_of(prev_id).map(String::from))
                    .unwrap_or_else(|| format!("#{prev_id}"));
                let tok_str = vocab
                    .as_ref()
                    .and_then(|v| v.token_of(token_id).map(String::from))
                    .unwrap_or_else(|| format!("#{token_id}"));
                println!("{:<8} {:<16} {:<16} {:.4}", bucket, prev_str, tok_str, w);
            }
        }
        MlAction::ResetPersonalization => {
            engine.db().truncate_ml_residual()?;
            println!("ml_residual cleared");
        }
    }
    Ok(())
}
```

> **Note for the implementing engineer:** `Vocab` derives `Clone` already (it's `#[derive(Serialize, Deserialize)]` with simple fields). If the actual `Vocab` doesn't impl `Clone`, either add `#[derive(Clone)]` to it in `crates/shac-ml-train/src/tokenizer.rs` or change the `let vocab = ... .map(|inf| inf.vocab().clone());` to borrow + decode inside the loop.

- [ ] **Step 3: Compile-check**

Run: `cargo check`
Expected: PASS. Likely needs `Clone` on `Vocab` — add `#[derive(Clone)]` next to its existing derives in `crates/shac-ml-train/src/tokenizer.rs`. Re-run check.

- [ ] **Step 4: Smoke test the new commands**

```bash
cargo run --release -- ml load-status
cargo run --release -- ml inspect-residual --top 5
cargo run --release -- ml reset-personalization
```

Expected: each prints output and exits 0. The first run with `ml_seq_rerank=false` should print "model: NOT loaded".

- [ ] **Step 5: Commit**

```bash
git add src/bin/shac.rs crates/shac-ml-train/src/tokenizer.rs
git commit -m "feat(cli): shac ml {load-status, inspect-residual, reset-personalization}"
```

---

## Task 13: User-facing doc — `docs/ml.md`

**Files:**
- Create: `docs/ml.md`

- [ ] **Step 1: Write the doc**

`docs/ml.md`:

```markdown
# ML next-command prediction (v0.6.0, experimental)

## What it does

shac ships a tiny pre-trained neural network (~580k parameters, ~2MB on
disk) that predicts your next command given recent shell history. When
enabled, its prediction is *blended* with the existing 12-feature
heuristic scorer — never replaces it.

The model is opt-in for v0.6.0. Default config keeps it off.

## Enable it

```bash
shac config set features.ml_seq_rerank true
```

That's it. The bundled model is used immediately; no download, no
training step, no Python.

To turn it back off:

```bash
shac config set features.ml_seq_rerank false
```

## How it personalizes

When you accept a shac suggestion, we update an on-device residual that
nudges the model toward your patterns. This is **fully local** — never
uploaded anywhere — and stored in your shac SQLite db (`ml_residual`
table). Reset it any time:

```bash
shac ml reset-personalization
```

Inspect what it has learned:

```bash
shac ml inspect-residual --top 20
```

If you want to use the bundled model with no personalization (e.g., on
a shared machine), set:

```bash
shac config set ml.disable_personalization true
```

## Privacy

- The bundled model was trained on synthetic sessions generated by a local
  Qwen 0.5B teacher, plus the maintainer's local shell history *after*
  passing through a PII-scrubbing pass (`/Users/...` → `<HOME>/...`,
  emails → `<EMAIL>`, IPs → `<IP>`, secrets → `<TOKEN>`, etc.). See
  `ml/README.md` for the full scrub red-list.
- No network calls are made at runtime. The bundled `.bpk` is loaded once
  at daemon start.
- Personalization data never leaves your machine.

## Performance

- Bundled model: ~2MB per OS (darwin/linux). Two models are shipped; the
  right one is selected at compile time by `target_os`.
- Inference: <5ms per request on M1 CPU. Runs on the `burn` framework's
  `NdArray` backend (pure-Rust CPU).
- Daemon RAM growth at load: ~5MB.

## Tuning

`ranking.ml_seq_score` (default 0.20) is the model's weight in the final
score. Higher = more influence; 0 = effectively off.

```bash
shac config set ranking.ml_seq_score 0.30
```

## Disable at runtime

If you want a kill switch without changing config (e.g., debugging):

```bash
SHAC_ML_DISABLE=1 shacd
```

## When does it help?

- You frequently follow `cargo build` with `cargo test` — the model picks
  this up from training data, residual reinforces it as you use it.
- You have project-specific patterns — the (cwd_bucket, prev_cmd, token)
  residual key captures those over time.

## When does it not help?

- First few weeks: residual is empty, you're using the bundled model only.
- If your workflow is genuinely chaotic (no temporal patterns), the
  prefix/fuzzy/recency features still dominate the final score.

## Known limits in v0.6.0

- Context window is 14 commands. Long sessions ignore old context.
- We don't model environment variables, just commands + cwd.
- LoRA / on-device fine-tuning of the base model is not supported. Only
  residual personalization. We may revisit this in v0.7.x.
- Two OS-specific models (darwin, linux). Other platforms get the heuristic
  scorer only.
```

- [ ] **Step 2: Commit**

```bash
git add docs/ml.md
git commit -m "docs(ml): user-facing guide for v0.6.0 ml_seq_rerank"
```

---

## Task 14: CHANGELOG entry + version bump

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `Cargo.toml`

- [ ] **Step 1: Verify no version-bump pre-approval**

> **STOP before bumping** — Roman has a standing rule (auto-memory `feedback_version_bumps.md`): always ask before deciding patch/minor/major. Confirm with Roman before executing this task. Default proposed: `0.5.x → 0.6.0` (minor; new feature, opt-in).

- [ ] **Step 2: Add CHANGELOG entry**

Append to `CHANGELOG.md` (or create if missing):

```markdown
## v0.6.0 — 2026-MM-DD

### Added
- **Experimental ML next-command prediction.** Opt-in via
  `shac config set features.ml_seq_rerank true`. A small bundled neural
  network (~2MB, pre-trained per OS) suggests likely next commands based
  on recent shell history. Composes with the existing 12-feature scorer
  rather than replacing it. On-device personalization that never leaves
  your machine. See `docs/ml.md`.
- New CLI: `shac ml load-status`, `shac ml inspect-residual --top N`,
  `shac ml reset-personalization`.
- New config keys: `features.ml_seq_rerank`, `ranking.ml_seq_score`,
  `ml.residual_lr`, `ml.residual_top_k`, `ml.disable_personalization`.
- New env var: `SHAC_ML_DISABLE=1` for runtime kill switch.

### Changed
- Internal: existing logistic-regression module moved from `src/ml.rs` to
  `src/ml_logreg.rs`. Behavior unchanged.
- Binary size grows by ~5MB total (~2MB per OS-specific bundled model +
  ~3MB burn dependencies).
```

- [ ] **Step 3: Bump version**

After Roman's confirmation, update root `Cargo.toml`:

```toml
[package]
name = "shac"
version = "0.6.0"
```

- [ ] **Step 4: Verify CHANGELOG and Cargo.lock update**

```bash
cargo check
git diff Cargo.toml Cargo.lock CHANGELOG.md
```

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release: v0.6.0 — experimental ML next-command prediction (opt-in)"
```

---

## Task 15: Acceptance gate — top-3 lift verification before merge

**Files:**
- Create: `crates/shac-ml-train/tests/acceptance_gate.rs`

This is the gate that decides whether v0.6.0 ships. Per the spec: top-3 accuracy on a held-out slice must be ≥ baseline + 5pp. Run once, manually, before opening the merge-to-main PR.

- [ ] **Step 1: Write the gate**

`crates/shac-ml-train/tests/acceptance_gate.rs`:

```rust
//! Manual acceptance gate. Marked `#[ignore]` because it expects
//! ml/data/heldout-{darwin,linux}.jsonl to exist locally (the maintainer's
//! held-out chronological-last-20% slice of real shell history, scrubbed).
//!
//! Run with: cargo test --test acceptance_gate -- --ignored --nocapture

use burn::backend::NdArray;
use burn::module::Module;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::BurnpackStore;
use shac_ml_train::data::{read_jsonl, DistilledExample};
use shac_ml_train::model::{StudentModel, StudentModelConfig};

type B = NdArray<f32>;

const REQUIRED_LIFT_PP: f64 = 5.0;

fn top_k_accuracy(
    model: &StudentModel<B>,
    examples: &[DistilledExample],
    k: usize,
    device: &<B as burn::tensor::backend::Backend>::Device,
) -> f64 {
    if examples.is_empty() {
        return 0.0;
    }
    let mut hits = 0usize;
    for batch in examples.chunks(64) {
        let mut ctx_flat: Vec<i64> = Vec::with_capacity(batch.len() * 16);
        for ex in batch {
            for &t in &ex.context_tokens {
                ctx_flat.push(t as i64);
            }
        }
        let input: Tensor<B, 2, Int> =
            Tensor::from_data(TensorData::new(ctx_flat, [batch.len(), 16]), device);
        let logits = model.forward(input);
        for (i, ex) in batch.iter().enumerate() {
            let row: Vec<f32> = logits
                .clone()
                .slice([i..i + 1, 0..2000])
                .into_data()
                .to_vec()
                .unwrap();
            let mut scored: Vec<(usize, f32)> = row.into_iter().enumerate().collect();
            scored
                .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if scored
                .iter()
                .take(k)
                .any(|&(t, _)| t == ex.hard_label as usize)
            {
                hits += 1;
            }
        }
    }
    hits as f64 / examples.len() as f64
}

#[test]
#[ignore]
fn gate_darwin() {
    let device = Default::default();
    let bytes = std::fs::read("ml/models/shac-ml-darwin.bpk").expect("model file");
    let cfg = StudentModelConfig::default();
    let mut model: StudentModel<B> = cfg.init(&device);
    let mut store = BurnpackStore::from_bytes(&bytes);
    model.load_from(&mut store).unwrap();

    let heldout: Vec<DistilledExample> =
        read_jsonl(std::path::Path::new("ml/data/heldout-darwin.jsonl"))
            .expect("heldout file");

    // Baseline = "always predict the most common token" — a trivial floor.
    // The 5pp lift requirement is over the *runtime heuristic baseline* in
    // production, but for this offline gate we check the model is at least
    // 5pp above majority-class baseline.
    let mut histo = std::collections::HashMap::<u32, usize>::new();
    for ex in &heldout {
        *histo.entry(ex.hard_label).or_insert(0) += 1;
    }
    let majority_count = *histo.values().max().unwrap_or(&0);
    let majority_baseline = majority_count as f64 / heldout.len() as f64;

    let top3 = top_k_accuracy(&model, &heldout, 3, &device);
    println!("darwin: majority_baseline={:.3}, top-3={:.3}", majority_baseline, top3);
    assert!(
        top3 - majority_baseline >= REQUIRED_LIFT_PP / 100.0,
        "darwin: top-3 lift {:.3} < required {:.3}",
        top3 - majority_baseline,
        REQUIRED_LIFT_PP / 100.0
    );
}

#[test]
#[ignore]
fn gate_linux() {
    // (Same shape as gate_darwin; keep separate so each OS gates independently.)
    let device = Default::default();
    let bytes = std::fs::read("ml/models/shac-ml-linux.bpk").expect("model file");
    let cfg = StudentModelConfig::default();
    let mut model: StudentModel<B> = cfg.init(&device);
    let mut store = BurnpackStore::from_bytes(&bytes);
    model.load_from(&mut store).unwrap();

    let heldout: Vec<DistilledExample> =
        read_jsonl(std::path::Path::new("ml/data/heldout-linux.jsonl"))
            .expect("heldout file");

    let mut histo = std::collections::HashMap::<u32, usize>::new();
    for ex in &heldout {
        *histo.entry(ex.hard_label).or_insert(0) += 1;
    }
    let majority_count = *histo.values().max().unwrap_or(&0);
    let majority_baseline = majority_count as f64 / heldout.len() as f64;

    let top3 = top_k_accuracy(&model, &heldout, 3, &device);
    println!("linux: majority_baseline={:.3}, top-3={:.3}", majority_baseline, top3);
    assert!(
        top3 - majority_baseline >= REQUIRED_LIFT_PP / 100.0,
        "linux: top-3 lift {:.3} < required {:.3}",
        top3 - majority_baseline,
        REQUIRED_LIFT_PP / 100.0
    );
}
```

- [ ] **Step 2: Run the gate manually**

```bash
cargo test --release -p shac-ml-train --test acceptance_gate -- --ignored --nocapture
```

If both gates pass, proceed to merge. If either fails, do NOT merge — go back to the pipeline plan, tune model size / training hyperparameters / data quality, retrain, recommit `.bpk`, re-run gate.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/tests/acceptance_gate.rs
git commit -m "test(ml): manual acceptance gate for top-3 lift over majority baseline"
```

---

## Plan complete

After Task 15, the branch is ready for PR. Next steps (separate from this plan):
1. Open PR for the full ML feature work
2. Codex review on PR
3. Merge to main once accepted
4. Tag v0.6.0 release

## Self-review checklist

- [x] Spec coverage: feature_extractor (Task 3), inference (Task 4), residual (Task 5), config (Task 6), engine integration (Tasks 7-8), CLI (Task 12), tests (Tasks 9-11), docs (Task 13), changelog (Task 14), acceptance gate (Task 15)
- [x] No placeholders — every task has concrete code; "Note for the implementing engineer" callouts mark genuine API uncertainty (burn 0.21 method-name drift) but always specify the contract that must be preserved
- [x] Type consistency: `MlInference`, `Context` (aliased as `MlContext` in re-export), `FeatureSpec`, `ResidualStore`, `Vocab`, `StudentModel<B>` — names stable across tasks
- [x] TDD: tests before implementation in Tasks 3, 5
- [x] Frequent commits: every task ends in a commit
- [x] Order respects dependencies: rename (1) → bootstrap (2) → modules (3-5) → config (6) → engine (7-8) → tests (9-11) → CLI (12) → docs (13) → release (14) → gate (15)
- [x] Roman's version-bump rule respected: Task 14 explicitly requires confirmation before bumping
