use burn::backend::NdArray;
use burn::tensor::{Int, Tensor, TensorData};
use shac_ml_train::model::{StudentModel, StudentModelConfig};

type B = NdArray<f32>;

#[test]
fn forward_pass_returns_correct_shape() {
    let device = Default::default();
    let cfg = StudentModelConfig::default();
    let model: StudentModel<B> = cfg.init(&device);

    let input_ids: Vec<i64> = vec![
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    let input: Tensor<B, 2, Int> = Tensor::from_data(TensorData::new(input_ids, [2, 16]), &device);

    let logits = model.forward(input);
    let dims = logits.dims();
    assert_eq!(
        dims,
        [2, 2000],
        "expected [batch=2, vocab_size=2000], got {:?}",
        dims
    );

    let data = logits.into_data();
    let flat: Vec<f32> = data.to_vec().unwrap();
    assert!(
        flat.iter().all(|x| x.is_finite()),
        "logits contained non-finite value"
    );
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
