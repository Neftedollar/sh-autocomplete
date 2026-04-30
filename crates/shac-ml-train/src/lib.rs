//! Maintainer pipeline for shac's ML next-command predictor.
//!
//! Modules `model` and `tokenizer` are designed to be reused by the
//! main shac runtime crate via `default-features = false, features = ["model-only"]`.

#[cfg(feature = "full")]
pub mod data;
#[cfg(feature = "full")]
pub mod personas;
// `qwen` is intentionally NOT gated: the trait, GenerationConfig, and MockQwen
// must be visible without `full` so that pipeline smoke tests (T13) can use
// MockQwen. Only the `Qwen` struct itself is gated inside the module.
pub mod qwen;
#[cfg(feature = "full")]
pub mod scrub;

pub mod model;
pub mod tokenizer;
