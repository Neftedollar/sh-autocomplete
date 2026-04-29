# shac ML Next-Command Prediction — Design Spec

**Date:** 2026-04-29
**Status:** approved, ready for implementation plan
**Target version:** v0.6.0 (experimental, opt-in)

## Problem

shac's current ranking engine is a 12-feature linear model: `prefix_score`, `fuzzy_score`, `transition_score`, `recency_score`, etc. It captures direct signals well but misses longer-range patterns (e.g., "in this project I usually run cargo test after cargo build, even though the bigram cargo build → cargo test isn't dominant globally"). The `transitions` table is bigram-only; longer contexts blow up combinatorially without a parametric model.

We want a small neural network that predicts the next command given a context window (recent commands, cwd, environment), composed with the existing scorer rather than replacing it. The constraint: ship-in-binary, sub-5ms inference, single-binary install story preserved.

## Goals

1. Improve top-3 / top-5 ranking accuracy on real shell history by ≥5 percentage points over the current heuristic baseline.
2. Ship as opt-in feature in v0.6.0 (`features.ml_seq_rerank=false` by default; `true` to enable).
3. No Python at any stage — training, inference, data preparation. Pure-Rust workspace.
4. Preserve privacy: bundled model contains no personal paths or identifiers; on-device personalization (residual) never leaves the user's machine.
5. Keep production binary lean: bundled ONNX models ≤5MB combined, daemon RAM growth ≤50MB at load.

**Success metric:** after dogfooding for 2 weeks, Roman reports observable improvement in Tab acceptance rate (subjective) and top-3 accuracy on a held-out slice (chronological last 20% of his real shell history, never used in training) is ≥5pp above the **current scorer baseline** (existing 12-feature engine running on the same held-out, with `features.ml_seq_rerank=false`).

## Non-goals

- Replacing the existing 12-feature scorer (we *blend*, not replace)
- LoRA / full fine-tuning on user device — deferred to a future spec when burn matures or Python toolchain is acceptable
- GPU/distributed training — all pipelines run on a single CPU/Mac M1
- Models above ~5M parameters — we want sub-5ms CPU inference
- LLM-style generative completion (token-by-token writing) — out of scope; we predict the *next command unit*, not free text
- Continual learning automation — user-triggered `shac ml retrain` is not in v0.6.0

## Architecture overview

Two completely separate code paths, coupled only by the bundled ONNX artifact:

**Maintainer pipeline (offline, run by repo maintainer before release):**
1. Generate synthetic training data via local Qwen 0.5B
2. Scrub PII from real history corpus
3. Distillation pass: query Qwen as teacher, save soft targets
4. Train tiny student model with mixed (hard + soft) loss using `burn`
5. Export ONNX, commit to repo
6. Repeat per OS (darwin, linux)

**Production runtime (in shipped daemon):**
1. Bundled ONNX is loaded via `tract` at daemon start
2. On each `/complete` request, build context features, run forward pass, get distribution over vocab
3. For each candidate from existing scorer, look up its first-token probability in the NN distribution → `ml_seq_score` feature
4. Blend `ml_seq_score` with the 12 existing features via the existing `RankingWeights` system
5. Apply per-user residual personalization (online, in-daemon) on top of NN logits

```
                    MAINTAINER WORKFLOW (one-time, ~6 hours total)
   ┌─────────────────────────────────────────────────────────────────┐
   │                                                                 │
   │  ml/data/personas.toml ──▶ gen-synthetic (Qwen) ──▶ synthetic-  │
   │                                                     {os}.jsonl  │
   │  zsh_history import ──▶ scrub ────────────────────▶ scrubbed-   │
   │                                                     {os}.jsonl  │
   │                                                                 │
   │  scrubbed + synthetic ──▶ distill (Qwen teacher) ─▶ distill-    │
   │                                                     {os}.jsonl  │
   │                                                                 │
   │  distill-{os} ──▶ train (burn) ──▶ shac-ml-{os}.onnx (committed)│
   │                                                                 │
   └─────────────────────────────────────────────────────────────────┘
                                     │
                                     │ git push, ONNX bundled in shac binary
                                     ▼
                    PRODUCTION (in shipped daemon, sub-5ms hot path)
   ┌─────────────────────────────────────────────────────────────────┐
   │                                                                 │
   │  Tab pressed ──▶ engine.complete(req) ──▶ collect_candidates    │
   │                                                                 │
   │                          ┌─ tract.run(model, features) ──▶ NN   │
   │                          │   distribution over 2k vocab          │
   │                          ▼                                       │
   │  for each candidate:                                             │
   │    base_logit = nn_dist[first_token_id]                          │
   │    residual = ml_residual[(cwd_bucket, prev_cmd, token_id)]     │
   │    ml_seq_score = base_logit + residual                          │
   │                                                                 │
   │  rank with existing 12-feature scorer + ml_seq_score             │
   │                                                                 │
   │  on accepted_completion:                                         │
   │    update ml_residual via online SGD step                        │
   │                                                                 │
   └─────────────────────────────────────────────────────────────────┘
```

## Workspace layout

```
shac/
├── Cargo.toml                  workspace root
├── src/                        main shac (lib + bins)
│   ├── lib.rs
│   ├── ml/                     NEW: production ml inference module
│   │   ├── mod.rs
│   │   ├── feature_extractor.rs  build context features from CompletionRequest
│   │   ├── inference.rs          tract-based ONNX inference + caching
│   │   └── residual.rs           online residual personalization
│   ├── engine.rs               MODIFIED: integrate ml_seq_score
│   ├── db.rs                   MODIFIED: ml_residual table
│   ├── config.rs               MODIFIED: features.ml_seq_rerank, ranking.ml_seq_score
│   ├── protocol.rs
│   └── ...
├── crates/
│   └── shac-ml-train/          NEW: maintainer-only crate
│       ├── Cargo.toml          deps: burn, mistralrs, ndarray, anyhow
│       └── src/
│           ├── bin/
│           │   ├── gen_synthetic.rs
│           │   ├── scrub.rs
│           │   ├── distill.rs
│           │   └── train.rs
│           ├── personas.rs
│           ├── tokenizer.rs
│           ├── model.rs        student model definition (burn)
│           └── lib.rs
├── ml/                         data + artifacts (committed to repo)
│   ├── data/
│   │   ├── personas.toml       persona definitions
│   │   ├── synthetic-darwin.jsonl
│   │   ├── synthetic-linux.jsonl
│   │   ├── scrubbed-darwin.jsonl
│   │   ├── scrubbed-linux.jsonl
│   │   ├── distill-darwin.jsonl
│   │   └── distill-linux.jsonl
│   ├── models/
│   │   ├── shac-ml-darwin.onnx     ~2MB, included in binary
│   │   ├── shac-ml-linux.onnx      ~2MB, included in binary
│   │   ├── vocab.json              shared 2k vocab + special tokens
│   │   └── feature-spec.json       feature schema (cwd buckets, etc.)
│   └── README.md               training & rebuild instructions
└── ...
```

## Component: Synthetic data generation (`gen-synthetic`)

**Purpose:** produce realistic shell command sequences for training. Replaces external API call to Haiku/Anthropic.

**Tool:** local Qwen 2.5 0.5B Instruct (GGUF format, ~350MB weights downloaded once to `~/.cache/shac-ml/`). Run via `mistralrs` crate, pure Rust.

**Personas (`ml/data/personas.toml`):** ~20 distinct dev archetypes, each with:
- `os: darwin | linux`
- `cwd_pattern: ~/dev/<rust-project>` (template, used to seed realistic paths)
- `tools_installed: [git, cargo, rustup, brew, docker, ...]`
- `typical_session_length: 5..30`
- `style_prompt`: one-paragraph description ("a backend engineer writing a Rust web service, frequent cargo test cycles, deploys via docker")

Example excerpt:
```toml
[[persona]]
id = "rust-backend"
os = "darwin"
cwd_pattern = "~/dev/<project>"
tools_installed = ["git", "cargo", "rustup", "docker", "brew", "kubectl"]
typical_session_length = 12
style_prompt = """
A backend Rust engineer working on a web service. Frequent cycles of
cargo test, cargo build, git diff/add/commit, occasional docker compose
up for local services. Uses kubectl for production debugging.
"""
sessions_to_generate = 200
```

**Generation logic:**
1. For each persona × N sessions: build a prompt asking Qwen to "generate a realistic ~K-command shell session by this user". Specify cwd patterns, tool inventory, no synthetic shell prompts.
2. Parse Qwen's output into `CompletionEvent` records: `{ts, cwd, command, prev_command, persona_id}`
3. Reject malformed lines (commands containing newlines, suspiciously long, prompt-leaking).
4. Write to `ml/data/synthetic-{os}.jsonl`, one JSON record per line.

**Throughput:** ~50ms/token on M1 CPU via mistralrs, ~5 tokens/command average. Target volume: 20 personas × 100 sessions × 12 commands ≈ 24k commands per OS. Generation time: ~3 hours per OS on M1 CPU, ~30 minutes with Metal backend. Linux personas: separate generation pass on the same maintainer machine (Metal-accelerated where available); Linux dataset is synthetic only initially since real Linux history isn't available to the maintainer.

**Quality safeguards:**
- Vocab guardrails: reject sessions whose unique-command count drops below 5 (likely prompt-loop)
- Hard caps on retry: 3 attempts per persona, after which we accept whatever was generated
- Spot-check tooling: `gen-synthetic --dry-run --persona rust-backend` prints first 10 generated commands without writing JSONL

## Component: Path scrubbing (`scrub`)

**Purpose:** strip PII from real history (`~/.zsh_history`, shac `history_events`) before it joins the synthetic corpus. Bundled model must not leak personal paths/usernames/IPs.

**Implementation:** `crates/shac-ml-train/src/bin/scrub.rs` reads JSONL, applies regex transformations, writes scrubbed JSONL.

**Rules (v1):**
| Pattern | Replacement |
|---|---|
| `/Users/[^/]+/...` | `<HOME>/...` |
| `/home/[^/]+/...` | `<HOME>/...` |
| `/var/folders/[^/]+/[^/]+/...` | `<TMPDIR>/...` |
| `/tmp/[a-zA-Z0-9_.-]{8,}` | `<TMPDIR>/<id>` |
| email addresses | `<EMAIL>` |
| IP addresses (v4 + v6) | `<IP>` |
| hex tokens (≥16 chars, base16/64) | `<TOKEN>` |
| GitHub URLs with personal user | `<GITHUB_URL>` (preserve repo name component if it looks generic) |
| AWS access key IDs (`AKIA…`) | `<AWS_KEY>` |
| Bearer tokens in env-style assignments | `<SECRET>` |

**Tests (`scrub.rs` integration tests):** explicit "red list" of strings that MUST be scrubbed. CI runs scrubber on a fixture file containing each rule's positive cases, asserts none reach the output unchanged.

**Out-of-scope:** semantic scrubbing (e.g., detecting that `ssh prod-server-1` references a private hostname) — out of v1 scope, accepted risk.

## Component: Distillation (`distill`)

**Purpose:** generate soft training targets from Qwen 0.5B for each (context, next_command) pair. The tiny student model learns to mimic Qwen's distribution rather than just predicting the single ground-truth token.

**Why:** with ~30k synthetic + ~1k real commands, a tiny 500k-param model trained on hard one-hot labels will overfit and miss soft similarity signals (e.g., `cargo test` and `cargo check` are interchangeable in many contexts). Soft targets from Qwen capture this. Standard knowledge-distillation technique (Hinton et al., 2015).

**Algorithm:**
1. Read scrubbed + synthetic JSONL.
2. For each `CompletionEvent`, build `Context = (cwd_bucket, last_8_commands, prefix_or_empty, os)`.
3. Encode context as a natural-language prompt fed to Qwen: "Given user's working directory ~/dev/<bucket-id>, recent commands [git status, git add ., …], current input prefix '': predict the most likely next command. Output as a token-probability table, top-50."
4. Parse Qwen's output into `(token, probability)` pairs.
5. Project Qwen's vocab → shac vocab (2k tokens):
   - Tokenize each Qwen token into shac word-level tokens
   - Sum probabilities of all Qwen tokens that map to the same shac token (first-token approximation)
   - Renormalize over shac vocab
6. Save record: `{context_features, hard_label, soft_distribution}` → `ml/data/distill-{os}.jsonl`

**Cost:** ~50ms per Qwen forward × 50k contexts = ~40 minutes per OS. ~80 minutes for both. One-time before training.

**Vocab construction (offline, one-time):**
- Extract top-2000 most frequent tokens from scrubbed+synthetic combined data
- Augment with a fixed list of shell special tokens prepended to vocab (so their token-ids are stable across rebuilds):
  - Structural: `<PIPE>` (|), `<REDIRECT>` (>, >>), `<AND>` (&&), `<OR>` (||), `<BG>` (&), `<SUBSHELL>` (`$()`), `<HEREDOC>` (<<)
  - Path placeholders: `<HOME>`, `<TMPDIR>`, `<DOT>` (.), `<DOTDOT>` (..)
  - Sentinels: `<UNK>`, `<EOS>`, `<PAD>`, `<BOS>`
  - Common flags as a single token-id each: `--help`, `--version`, `-h`, `-v`, `-r`, `-rf`, `-la`, `-i`, `-f`, `-y`, `-n`, `-m`, `-c`, `-p`, `-d`, `-e`, `--dry-run`, `--force`, `--no-cache`
  - The full list of fixed special tokens is enumerated in `crates/shac-ml-train/src/tokenizer.rs::SPECIAL_TOKENS` and version-pinned alongside the model
- Save as `ml/models/vocab.json`, shipped alongside ONNX

## Component: Tiny student model (`train`)

**Goal:** train a small sequence model (~500k params) on distilled data, export ONNX.

**Architecture decision:** v1 uses a small decoder-only Transformer ("mini-GPT"), not LSTM:
- 4 layers, 4 heads, hidden dim 64, intermediate dim 128
- Vocab embedding 2000 × 64 = 128k params
- Each transformer block ~80k params × 4 = 320k
- Output projection 64 × 2000 = 128k
- Total: ~580k params, ~2.3MB FP16 ONNX
- Context length 16 tokens (recent commands tokenized + cwd_bucket + os flag)

Rationale over LSTM:
- ONNX export from `burn` for transformers is more stable than for stateful RNNs
- tract has optimized matmul kernels — transformer is mostly matmul → fast inference
- Modern architecture, future-proof

**Inputs (per training example):**
- `tokens: [u32; 16]` — flattened (cwd_bucket, os_flag, last_8_commands_tokens, prefix_token)
- `attention_mask: [bool; 16]`
- `hard_label: u32` — ground-truth next-token id
- `soft_targets: [f32; 2000]` — Qwen's distribution

**Loss function:**
```
L = α · CE(student_logits, hard_label) + (1-α) · KL(student_logits || soft_targets) · T²
α = 0.5
T = 4.0    (temperature, sharpens/smooths soft distributions per Hinton)
```

**Training (`burn` API):**
- Optimizer: AdamW, lr=3e-4, weight_decay=1e-2
- Batch size: 64
- Epochs: 10
- LR schedule: cosine annealing
- Validation: held-out 10% of distilled data, early stop on val_loss plateau
- Logging: print loss every 100 steps; emit final metrics (train/val CE, KL, top-1/3/5 accuracy) to stdout

**Compute budget:** ~50k examples × 10 epochs / 64 batch ≈ 8000 steps. ~10-15 min on M1 CPU per OS.

**ONNX export:** `burn-import-onnx` produces `shac-ml-{os}.onnx`. Validate roundtrip: load via `tract`, run on 10 test inputs, assert outputs match burn's outputs to ε=1e-4.

**Output artifacts (committed):**
- `ml/models/shac-ml-darwin.onnx`
- `ml/models/shac-ml-linux.onnx`
- `ml/models/vocab.json`
- `ml/models/feature-spec.json` (input schema versioning)

## Component: Production inference (`src/ml/inference.rs`)

**Loaded once at daemon start:**
```rust
pub struct MlInference {
    model: tract_onnx::prelude::SimplePlan<...>,  // tract compiled graph
    vocab: Vocab,                                   // token <-> id
    feature_spec: FeatureSpec,                      // OS-aware feature layout
}

impl MlInference {
    pub fn load() -> Result<Self> {
        let bytes = if cfg!(target_os = "macos") {
            include_bytes!("../../ml/models/shac-ml-darwin.onnx")
        } else if cfg!(target_os = "linux") {
            include_bytes!("../../ml/models/shac-ml-linux.onnx")
        } else {
            return Err(...);  // unsupported OS
        };
        // tract compile pipeline
        ...
    }
}
```

**Per-request:**
```rust
pub fn distribution(&self, ctx: &Context) -> Result<Vec<f32>> {
    let features = build_features(ctx, &self.vocab);  // [u32; 16]
    let input = tract_ndarray::Array1::from_iter(features).into_shape(...);
    let output = self.model.run(tvec!(input.into()))?;
    let logits: &[f32] = output[0].to_array_view::<f32>()?.as_slice().unwrap();
    let probs = softmax(logits);
    Ok(probs.to_vec())  // length 2000
}
```

**Latency target:** p99 ≤ 5ms on M1 CPU. tract's compiler optimizes for static input shapes — we pin shapes to avoid recompilation.

**Caching:** model loaded once at daemon start, reused for every request. No per-request compilation.

**Memory:** model takes ~5MB resident (FP16 weights + activations). Negligible.

## Component: ML score blending in engine (`src/engine.rs`)

**Add a 13th feature:** `ml_seq_score`, computed when `features.ml_seq_rerank=true`.

**Computation per candidate:**
```rust
fn score_candidate(...) {
    // existing 12 features computed
    let ml_seq = if self.config.features.ml_seq_rerank {
        let dist = self.ml.distribution(&ctx)?;
        let token_id = self.ml.vocab.lookup_first_token(&candidate.insert_text);
        let base_log_prob = dist[token_id].log();
        let residual = self.ml_residual.lookup(ctx_features, token_id);
        base_log_prob + residual  // additive in log space
    } else {
        0.0
    };
    let score = ... + self.config.ranking.ml_seq_score * ml_seq;
}
```

**`RankingWeights` gains a new field:**
```rust
pub struct RankingWeights {
    // ... existing 12 fields ...
    pub ml_seq_score: f64,  // default 0.0 → effectively off until user opts in
}
```

When `features.ml_seq_rerank=true`, default `ranking.ml_seq_score=0.20` (gives the ML signal moderate weight; user can tune).

## Component: Residual personalization (`src/ml/residual.rs`)

**On-disk schema (new SQLite table):**
```sql
CREATE TABLE IF NOT EXISTS ml_residual (
    cwd_bucket   INTEGER NOT NULL,
    prev_cmd_id  INTEGER NOT NULL,
    token_id     INTEGER NOT NULL,
    weight       REAL NOT NULL,
    updated_at   INTEGER NOT NULL,
    PRIMARY KEY (cwd_bucket, prev_cmd_id, token_id)
);
CREATE INDEX idx_ml_residual_lookup ON ml_residual (cwd_bucket, prev_cmd_id);
```

**In-memory cache:**
- `Mutex<HashMap<(u8, u16, u16), f32>>` for O(1) lookup
- Loaded from SQLite at daemon start
- Flushed back on `record_command` and on graceful shutdown

**Update on `accepted_completion`:**
```rust
fn update_residual(&self, ctx: &Context, accepted_token_id: u16) {
    let dist = self.ml.distribution(&ctx);
    let key_features = (ctx.cwd_bucket, ctx.prev_cmd_id);
    let lr = 0.05;
    for top_k_token in dist.iter().enumerate().top_k(20) {
        let p_user = dist[top_k_token];
        let target = if top_k_token == accepted_token_id { 1.0 } else { 0.0 };
        let grad = lr * (p_user - target);
        let key = (key_features.0, key_features.1, top_k_token);
        let entry = self.cache.entry(key).or_insert(0.0);
        *entry -= grad;
    }
}
```

**Cost:** ~5μs per update on hot path. Acceptable.

**CLI:** `shac ml reset-personalization` truncates the table.

**Inspection:** `shac ml inspect-residual --top 20` prints the top-N strongest residuals with their (cwd_bucket, prev_cmd_id, token) decoded — useful for debugging "why is shac always suggesting X?".

## Configuration

**New config keys (`~/.config/shac/config.toml`):**
| key | default | meaning |
|---|---|---|
| `features.ml_seq_rerank` | `false` | enable ML-based reranking (opt-in for v0.6.0) |
| `ranking.ml_seq_score` | `0.20` | weight of ML signal in scoring (0..1, only effective when feature is on) |
| `ml.residual_lr` | `0.05` | learning rate for residual updates (advanced) |
| `ml.residual_top_k` | `20` | how many tokens get residual updates per accepted completion |
| `ml.disable_personalization` | `false` | skip residual lookups + updates entirely (use bundled model only) |

**Env vars:**
- `SHAC_ML_DISABLE=1` — force disable ML at runtime regardless of config (for debugging)

## CLI surface

```
shac ml load-status                        # print model load status, OS detection, residual size
shac ml inspect-residual [--top N]         # show strongest personalization signals
shac ml reset-personalization              # clear ml_residual table
```

(No `shac ml train` or `shac ml fine-tune` commands in v0.6.0 — training is maintainer-only.)

## Testing strategy

### Unit tests in `crates/shac-ml-train/`
- `scrub.rs`: red-list of inputs (with PII) must be scrubbed in output
- `tokenizer.rs`: roundtrip tokenize/detokenize equivalence on golden corpus
- `model.rs`: forward pass on toy input gives non-NaN output of correct shape

### Unit tests in `src/ml/`
- `feature_extractor.rs`: known (cwd, prev_cmds) → known feature tensor
- `inference.rs`: model loads, runs forward on dummy input, returns 2000-len distribution summing to ~1.0
- `residual.rs`: update path is mathematically correct (gradient signs match expectation)

### Integration tests
- `tests/ml_inference.rs`: spawn daemon with `features.ml_seq_rerank=true`, run `shac complete` in a fixture cwd, assert ML latency under 10ms (CI tolerance) and assert score is influenced by ML
- `tests/ml_residual.rs`: simulate 50 accepted_completions, assert residual table grows, assert `shac ml reset-personalization` clears it
- `tests/ml_blend.rs`: with `ranking.ml_seq_score=0`, output should match ML-disabled run; with weight=1, output should be ML-dominant

### Maintainer pipeline tests
- `crates/shac-ml-train/tests/pipeline_smoke.rs`: end-to-end run of `gen-synthetic` (with mock Qwen) → `scrub` → `train` → produced ONNX loads via tract roundtrip without error

### Manual evaluation (before release)
- `cargo run -p shac-ml-train --bin eval --input ml/data/heldout-real.jsonl` prints top-1/3/5 accuracy on real held-out shell history
- Acceptance gate for v0.6.0 release: top-3 accuracy ≥ baseline + 5pp on heldout

## Rollout

- Version: v0.6.0 (minor bump — feature addition, opt-in by default)
- Default config: `features.ml_seq_rerank=false`. User must explicitly enable.
- CHANGELOG entry: "Experimental: ML-based command prediction (opt-in via `shac config set features.ml_seq_rerank true`). Bundled tiny models (~2MB each for darwin/linux). Online personalization on-device. See docs/ml.md for details."
- New doc: `docs/ml.md` explaining what the ML does, how to enable, privacy story, how to reset personalization
- Feature stays opt-in for at least one minor version cycle. Promotion to default depends on real-world feedback.

## Risks and mitigations

| risk | mitigation |
|---|---|
| Tiny model degrades quality vs current scorer | Opt-in default (`false`); `eval --baseline` gate in CI before release; honest CHANGELOG note |
| `burn` API breakage between versions | Pin exact version `burn = "=0.13.x"`; avoid bleeding-edge features; document upgrade procedure |
| ONNX export incompatibility with `tract` | Roundtrip test in maintainer pipeline (export, load, compare outputs) is mandatory before commit |
| PII leak in bundled vocab | Mandatory `scrub` step + red-list test; manual vocab inspection before each release |
| Personalization grows unbounded on disk | Cap residual table at 100k rows; LRU-evict by `updated_at` if exceeded |
| Residual makes top suggestions unstable | `lr=0.05` is conservative; `disable_personalization=true` escape hatch; weight in `ranking.ml_seq_score` is tunable |
| `mistralrs` cannot run Qwen on Linux CI runners | Maintainer pipeline runs on macOS only initially; Linux model is generated on macOS too (cross-OS data, OS-specific personas) |
| Distillation step takes too long for fast iteration | Cache distillation outputs in JSONL; `train` reads cached file, doesn't re-query Qwen |

## Open items (deferred to v0.6.1+ or future specs)

- Full LoRA fine-tuning on user device (when `burn` matures or Python is acceptable)
- User-triggered `shac ml retrain` command for power users
- Asynchronous ghost-text via Qwen-class LLM (separate "background ML mode")
- Cross-platform deterministic builds (currently maintainer pipeline assumes M1)
- Window function: extend context beyond 8 commands when user has long shell session
- Per-project model fine-tuning (one residual per project_root vs current cwd_bucket)
- **Opt-in anonymized telemetry → next-gen base model.** Explicit one-command opt-in (`shac ml contribute --enable`), collects scrubbed sequences (same scrubbing rules as §"Path scrubbing", run client-side before upload), uploads to a maintainer-controlled endpoint, and feeds the next round of synthetic+real training data for the shipped model. Requires: (1) endpoint + storage infra, (2) double-scrub audit (client + server), (3) clear privacy policy doc, (4) per-OS bucketing preserved, (5) k-anonymity threshold before any sequence enters training set, (6) easy `--disable` and "delete my contributions" flow. Out of scope for v0.6.0 — design separately once we have organic install base to make the data meaningful.

## Decision log

| Decision | Rationale |
|---|---|
| Use `burn` for training, not Python+PyTorch | Pure-Rust pipeline requirement (Roman's hard constraint) |
| Use `mistralrs` for Qwen inference | Pure-Rust, supports Qwen, mature enough as of 2026 Q2 |
| Tiny model architecture: mini-Transformer not LSTM | tract optimization, ONNX export stability, future-proof |
| Two ONNX models (darwin/linux) instead of one with OS feature | Cleaner mental model, easier to reason about quality per OS, tiny model handles routing poorly |
| Distillation via Qwen teacher | +3-5pp accuracy without increasing student size; standard technique |
| Residual personalization, not full fine-tuning | Pure-Rust, online, deterministic, no catastrophic forgetting |
| Synthetic data via local Qwen, not Haiku API | No external API dependency, hermetic pipeline, no per-build cost |
| Opt-in (default false) for v0.6.0 | Minimize regression risk on existing users; gather opt-in dogfood feedback before promotion |
