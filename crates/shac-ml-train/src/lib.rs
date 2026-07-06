//! Maintainer pipeline for shac's ML next-command predictor.
//!
//! Modules `model` and `tokenizer` are designed to be reused by the
//! main shac runtime crate via `default-features = false, features = ["model-only"]`.

// `data` and `personas` are gated on `scrub` rather than `full`: neither pulls
// in mistralrs/burn-autodiff (just anyhow/serde/toml), and the scrub bin plus
// the personas/pipeline-smoke tests all need them under the lightweight CI
// lane (`--no-default-features --features "model-only scrub"`). `full`
// includes `scrub`, so nothing changes for local default builds.
#[cfg(feature = "scrub")]
pub mod data;
#[cfg(feature = "scrub")]
pub mod personas;
// `qwen` is intentionally NOT gated: the trait, GenerationConfig, and MockQwen
// must be visible without `full` so that pipeline smoke tests (T13) can use
// MockQwen. Only the `Qwen` struct itself is gated inside the module.
pub mod qwen;
#[cfg(feature = "scrub")]
pub mod scrub;

pub mod model;
pub mod tokenizer;
