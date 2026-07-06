//! End-to-end pipeline smoke test. No network, no GGUF, no real Qwen —
//! runs in <30s in CI.
//!
//! Exercises the full pipeline contract:
//!   scrub PII → build vocab → construct DistilledExample →
//!   init StudentModel → save .bpk → reload .bpk → forward() outputs match

use burn::backend::NdArray;
use burn::tensor::{Int, Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};
use shac_ml_train::data::{DistilledExample, SyntheticEvent, SCHEMA_VERSION};
use shac_ml_train::model::{StudentModel, StudentModelConfig};
use shac_ml_train::scrub::scrub_text;
use shac_ml_train::tokenizer::Vocab;

type B = NdArray<f32>;

#[test]
fn end_to_end_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();

    // ------------------------------------------------------------------ //
    // 1. Synthetic events with PII paths                                  //
    // ------------------------------------------------------------------ //
    let events = vec![
        SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: "rust-test".into(),
            session_id: 0,
            os: "darwin".into(),
            cwd: "/Users/roman/dev/shac".into(),
            command: "cargo test".into(),
            prev_command: None,
            ts_offset_secs: 0,
        },
        SyntheticEvent {
            schema_version: SCHEMA_VERSION,
            persona_id: "rust-test".into(),
            session_id: 0,
            os: "darwin".into(),
            cwd: "/Users/roman/dev/shac".into(),
            command: "git status".into(),
            prev_command: Some("cargo test".into()),
            ts_offset_secs: 30,
        },
    ];

    // ------------------------------------------------------------------ //
    // 2. Scrub PII — /Users/roman/... → <HOME>/...                       //
    // ------------------------------------------------------------------ //
    let mut scrubbed = events.clone();
    for ev in &mut scrubbed {
        ev.cwd = scrub_text(&ev.cwd);
    }
    assert_eq!(
        scrubbed[0].cwd, "<HOME>/dev/shac",
        "scrub_text should replace /Users/<name> with <HOME>"
    );
    assert_eq!(scrubbed[1].cwd, "<HOME>/dev/shac");

    // ------------------------------------------------------------------ //
    // 3. Build a tiny vocab from the corpus                               //
    // ------------------------------------------------------------------ //
    let corpus: Vec<String> = scrubbed.iter().map(|e| e.command.clone()).collect();
    let vocab = Vocab::build_from_corpus(&corpus, 50);

    assert!(vocab.id_of("cargo").is_some(), "cargo should be in vocab");
    assert!(vocab.id_of("git").is_some(), "git should be in vocab");

    let pad = vocab.id_of("<PAD>").unwrap();
    let bos = vocab.id_of("<BOS>").unwrap();

    // ------------------------------------------------------------------ //
    // 4. Construct a DistilledExample directly (skip Qwen entirely)      //
    // ------------------------------------------------------------------ //
    let ctx_len = 16_usize;
    let mut ctx_vec: Vec<u32> = vec![pad; ctx_len];
    ctx_vec[0] = bos;
    ctx_vec[1] = vocab.encode_word("cargo");

    let git_id = vocab.encode_word("git");
    let _example = DistilledExample {
        schema_version: SCHEMA_VERSION,
        os: "darwin".into(),
        cwd_bucket: 0,
        context_tokens: ctx_vec.clone(),
        hard_label: git_id,
        soft_targets_top: vec![(git_id, 1.0)],
    };

    // ------------------------------------------------------------------ //
    // 5. Initialise StudentModel with actual vocab size                   //
    // ------------------------------------------------------------------ //
    let device = Default::default();
    let cfg = StudentModelConfig {
        vocab_size: vocab.size(),
        ..StudentModelConfig::default()
    };
    let model: StudentModel<B> = cfg.init(&device);

    // ------------------------------------------------------------------ //
    // 6. Save .bpk  (save_into takes &self — no clone needed)            //
    // ------------------------------------------------------------------ //
    let bpk_path = tmp.path().join("model.bpk");
    let mut store = BurnpackStore::from_file(&bpk_path)
        .auto_extension(false)
        .overwrite(true);
    model.save_into(&mut store).unwrap();

    // File must exist and be non-empty.
    let meta = bpk_path
        .metadata()
        .expect("bpk file should exist after save");
    assert!(meta.len() > 0, "saved .bpk file must not be empty");

    // ------------------------------------------------------------------ //
    // 7. Forward pass on the original model                               //
    // ------------------------------------------------------------------ //
    let input_data: Vec<i64> = ctx_vec.iter().map(|&x| x as i64).collect();

    let input1: Tensor<B, 2, Int> =
        Tensor::from_data(TensorData::new(input_data.clone(), [1, ctx_len]), &device);
    let logits1 = model.forward(input1);

    // Shape check: [1, vocab.size()]
    assert_eq!(
        logits1.dims(),
        [1, vocab.size()],
        "logits shape should be [batch=1, vocab_size]"
    );
    let flat1: Vec<f32> = logits1.into_data().to_vec().unwrap();
    assert!(
        flat1.iter().all(|x| x.is_finite()),
        "original model logits contain non-finite values"
    );

    // ------------------------------------------------------------------ //
    // 8. Reload from .bpk and forward                                     //
    // ------------------------------------------------------------------ //
    let mut model_reloaded: StudentModel<B> = cfg.init(&device);
    let mut store2 = BurnpackStore::from_file(&bpk_path).auto_extension(false);
    model_reloaded.load_from(&mut store2).unwrap();

    let input2: Tensor<B, 2, Int> =
        Tensor::from_data(TensorData::new(input_data, [1, ctx_len]), &device);
    let logits2 = model_reloaded.forward(input2);
    let flat2: Vec<f32> = logits2.into_data().to_vec().unwrap();

    // ------------------------------------------------------------------ //
    // 9. Roundtrip equivalence check (ε = 1e-6 element-wise)             //
    // ------------------------------------------------------------------ //
    assert_eq!(
        flat1.len(),
        flat2.len(),
        "reloaded model output length must match original"
    );

    let mut max_diff: f32 = 0.0;
    for (i, (a, b)) in flat1.iter().zip(flat2.iter()).enumerate() {
        let diff = (a - b).abs();
        if diff > max_diff {
            max_diff = diff;
        }
        assert!(
            diff < 1e-6,
            "save/load roundtrip drift at index {i}: {a} vs {b} (diff={diff})"
        );
    }

    assert!(
        flat2.iter().all(|x| x.is_finite()),
        "reloaded model logits contain non-finite values"
    );

    eprintln!("Roundtrip max abs diff: {max_diff:.2e} (threshold 1e-6)");
}
