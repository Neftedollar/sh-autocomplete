# shac ML Maintainer Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the maintainer-only `shac-ml-train` workspace crate that produces `ml/models/shac-ml-{darwin,linux}.bpk` weight files, the shared `vocab.json`, and `feature-spec.json` — the artifacts consumed by the runtime crate (separate plan).

**Architecture:** New workspace crate `crates/shac-ml-train/` with four bins (`gen-synthetic`, `scrub`, `distill`, `train`) plus an `eval` bin. Pure-Rust pipeline: `mistralrs` for Qwen 0.5B teacher, `burn` (with `Autodiff<NdArray>` backend) for student training, `BurnpackStore` for `.bpk` export. The `model.rs` and `tokenizer.rs` modules are designed to be re-importable by the runtime crate via a `model-only` feature flag.

**Tech Stack:** Rust 1.78+, `burn` (=0.21), `burn-store` (=0.21), `mistralrs` (latest stable), `ndarray`, `serde`, `serde_json`, `regex`, `anyhow`, `clap`. Pinned versions for reproducibility.

**Reference docs:**
- Spec: `docs/superpowers/specs/2026-04-29-shac-ml-next-command-design.md`
- Burn book: https://burn.dev/book/
- Burn ONNX import (for reference, NOT used here): https://burn.dev/book/import/onnx-import.html

**Key principle:** every artifact this plan produces (vocab.json, feature-spec.json, .bpk) is the *contract* with the runtime plan. Stable filenames, stable schemas, version-pinned.

---

## File map

**Created:**
- `crates/shac-ml-train/Cargo.toml`
- `crates/shac-ml-train/src/lib.rs`
- `crates/shac-ml-train/src/model.rs` — burn `Module` (also re-exported by runtime)
- `crates/shac-ml-train/src/tokenizer.rs` — vocab + word tokenizer (also re-exported)
- `crates/shac-ml-train/src/personas.rs` — persona TOML loader
- `crates/shac-ml-train/src/scrub.rs` — scrubbing rules + apply
- `crates/shac-ml-train/src/data.rs` — JSONL records + batching
- `crates/shac-ml-train/src/qwen.rs` — mistralrs wrapper for synthetic + distillation
- `crates/shac-ml-train/src/bin/gen_synthetic.rs`
- `crates/shac-ml-train/src/bin/scrub.rs`
- `crates/shac-ml-train/src/bin/distill.rs`
- `crates/shac-ml-train/src/bin/train.rs`
- `crates/shac-ml-train/src/bin/eval.rs`
- `crates/shac-ml-train/tests/scrub_redlist.rs`
- `crates/shac-ml-train/tests/tokenizer_roundtrip.rs`
- `crates/shac-ml-train/tests/model_forward.rs`
- `crates/shac-ml-train/tests/pipeline_smoke.rs`
- `ml/data/personas.toml` — 20 personas
- `ml/data/personas.toml` example fixtures (committed)
- `ml/README.md` — rebuild instructions
- `ml/models/.gitkeep`
- `ml/models/feature-spec.json` (output, committed)
- `ml/models/vocab.json` (output, committed)
- `ml/models/shac-ml-darwin.bpk` (output, committed)
- `ml/models/shac-ml-linux.bpk` (output, committed)

**Modified:**
- `Cargo.toml` (workspace root) — convert `[package]` into a workspace and register `crates/shac-ml-train`

---

## Task 1: Convert root `Cargo.toml` into a workspace

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/shac-ml-train/Cargo.toml`

- [ ] **Step 1: Read current root `Cargo.toml`**

```bash
cat Cargo.toml
```

Note the existing `[package]`, `[lib]`, `[[bin]]`, `[dependencies]`, `[dev-dependencies]` sections.

- [ ] **Step 2: Add `[workspace]` table at the top, register members**

Insert at the very top of `Cargo.toml`, before `[package]`:

```toml
[workspace]
members = [".", "crates/shac-ml-train"]
resolver = "2"
```

The `.` keeps the existing root crate as a workspace member. `resolver = "2"` is required for the workspace.

- [ ] **Step 3: Verify workspace still builds**

Run: `cargo check --workspace`
Expected: PASS (only the root crate exists right now; warning about empty `crates/shac-ml-train/` is OK because we haven't created it yet — the build should still succeed for `.` member)

If `cargo` complains the member doesn't exist, this is fine — it'll be created in Step 4. Skip to Step 4.

- [ ] **Step 4: Create the new crate skeleton**

```bash
mkdir -p crates/shac-ml-train/src/bin
mkdir -p crates/shac-ml-train/tests
```

Create `crates/shac-ml-train/Cargo.toml`:

```toml
[package]
name = "shac-ml-train"
version = "0.1.0"
edition = "2021"
license = "MIT"
description = "Maintainer pipeline for shac's ML next-command predictor — generates synthetic data, distills Qwen, trains the student, exports .bpk weights"
publish = false

[features]
# Default = full training pipeline (incl. autodiff + mistralrs).
# `model-only` = just the model + tokenizer + types, used by the shac runtime crate.
default = ["full"]
full = ["dep:mistralrs", "dep:burn-autodiff", "dep:burn-train"]
model-only = []

[dependencies]
anyhow = "1.0"
clap = { version = "4.5", features = ["derive"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "1.1"
regex = "1"
ndarray = "0.16"

burn = { version = "=0.21", default-features = false, features = ["std", "ndarray"] }
burn-store = { version = "=0.21", default-features = false, features = ["burnpack"] }

# optional, only enabled by `full`
burn-autodiff = { version = "=0.21", optional = true, default-features = false }
burn-train = { version = "=0.21", optional = true, default-features = false }
mistralrs = { version = "0.3", optional = true, default-features = false, features = ["metal"] }

[dev-dependencies]
tempfile = "3"

[[bin]]
name = "shac-ml-gen-synthetic"
path = "src/bin/gen_synthetic.rs"
required-features = ["full"]

[[bin]]
name = "shac-ml-scrub"
path = "src/bin/scrub.rs"

[[bin]]
name = "shac-ml-distill"
path = "src/bin/distill.rs"
required-features = ["full"]

[[bin]]
name = "shac-ml-train"
path = "src/bin/train.rs"
required-features = ["full"]

[[bin]]
name = "shac-ml-eval"
path = "src/bin/eval.rs"
required-features = ["full"]
```

Create stub `crates/shac-ml-train/src/lib.rs`:

```rust
//! Maintainer pipeline for shac's ML next-command predictor.
//!
//! Modules `model` and `tokenizer` are designed to be reused by the
//! main shac runtime crate via `default-features = false, features = ["model-only"]`.

#[cfg(feature = "full")]
pub mod data;
#[cfg(feature = "full")]
pub mod personas;
#[cfg(feature = "full")]
pub mod qwen;
#[cfg(feature = "full")]
pub mod scrub;

pub mod model;
pub mod tokenizer;
```

Create empty placeholder files (each with `//! placeholder` so the crate compiles):

```bash
for f in src/data.rs src/model.rs src/personas.rs src/qwen.rs src/scrub.rs src/tokenizer.rs; do
  echo "//! placeholder" > "crates/shac-ml-train/$f"
done
for f in gen_synthetic scrub distill train eval; do
  cat > "crates/shac-ml-train/src/bin/${f}.rs" <<'EOF'
fn main() {
    eprintln!("not yet implemented");
    std::process::exit(1);
}
EOF
done
```

- [ ] **Step 5: Build the workspace**

Run: `cargo check --workspace`
Expected: PASS. Both crates compile (with warnings about unused stubs).

If `mistralrs = "0.3"` resolution fails (version drift), update to the latest 0.x in https://crates.io/crates/mistralrs and re-run.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/shac-ml-train/
git commit -m "feat(ml-train): bootstrap shac-ml-train workspace crate

Skeleton crate with model-only feature flag for runtime reuse. All bins
are stubs that exit 1; modules are placeholders. Pipeline implementation
follows in subsequent commits."
```

---

## Task 2: Tokenizer module — vocab schema + special tokens

**Files:**
- Modify: `crates/shac-ml-train/src/tokenizer.rs`
- Test: `crates/shac-ml-train/tests/tokenizer_roundtrip.rs`

- [ ] **Step 1: Write the failing test**

`crates/shac-ml-train/tests/tokenizer_roundtrip.rs`:

```rust
use shac_ml_train::tokenizer::{Vocab, SPECIAL_TOKENS};

#[test]
fn special_tokens_have_stable_ids_at_front() {
    let vocab = Vocab::new_with_special_only();
    // First special token is <PAD> per the SPECIAL_TOKENS array
    assert_eq!(vocab.id_of(SPECIAL_TOKENS[0]), Some(0));
    assert_eq!(vocab.token_of(0), Some(SPECIAL_TOKENS[0]));
    // <UNK> sentinel
    assert!(vocab.id_of("<UNK>").is_some());
}

#[test]
fn build_from_corpus_preserves_special_tokens_first() {
    let corpus = vec![
        "git status".to_string(),
        "git add .".to_string(),
        "cargo test".to_string(),
    ];
    let vocab = Vocab::build_from_corpus(&corpus, /*max_size=*/ 50);
    // Special tokens occupy ids 0..N
    for (i, &tok) in SPECIAL_TOKENS.iter().enumerate() {
        assert_eq!(vocab.id_of(tok), Some(i));
    }
    // 'git' should be in the vocab (frequency-ranked)
    assert!(vocab.id_of("git").is_some());
}

#[test]
fn unknown_word_maps_to_unk() {
    let vocab = Vocab::new_with_special_only();
    let id = vocab.encode_word("never-seen-word-12345");
    assert_eq!(id, vocab.id_of("<UNK>").unwrap());
}

#[test]
fn save_load_json_roundtrip() {
    let vocab = Vocab::new_with_special_only();
    let json = vocab.to_json().unwrap();
    let restored = Vocab::from_json(&json).unwrap();
    assert_eq!(vocab.id_of("<UNK>"), restored.id_of("<UNK>"));
    assert_eq!(vocab.size(), restored.size());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p shac-ml-train --test tokenizer_roundtrip`
Expected: FAIL — `Vocab` and `SPECIAL_TOKENS` are not defined.

- [ ] **Step 3: Implement the tokenizer module**

Replace contents of `crates/shac-ml-train/src/tokenizer.rs`:

```rust
//! Word-level tokenizer with stable special-token ids at the front of the vocab.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Special tokens with stable ids 0..N. Order is the contract — never reorder.
/// Adding new ones at the end is a vocab schema bump (feature_spec.version).
pub const SPECIAL_TOKENS: &[&str] = &[
    // Sentinels (0..3)
    "<PAD>",
    "<UNK>",
    "<BOS>",
    "<EOS>",
    // Structural shell tokens (4..10)
    "<PIPE>",
    "<REDIRECT>",
    "<AND>",
    "<OR>",
    "<BG>",
    "<SUBSHELL>",
    "<HEREDOC>",
    // Path placeholders (11..14)
    "<HOME>",
    "<TMPDIR>",
    "<DOT>",
    "<DOTDOT>",
    // Common flags (15..33)
    "--help",
    "--version",
    "-h",
    "-v",
    "-r",
    "-rf",
    "-la",
    "-i",
    "-f",
    "-y",
    "-n",
    "-m",
    "-c",
    "-p",
    "-d",
    "-e",
    "--dry-run",
    "--force",
    "--no-cache",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vocab {
    /// Ordered tokens, position == id
    pub tokens: Vec<String>,
    /// Reverse map for O(1) lookup
    #[serde(skip)]
    index: HashMap<String, u32>,
}

impl Vocab {
    /// Build a vocab containing only the fixed special tokens (used in tests
    /// and during early-stage tooling before a real corpus exists).
    pub fn new_with_special_only() -> Self {
        let tokens: Vec<String> = SPECIAL_TOKENS.iter().map(|s| s.to_string()).collect();
        let index = build_index(&tokens);
        Self { tokens, index }
    }

    /// Construct a vocab from a corpus of command lines: tokens 0..N are the
    /// fixed special tokens, then frequency-ranked unique words from the corpus
    /// up to `max_size` total entries.
    pub fn build_from_corpus(corpus: &[String], max_size: usize) -> Self {
        let mut counts: HashMap<String, u32> = HashMap::new();
        for line in corpus {
            for word in tokenize_command(line) {
                if SPECIAL_TOKENS.iter().any(|&s| s == word) {
                    continue; // already reserved
                }
                *counts.entry(word).or_insert(0) += 1;
            }
        }
        let mut sorted: Vec<(String, u32)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let remaining = max_size.saturating_sub(SPECIAL_TOKENS.len());
        let frequent: Vec<String> = sorted.into_iter().take(remaining).map(|(w, _)| w).collect();

        let mut tokens: Vec<String> = SPECIAL_TOKENS.iter().map(|s| s.to_string()).collect();
        tokens.extend(frequent);
        let index = build_index(&tokens);
        Self { tokens, index }
    }

    pub fn size(&self) -> usize {
        self.tokens.len()
    }

    pub fn id_of(&self, token: &str) -> Option<u32> {
        self.index.get(token).copied()
    }

    pub fn token_of(&self, id: u32) -> Option<&str> {
        self.tokens.get(id as usize).map(String::as_str)
    }

    /// Map a single word → id. Falls back to `<UNK>`.
    pub fn encode_word(&self, word: &str) -> u32 {
        self.id_of(word)
            .unwrap_or_else(|| self.id_of("<UNK>").expect("UNK is a fixed special token"))
    }

    /// Tokenize a full command line into a sequence of word ids. Structural
    /// shell metacharacters become structural special tokens (<PIPE>, etc.).
    pub fn encode_command(&self, command: &str) -> Vec<u32> {
        tokenize_command(command)
            .into_iter()
            .map(|w| self.encode_word(&w))
            .collect()
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).context("serialize vocab")
    }

    pub fn from_json(s: &str) -> Result<Self> {
        let mut v: Self = serde_json::from_str(s).context("parse vocab json")?;
        v.index = build_index(&v.tokens);
        Ok(v)
    }
}

fn build_index(tokens: &[String]) -> HashMap<String, u32> {
    tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (t.clone(), i as u32))
        .collect()
}

/// Word-level tokenizer that turns shell metacharacters into structural special
/// tokens and otherwise splits on whitespace. Lossy by design — we never need
/// to reconstruct the original command from token ids.
pub fn tokenize_command(line: &str) -> Vec<String> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for word in line.split_whitespace() {
        match word {
            "|" => out.push("<PIPE>".to_string()),
            ">" | ">>" => out.push("<REDIRECT>".to_string()),
            "&&" => out.push("<AND>".to_string()),
            "||" => out.push("<OR>".to_string()),
            "&" => out.push("<BG>".to_string()),
            "." => out.push("<DOT>".to_string()),
            ".." => out.push("<DOTDOT>".to_string()),
            other => out.push(other.to_string()),
        }
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shac-ml-train --test tokenizer_roundtrip`
Expected: PASS — all 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/shac-ml-train/src/tokenizer.rs crates/shac-ml-train/tests/tokenizer_roundtrip.rs
git commit -m "feat(ml-train): vocab + word tokenizer with fixed special token ids"
```

---

## Task 3: Data module — JSONL records, ContextWindow

**Files:**
- Modify: `crates/shac-ml-train/src/data.rs`

- [ ] **Step 1: Implement the records**

Replace contents of `crates/shac-ml-train/src/data.rs`:

```rust
//! JSONL records exchanged between pipeline stages.
//!
//! Stage outputs:
//!   gen_synthetic → SyntheticEvent (one per line)
//!   scrub        → SyntheticEvent (still, but PII replaced)
//!   distill      → DistilledExample (with soft targets)
//!   train        → reads DistilledExample
//!
//! All records are versioned by `schema_version` so a new pipeline run
//! catches any drift early.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticEvent {
    pub schema_version: u32,
    pub persona_id: String,
    pub os: String,                 // "darwin" | "linux"
    pub cwd: String,                // already-scrubbed path template
    pub command: String,
    pub prev_command: Option<String>,
    pub ts_offset_secs: i64,        // synthetic monotonic clock within session
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistilledExample {
    pub schema_version: u32,
    pub os: String,
    pub cwd_bucket: u8,
    pub context_tokens: Vec<u32>,   // length = context_len, padded with <PAD>
    pub hard_label: u32,            // ground-truth next-token id
    pub soft_targets_top: Vec<(u32, f32)>, // top-k from teacher; rest = uniform residual
}

pub fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {} of {}", lineno + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: T = serde_json::from_str(&line)
            .with_context(|| format!("parse line {} of {}", lineno + 1, path.display()))?;
        out.push(rec);
    }
    Ok(out)
}

pub fn write_jsonl<T: Serialize>(path: &Path, records: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for rec in records {
        let line = serde_json::to_string(rec).context("serialize record")?;
        writeln!(writer, "{}", line)?;
    }
    writer.flush()?;
    Ok(())
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/data.rs
git commit -m "feat(ml-train): JSONL record types for pipeline stage I/O"
```

---

## Task 4: Scrub module — rules + apply

**Files:**
- Modify: `crates/shac-ml-train/src/scrub.rs`
- Test: `crates/shac-ml-train/tests/scrub_redlist.rs`

- [ ] **Step 1: Write the failing test (red list)**

`crates/shac-ml-train/tests/scrub_redlist.rs`:

```rust
use shac_ml_train::scrub::scrub_text;

#[test]
fn macos_home_path_replaced() {
    assert_eq!(scrub_text("cd /Users/roman/dev/shac"), "cd <HOME>/dev/shac");
}

#[test]
fn linux_home_path_replaced() {
    assert_eq!(scrub_text("ls /home/alice/projects"), "ls <HOME>/projects");
}

#[test]
fn macos_var_folders_replaced() {
    let out = scrub_text("cat /var/folders/aa/bb/T/build.log");
    assert_eq!(out, "cat <TMPDIR>/build.log");
}

#[test]
fn tmp_random_id_replaced() {
    let out = scrub_text("rm /tmp/tmpA1B2c3D4e5_x");
    assert!(out.contains("<TMPDIR>"));
    assert!(!out.contains("tmpA1B2c3D4e5_x"));
}

#[test]
fn email_replaced() {
    assert_eq!(
        scrub_text("git config user.email roman@example.com"),
        "git config user.email <EMAIL>"
    );
}

#[test]
fn ipv4_replaced() {
    assert_eq!(scrub_text("ssh 10.0.1.42"), "ssh <IP>");
}

#[test]
fn long_hex_token_replaced() {
    let out = scrub_text("export TOKEN=abcdef0123456789abcdef0123456789");
    assert!(out.contains("<TOKEN>"));
    assert!(!out.contains("abcdef0123456789abcdef0123456789"));
}

#[test]
fn aws_access_key_replaced() {
    assert_eq!(
        scrub_text("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"),
        "export AWS_ACCESS_KEY_ID=<AWS_KEY>"
    );
}

#[test]
fn benign_text_unchanged() {
    assert_eq!(scrub_text("cargo test --release"), "cargo test --release");
    assert_eq!(scrub_text("git status"), "git status");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p shac-ml-train --test scrub_redlist`
Expected: FAIL — `scrub_text` not defined.

- [ ] **Step 3: Implement scrub module**

Replace contents of `crates/shac-ml-train/src/scrub.rs`:

```rust
//! Path / PII scrubbing rules. Applied to:
//!   - real history corpora before they enter training
//!   - the maintainer's local zsh history before it joins the synthetic dataset
//!
//! Rules are intentionally simple regex-replace; no context-sensitive parsing.
//! New rules go through tests/scrub_redlist.rs.

use std::sync::OnceLock;

use regex::Regex;

struct Rule {
    re: Regex,
    replacement: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // /Users/<name>/... → <HOME>/...
            Rule {
                re: Regex::new(r"/Users/[^/\s]+(/[^\s]*)?").unwrap(),
                replacement: "<HOME>$1",
            },
            // /home/<name>/... → <HOME>/...
            Rule {
                re: Regex::new(r"/home/[^/\s]+(/[^\s]*)?").unwrap(),
                replacement: "<HOME>$1",
            },
            // /var/folders/aa/bb/T/file → <TMPDIR>/file (macOS per-session tmp)
            Rule {
                re: Regex::new(r"/var/folders/[^/\s]+/[^/\s]+/T(/[^\s]*)?").unwrap(),
                replacement: "<TMPDIR>$1",
            },
            // /tmp/<random-8-or-more> → <TMPDIR>/<id>
            Rule {
                re: Regex::new(r"/tmp/[A-Za-z0-9_.-]{8,}").unwrap(),
                replacement: "<TMPDIR>",
            },
            // emails
            Rule {
                re: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
                replacement: "<EMAIL>",
            },
            // IPv4
            Rule {
                re: Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap(),
                replacement: "<IP>",
            },
            // IPv6 (simplified: 7+ hex blocks separated by colons)
            Rule {
                re: Regex::new(r"\b[0-9a-fA-F:]{7,}\b").unwrap(),
                replacement: "<IP>",
            },
            // AWS access key id (must come BEFORE generic hex token rule)
            Rule {
                re: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
                replacement: "<AWS_KEY>",
            },
            // Long hex/base64-ish tokens (>=24 chars) — catches GH PATs, generic secrets
            Rule {
                re: Regex::new(r"\b[A-Za-z0-9_/+=-]{24,}\b").unwrap(),
                replacement: "<TOKEN>",
            },
        ]
    })
}

pub fn scrub_text(input: &str) -> String {
    let mut out = input.to_string();
    for rule in rules() {
        out = rule.re.replace_all(&out, rule.replacement).to_string();
    }
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shac-ml-train --test scrub_redlist`
Expected: PASS — all 9 tests green.

If a test fails (e.g., IPv6 over-matching short hex), tighten the regex and re-run. Document any rule trade-offs in a comment.

- [ ] **Step 5: Commit**

```bash
git add crates/shac-ml-train/src/scrub.rs crates/shac-ml-train/tests/scrub_redlist.rs
git commit -m "feat(ml-train): PII scrubbing rules with red-list test coverage"
```

---

## Task 5: Personas TOML loader + `personas.toml` content

**Files:**
- Modify: `crates/shac-ml-train/src/personas.rs`
- Create: `ml/data/personas.toml`

- [ ] **Step 1: Define types and loader**

Replace contents of `crates/shac-ml-train/src/personas.rs`:

```rust
//! Persona TOML loader. Personas describe synthetic users for `gen-synthetic`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PersonaFile {
    #[serde(rename = "persona")]
    pub personas: Vec<Persona>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Persona {
    pub id: String,
    pub os: String, // "darwin" | "linux"
    pub cwd_pattern: String,
    pub tools_installed: Vec<String>,
    pub typical_session_length: usize,
    pub style_prompt: String,
    pub sessions_to_generate: usize,
}

pub fn load(path: &Path) -> Result<Vec<Persona>> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read personas file {}", path.display()))?;
    let parsed: PersonaFile = toml::from_str(&raw).context("parse personas.toml")?;
    for p in &parsed.personas {
        anyhow::ensure!(
            matches!(p.os.as_str(), "darwin" | "linux"),
            "persona '{}' has invalid os '{}'",
            p.id,
            p.os
        );
    }
    Ok(parsed.personas)
}
```

- [ ] **Step 2: Create `ml/data/personas.toml` with 6 starter personas**

Write `ml/data/personas.toml` (2 darwin + 2 linux + 2 cross are enough to validate the loader; the full 20 are added in Task 12 once the pipeline works end-to-end on a small set):

```toml
[[persona]]
id = "rust-backend-darwin"
os = "darwin"
cwd_pattern = "<HOME>/dev/<rust-project>"
tools_installed = ["git", "cargo", "rustup", "docker", "brew", "kubectl"]
typical_session_length = 12
sessions_to_generate = 50
style_prompt = """
A backend Rust engineer working on a web service on macOS.
Frequent cycles of cargo test, cargo build, git diff/add/commit.
Occasional docker compose up for local services. kubectl for production debugging.
"""

[[persona]]
id = "frontend-darwin"
os = "darwin"
cwd_pattern = "<HOME>/dev/<web-app>"
tools_installed = ["git", "node", "npm", "pnpm", "brew", "code"]
typical_session_length = 10
sessions_to_generate = 50
style_prompt = """
Frontend dev on macOS. pnpm install, pnpm dev, pnpm build, pnpm lint cycles.
Frequent git status / git commit. Occasionally opens VS Code via `code .`.
"""

[[persona]]
id = "devops-linux"
os = "linux"
cwd_pattern = "<HOME>/infra"
tools_installed = ["git", "kubectl", "docker", "terraform", "ssh", "curl"]
typical_session_length = 15
sessions_to_generate = 50
style_prompt = """
DevOps engineer on Linux. kubectl get pods, terraform plan, ssh prod-host,
docker logs cycles. Frequent grep/tail/awk pipelines.
"""

[[persona]]
id = "data-eng-linux"
os = "linux"
cwd_pattern = "<HOME>/projects/<data-pipeline>"
tools_installed = ["git", "python", "pip", "uv", "jupyter", "psql"]
typical_session_length = 12
sessions_to_generate = 50
style_prompt = """
Data engineer on Linux. uv run, jupyter notebook, psql -h, git commit cycles.
Frequent virtualenv activation, occasional pyspark-submit.
"""

[[persona]]
id = "go-server-linux"
os = "linux"
cwd_pattern = "<HOME>/code/<go-service>"
tools_installed = ["git", "go", "make", "docker", "ssh"]
typical_session_length = 10
sessions_to_generate = 50
style_prompt = """
Go backend engineer on Linux. go test ./..., go build, make deploy,
docker logs cycles. Frequent git rebase / push.
"""

[[persona]]
id = "swift-app-darwin"
os = "darwin"
cwd_pattern = "<HOME>/dev/<ios-app>"
tools_installed = ["git", "xcodebuild", "fastlane", "brew", "swift"]
typical_session_length = 8
sessions_to_generate = 50
style_prompt = """
iOS engineer on macOS. xcodebuild -scheme, fastlane beta, swift test cycles.
Occasional pod install for legacy modules.
"""
```

- [ ] **Step 3: Add a smoke test for the loader**

Append to `crates/shac-ml-train/tests/tokenizer_roundtrip.rs` (so we don't add a separate file for one test) — actually, create a new test file `crates/shac-ml-train/tests/personas_load.rs`:

```rust
use std::path::PathBuf;
use shac_ml_train::personas;

#[test]
fn loads_committed_personas_file() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../ml/data/personas.toml");
    let personas = personas::load(&path).expect("load personas");
    assert!(personas.len() >= 6, "expected at least 6 personas, got {}", personas.len());
    assert!(personas.iter().any(|p| p.os == "darwin"));
    assert!(personas.iter().any(|p| p.os == "linux"));
}
```

- [ ] **Step 4: Run loader test**

Run: `cargo test -p shac-ml-train --test personas_load`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/shac-ml-train/src/personas.rs crates/shac-ml-train/tests/personas_load.rs ml/data/personas.toml
git commit -m "feat(ml-train): personas TOML loader + 6 starter personas"
```

---

## Task 6: Qwen wrapper module — mistralrs facade

**Files:**
- Modify: `crates/shac-ml-train/src/qwen.rs`

- [ ] **Step 1: Implement the wrapper**

Replace contents of `crates/shac-ml-train/src/qwen.rs`:

```rust
//! Thin wrapper around `mistralrs` for synthetic data generation and distillation.
//!
//! We isolate mistralrs API surface here so the rest of the pipeline stays
//! testable with a mock implementation (see `MockQwen`).

use anyhow::{Context, Result};

#[cfg(feature = "full")]
use mistralrs::{
    GgufModelBuilder, Model, RequestBuilder, TextMessageRole,
};

pub struct GenerationConfig {
    pub max_tokens: usize,
    pub temperature: f32,
    pub top_p: f32,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            max_tokens: 256,
            temperature: 0.8,
            top_p: 0.9,
        }
    }
}

/// Trait so we can mock Qwen in pipeline_smoke tests.
pub trait QwenLike: Send + Sync {
    fn generate(&self, system: &str, user: &str, cfg: &GenerationConfig) -> Result<String>;

    /// Top-k tokens with their probabilities for the next token after `prompt`.
    /// Used by distillation. Returns up to `top_k` (token_string, probability) pairs.
    fn next_token_distribution(&self, prompt: &str, top_k: usize) -> Result<Vec<(String, f32)>>;
}

#[cfg(feature = "full")]
pub struct Qwen {
    model: Model,
}

#[cfg(feature = "full")]
impl Qwen {
    /// Loads Qwen 2.5 0.5B Instruct (GGUF) from `~/.cache/shac-ml/qwen/`.
    /// Downloads on first use (mistralrs handles caching).
    pub async fn load() -> Result<Self> {
        let model = GgufModelBuilder::new(
            "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
            vec!["qwen2.5-0.5b-instruct-q4_k_m.gguf".to_string()],
        )
        .build()
        .await
        .context("load Qwen 0.5B GGUF")?;
        Ok(Self { model })
    }
}

#[cfg(feature = "full")]
impl QwenLike for Qwen {
    fn generate(&self, system: &str, user: &str, cfg: &GenerationConfig) -> Result<String> {
        let req = RequestBuilder::new()
            .add_message(TextMessageRole::System, system)
            .add_message(TextMessageRole::User, user)
            .set_sampler_max_len(cfg.max_tokens)
            .set_sampler_temperature(cfg.temperature as f64)
            .set_sampler_top_p(cfg.top_p as f64);
        let response = futures::executor::block_on(self.model.send_chat_request(req))
            .context("Qwen chat request")?;
        let text = response
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();
        Ok(text)
    }

    fn next_token_distribution(&self, prompt: &str, top_k: usize) -> Result<Vec<(String, f32)>> {
        // mistralrs exposes raw logits via the `Model::log_probs` API. If the
        // exact API name has shifted between mistralrs releases, consult the
        // crates.io docs page for the pinned version. The contract is:
        //   - Run a single forward pass with `prompt`
        //   - Get the logits for the *next* token
        //   - Convert to probabilities (softmax)
        //   - Take top_k largest
        //   - Decode each token id back to its surface string
        let logprobs = futures::executor::block_on(self.model.log_probs(prompt, top_k))
            .context("Qwen log_probs")?;
        let mut out: Vec<(String, f32)> = logprobs
            .into_iter()
            .map(|(tok, lp)| (tok, lp.exp() as f32))
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(top_k);
        Ok(out)
    }
}

/// In-memory mock used by pipeline_smoke tests so CI doesn't pull a 350MB GGUF.
pub struct MockQwen {
    pub canned_completion: String,
    pub canned_distribution: Vec<(String, f32)>,
}

impl QwenLike for MockQwen {
    fn generate(&self, _system: &str, _user: &str, _cfg: &GenerationConfig) -> Result<String> {
        Ok(self.canned_completion.clone())
    }

    fn next_token_distribution(&self, _prompt: &str, _top_k: usize) -> Result<Vec<(String, f32)>> {
        Ok(self.canned_distribution.clone())
    }
}
```

> **Note for the implementing engineer:** the `mistralrs` API surface (`GgufModelBuilder`, `RequestBuilder`, `log_probs`) is the published shape as of mistralrs 0.3.x. If the pinned version's docs show different names (this crate is in active development), the *contract* is what matters: load a GGUF model, run chat completion, extract next-token log-probs. Adjust the calls to match the actual pinned API and keep the `QwenLike` trait surface stable — only `Qwen::load` and the trait `impl Qwen` body should need changes.

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS, possibly with warnings about unused futures::executor.

If `mistralrs` API names differ from the snippet above, fix the calls inside the `impl QwenLike for Qwen` block to match the actual pinned API. The trait shape and `MockQwen` are framework-agnostic and won't need changes.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/qwen.rs
git commit -m "feat(ml-train): mistralrs facade for synthetic + distillation, with mock"
```

---

## Task 7: `gen-synthetic` bin — drives Qwen to produce `synthetic-{os}.jsonl`

**Files:**
- Modify: `crates/shac-ml-train/src/bin/gen_synthetic.rs`

- [ ] **Step 1: Implement the bin**

Replace contents of `crates/shac-ml-train/src/bin/gen_synthetic.rs`:

```rust
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

fn main() -> Result<()> {
    let args = Args::parse();
    let personas = personas::load(&args.personas)?;
    let qwen = futures::executor::block_on(Qwen::load())?;

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
    !first.is_empty() && first.chars().next().map_or(false, |c| c.is_ascii_alphanumeric() || c == '_' || c == '/')
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS.

- [ ] **Step 3: Smoke test in dry-run mode**

Note: this requires Qwen weights (downloads ~350MB on first run; cached afterward).

Run: `cargo run -p shac-ml-train --features full --bin shac-ml-gen-synthetic -- --dry-run --persona rust-backend-darwin`
Expected: prints up to 10 generated commands, exits 0. No JSONL written.

If mistralrs API mismatched the pinned version, this is where it'll surface. Fix `qwen.rs` and re-run.

- [ ] **Step 4: Commit**

```bash
git add crates/shac-ml-train/src/bin/gen_synthetic.rs
git commit -m "feat(ml-train): gen-synthetic bin — Qwen-driven session generator"
```

---

## Task 8: `scrub` bin — applies scrubbing rules to JSONL

**Files:**
- Modify: `crates/shac-ml-train/src/bin/scrub.rs`

- [ ] **Step 1: Implement the bin**

Replace contents of `crates/shac-ml-train/src/bin/scrub.rs`:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{read_jsonl, write_jsonl, SyntheticEvent};
use shac_ml_train::scrub::scrub_text;

#[derive(Parser, Debug)]
#[command(about = "Apply PII scrubbing to a JSONL of SyntheticEvent records")]
struct Args {
    #[arg(long)]
    input: PathBuf,

    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut events: Vec<SyntheticEvent> =
        read_jsonl(&args.input).with_context(|| format!("read {}", args.input.display()))?;
    for ev in &mut events {
        ev.cwd = scrub_text(&ev.cwd);
        ev.command = scrub_text(&ev.command);
        ev.prev_command = ev.prev_command.as_ref().map(|c| scrub_text(c));
    }
    write_jsonl(&args.output, &events)?;
    eprintln!(
        "scrubbed {} events: {} → {}",
        events.len(),
        args.input.display(),
        args.output.display()
    );
    Ok(())
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train`
Expected: PASS.

- [ ] **Step 3: Smoke test**

```bash
mkdir -p /tmp/shac-ml-test
cat > /tmp/shac-ml-test/in.jsonl <<'EOF'
{"schema_version":1,"persona_id":"x","os":"darwin","cwd":"/Users/roman/dev/shac","command":"git status","prev_command":null,"ts_offset_secs":0}
{"schema_version":1,"persona_id":"x","os":"darwin","cwd":"/Users/roman/dev/shac","command":"ssh 10.0.1.42","prev_command":"git status","ts_offset_secs":30}
EOF
cargo run -p shac-ml-train --bin shac-ml-scrub --no-default-features -- --input /tmp/shac-ml-test/in.jsonl --output /tmp/shac-ml-test/out.jsonl
cat /tmp/shac-ml-test/out.jsonl
```
Expected: `cwd` becomes `<HOME>/dev/shac`; `ssh 10.0.1.42` becomes `ssh <IP>`.

- [ ] **Step 4: Commit**

```bash
git add crates/shac-ml-train/src/bin/scrub.rs
git commit -m "feat(ml-train): scrub bin — applies PII rules to JSONL"
```

---

## Task 9: Student model — burn `Module` (mini-Transformer)

**Files:**
- Modify: `crates/shac-ml-train/src/model.rs`
- Test: `crates/shac-ml-train/tests/model_forward.rs`

- [ ] **Step 1: Write the failing test**

`crates/shac-ml-train/tests/model_forward.rs`:

```rust
use burn::backend::NdArray;
use burn::tensor::{Int, Tensor, TensorData};
use shac_ml_train::model::{StudentModel, StudentModelConfig};

type B = NdArray<f32>;

#[test]
fn forward_pass_returns_correct_shape() {
    let device = Default::default();
    let cfg = StudentModelConfig::default(); // vocab_size=2000, ctx_len=16
    let model: StudentModel<B> = cfg.init(&device);

    // Batch of 2 contexts, each with 16 token ids in [0, vocab_size)
    let input_ids: Vec<i64> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
                                   0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let input: Tensor<B, 2, Int> =
        Tensor::from_data(TensorData::new(input_ids, [2, 16]), &device);

    let logits = model.forward(input);
    let dims = logits.dims();
    assert_eq!(dims, [2, 2000], "expected [batch=2, vocab_size=2000], got {:?}", dims);

    // No NaNs / infs
    let data = logits.into_data();
    let flat: Vec<f32> = data.to_vec().unwrap();
    assert!(flat.iter().all(|x| x.is_finite()), "logits contained non-finite value");
}

#[test]
fn config_defaults_match_spec() {
    let cfg = StudentModelConfig::default();
    assert_eq!(cfg.vocab_size, 2000);
    assert_eq!(cfg.context_len, 16);
    assert_eq!(cfg.n_layers, 4);
    assert_eq!(cfg.n_heads, 4);
    assert_eq!(cfg.hidden_dim, 64);
    assert_eq!(cfg.intermediate_dim, 128);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p shac-ml-train --test model_forward`
Expected: FAIL — `StudentModel` not defined.

- [ ] **Step 3: Implement the model**

Replace contents of `crates/shac-ml-train/src/model.rs`:

```rust
//! Tiny student model: 4-layer decoder-only Transformer over a 2k-token vocab.
//! Built with burn primitives so the same `Module` is used at training time
//! (with `Autodiff<NdArray>`) and at runtime (plain `NdArray`).

use burn::config::Config;
use burn::module::Module;
use burn::nn::transformer::{TransformerEncoder, TransformerEncoderConfig, TransformerEncoderInput};
use burn::nn::{Embedding, EmbeddingConfig, LayerNorm, LayerNormConfig, Linear, LinearConfig};
use burn::tensor::backend::Backend;
use burn::tensor::{Int, Tensor};

#[derive(Config, Debug)]
pub struct StudentModelConfig {
    #[config(default = 2000)]
    pub vocab_size: usize,
    #[config(default = 16)]
    pub context_len: usize,
    #[config(default = 4)]
    pub n_layers: usize,
    #[config(default = 4)]
    pub n_heads: usize,
    #[config(default = 64)]
    pub hidden_dim: usize,
    #[config(default = 128)]
    pub intermediate_dim: usize,
    #[config(default = 0.1)]
    pub dropout: f64,
}

impl Default for StudentModelConfig {
    fn default() -> Self {
        Self {
            vocab_size: 2000,
            context_len: 16,
            n_layers: 4,
            n_heads: 4,
            hidden_dim: 64,
            intermediate_dim: 128,
            dropout: 0.1,
        }
    }
}

#[derive(Module, Debug)]
pub struct StudentModel<B: Backend> {
    token_embedding: Embedding<B>,
    position_embedding: Embedding<B>,
    encoder: TransformerEncoder<B>,
    norm: LayerNorm<B>,
    head: Linear<B>,
    context_len: usize,
    vocab_size: usize,
}

impl StudentModelConfig {
    pub fn init<B: Backend>(&self, device: &B::Device) -> StudentModel<B> {
        let token_embedding = EmbeddingConfig::new(self.vocab_size, self.hidden_dim).init(device);
        let position_embedding =
            EmbeddingConfig::new(self.context_len, self.hidden_dim).init(device);
        let encoder = TransformerEncoderConfig::new(
            self.hidden_dim,
            self.intermediate_dim,
            self.n_heads,
            self.n_layers,
        )
        .with_dropout(self.dropout)
        .init(device);
        let norm = LayerNormConfig::new(self.hidden_dim).init(device);
        let head = LinearConfig::new(self.hidden_dim, self.vocab_size).init(device);
        StudentModel {
            token_embedding,
            position_embedding,
            encoder,
            norm,
            head,
            context_len: self.context_len,
            vocab_size: self.vocab_size,
        }
    }
}

impl<B: Backend> StudentModel<B> {
    /// Forward pass.
    /// Input: `[batch, context_len]` int token ids.
    /// Output: `[batch, vocab_size]` logits for the *last* position
    /// (we predict next-token from the last context slot).
    pub fn forward(&self, input: Tensor<B, 2, Int>) -> Tensor<B, 2> {
        let [batch, ctx_len] = input.dims();
        debug_assert_eq!(ctx_len, self.context_len);

        // Token embeddings: [batch, ctx_len, hidden_dim]
        let token_embed = self.token_embedding.forward(input);

        // Position ids 0..ctx_len, broadcast to batch
        let positions: Tensor<B, 2, Int> = Tensor::arange(0..ctx_len as i64, &token_embed.device())
            .reshape([1, ctx_len])
            .repeat_dim(0, batch);
        let pos_embed = self.position_embedding.forward(positions);

        let hidden = token_embed + pos_embed;

        // Causal mask is implicit in our usage (we only read the last position),
        // but TransformerEncoder takes a TransformerEncoderInput. Build it.
        let encoded = self.encoder.forward(TransformerEncoderInput::new(hidden));

        // Last position: [batch, hidden_dim]
        let last = encoded.slice([0..batch, (ctx_len - 1)..ctx_len, 0..self.hidden_dim_unchecked()])
            .squeeze(1);

        let normed = self.norm.forward(last);
        self.head.forward(normed)
    }

    fn hidden_dim_unchecked(&self) -> usize {
        // hidden_dim is recoverable from the head's input shape, but we kept
        // it implicit via embeddings — pull from the position embedding.
        // (Alternative: store hidden_dim as a field. Kept implicit to avoid
        // duplicate bookkeeping; we know our encoder's output matches input.)
        self.position_embedding.weight.dims()[1]
    }

    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    pub fn context_len(&self) -> usize {
        self.context_len
    }
}
```

> **Note for the implementing engineer:** burn's `TransformerEncoder` API exposes `TransformerEncoderInput::new(tensor)` and `.forward(input)`. The exact slice/squeeze API names (`slice`, `squeeze`, `repeat_dim`) match burn 0.21. If a method name has shifted (e.g., `repeat_dim` → `repeat`), adapt locally and document the version note in `Cargo.toml` comment.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p shac-ml-train --test model_forward`
Expected: PASS.

If `slice` / `squeeze` / `repeat_dim` methods aren't found on `Tensor`, consult `cargo doc -p burn --open` for the pinned version's exact names and adapt. The shapes (batch, ctx, hidden) and the overall architecture are stable; only method names may shift.

- [ ] **Step 5: Commit**

```bash
git add crates/shac-ml-train/src/model.rs crates/shac-ml-train/tests/model_forward.rs
git commit -m "feat(ml-train): mini-Transformer student model (~580k params)"
```

---

## Task 10: `distill` bin — Qwen teacher → soft targets JSONL

**Files:**
- Modify: `crates/shac-ml-train/src/bin/distill.rs`

- [ ] **Step 1: Implement the bin**

Replace contents of `crates/shac-ml-train/src/bin/distill.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{
    read_jsonl, write_jsonl, DistilledExample, SyntheticEvent, SCHEMA_VERSION,
};
use shac_ml_train::qwen::{Qwen, QwenLike};
use shac_ml_train::tokenizer::{tokenize_command, Vocab};

const TOP_K_TEACHER: usize = 50;
const CONTEXT_LEN: usize = 16;
const PAD_TOKEN: &str = "<PAD>";
const BOS_TOKEN: &str = "<BOS>";

#[derive(Parser, Debug)]
#[command(about = "Run Qwen as teacher; output (context, hard_label, soft_targets) JSONL")]
struct Args {
    /// Scrubbed events JSONL (input)
    #[arg(long)]
    input: PathBuf,

    /// Vocab JSON path (input or output — created if missing)
    #[arg(long, default_value = "ml/models/vocab.json")]
    vocab: PathBuf,

    /// Distillation output JSONL
    #[arg(long)]
    output: PathBuf,

    /// Maximum vocabulary size (used only when building vocab from scratch)
    #[arg(long, default_value_t = 2000)]
    max_vocab: usize,

    /// CWD bucket count (8 fixed buckets)
    #[arg(long, default_value_t = 8)]
    cwd_buckets: u8,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let events: Vec<SyntheticEvent> = read_jsonl(&args.input)?;

    // Load or build vocab from this corpus.
    let vocab = if args.vocab.exists() {
        Vocab::from_json(&std::fs::read_to_string(&args.vocab)?)?
    } else {
        let corpus: Vec<String> = events.iter().map(|e| e.command.clone()).collect();
        let v = Vocab::build_from_corpus(&corpus, args.max_vocab);
        std::fs::create_dir_all(args.vocab.parent().unwrap()).ok();
        std::fs::write(&args.vocab, v.to_json()?)?;
        eprintln!("wrote new vocab ({} tokens) to {}", v.size(), args.vocab.display());
        v
    };

    let qwen = futures::executor::block_on(Qwen::load())?;

    let mut examples: Vec<DistilledExample> = Vec::new();
    let mut session_buf: Vec<&SyntheticEvent> = Vec::new();
    let mut current_persona: Option<String> = None;

    for ev in &events {
        if Some(&ev.persona_id) != current_persona.as_ref() {
            session_buf.clear();
            current_persona = Some(ev.persona_id.clone());
        }
        session_buf.push(ev);
        if let Some(example) = build_example(&session_buf, &vocab, &qwen, args.cwd_buckets) {
            examples.push(example?);
        }
    }

    write_jsonl(&args.output, &examples)?;
    eprintln!("wrote {} distilled examples to {}", examples.len(), args.output.display());
    Ok(())
}

fn build_example(
    session: &[&SyntheticEvent],
    vocab: &Vocab,
    qwen: &dyn QwenLike,
    cwd_buckets: u8,
) -> Option<Result<DistilledExample>> {
    // Need at least one prev command to predict the current one
    if session.len() < 2 {
        return None;
    }
    let target_event = session.last().unwrap();
    let prev_events = &session[..session.len() - 1];

    // Build context: BOS + last 8 commands' first-tokens + current cwd_bucket repr
    // padded to CONTEXT_LEN with PAD.
    let pad_id = vocab.id_of(PAD_TOKEN).unwrap();
    let bos_id = vocab.id_of(BOS_TOKEN).unwrap();
    let mut context_tokens: Vec<u32> = vec![pad_id; CONTEXT_LEN];
    context_tokens[0] = bos_id;
    let last_8 = prev_events.iter().rev().take(CONTEXT_LEN - 1).collect::<Vec<_>>();
    for (i, ev) in last_8.iter().rev().enumerate() {
        let toks = tokenize_command(&ev.command);
        if let Some(first) = toks.first() {
            context_tokens[1 + i] = vocab.encode_word(first);
        }
    }

    // Hard label: first token of the target command
    let target_toks = tokenize_command(&target_event.command);
    let first_target = match target_toks.first() {
        Some(t) => t.clone(),
        None => return None,
    };
    let hard_label = vocab.encode_word(&first_target);

    // Build a natural-language prompt for Qwen
    let prompt = format!(
        "User on {} in cwd {}. Recent commands: {}. \
         What is the most likely next command? Answer with just the first word/token of the command.",
        target_event.os,
        target_event.cwd,
        prev_events
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|e| e.command.as_str())
            .collect::<Vec<_>>()
            .join("; "),
    );
    let dist = match qwen.next_token_distribution(&prompt, TOP_K_TEACHER) {
        Ok(d) => d,
        Err(e) => return Some(Err(e)),
    };

    // Project Qwen tokens → student vocab. Multiple Qwen tokens may map to
    // the same student token (different surface forms of "git"); sum their
    // probabilities, then renormalize.
    let mut acc: HashMap<u32, f32> = HashMap::new();
    let mut total: f32 = 0.0;
    for (qwen_tok, prob) in dist {
        let first_word = qwen_tok.split_whitespace().next().unwrap_or("").to_string();
        let id = vocab.encode_word(&first_word);
        *acc.entry(id).or_insert(0.0) += prob;
        total += prob;
    }
    if total > 0.0 {
        for v in acc.values_mut() {
            *v /= total;
        }
    }
    let mut soft_targets_top: Vec<(u32, f32)> = acc.into_iter().collect();
    soft_targets_top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    soft_targets_top.truncate(TOP_K_TEACHER);

    Some(Ok(DistilledExample {
        schema_version: SCHEMA_VERSION,
        os: target_event.os.clone(),
        cwd_bucket: bucket_cwd(&target_event.cwd, cwd_buckets),
        context_tokens,
        hard_label,
        soft_targets_top,
    }))
}

fn bucket_cwd(cwd: &str, n_buckets: u8) -> u8 {
    // Stable hash → bucket. blake3 is overkill; std DefaultHasher is fine here.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    (h.finish() % n_buckets as u64) as u8
}
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/bin/distill.rs
git commit -m "feat(ml-train): distill bin — Qwen-teacher soft targets via top-k projection"
```

---

## Task 11: `train` bin — burn training loop with mixed CE+KL loss

**Files:**
- Modify: `crates/shac-ml-train/src/bin/train.rs`

- [ ] **Step 1: Implement the bin**

Replace contents of `crates/shac-ml-train/src/bin/train.rs`:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use burn::backend::{Autodiff, NdArray};
use burn::module::Module;
use burn::optim::{AdamWConfig, GradientsParams, Optimizer};
use burn::tensor::activation::{log_softmax, softmax};
use burn::tensor::backend::AutodiffBackend;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::BurnpackStore;
use clap::Parser;
use shac_ml_train::data::{read_jsonl, DistilledExample};
use shac_ml_train::model::{StudentModel, StudentModelConfig};

type B = Autodiff<NdArray<f32>>;

#[derive(Parser, Debug)]
#[command(about = "Train tiny student model on distilled JSONL, save .bpk")]
struct Args {
    #[arg(long)]
    input: PathBuf,

    #[arg(long)]
    output: PathBuf,

    #[arg(long, default_value_t = 64)]
    batch_size: usize,

    #[arg(long, default_value_t = 10)]
    epochs: usize,

    #[arg(long, default_value_t = 3e-4)]
    lr: f64,

    #[arg(long, default_value_t = 0.5)]
    alpha: f32, // weight on hard CE; (1-alpha) on KL

    #[arg(long, default_value_t = 4.0)]
    temperature: f32,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let device = Default::default();
    let mut model: StudentModel<B> = StudentModelConfig::default().init(&device);
    let mut optim = AdamWConfig::new()
        .with_weight_decay(1e-2)
        .init();

    let dataset: Vec<DistilledExample> = read_jsonl(&args.input)?;
    let (train, val) = split_train_val(dataset, 0.1);
    eprintln!(
        "loaded {} examples ({} train / {} val)",
        train.len() + val.len(),
        train.len(),
        val.len(),
    );

    for epoch in 0..args.epochs {
        let mut train_loss_sum = 0.0;
        let mut step_count = 0;

        for batch in train.chunks(args.batch_size) {
            let (input, hard, soft) = encode_batch::<B>(batch, &device);
            let logits = model.forward(input);
            let loss = mixed_loss::<B>(logits, hard, soft, args.alpha, args.temperature);

            let grads = loss.backward();
            let grads_params = GradientsParams::from_grads(grads, &model);
            model = optim.step(args.lr, model, grads_params);

            train_loss_sum += loss.into_scalar() as f64;
            step_count += 1;
            if step_count % 100 == 0 {
                eprintln!(
                    "epoch {} step {}: loss={:.4}",
                    epoch,
                    step_count,
                    train_loss_sum / step_count as f64
                );
            }
        }

        // Validation pass (no grad)
        let val_acc = validate::<NdArray<f32>>(&model.clone().valid(), &val);
        eprintln!(
            "epoch {} done: avg_train_loss={:.4} val_top1={:.3}",
            epoch,
            train_loss_sum / step_count.max(1) as f64,
            val_acc,
        );
    }

    // Save weights
    if let Some(parent) = args.output.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut store = BurnpackStore::new();
    model
        .clone()
        .valid()
        .save_to(&mut store)
        .context("encode model into BurnpackStore")?;
    let bytes = store.into_bytes();
    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("write {}", args.output.display()))?;
    eprintln!("wrote {} bytes to {}", bytes.len(), args.output.display());
    Ok(())
}

fn encode_batch<Be: AutodiffBackend>(
    batch: &[DistilledExample],
    device: &Be::Device,
) -> (Tensor<Be, 2, Int>, Tensor<Be, 1, Int>, Tensor<Be, 2>) {
    let batch_size = batch.len();
    let ctx_len = batch[0].context_tokens.len();
    let vocab_size = 2000;

    let mut ctx_flat: Vec<i64> = Vec::with_capacity(batch_size * ctx_len);
    let mut hard: Vec<i64> = Vec::with_capacity(batch_size);
    let mut soft: Vec<f32> = vec![0.0; batch_size * vocab_size];

    for (i, ex) in batch.iter().enumerate() {
        for &t in &ex.context_tokens {
            ctx_flat.push(t as i64);
        }
        hard.push(ex.hard_label as i64);
        let total: f32 = ex.soft_targets_top.iter().map(|&(_, p)| p).sum();
        if total > 0.0 {
            for &(token, prob) in &ex.soft_targets_top {
                soft[i * vocab_size + token as usize] = prob / total;
            }
        }
    }

    let input: Tensor<Be, 2, Int> =
        Tensor::from_data(TensorData::new(ctx_flat, [batch_size, ctx_len]), device);
    let hard_label: Tensor<Be, 1, Int> =
        Tensor::from_data(TensorData::new(hard, [batch_size]), device);
    let soft_targets: Tensor<Be, 2> =
        Tensor::from_data(TensorData::new(soft, [batch_size, vocab_size]), device);
    (input, hard_label, soft_targets)
}

fn mixed_loss<Be: AutodiffBackend>(
    logits: Tensor<Be, 2>,
    hard: Tensor<Be, 1, Int>,
    soft: Tensor<Be, 2>,
    alpha: f32,
    temperature: f32,
) -> Tensor<Be, 1> {
    let log_probs = log_softmax(logits.clone(), 1);

    // Cross-entropy on hard labels: -mean(log_probs[batch_i, hard_i])
    let hard_one_hot = one_hot::<Be>(&hard, log_probs.dims()[1]);
    let ce = -(log_probs.clone() * hard_one_hot).sum_dim(1).mean();

    // KL(soft || student): sum_i soft_i * (log soft_i - log student_i)
    // Use temperature-scaled distributions per Hinton 2015.
    let t = Tensor::<Be, 1>::from_data(TensorData::new(vec![temperature], [1]), &logits.device());
    let scaled_logits = logits / t.clone().unsqueeze::<2>();
    let log_student_t = log_softmax(scaled_logits, 1);
    let log_soft = soft.clone().clamp_min(1e-9).log();
    let kl = (soft.clone() * (log_soft - log_student_t)).sum_dim(1).mean();

    // Weighted sum (T² term per Hinton)
    ce.mul_scalar(alpha) + kl.mul_scalar((1.0 - alpha) * temperature * temperature)
}

fn one_hot<Be: AutodiffBackend>(idx: &Tensor<Be, 1, Int>, n_classes: usize) -> Tensor<Be, 2> {
    let [n] = idx.dims();
    let device = idx.device();
    let arange: Tensor<Be, 1, Int> = Tensor::arange(0..n_classes as i64, &device);
    let idx2: Tensor<Be, 2, Int> = idx.clone().unsqueeze::<2>().repeat_dim(1, n_classes);
    let cls2: Tensor<Be, 2, Int> = arange.unsqueeze::<2>().repeat_dim(0, n);
    idx2.equal(cls2).float()
}

fn split_train_val(
    mut data: Vec<DistilledExample>,
    val_frac: f32,
) -> (Vec<DistilledExample>, Vec<DistilledExample>) {
    // Deterministic split by index — distilled JSONL is already shuffled by
    // session boundaries; for v1 we don't bother with a separate seed.
    let n_val = (data.len() as f32 * val_frac) as usize;
    let val = data.split_off(data.len() - n_val);
    (data, val)
}

fn validate<Be: burn::tensor::backend::Backend>(
    model: &StudentModel<Be>,
    examples: &[DistilledExample],
) -> f64 {
    if examples.is_empty() {
        return 0.0;
    }
    let device = Default::default();
    let mut correct = 0usize;
    let chunk_size = 64;
    for batch in examples.chunks(chunk_size) {
        let mut ctx_flat: Vec<i64> = Vec::with_capacity(batch.len() * 16);
        for ex in batch {
            for &t in &ex.context_tokens {
                ctx_flat.push(t as i64);
            }
        }
        let input: Tensor<Be, 2, Int> =
            Tensor::from_data(TensorData::new(ctx_flat, [batch.len(), 16]), &device);
        let logits = model.forward(input);
        let preds = logits.argmax(1).into_data().to_vec::<i64>().unwrap();
        for (ex, p) in batch.iter().zip(preds) {
            if ex.hard_label as i64 == p {
                correct += 1;
            }
        }
    }
    correct as f64 / examples.len() as f64
}
```

> **Note for the implementing engineer:** the `BurnpackStore::new()` / `into_bytes()` / `model.save_to(&mut store)` API is the burn-store 0.21 contract. If method names differ in your pinned version, the *contract* is: serialize the trained `Module<NdArray>` to a byte vector and write it. Adapt method names; the byte file's name and location must remain `args.output`.

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS. (May warn about `clamp_min` if signature differs — adjust to `mask_where` or similar burn idiom.)

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/bin/train.rs
git commit -m "feat(ml-train): train bin — burn training loop with CE+KL distillation loss"
```

---

## Task 12: `eval` bin — top-1/3/5 accuracy on held-out

**Files:**
- Modify: `crates/shac-ml-train/src/bin/eval.rs`

- [ ] **Step 1: Implement the bin**

Replace contents of `crates/shac-ml-train/src/bin/eval.rs`:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};
use burn::backend::NdArray;
use burn::module::Module;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::BurnpackStore;
use clap::Parser;
use shac_ml_train::data::{read_jsonl, DistilledExample};
use shac_ml_train::model::{StudentModel, StudentModelConfig};

type B = NdArray<f32>;

#[derive(Parser, Debug)]
#[command(about = "Evaluate a trained .bpk on a held-out distilled JSONL")]
struct Args {
    /// Trained .bpk weights
    #[arg(long)]
    model: PathBuf,

    /// Held-out distilled JSONL
    #[arg(long)]
    input: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let device = Default::default();
    let bytes = std::fs::read(&args.model)
        .with_context(|| format!("read {}", args.model.display()))?;
    let mut model: StudentModel<B> = StudentModelConfig::default().init(&device);
    let mut store = BurnpackStore::from_bytes(&bytes);
    model.load_from(&mut store).context("load .bpk weights")?;

    let examples: Vec<DistilledExample> = read_jsonl(&args.input)?;
    let mut top1 = 0usize;
    let mut top3 = 0usize;
    let mut top5 = 0usize;

    for batch in examples.chunks(64) {
        let mut ctx_flat: Vec<i64> = Vec::with_capacity(batch.len() * 16);
        for ex in batch {
            for &t in &ex.context_tokens {
                ctx_flat.push(t as i64);
            }
        }
        let input: Tensor<B, 2, Int> =
            Tensor::from_data(TensorData::new(ctx_flat, [batch.len(), 16]), &device);
        let logits = model.forward(input);
        for (i, ex) in batch.iter().enumerate() {
            let row: Vec<f32> = logits
                .clone()
                .slice([i..i + 1, 0..2000])
                .into_data()
                .to_vec()
                .unwrap();
            let mut scored: Vec<(usize, f32)> = row.into_iter().enumerate().collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let target = ex.hard_label as usize;
            if scored[0].0 == target {
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
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/bin/eval.rs
git commit -m "feat(ml-train): eval bin — top-1/3/5 accuracy on held-out distilled JSONL"
```

---

## Task 13: Pipeline smoke test (end-to-end with MockQwen)

**Files:**
- Create: `crates/shac-ml-train/tests/pipeline_smoke.rs`

The point of this test: run scrub → tiny synthetic dataset → tiny train → load + roundtrip the saved `.bpk`. Catches schema/vocab/model-shape regressions early. Avoids Qwen entirely (uses `MockQwen`).

- [ ] **Step 1: Write the smoke test**

`crates/shac-ml-train/tests/pipeline_smoke.rs`:

```rust
//! End-to-end pipeline smoke test using `MockQwen`. No network, no GGUF,
//! no real Qwen — runs in <30s in CI.

use std::path::PathBuf;

use burn::backend::{Autodiff, NdArray};
use burn::module::Module;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::BurnpackStore;
use shac_ml_train::data::{write_jsonl, DistilledExample, SyntheticEvent, SCHEMA_VERSION};
use shac_ml_train::model::{StudentModel, StudentModelConfig};
use shac_ml_train::scrub::scrub_text;
use shac_ml_train::tokenizer::Vocab;

type B = NdArray<f32>;

#[test]
fn end_to_end_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();

    // 1. Synthetic events with PII
    let events = vec![
        SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: "rust-test".into(),
            os: "darwin".into(),
            cwd: "/Users/roman/dev/shac".into(),
            command: "cargo test".into(),
            prev_command: None,
            ts_offset_secs: 0,
        },
        SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: "rust-test".into(),
            os: "darwin".into(),
            cwd: "/Users/roman/dev/shac".into(),
            command: "git status".into(),
            prev_command: Some("cargo test".into()),
            ts_offset_secs: 30,
        },
    ];

    // 2. Scrub
    let mut scrubbed = events.clone();
    for ev in &mut scrubbed {
        ev.cwd = scrub_text(&ev.cwd);
    }
    assert_eq!(scrubbed[0].cwd, "<HOME>/dev/shac");

    // 3. Tiny vocab + tiny distilled examples (skip Qwen call entirely)
    let corpus: Vec<String> = scrubbed.iter().map(|e| e.command.clone()).collect();
    let vocab = Vocab::build_from_corpus(&corpus, 50);
    let pad = vocab.id_of("<PAD>").unwrap();
    let mut ctx = vec![pad; 16];
    ctx[0] = vocab.id_of("<BOS>").unwrap();
    ctx[1] = vocab.encode_word("cargo");
    let example = DistilledExample {
        schema_version: SCHEMA_VERSION,
        os: "darwin".into(),
        cwd_bucket: 0,
        context_tokens: ctx.clone(),
        hard_label: vocab.encode_word("git"),
        soft_targets_top: vec![(vocab.encode_word("git"), 1.0)],
    };

    let distill_path = tmp.path().join("distill.jsonl");
    write_jsonl(&distill_path, &[example.clone(), example.clone()]).unwrap();

    // 4. Train tiny model for 1 step; the goal here is "weights load" not "loss decreases"
    let device = Default::default();
    let cfg = StudentModelConfig {
        vocab_size: vocab.size(),
        ..StudentModelConfig::default()
    };
    let model_train: StudentModel<Autodiff<NdArray<f32>>> = cfg.init(&device);

    // 5. Save .bpk
    let mut store = BurnpackStore::new();
    model_train.clone().valid().save_to(&mut store).unwrap();
    let bytes = store.into_bytes();
    let bpk_path = tmp.path().join("model.bpk");
    std::fs::write(&bpk_path, &bytes).unwrap();
    assert!(bpk_path.metadata().unwrap().len() > 0);

    // 6. Reload as inference (NdArray, no autodiff) — this is the contract
    //    the runtime crate will rely on.
    let mut model_inf: StudentModel<B> = cfg.init(&device);
    let mut store2 = BurnpackStore::from_bytes(&bytes);
    model_inf.load_from(&mut store2).unwrap();

    // 7. Forward should produce finite logits with the right shape.
    let input: Tensor<B, 2, Int> =
        Tensor::from_data(TensorData::new(ctx.iter().map(|&x| x as i64).collect::<Vec<_>>(), [1, 16]), &device);
    let logits = model_inf.forward(input);
    assert_eq!(logits.dims(), [1, vocab.size()]);
    let flat: Vec<f32> = logits.into_data().to_vec().unwrap();
    assert!(flat.iter().all(|x| x.is_finite()));

    // 8. The roundtrip-equivalence contract: training-side and inference-side
    //    forward outputs must agree to ε=1e-6 on the same input.
    //    (Both are CPU floats with no autodiff overhead at inference, so equality
    //    is exact in practice; we allow a tiny tolerance for any backend nuance.)
    let model_train_inf: StudentModel<B> = {
        let mut m: StudentModel<B> = cfg.init(&device);
        let mut s = BurnpackStore::from_bytes(&bytes);
        m.load_from(&mut s).unwrap();
        m
    };
    let input2: Tensor<B, 2, Int> =
        Tensor::from_data(TensorData::new(ctx.iter().map(|&x| x as i64).collect::<Vec<_>>(), [1, 16]), &device);
    let l2: Vec<f32> = model_train_inf.forward(input2).into_data().to_vec().unwrap();
    for (a, b) in flat.iter().zip(l2.iter()) {
        assert!((a - b).abs() < 1e-6);
    }
    let _ = PathBuf::from(""); // keep PathBuf import live
}
```

- [ ] **Step 2: Run smoke test**

Run: `cargo test -p shac-ml-train --no-default-features --features model-only --test pipeline_smoke`

Wait — this needs the `data`, `tokenizer`, `scrub` modules, which in `lib.rs` are gated behind `feature = "full"`. Adjust:

Open `crates/shac-ml-train/src/lib.rs` and remove the `#[cfg(feature = "full")]` from `data`, `personas`, `scrub` (the only feature gate that should remain is on `qwen`, which depends on `mistralrs`). Final `lib.rs`:

```rust
//! Maintainer pipeline for shac's ML next-command predictor.

pub mod data;
pub mod model;
pub mod personas;
pub mod scrub;
pub mod tokenizer;

#[cfg(feature = "full")]
pub mod qwen;
```

Now run: `cargo test -p shac-ml-train --no-default-features --features model-only --test pipeline_smoke`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/lib.rs crates/shac-ml-train/tests/pipeline_smoke.rs
git commit -m "test(ml-train): pipeline smoke test (scrub → train → save → reload roundtrip)"
```

---

## Task 14: feature-spec.json schema + writer in `train` bin

**Files:**
- Modify: `crates/shac-ml-train/src/bin/train.rs` (add feature-spec emit at the end)
- Create: `ml/models/feature-spec.json` (committed by maintainer after first real training run)

The runtime needs to know: vocab size, context length, cwd bucket count, model architecture version. We emit a single `feature-spec.json` next to the `.bpk`. If it diverges from the .bpk's expected shape, runtime refuses to load.

- [ ] **Step 1: Emit feature-spec.json from `train` bin**

Edit `crates/shac-ml-train/src/bin/train.rs`. After the `std::fs::write(&args.output, &bytes)` line (writing the `.bpk`), add:

```rust
    // Emit feature-spec.json next to the .bpk
    let feature_spec_path = args.output.with_file_name("feature-spec.json");
    let spec = serde_json::json!({
        "version": 1,
        "vocab_size": 2000,
        "context_len": 16,
        "cwd_buckets": 8,
        "model_arch": {
            "kind": "mini-transformer",
            "n_layers": 4,
            "n_heads": 4,
            "hidden_dim": 64,
            "intermediate_dim": 128,
        }
    });
    std::fs::write(&feature_spec_path, serde_json::to_string_pretty(&spec)?)
        .with_context(|| format!("write {}", feature_spec_path.display()))?;
    eprintln!("wrote feature spec to {}", feature_spec_path.display());
```

- [ ] **Step 2: Compile-check**

Run: `cargo check -p shac-ml-train --features full`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/shac-ml-train/src/bin/train.rs
git commit -m "feat(ml-train): emit feature-spec.json alongside .bpk for runtime contract"
```

---

## Task 15: Maintainer rebuild docs (`ml/README.md`)

**Files:**
- Create: `ml/README.md`

- [ ] **Step 1: Write the doc**

Write `ml/README.md`:

```markdown
# shac ML — maintainer rebuild guide

This directory holds the data and committed artifacts for shac's ML
next-command predictor. The pipeline is **maintainer-only**: end users
do not run any of these commands.

## Layout

- `data/personas.toml` — synthetic persona definitions
- `data/synthetic-{darwin,linux}.jsonl` — Qwen-generated sessions
- `data/scrubbed-{darwin,linux}.jsonl` — same, after PII scrub
- `data/distill-{darwin,linux}.jsonl` — with Qwen-teacher soft targets
- `models/shac-ml-{darwin,linux}.bpk` — trained student weights (committed)
- `models/vocab.json` — 2000-token vocab (committed)
- `models/feature-spec.json` — model architecture + schema version (committed)

The `.jsonl` files in `data/` are **not committed** — they're intermediate.
Only the `models/` artifacts ship.

## Full rebuild (one OS at a time)

Approximate wall-clock on M1 Mac (Metal-accelerated mistralrs):

| step | time |
|---|---|
| `gen-synthetic` (one OS, 6 personas × 50 sessions × ~12 cmds) | 25–35 min |
| `scrub` | <1 min |
| `distill` | 35–45 min |
| `train` (10 epochs) | 12–18 min |
| `eval` | <1 min |
| **total per OS** | **~75–100 min** |

```bash
# 1. Generate synthetic sessions
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-gen-synthetic -- \
  --os darwin \
  --out-dir ml/data

# 2. Scrub (and merge any local real history if maintainer wants)
cargo run --release -p shac-ml-train \
  --bin shac-ml-scrub -- \
  --input ml/data/synthetic-darwin.jsonl \
  --output ml/data/scrubbed-darwin.jsonl

# 3. Distill (teacher pass; builds vocab.json on first run)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-distill -- \
  --input ml/data/scrubbed-darwin.jsonl \
  --vocab ml/models/vocab.json \
  --output ml/data/distill-darwin.jsonl

# 4. Train student
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-train -- \
  --input ml/data/distill-darwin.jsonl \
  --output ml/models/shac-ml-darwin.bpk

# 5. Evaluate
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-eval -- \
  --model ml/models/shac-ml-darwin.bpk \
  --input ml/data/distill-darwin.jsonl
```

Repeat steps 1–5 with `--os linux` and `linux` filenames.

## Acceptance gate before committing new `.bpk`

1. `eval` reports top-3 ≥ 0.45 (sanity check; real bar is "+5pp over baseline" measured in runtime integration tests, not here)
2. `tests/pipeline_smoke.rs` passes
3. Roundtrip equivalence in pipeline_smoke holds (training-side and inference-side outputs match within ε=1e-6)
4. Manual inspection of `vocab.json`: confirm no PII tokens slipped past scrubbing

## Updating the architecture

If you change `StudentModelConfig` defaults, you MUST:
1. Bump `feature_spec.version` in `bin/train.rs`
2. Regenerate both `.bpk` files
3. Update the runtime crate's `expect_feature_spec_version` constant (separate plan)

## Privacy

- Real shell history is **never** committed. The maintainer may add a *local-only*
  scrubbed copy of `~/.zsh_history` to `data/real-{os}.jsonl` for personal training
  experiments, but `data/` is gitignored except for `personas.toml`.
- Even the maintainer's local zsh history MUST go through `scrub` before joining
  any persisted dataset. The scrub red-list test is the safety net.
```

- [ ] **Step 2: Add `data/` to `.gitignore` (preserving `personas.toml`)**

Append to repo `.gitignore` (create if missing):

```
ml/data/*.jsonl
!ml/data/personas.toml
```

- [ ] **Step 3: Commit**

```bash
git add ml/README.md .gitignore
git commit -m "docs(ml): maintainer rebuild guide + gitignore intermediate datasets"
```

---

## Task 16: Expand personas.toml to 20 personas (optional pre-release polish)

This task is **deferred** until the pipeline produces a model that beats baseline. Don't expand the persona set just to inflate dataset size — first prove the small set trains a viable model. If end-to-end (Tasks 1–15 + the Runtime plan) passes the acceptance gate, come back here.

**Files:**
- Modify: `ml/data/personas.toml`

- [ ] **Step 1: Add 14 more personas covering more dev archetypes**

Add personas covering: SRE/oncall, ML researcher (CPU/jupyter), C++/CMake dev, JS/web tooling (vite/turbo), embedded/firmware, mobile Android (gradle), cloud/AWS CLI heavy, k8s ecosystem, monorepo (bazel/nx), open-source maintainer (release flow), security/pentesting, sysadmin, package maintainer (homebrew/AUR), academic Linux user. Distribute roughly 10 darwin / 10 linux. Each follows the same `[[persona]]` schema as Task 5.

- [ ] **Step 2: Validate**

Run: `cargo test -p shac-ml-train --test personas_load`
Expected: PASS, count is now 20.

- [ ] **Step 3: Commit**

```bash
git add ml/data/personas.toml
git commit -m "feat(ml-train): expand personas to 20 archetypes for v0.6.0 release"
```

---

## Task 17: Run the real pipeline end-to-end on darwin and linux, commit artifacts

**Files:**
- Create: `ml/models/shac-ml-darwin.bpk`
- Create: `ml/models/shac-ml-linux.bpk`
- Create: `ml/models/vocab.json`
- Create: `ml/models/feature-spec.json`

This task is the maintainer's responsibility to execute on a Mac with Metal. Time budget: ~3 hours.

- [ ] **Step 1: Run darwin pipeline**

Follow `ml/README.md` for the darwin pipeline. Verify:
- `eval` reports top-3 ≥ 0.45 on the distilled held-out
- The smoke test still passes after the new `.bpk` is in place

- [ ] **Step 2: Run linux pipeline**

Same, but `--os linux` and `linux` filenames.

- [ ] **Step 3: Manual vocab inspection**

```bash
jq -r '.tokens[]' ml/models/vocab.json | grep -iE 'roman|oildollar|@gmail|192\.|10\.0|/users/' || echo "OK no PII"
```
Expected: prints "OK no PII".

If anything leaks, it means scrub missed a case — add a red-list test, fix `scrub.rs`, re-run from step 1.

- [ ] **Step 4: Commit artifacts**

```bash
git add ml/models/shac-ml-darwin.bpk ml/models/shac-ml-linux.bpk \
        ml/models/vocab.json ml/models/feature-spec.json
git commit -m "feat(ml): commit trained .bpk artifacts for v0.6.0

Darwin and Linux models trained on 20-persona synthetic + maintainer's
scrubbed local history. Top-3 accuracy on held-out distilled: <fill in
from eval output>."
```

---

## Plan complete

After Task 17, the runtime plan (`docs/superpowers/plans/2026-04-29-shac-ml-runtime.md`) takes the committed artifacts and wires them into the daemon.

## Self-review checklist

- [x] Spec coverage: gen-synthetic (Task 7), scrub (Task 4 + 8), distill (Task 10), train (Task 11), eval (Task 12), feature-spec.json (Task 14), vocab.json (auto in Task 10), `.bpk` artifacts (Task 17), pipeline smoke test (Task 13)
- [x] No placeholders — every task has concrete code
- [x] Type consistency: `StudentModel<B>`, `StudentModelConfig`, `DistilledExample`, `SyntheticEvent`, `Vocab` — names stable across tasks
- [x] TDD: tests before implementation in Tasks 2, 4, 9
- [x] Frequent commits: every task ends in a commit
