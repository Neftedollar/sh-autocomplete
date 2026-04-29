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
