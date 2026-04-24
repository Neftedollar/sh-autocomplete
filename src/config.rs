use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureFlags {
    pub history_ranking: bool,
    pub doc_search: bool,
    pub project_context: bool,
    pub ml_rerank: bool,
    pub inline_zsh: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            history_ranking: true,
            doc_search: true,
            project_context: true,
            ml_rerank: false,
            inline_zsh: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingWeights {
    pub prefix_score: f64,
    pub fuzzy_score: f64,
    pub global_usage_score: f64,
    pub cwd_usage_score: f64,
    pub recency_score: f64,
    pub transition_score: f64,
    pub project_affinity_score: f64,
    pub position_score: f64,
    pub source_prior: f64,
    pub doc_match_score: f64,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            prefix_score: 0.32,
            fuzzy_score: 0.18,
            global_usage_score: 0.10,
            cwd_usage_score: 0.08,
            recency_score: 0.08,
            transition_score: 0.08,
            project_affinity_score: 0.07,
            position_score: 0.04,
            source_prior: 0.03,
            doc_match_score: 0.02,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub zsh: ZshUiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ZshUiConfig {
    pub menu_detail: String,
    pub show_kind: bool,
    pub show_source: bool,
    pub show_description: bool,
    pub max_description_width: usize,
    pub max_items: usize,
}

impl Default for ZshUiConfig {
    fn default() -> Self {
        Self {
            menu_detail: "compact".to_string(),
            show_kind: false,
            show_source: false,
            show_description: true,
            max_description_width: 72,
            max_items: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub enabled: bool,
    pub features: FeatureFlags,
    pub ranking: RankingWeights,
    pub ui: UiConfig,
    pub max_results: usize,
    pub daemon_timeout_ms: u64,
    pub ml_model_file: Option<String>,
    pub ml_blend_weight: f64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            features: FeatureFlags::default(),
            ranking: RankingWeights::default(),
            ui: UiConfig::default(),
            max_results: 12,
            daemon_timeout_ms: 150,
            ml_model_file: None,
            ml_blend_weight: 0.35,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub config_file: PathBuf,
    pub db_file: PathBuf,
    pub socket_file: PathBuf,
    pub pid_file: PathBuf,
    pub shell_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home_dir = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let config_dir = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".config"))
            .join("shac");
        let data_dir = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".local/share"))
            .join("shac");
        let state_dir = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".local/state"))
            .join("shac");
        let shell_dir = config_dir.join("shell");
        Ok(Self {
            config_file: config_dir.join("config.toml"),
            db_file: data_dir.join("shac.db"),
            socket_file: state_dir.join("shacd.sock"),
            pid_file: state_dir.join("shacd.pid"),
            config_dir,
            data_dir,
            state_dir,
            shell_dir,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir).context("create config dir")?;
        fs::create_dir_all(&self.data_dir).context("create data dir")?;
        fs::create_dir_all(&self.state_dir).context("create state dir")?;
        fs::create_dir_all(&self.shell_dir).context("create shell dir")?;
        Ok(())
    }
}

impl AppConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        if !paths.config_file.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&paths.config_file).context("read config")?;
        toml::from_str(&raw).context("parse config")
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure()?;
        let raw = toml::to_string_pretty(self).context("serialize config")?;
        fs::write(&paths.config_file, raw).context("write config")?;
        Ok(())
    }

    pub fn get_key(&self, key: &str) -> Option<String> {
        match key {
            "enabled" => Some(self.enabled.to_string()),
            "features.history_ranking" => Some(self.features.history_ranking.to_string()),
            "features.doc_search" => Some(self.features.doc_search.to_string()),
            "features.project_context" => Some(self.features.project_context.to_string()),
            "features.ml_rerank" => Some(self.features.ml_rerank.to_string()),
            "features.inline_zsh" => Some(self.features.inline_zsh.to_string()),
            "max_results" => Some(self.max_results.to_string()),
            "daemon_timeout_ms" => Some(self.daemon_timeout_ms.to_string()),
            "ml_model_file" => Some(self.ml_model_file.clone().unwrap_or_default()),
            "ml_blend_weight" => Some(self.ml_blend_weight.to_string()),
            "ranking.prefix_score" => Some(self.ranking.prefix_score.to_string()),
            "ranking.fuzzy_score" => Some(self.ranking.fuzzy_score.to_string()),
            "ranking.global_usage_score" => Some(self.ranking.global_usage_score.to_string()),
            "ranking.cwd_usage_score" => Some(self.ranking.cwd_usage_score.to_string()),
            "ranking.recency_score" => Some(self.ranking.recency_score.to_string()),
            "ranking.transition_score" => Some(self.ranking.transition_score.to_string()),
            "ranking.project_affinity_score" => {
                Some(self.ranking.project_affinity_score.to_string())
            }
            "ranking.position_score" => Some(self.ranking.position_score.to_string()),
            "ranking.source_prior" => Some(self.ranking.source_prior.to_string()),
            "ranking.doc_match_score" => Some(self.ranking.doc_match_score.to_string()),
            "ui.zsh.menu_detail" => Some(self.ui.zsh.menu_detail.clone()),
            "ui.zsh.show_kind" => Some(self.ui.zsh.show_kind.to_string()),
            "ui.zsh.show_source" => Some(self.ui.zsh.show_source.to_string()),
            "ui.zsh.show_description" => Some(self.ui.zsh.show_description.to_string()),
            "ui.zsh.max_description_width" => Some(self.ui.zsh.max_description_width.to_string()),
            "ui.zsh.max_items" => Some(self.ui.zsh.max_items.to_string()),
            _ => None,
        }
    }

    pub fn set_key(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "enabled" => self.enabled = parse_bool(value)?,
            "features.history_ranking" => self.features.history_ranking = parse_bool(value)?,
            "features.doc_search" => self.features.doc_search = parse_bool(value)?,
            "features.project_context" => self.features.project_context = parse_bool(value)?,
            "features.ml_rerank" => self.features.ml_rerank = parse_bool(value)?,
            "features.inline_zsh" => self.features.inline_zsh = parse_bool(value)?,
            "max_results" => self.max_results = value.parse()?,
            "daemon_timeout_ms" => self.daemon_timeout_ms = value.parse()?,
            "ml_model_file" => {
                self.ml_model_file = if value.is_empty() || value == "none" {
                    None
                } else {
                    Some(value.to_string())
                }
            }
            "ml_blend_weight" => self.ml_blend_weight = value.parse()?,
            "ranking.prefix_score" => self.ranking.prefix_score = value.parse()?,
            "ranking.fuzzy_score" => self.ranking.fuzzy_score = value.parse()?,
            "ranking.global_usage_score" => self.ranking.global_usage_score = value.parse()?,
            "ranking.cwd_usage_score" => self.ranking.cwd_usage_score = value.parse()?,
            "ranking.recency_score" => self.ranking.recency_score = value.parse()?,
            "ranking.transition_score" => self.ranking.transition_score = value.parse()?,
            "ranking.project_affinity_score" => {
                self.ranking.project_affinity_score = value.parse()?
            }
            "ranking.position_score" => self.ranking.position_score = value.parse()?,
            "ranking.source_prior" => self.ranking.source_prior = value.parse()?,
            "ranking.doc_match_score" => self.ranking.doc_match_score = value.parse()?,
            "ui.zsh.menu_detail" => self.ui.zsh.menu_detail = parse_menu_detail(value)?,
            "ui.zsh.show_kind" => self.ui.zsh.show_kind = parse_bool(value)?,
            "ui.zsh.show_source" => self.ui.zsh.show_source = parse_bool(value)?,
            "ui.zsh.show_description" => self.ui.zsh.show_description = parse_bool(value)?,
            "ui.zsh.max_description_width" => self.ui.zsh.max_description_width = value.parse()?,
            "ui.zsh.max_items" => self.ui.zsh.max_items = value.parse()?,
            _ => anyhow::bail!("unsupported config key: {key}"),
        }
        Ok(())
    }
}

fn parse_menu_detail(value: &str) -> Result<String> {
    match value {
        "minimal" | "compact" | "verbose" | "debug" => Ok(value.to_string()),
        _ => anyhow::bail!("expected one of minimal|compact|verbose|debug, got {value}"),
    }
}

fn parse_bool(value: &str) -> Result<bool> {
    match value {
        "true" | "1" | "on" => Ok(true),
        "false" | "0" | "off" => Ok(false),
        _ => anyhow::bail!("expected boolean-like value, got {value}"),
    }
}
