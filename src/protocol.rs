use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const TRUST_INTERACTIVE: &str = "interactive";
pub const TRUST_SCRIPT_LIKE: &str = "script_like";
pub const TRUST_UNKNOWN: &str = "unknown";
pub const TRUST_LEGACY: &str = "legacy";

pub const PROVENANCE_TYPED_MANUAL: &str = "typed_manual";
pub const PROVENANCE_ACCEPTED_COMPLETION: &str = "accepted_completion";
pub const PROVENANCE_PASTED: &str = "pasted";
pub const PROVENANCE_HISTORY_EXPANSION: &str = "history_expansion";
pub const PROVENANCE_UNKNOWN: &str = "unknown";
pub const PROVENANCE_LEGACY: &str = "legacy";

pub const PROVENANCE_SOURCE_ZSH_BRACKETED_PASTE: &str = "zsh_bracketed_paste";
pub const PROVENANCE_SOURCE_ZSH_PASTE_HEURISTIC: &str = "zsh_paste_heuristic";
pub const PROVENANCE_SOURCE_UNKNOWN: &str = "unknown";

pub const PROVENANCE_CONFIDENCE_EXACT: &str = "exact";
pub const PROVENANCE_CONFIDENCE_HEURISTIC: &str = "heuristic";
pub const PROVENANCE_CONFIDENCE_UNKNOWN: &str = "unknown";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub tty: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryHint {
    pub prev_command: Option<String>,
    #[serde(default)]
    pub runtime_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub shell: String,
    pub line: String,
    pub cursor: usize,
    pub cwd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub session: SessionInfo,
    pub history_hint: HistoryHint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionMeta {
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub item_key: String,
    pub insert_text: String,
    pub display: String,
    pub kind: String,
    pub score: f64,
    pub source: String,
    pub meta: CompletionMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub request_id: Option<i64>,
    pub items: Vec<CompletionItem>,
    pub mode: String,
    pub fallback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainFeature {
    pub name: String,
    pub value: f64,
    pub weight: f64,
    pub contribution: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainItem {
    pub display: String,
    pub score: f64,
    pub source: String,
    pub features: Vec<ExplainFeature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainResponse {
    pub query: String,
    pub items: Vec<ExplainItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordCommandRequest {
    pub command: String,
    pub cwd: String,
    pub shell: Option<String>,
    pub trust: Option<String>,
    pub provenance: Option<String>,
    pub provenance_source: Option<String>,
    pub provenance_confidence: Option<String>,
    pub origin: Option<String>,
    pub tty_present: Option<bool>,
    pub exit_status: Option<i32>,
    pub accepted_request_id: Option<i64>,
    pub accepted_item_key: Option<String>,
    pub accepted_rank: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub commands: i64,
    pub docs: i64,
    pub history_events: i64,
    pub transitions: i64,
    pub project_profiles: i64,
    pub dir_cache_entries: i64,
    pub completion_requests: i64,
    pub completion_items: i64,
    pub accepted_completions: i64,
    pub legacy_history_events: i64,
    pub interactive_history_events: i64,
    pub script_like_history_events: i64,
    pub clean_completion_requests: i64,
    pub legacy_completion_requests: i64,
    pub accepted_clean_completions: i64,
    pub pasted_history_events: i64,
    pub exact_pasted_history_events: i64,
    pub heuristic_pasted_history_events: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStatusResponse {
    pub history_events: i64,
    pub legacy_history_events: i64,
    pub interactive_history_events: i64,
    pub script_like_history_events: i64,
    pub completion_requests: i64,
    pub clean_completion_requests: i64,
    pub legacy_completion_requests: i64,
    pub accepted_clean_completions: i64,
    pub pasted_history_events: i64,
    pub exact_pasted_history_events: i64,
    pub heuristic_pasted_history_events: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEvent {
    pub id: i64,
    pub ts: i64,
    pub cwd: String,
    pub command: String,
    pub shell: Option<String>,
    pub trust: String,
    pub provenance: String,
    pub provenance_source: String,
    pub provenance_confidence: String,
    pub origin: String,
    pub tty_present: bool,
}
