//! Exercises the actual `shac-ml-train` / `shac-ml-eval` binaries (not just
//! their internal functions) against a tiny hand-built vocab + dataset, to
//! verify the finding #9 vocab-size contract end-to-end:
//!   - both bins take `--vocab` and derive `vocab_size` from it
//!   - `feature-spec.json` records the actual vocab_size/cwd_buckets, not a
//!     hardcoded literal
//!   - a dataset that doesn't match `--vocab` is rejected, not silently
//!     mistrained/mis-evaluated

use std::process::Command;

use shac_ml_train::data::{write_jsonl, DistilledExample, SCHEMA_VERSION};
use shac_ml_train::tokenizer::Vocab;

const CTX_LEN: usize = 16;

fn tiny_vocab() -> Vocab {
    let corpus = vec![
        "cargo build".to_string(),
        "cargo test".to_string(),
        "git status".to_string(),
    ];
    Vocab::build_from_corpus(&corpus, 64)
}

fn example(vocab: &Vocab, first_word: &str, hard_word: &str) -> DistilledExample {
    let bos = vocab.encode_word("<BOS>");
    let pad = vocab.encode_word("<PAD>");
    let mut context_tokens = vec![pad; CTX_LEN];
    context_tokens[0] = bos;
    context_tokens[1] = vocab.encode_word(first_word);
    let hard_label = vocab.encode_word(hard_word);
    DistilledExample {
        schema_version: SCHEMA_VERSION,
        os: "darwin".to_string(),
        cwd_bucket: 0,
        context_tokens,
        hard_label,
        soft_targets_top: vec![(hard_label, 1.0)],
    }
}

#[test]
fn train_then_eval_binaries_roundtrip_with_explicit_vocab() {
    let tmp = tempfile::tempdir().unwrap();

    let vocab = tiny_vocab();
    let vocab_path = tmp.path().join("vocab.json");
    std::fs::write(&vocab_path, vocab.to_json().unwrap()).unwrap();

    // A handful of repeated examples is enough to exercise one training
    // epoch and a non-empty eval; correctness of the loss/accuracy math is
    // covered elsewhere (model_forward.rs, pipeline_smoke.rs).
    let examples: Vec<DistilledExample> = (0..16)
        .map(|i| {
            if i % 2 == 0 {
                example(&vocab, "cargo", "build")
            } else {
                example(&vocab, "git", "status")
            }
        })
        .collect();
    let data_path = tmp.path().join("distill.jsonl");
    write_jsonl(&data_path, &examples).unwrap();

    let bpk_path = tmp.path().join("model.bpk");

    let train_status = Command::new(env!("CARGO_BIN_EXE_shac-ml-train"))
        .args([
            "--input",
            data_path.to_str().unwrap(),
            "--output",
            bpk_path.to_str().unwrap(),
            "--vocab",
            vocab_path.to_str().unwrap(),
            "--epochs",
            "1",
            "--batch-size",
            "8",
        ])
        .status()
        .expect("run shac-ml-train");
    assert!(train_status.success(), "shac-ml-train should exit 0");

    assert!(bpk_path.exists(), ".bpk must be written");
    let feature_spec_path = tmp.path().join("feature-spec.json");
    assert!(
        feature_spec_path.exists(),
        "feature-spec.json must be written"
    );
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&feature_spec_path).unwrap()).unwrap();
    assert_eq!(
        spec["vocab_size"].as_u64().unwrap() as usize,
        vocab.size(),
        "feature-spec.json vocab_size must reflect the actual --vocab, not a hardcoded default"
    );
    assert_eq!(
        spec["cwd_buckets"].as_u64().unwrap(),
        8,
        "feature-spec.json cwd_buckets should equal the declared --cwd-buckets default"
    );

    let eval_output = Command::new(env!("CARGO_BIN_EXE_shac-ml-eval"))
        .args([
            "--model",
            bpk_path.to_str().unwrap(),
            "--input",
            data_path.to_str().unwrap(),
            "--vocab",
            vocab_path.to_str().unwrap(),
        ])
        .output()
        .expect("run shac-ml-eval");
    assert!(eval_output.status.success(), "shac-ml-eval should exit 0");
    let stdout = String::from_utf8_lossy(&eval_output.stdout);
    assert!(stdout.contains("top-1:"), "eval output was: {stdout}");
}

#[test]
fn train_binary_fails_fast_on_vocab_dataset_mismatch() {
    let tmp = tempfile::tempdir().unwrap();

    // Build the dataset against the *full* tiny vocab...
    let full_vocab = tiny_vocab();
    let examples = vec![example(&full_vocab, "cargo", "build")];
    let data_path = tmp.path().join("distill.jsonl");
    write_jsonl(&data_path, &examples).unwrap();

    // ...but hand --vocab a special-tokens-only vocab that can't represent
    // the "cargo"/"build" ids the dataset actually uses.
    let small_vocab = Vocab::new_with_special_only();
    assert!(
        small_vocab.size() < full_vocab.size(),
        "fixture assumption: special-only vocab must be smaller"
    );
    let vocab_path = tmp.path().join("vocab.json");
    std::fs::write(&vocab_path, small_vocab.to_json().unwrap()).unwrap();

    let bpk_path = tmp.path().join("model.bpk");
    let output = Command::new(env!("CARGO_BIN_EXE_shac-ml-train"))
        .args([
            "--input",
            data_path.to_str().unwrap(),
            "--output",
            bpk_path.to_str().unwrap(),
            "--vocab",
            vocab_path.to_str().unwrap(),
            "--epochs",
            "1",
        ])
        .output()
        .expect("run shac-ml-train");
    assert!(
        !output.status.success(),
        "shac-ml-train must fail fast on a vocab/dataset mismatch instead of panicking or silently corrupting"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("vocab_size"),
        "error should explain the vocab_size mismatch, got: {stderr}"
    );
    assert!(!bpk_path.exists(), "no .bpk should be written on failure");
}
