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
