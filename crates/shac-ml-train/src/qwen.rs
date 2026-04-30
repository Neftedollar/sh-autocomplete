//! Qwen model wrapper for the shac ML training pipeline.
//!
//! # Feature gating
//!
//! - [`GenerationConfig`], [`QwenLike`], and [`MockQwen`] are always compiled
//!   (no feature gate). Tests and pipeline smoke runs can use `MockQwen` without
//!   pulling in the full mistralrs stack.
//! - [`Qwen`] is compiled only with `--features full`. It wraps a real mistralrs
//!   model loaded from Hugging Face.

use anyhow::Result;

// ---------------------------------------------------------------------------
// Public always-on types
// ---------------------------------------------------------------------------

/// Configuration controlling text generation behaviour.
#[derive(Debug, Clone)]
pub struct GenerationConfig {
    /// Maximum number of tokens to generate.
    pub max_tokens: usize,
    /// Sampling temperature (0.0 = greedy).
    pub temperature: f32,
    /// Nucleus-sampling top-p threshold.
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

/// Trait implemented by both the real [`Qwen`] model and [`MockQwen`].
///
/// Methods are synchronous so callers don't need an async runtime. The real
/// implementation wraps async mistralrs calls with a dedicated tokio runtime.
pub trait QwenLike: Send + Sync {
    /// Generate a completion given a system and user prompt.
    fn generate(&self, system: &str, user: &str, cfg: &GenerationConfig) -> Result<String>;

    /// Return the top-`top_k` next-token distribution for `prompt`.
    ///
    /// Each entry is `(token_text, probability)` with probabilities summing
    /// approximately to 1.0. Token text may be the raw byte sequence rendered
    /// as a lossy UTF-8 string.
    ///
    /// # Implementation notes (Path A — native logprobs)
    ///
    /// We use `RequestBuilder::return_logprobs(true)` with
    /// `set_sampler_topn_logprobs(top_k)` and `max_len = 1`. mistralrs 0.8
    /// populates `Choice.logprobs.content[0].top_logprobs` with the top-k
    /// entries for the single generated token. We read `TopLogprob.bytes`
    /// (decoded text) and convert `logprob` (natural log probability) to
    /// probability via `exp()`.
    ///
    /// This gives exact probabilities from the model's softmax, not a
    /// sampling approximation.
    fn next_token_distribution(&self, prompt: &str, top_k: usize) -> Result<Vec<(String, f32)>>;
}

// ---------------------------------------------------------------------------
// MockQwen — always compiled, no feature gate
// ---------------------------------------------------------------------------

/// A test double for [`QwenLike`] that returns canned values.
///
/// Used in pipeline smoke tests (T13) which run without the `full` feature.
pub struct MockQwen {
    /// Returned verbatim by [`QwenLike::generate`].
    pub canned_completion: String,
    /// Returned verbatim by [`QwenLike::next_token_distribution`].
    pub canned_distribution: Vec<(String, f32)>,
}

impl Default for MockQwen {
    fn default() -> Self {
        Self {
            canned_completion: "mock completion".to_string(),
            canned_distribution: vec![
                ("▁the".to_string(), 0.4),
                ("▁a".to_string(), 0.3),
                ("▁is".to_string(), 0.2),
                ("▁of".to_string(), 0.1),
            ],
        }
    }
}

impl QwenLike for MockQwen {
    fn generate(&self, _system: &str, _user: &str, _cfg: &GenerationConfig) -> Result<String> {
        Ok(self.canned_completion.clone())
    }

    fn next_token_distribution(
        &self,
        _prompt: &str,
        _top_k: usize,
    ) -> Result<Vec<(String, f32)>> {
        Ok(self.canned_distribution.clone())
    }
}

// ---------------------------------------------------------------------------
// Qwen — real mistralrs wrapper, only compiled with feature "full"
// ---------------------------------------------------------------------------

#[cfg(feature = "full")]
mod full_impl {
    use super::{GenerationConfig, QwenLike, Result};

    use anyhow::anyhow;
    use mistralrs::{
        IsqBits, ModelBuilder, RequestBuilder, SamplingParams, TextMessageRole, TextMessages,
    };
    use std::sync::Arc;
    use tokio::runtime::Runtime;

    /// Hugging Face model repo to load. Qwen3-0.6B is the smallest Qwen3
    /// variant and loads quickly for synthetic data generation and distillation.
    /// Adjust to `Qwen/Qwen2.5-0.5B-Instruct` if preferred.
    const MODEL_ID: &str = "Qwen/Qwen3-0.6B";

    /// Real Qwen model backed by mistralrs 0.8.
    ///
    /// # Logprobs strategy (Path A — native logprobs)
    ///
    /// mistralrs 0.8 exposes per-token log-probabilities natively via
    /// `RequestBuilder::return_logprobs(true)` and
    /// `set_sampler_topn_logprobs(k)`. We request `max_len = 1` to generate
    /// exactly one token and read `Choice.logprobs.content[0].top_logprobs`.
    /// Each `TopLogprob` carries a pre-softmax log-probability (`logprob`) and
    /// the decoded token string (`bytes`). We convert to probabilities with
    /// `exp()`. No sampling approximation is involved.
    pub struct Qwen {
        model: mistralrs::Model,
        rt: Arc<Runtime>,
    }

    impl Qwen {
        /// Load the model, downloading weights from Hugging Face if necessary.
        ///
        /// Uses `with_auto_isq(IsqBits::Four)` for in-situ 4-bit quantization
        /// so the model fits comfortably in memory on both Mac (Metal CPU
        /// fallback) and Linux CPU.
        pub async fn load() -> Result<Self> {
            let model = ModelBuilder::new(MODEL_ID)
                .with_auto_isq(IsqBits::Four)
                .build()
                .await
                .map_err(|e| anyhow!("Failed to load Qwen model: {e}"))?;

            // Capture (or create) the current tokio runtime for the blocking
            // methods. `Runtime::new()` here would create a *second* runtime
            // nested inside the outer one; instead we get the current handle
            // and build a new single-threaded runtime for blocking dispatch.
            //
            // NOTE: trait methods are synchronous, so they must block the
            // calling thread. We store a dedicated `Runtime` for that purpose.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow!("Failed to build runtime: {e}"))?;

            Ok(Self {
                model,
                rt: Arc::new(rt),
            })
        }
    }

    impl QwenLike for Qwen {
        fn generate(&self, system: &str, user: &str, cfg: &GenerationConfig) -> Result<String> {
            let messages = TextMessages::new()
                .add_message(TextMessageRole::System, system)
                .add_message(TextMessageRole::User, user);

            let request = RequestBuilder::from(messages)
                .set_sampling(SamplingParams {
                    temperature: Some(cfg.temperature as f64),
                    top_p: Some(cfg.top_p as f64),
                    max_len: Some(cfg.max_tokens),
                    ..SamplingParams::neutral()
                });

            let response = self
                .rt
                .block_on(self.model.send_chat_request(request))
                .map_err(|e| anyhow!("Inference error: {e}"))?;

            response
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.message.content)
                .ok_or_else(|| anyhow!("Model returned no content"))
        }

        fn next_token_distribution(
            &self,
            prompt: &str,
            top_k: usize,
        ) -> Result<Vec<(String, f32)>> {
            // Use a plain user message with no system prompt; the prompt is
            // treated as the full context whose next token distribution we want.
            let messages = TextMessages::new()
                .add_message(TextMessageRole::User, prompt);

            let request = RequestBuilder::from(messages)
                // Request exactly 1 token so we capture the first-token distribution.
                .set_sampling(SamplingParams {
                    temperature: Some(1.0),
                    max_len: Some(1),
                    top_n_logprobs: top_k,
                    ..SamplingParams::neutral()
                })
                // Enable logprobs so the response carries top-k entries.
                .return_logprobs(true);

            let response = self
                .rt
                .block_on(self.model.send_chat_request(request))
                .map_err(|e| anyhow!("Inference error: {e}"))?;

            let choice = response
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("Model returned no choices"))?;

            let logprobs_content = choice
                .logprobs
                .and_then(|lp| lp.content)
                .ok_or_else(|| anyhow!("Model returned no logprobs — was return_logprobs set?"))?;

            // The first (and only, given max_len=1) ResponseLogprob holds
            // top_logprobs for the generated token position.
            let first = logprobs_content
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("Empty logprobs content"))?;

            // Convert top_logprobs entries to (token_text, probability).
            // `TopLogprob.bytes` is the decoded token string; fall back to
            // the raw token field formatted as a string if bytes is None.
            let mut dist: Vec<(String, f32)> = first
                .top_logprobs
                .into_iter()
                .map(|tlp| {
                    let tok_text = tlp
                        .bytes
                        .unwrap_or_else(|| format!("<token:{}>", tlp.token));
                    let prob = tlp.logprob.exp();
                    (tok_text, prob)
                })
                .collect();

            // Sort descending by probability for convenience.
            dist.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            Ok(dist)
        }
    }
}

#[cfg(feature = "full")]
pub use full_impl::Qwen;
