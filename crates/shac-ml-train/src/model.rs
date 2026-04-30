//! Tiny student model: 4-layer decoder-only Transformer over a 2k-token vocab.
//! Built with burn primitives so the same `Module` is used at training time
//! (with `Autodiff<NdArray>`) and at runtime (plain `NdArray`).
//!
//! # Version note (burn 0.21.0-pre.4)
//! - `TransformerEncoderConfig::new(d_model, d_ff, n_heads, n_layers)` — note arg order
//!   (plan had hidden_dim, intermediate_dim, n_heads, n_layers but burn uses d_model, d_ff).
//! - `Tensor::arange(range, options)` — `options` accepts `&B::Device` via `From<&Device<B>>`.
//! - `.repeat_dim(dim, times)` — exists on base tensor (not `.repeat(&[...])`).
//! - `.squeeze_dim::<D2>(dim)` — const-generic form required (not `.squeeze(dim)`).
//! - `.slice([...])` — array of `Range<usize>` works as in plan.
//! - `Param::val()` — needed to get inner tensor from `Embedding.weight: Param<Tensor<B,2>>`.

use burn::config::Config;
use burn::module::Module;
use burn::nn::transformer::{TransformerEncoder, TransformerEncoderConfig, TransformerEncoderInput};
use burn::nn::{Embedding, EmbeddingConfig, LayerNorm, LayerNormConfig, Linear, LinearConfig};
use burn::tensor::backend::Backend;
use burn::tensor::{Int, Tensor};

/// Configuration for the student model.
#[derive(Config, Debug)]
pub struct StudentModelConfig {
    /// Vocabulary size. Default: 2000
    #[config(default = 2000)]
    pub vocab_size: usize,
    /// Maximum context length (sequence length). Default: 16
    #[config(default = 16)]
    pub context_len: usize,
    /// Number of Transformer encoder layers. Default: 4
    #[config(default = 4)]
    pub n_layers: usize,
    /// Number of attention heads. Default: 4
    #[config(default = 4)]
    pub n_heads: usize,
    /// Hidden (model) dimension. Default: 64
    #[config(default = 64)]
    pub hidden_dim: usize,
    /// Feed-forward intermediate dimension. Default: 128
    #[config(default = 128)]
    pub intermediate_dim: usize,
    /// Dropout rate. Default: 0.1
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

/// 4-layer decoder-only mini-Transformer student model.
///
/// Input:  `[batch, context_len]` Int token ids
/// Output: `[batch, vocab_size]` logits (last-position only)
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
    /// Initialise a `StudentModel` on the given device.
    pub fn init<B: Backend>(&self, device: &B::Device) -> StudentModel<B> {
        let token_embedding = EmbeddingConfig::new(self.vocab_size, self.hidden_dim).init(device);
        let position_embedding =
            EmbeddingConfig::new(self.context_len, self.hidden_dim).init(device);
        // burn 0.21.0-pre.4: TransformerEncoderConfig::new(d_model, d_ff, n_heads, n_layers)
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
    ///
    /// # Arguments
    /// * `input` — `[batch, context_len]` int tensor of token ids (0-indexed)
    ///
    /// # Returns
    /// `[batch, vocab_size]` logit tensor for the **last** context position.
    pub fn forward(&self, input: Tensor<B, 2, Int>) -> Tensor<B, 2> {
        let [batch, ctx_len] = input.dims();
        debug_assert_eq!(ctx_len, self.context_len);

        // Token embeddings → [batch, ctx_len, hidden_dim]
        let token_embed = self.token_embedding.forward(input);
        let device = token_embed.device();

        // Position ids 0..ctx_len, broadcast to [batch, ctx_len]
        // burn 0.21.0-pre.4: arange(range, &device), repeat_dim(dim, times)
        let positions: Tensor<B, 2, Int> = Tensor::arange(0..ctx_len as i64, &device)
            .reshape([1, ctx_len])
            .repeat_dim(0, batch);
        let pos_embed = self.position_embedding.forward(positions);

        let hidden = token_embed + pos_embed;

        // Transformer encoder: [batch, ctx_len, hidden_dim] → same shape
        let encoded = self
            .encoder
            .forward(TransformerEncoderInput::new(hidden));

        // Extract last position: [batch, 1, hidden_dim]
        // burn 0.21.0-pre.4: Param::val() needed to get tensor from Embedding.weight
        let hidden_dim = self.position_embedding.weight.val().dims()[1];
        // .slice takes array of Range<usize>; returns [batch, 1, hidden_dim]
        // .squeeze_dim::<D2>(dim) — const-generic output rank required
        let last: Tensor<B, 2> = encoded
            .slice([0..batch, (ctx_len - 1)..ctx_len, 0..hidden_dim])
            .squeeze_dim::<2>(1);

        let normed = self.norm.forward(last);
        self.head.forward(normed)
    }

    /// Vocabulary size this model was configured with.
    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    /// Context length (sequence length) this model was configured with.
    pub fn context_len(&self) -> usize {
        self.context_len
    }
}
