use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::{AppConfig, AppPaths};
use crate::context::{self, ParsedContext, TokenRole};
use crate::db::{AppDb, LoggedCompletionItem, StoredDoc};
use crate::indexer;
use crate::ml::{train_model, MlModel, TrainOptions, TrainingSample};
use crate::protocol::{
    CompletionItem, CompletionMeta, CompletionRequest, CompletionResponse, ExplainFeature,
    ExplainItem, ExplainResponse, MigrationStatusResponse, RecentEvent, RecordCommandRequest,
    StatsResponse, TRUST_INTERACTIVE, TRUST_UNKNOWN,
};

#[derive(Debug, Clone)]
struct Candidate {
    insert_text: String,
    display: String,
    kind: String,
    source: String,
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct FeatureBreakdown {
    name: &'static str,
    value: f64,
    weight: f64,
}

#[derive(Debug, Clone)]
struct RankedCandidate {
    item: CompletionItem,
    item_key: String,
    features: Vec<FeatureBreakdown>,
}

pub struct Engine {
    config: AppConfig,
    db: AppDb,
    ml_model: Option<MlModel>,
}

impl Engine {
    pub fn new(paths: &AppPaths) -> Result<Self> {
        paths.ensure()?;
        let config = AppConfig::load(paths)?;
        let db = AppDb::open(&paths.db_file)?;
        let ml_model = config
            .ml_model_file
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .map(|path| MlModel::load(&path))
            .transpose()?;
        Ok(Self {
            config,
            db,
            ml_model,
        })
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn db(&self) -> &AppDb {
        &self.db
    }

    pub fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse> {
        let parsed = context::parse(&req.line, req.cursor, Path::new(&req.cwd));
        let mut candidates = self.collect_candidates(&req, &parsed)?;
        if candidates.is_empty() {
            return Ok(CompletionResponse {
                request_id: None,
                items: Vec::new(),
                mode: "replace_token".to_string(),
                fallback: true,
            });
        }

        let prev_command = req
            .history_hint
            .prev_command
            .clone()
            .or_else(|| self.db.latest_command().ok().flatten());

        let max_path_rank = self.db.paths_index_max_rank().unwrap_or(0.0);
        let mut items: Vec<RankedCandidate> = candidates
            .drain(..)
            .map(|candidate| {
                self.score_candidate(
                    &candidate,
                    &parsed,
                    &req.cwd,
                    prev_command.as_deref(),
                    max_path_rank,
                )
            })
            .collect();

        items.sort_by(|left, right| cmp_score(right.item.score, left.item.score));
        items.truncate(self.config.max_results);
        let request_id = self.db.record_completion_request(
            &req.shell,
            &req.cwd,
            &req.line,
            req.cursor,
            &parsed.active_token,
            prev_command.as_deref(),
            if req.session.tty.is_some() {
                TRUST_INTERACTIVE
            } else {
                TRUST_UNKNOWN
            },
            &items
                .iter()
                .enumerate()
                .map(|(rank, item)| LoggedCompletionItem {
                    rank,
                    item_key: item.item_key.clone(),
                    insert_text: item.item.insert_text.clone(),
                    display: item.item.display.clone(),
                    kind: item.item.kind.clone(),
                    source: item.item.source.clone(),
                    score: item.item.score,
                    feature_json: serde_json::to_string(&feature_values(&item.features))
                        .unwrap_or_else(|_| "{}".to_string()),
                })
                .collect::<Vec<_>>(),
        )?;

        Ok(CompletionResponse {
            request_id: Some(request_id),
            items: items.into_iter().map(|item| item.item).collect(),
            mode: "replace_token".to_string(),
            fallback: false,
        })
    }

    pub fn explain(&self, req: CompletionRequest) -> Result<ExplainResponse> {
        let parsed = context::parse(&req.line, req.cursor, Path::new(&req.cwd));
        let prev_command = req
            .history_hint
            .prev_command
            .clone()
            .or_else(|| self.db.latest_command().ok().flatten());
        let max_path_rank = self.db.paths_index_max_rank().unwrap_or(0.0);
        let mut explained = self
            .collect_candidates(&req, &parsed)?
            .into_iter()
            .map(|candidate| {
                self.score_candidate(
                    &candidate,
                    &parsed,
                    &req.cwd,
                    prev_command.as_deref(),
                    max_path_rank,
                )
            })
            .collect::<Vec<_>>();
        explained.sort_by(|left, right| cmp_score(right.item.score, left.item.score));
        explained.truncate(self.config.max_results);

        Ok(ExplainResponse {
            query: parsed.active_token,
            items: explained
                .into_iter()
                .map(|ranked| ExplainItem {
                    display: ranked.item.display,
                    score: ranked.item.score,
                    source: ranked.item.source,
                    features: ranked
                        .features
                        .into_iter()
                        .map(|feature| ExplainFeature {
                            name: feature.name.to_string(),
                            value: feature.value,
                            weight: feature.weight,
                            contribution: feature.value * feature.weight,
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    pub fn record_command(&self, request: RecordCommandRequest) -> Result<()> {
        self.db.record_history(&request)?;
        Ok(())
    }

    pub fn reindex(&self, path_env: Option<&str>) -> Result<usize> {
        indexer::reindex_path_commands(&self.db, path_env)
    }

    pub fn stats(&self) -> Result<StatsResponse> {
        self.db.stats()
    }

    pub fn migration_status(&self) -> Result<MigrationStatusResponse> {
        self.db.migration_status()
    }

    pub fn training_samples(&self, limit: usize) -> Result<Vec<TrainingSample>> {
        self.db.training_samples(limit)
    }

    pub fn reset_personalization(&self) -> Result<()> {
        self.db.reset_personalization()
    }

    pub fn recent_events(&self, limit: usize) -> Result<Vec<RecentEvent>> {
        self.db.recent_events(limit)
    }
}

const ML_ACTIVATION_THRESHOLD: i64 = 50;
const ML_TRAIN_LIMIT: usize = 10_000;

/// Checks whether enough accepted completions have been collected to train and
/// auto-enable the ML model. Called once at daemon startup. Returns true when
/// the model was just activated for the first time.
pub fn maybe_auto_train(paths: &AppPaths) -> Result<bool> {
    let mut config = AppConfig::load(paths)?;
    if config.features.ml_rerank {
        return Ok(false);
    }
    let db = AppDb::open(&paths.db_file)?;
    let stats = db.stats()?;
    if stats.accepted_clean_completions < ML_ACTIVATION_THRESHOLD {
        return Ok(false);
    }
    let model_path = paths.data_dir.join("model.json");
    if !model_path.exists() {
        let samples = db.training_samples(ML_TRAIN_LIMIT)?;
        if samples.is_empty() {
            return Ok(false);
        }
        let model = train_model(&samples, &TrainOptions::default());
        model.save(&model_path)?;
    }
    config.ml_model_file = Some(model_path.to_string_lossy().into_owned());
    config.features.ml_rerank = true;
    config.save(paths)?;
    Ok(true)
}

impl Engine {
    fn collect_candidates(
        &self,
        req: &CompletionRequest,
        parsed: &ParsedContext,
    ) -> Result<Vec<Candidate>> {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        let active = parsed.active_token.as_str();
        let cd_empty_path_context = is_cd_path_context(parsed) && active.is_empty();

        if matches!(parsed.role, TokenRole::Command) {
            for (name, kind) in self.db.list_commands()? {
                if name.starts_with(active) || fuzzy_match_score(active, &name) > 0.0 {
                    push_candidate(
                        &mut candidates,
                        &mut seen,
                        Candidate {
                            insert_text: name.clone(),
                            display: name,
                            kind,
                            source: "path_index".to_string(),
                            description: None,
                        },
                    );
                }
            }
        }

        if self.config.features.history_ranking && !cd_empty_path_context {
            for command in &req.history_hint.runtime_commands {
                if let Some((insert_text, display, kind)) =
                    project_history_candidate(command, parsed)
                {
                    if insert_text.starts_with(active)
                        || fuzzy_match_score(active, &insert_text) > 0.0
                        || active.is_empty()
                    {
                        push_candidate(
                            &mut candidates,
                            &mut seen,
                            Candidate {
                                insert_text,
                                display,
                                kind,
                                source: "runtime_history".to_string(),
                                description: Some("Provided by current shell context".to_string()),
                            },
                        );
                    }
                }
            }

            let history_prefix = contextual_history_prefix(parsed);
            for entry in
                self.db
                    .frequent_history(&history_prefix, &req.cwd, self.config.max_results * 2)?
            {
                let history_candidate = project_history_candidate(&entry.command, parsed);
                if let Some((insert_text, display, kind)) = history_candidate {
                    push_candidate(
                        &mut candidates,
                        &mut seen,
                        Candidate {
                            insert_text,
                            display,
                            kind,
                            source: "history".to_string(),
                            description: Some("Previously executed command".to_string()),
                        },
                    );
                }
            }
        }

        if let Some(command) = parsed.command.as_deref() {
            if is_python_module_position(parsed) {
                for module in python_module_candidates() {
                    if module.name.starts_with(active)
                        || fuzzy_match_score(active, module.name) > 0.0
                        || active.is_empty()
                    {
                        push_candidate(
                            &mut candidates,
                            &mut seen,
                            Candidate {
                                insert_text: module.name.to_string(),
                                display: module.name.to_string(),
                                kind: "module".to_string(),
                                source: "builtin-index".to_string(),
                                description: Some(module.description.to_string()),
                            },
                        );
                    }
                }
            }

            if self.config.features.doc_search {
                for doc in self.db.docs_for_command(command)? {
                    if should_include_doc(&doc, active, parsed) {
                        push_candidate(
                            &mut candidates,
                            &mut seen,
                            Candidate {
                                insert_text: doc.item_value.clone(),
                                display: doc.item_value,
                                kind: doc.item_type,
                                source: doc.source,
                                description: Some(doc.description),
                            },
                        );
                    }
                }

                if !active.is_empty() {
                    if let Some(query) = sanitize_fts_query(active) {
                        for doc in self.db.search_docs(&query, self.config.max_results)? {
                            if should_include_doc(&doc, active, parsed) {
                                push_candidate(
                                    &mut candidates,
                                    &mut seen,
                                    Candidate {
                                        insert_text: doc.item_value.clone(),
                                        display: doc.item_value,
                                        kind: doc.item_type,
                                        source: "doc_search".to_string(),
                                        description: Some(doc.description),
                                    },
                                );
                            }
                        }
                    }
                }
            }

            self.dispatch_path_like(
                parsed,
                command,
                active,
                &req.cwd,
                &mut candidates,
                &mut seen,
            )?;
        }

        if cd_empty_path_context {
            // Suppress only history sources from polluting empty `cd ` results;
            // allow path_cache and path_jump to flow through so users see
            // local children plus global frecent jumps.
            candidates.retain(|c| c.source != "history" && c.source != "runtime_history");
        }

        if let Some(prev_command) = req
            .history_hint
            .prev_command
            .clone()
            .or_else(|| self.db.latest_command().ok().flatten())
        {
            for transition in self
                .db
                .transitions_from(&prev_command, self.config.max_results)?
            {
                if let Some((insert_text, display, kind)) =
                    project_history_candidate(&transition.next, parsed)
                {
                    if insert_text.starts_with(active)
                        || fuzzy_match_score(active, &insert_text) > 0.0
                        || active.is_empty()
                    {
                        push_candidate(
                            &mut candidates,
                            &mut seen,
                            Candidate {
                                insert_text,
                                display,
                                kind,
                                source: "transition".to_string(),
                                description: Some(format!(
                                    "Frequently used after `{prev_command}`"
                                )),
                            },
                        );
                    }
                } else if transition.next.starts_with(active) || active.is_empty() {
                    push_candidate(
                        &mut candidates,
                        &mut seen,
                        Candidate {
                            insert_text: transition.next.clone(),
                            display: transition.next,
                            kind: if matches!(parsed.role, TokenRole::Command) {
                                "command".to_string()
                            } else {
                                "history".to_string()
                            },
                            source: "transition".to_string(),
                            description: Some(format!("Frequently used after `{prev_command}`")),
                        },
                    );
                }
            }
        }

        Ok(candidates)
    }

    fn dispatch_path_like(
        &self,
        parsed: &ParsedContext,
        command: &str,
        active: &str,
        cwd: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        if matches!(parsed.role, TokenRole::Path)
            || matches!(parsed.prev_token.as_deref(), Some("cd"))
            || command == "cd"
        {
            let cd_like =
                command == "cd" || matches!(parsed.prev_token.as_deref(), Some("cd"));
            self.collect_path_candidates(active, cwd, cd_like, candidates, seen)?;
            // Hybrid-cd: also surface frecent paths from the global index for
            // cd-like contexts so deep directories can be jumped to from anywhere.
            if is_cd_path_context(parsed) {
                self.collect_global_path_candidates(active, cwd, candidates, seen)?;
            }
        }
        Ok(())
    }

    fn collect_global_path_candidates(
        &self,
        active: &str,
        cwd: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let filter = if active.is_empty() {
            None
        } else {
            Some(active)
        };
        let limit = self.config.max_results.saturating_mul(2).max(8);
        let rows = match self.db.top_paths(filter, limit) {
            Ok(rows) => rows,
            Err(_) => return Ok(()),
        };
        if rows.is_empty() {
            return Ok(());
        }

        let cwd_path = PathBuf::from(cwd);
        let home = dirs::home_dir();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for row in rows {
            let path = PathBuf::from(&row.path);
            // Skip paths equal to cwd.
            if path == cwd_path {
                continue;
            }
            // Skip direct children of cwd (already covered by collect_path_candidates).
            if path.parent().map(|p| p == cwd_path).unwrap_or(false) {
                continue;
            }
            // Skip missing paths (cheap stat).
            if !path.exists() {
                continue;
            }

            let abs = path.to_string_lossy().to_string();
            let display_path = shorten_with_home(&abs, home.as_deref());
            let insert_text = display_path.clone();
            let display = format!("\u{2192} {display_path}");
            let description = format_path_jump_description(row.is_git_repo, row.last_visit, now);

            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text,
                    display,
                    kind: "path_jump".to_string(),
                    source: "path_jump".to_string(),
                    description: Some(description),
                },
            );
        }
        Ok(())
    }

    fn collect_path_candidates(
        &self,
        token: &str,
        cwd: &str,
        dirs_only: bool,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let (dir, prefix) = split_path_token(token, cwd);
        let insertion_prefix = path_insertion_prefix(token);
        let dir_string = dir.to_string_lossy().to_string();
        let mtime = dir
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default();

        let entries = if let Some((cached_mtime, entries)) = self.db.get_dir_cache(&dir_string)? {
            if cached_mtime == mtime {
                entries
                    .split('\n')
                    .filter(|item| !item.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            } else {
                let entries = read_dir_entries(&dir)?;
                self.db
                    .upsert_dir_cache(&dir_string, mtime, &entries.join("\n"))?;
                entries
            }
        } else {
            let entries = read_dir_entries(&dir)?;
            self.db
                .upsert_dir_cache(&dir_string, mtime, &entries.join("\n"))?;
            entries
        };

        for entry in entries {
            if entry.starts_with(&prefix) {
                let entry_path = dir.join(&entry);
                let is_dir = entry_path.is_dir();
                if dirs_only && !is_dir {
                    continue;
                }
                let suffix = if is_dir { "/" } else { "" };
                let insert_text = format!("{insertion_prefix}{entry}{suffix}");
                push_candidate(
                    candidates,
                    seen,
                    Candidate {
                        insert_text: insert_text.clone(),
                        display: insert_text,
                        kind: "path".to_string(),
                        source: "path_cache".to_string(),
                        description: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn score_candidate(
        &self,
        candidate: &Candidate,
        parsed: &ParsedContext,
        cwd: &str,
        prev_command: Option<&str>,
        max_path_rank: f64,
    ) -> RankedCandidate {
        let active = parsed.active_token.as_str();
        let history_key = contextual_candidate_key(parsed, candidate);
        let first_word = history_key
            .split_whitespace()
            .next()
            .unwrap_or(history_key.as_str());

        let features = vec![
            feature(
                "prefix_score",
                prefix_score(active, &candidate.insert_text),
                self.config.ranking.prefix_score,
            ),
            feature(
                "fuzzy_score",
                fuzzy_match_score(active, &candidate.insert_text),
                self.config.ranking.fuzzy_score,
            ),
            feature(
                "global_usage_score",
                self.history_usage(&history_key, "").unwrap_or_default(),
                self.config.ranking.global_usage_score,
            ),
            feature(
                "cwd_usage_score",
                self.history_usage(&history_key, cwd).unwrap_or_default(),
                self.config.ranking.cwd_usage_score,
            ),
            feature(
                "recency_score",
                self.recency_score(&history_key).unwrap_or_default(),
                self.config.ranking.recency_score,
            ),
            feature(
                "transition_score",
                prev_command
                    .and_then(|prev| self.transition_score(prev, &history_key).ok())
                    .unwrap_or_default(),
                self.config.ranking.transition_score,
            ),
            feature(
                "project_affinity_score",
                project_affinity_score(&parsed.project_markers, first_word)
                    + self
                        .db
                        .project_tool_count(cwd, first_word)
                        .unwrap_or_default()
                        / 10.0,
                self.config.ranking.project_affinity_score,
            ),
            feature(
                "position_score",
                position_score(parsed, candidate),
                self.config.ranking.position_score,
            ),
            feature(
                "source_prior",
                source_prior(&candidate.source, &candidate.kind),
                self.config.ranking.source_prior,
            ),
            feature(
                "doc_match_score",
                doc_match_score(active, candidate.description.as_deref()),
                self.config.ranking.doc_match_score,
            ),
            feature(
                "path_frecency_score",
                path_frecency_score(candidate, &self.db, max_path_rank),
                self.config.ranking.path_frecency_score,
            ),
        ];

        let heuristic_score = features
            .iter()
            .map(|feature| feature.value * feature.weight)
            .sum::<f64>();
        let mut final_features = features.clone();
        let mut score = heuristic_score;

        if self.config.features.ml_rerank {
            if let Some(model) = &self.ml_model {
                let ml_score = model.predict(
                    &feature_values(&features),
                    &candidate.kind,
                    &candidate.source,
                );
                let blend = self.config.ml_blend_weight.clamp(0.0, 1.0);
                score = heuristic_score * (1.0 - blend) + ml_score * blend;
                final_features.push(feature("heuristic_score", heuristic_score, 1.0 - blend));
                final_features.push(feature("ml_model_score", ml_score, blend));
            }
        }

        RankedCandidate {
            item: CompletionItem {
                item_key: history_key.clone(),
                insert_text: candidate.insert_text.clone(),
                display: candidate.display.clone(),
                kind: candidate.kind.clone(),
                score,
                source: candidate.source.clone(),
                meta: CompletionMeta {
                    description: candidate.description.clone(),
                },
            },
            item_key: history_key,
            features: final_features,
        }
    }

    fn history_usage(&self, command: &str, cwd: &str) -> Result<f64> {
        let prefix = command.to_string();
        let entries = self.db.frequent_history(&prefix, cwd, 1)?;
        Ok(entries
            .into_iter()
            .find(|entry| entry.command == command || entry.command.starts_with(command))
            .map(|entry| (entry.count / 10.0).min(1.0))
            .unwrap_or_default())
    }

    fn recency_score(&self, command: &str) -> Result<f64> {
        let entries = self.db.frequent_history(command, "", 3)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        Ok(entries
            .into_iter()
            .find(|entry| entry.command == command || entry.command.starts_with(command))
            .map(|entry| {
                let age = (now - entry.last_seen).max(0) as f64;
                1.0 / (1.0 + age / 3600.0)
            })
            .unwrap_or_default())
    }

    fn transition_score(&self, prev: &str, next: &str) -> Result<f64> {
        let transitions = self.db.transitions_from(prev, 10)?;
        Ok(transitions
            .into_iter()
            .find(|entry| entry.next == next || entry.next.starts_with(next))
            .map(|entry| (entry.count / 5.0).min(1.0))
            .unwrap_or_default())
    }
}

fn feature(name: &'static str, value: f64, weight: f64) -> FeatureBreakdown {
    FeatureBreakdown {
        name,
        value,
        weight,
    }
}

fn feature_values(features: &[FeatureBreakdown]) -> HashMap<String, f64> {
    features
        .iter()
        .filter(|feature| feature.name != "heuristic_score" && feature.name != "ml_model_score")
        .map(|feature| (feature.name.to_string(), feature.value))
        .collect()
}

fn push_candidate(
    candidates: &mut Vec<Candidate>,
    seen: &mut HashSet<String>,
    candidate: Candidate,
) {
    let key = format!("{}::{}", candidate.kind, candidate.insert_text);
    if seen.insert(key) {
        candidates.push(candidate);
    }
}

fn cmp_score(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn prefix_score(query: &str, candidate: &str) -> f64 {
    if query.is_empty() {
        return 0.4;
    }
    if candidate.starts_with(query) {
        return 1.0;
    }
    0.0
}

fn fuzzy_match_score(query: &str, candidate: &str) -> f64 {
    if query.is_empty() {
        return 0.2;
    }
    let mut cursor = 0usize;
    let mut bonus = 0.0f64;
    let candidate_chars: Vec<char> = candidate.chars().collect();
    let n = candidate_chars.len();
    let q_len = query.chars().count();

    for ch in query.chars() {
        let Some(pos) = candidate_chars[cursor..]
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&ch))
        else {
            return 0.0;
        };
        let abs_pos = cursor + pos;

        if pos == 0 && cursor > 0 {
            // consecutive: adjacent to previous match
            bonus += 1.0;
        }
        if abs_pos == 0 {
            bonus += 1.0;
        } else {
            let prev = candidate_chars[abs_pos - 1];
            if matches!(prev, '-' | '_' | '.' | '/' | ' ') {
                // word boundary: match starts a new word
                bonus += 1.0;
            }
        }

        cursor = abs_pos + 1;
    }

    let base = q_len as f64 / n.max(1) as f64;
    let max_bonus = (q_len * 2) as f64;
    (base * (1.0 + bonus / max_bonus)).min(1.0)
}

fn should_include_doc(doc: &StoredDoc, active: &str, parsed: &ParsedContext) -> bool {
    if is_python_module_position(parsed) {
        return false;
    }

    match parsed.role {
        TokenRole::Option => doc.item_type == "option" && doc.item_value.starts_with(active),
        TokenRole::SubcommandOrArg => {
            (doc.item_type == "subcommand" || doc.item_type == "option")
                && (doc.item_value.starts_with(active)
                    || fuzzy_match_score(active, &doc.item_value) > 0.0)
        }
        TokenRole::Command => doc.item_type == "subcommand",
        TokenRole::Path => false,
    }
}

fn project_affinity_score(markers: &[String], command: &str) -> f64 {
    let hints: HashMap<&str, &[&str]> = HashMap::from([
        ("Cargo.toml", &["cargo", "rustc", "rustup"][..]),
        ("package.json", &["npm", "pnpm", "node", "yarn"][..]),
        ("pyproject.toml", &["python", "pip", "pytest", "uv"][..]),
        ("*.csproj", &["dotnet"][..]),
        ("*.sln", &["dotnet"][..]),
        (".git", &["git"][..]),
        ("Dockerfile", &["docker", "docker-compose"][..]),
        ("Makefile", &["make"][..]),
    ]);
    if markers
        .iter()
        .filter_map(|marker| hints.get(marker.as_str()))
        .flat_map(|commands| commands.iter())
        .any(|known| known == &command)
    {
        0.8
    } else {
        0.0
    }
}

fn position_score(parsed: &ParsedContext, candidate: &Candidate) -> f64 {
    match parsed.role {
        TokenRole::Command if candidate.kind == "command" || candidate.kind == "builtin" => 1.0,
        TokenRole::Option if candidate.kind == "option" => 1.0,
        TokenRole::Path if candidate.kind == "path" => 1.0,
        _ if is_cd_path_context(parsed) && candidate.kind == "path" => 1.0,
        _ if is_cd_path_context(parsed) && candidate.kind == "path_jump" => 1.0,
        TokenRole::SubcommandOrArg if candidate.kind == "module" => 1.0,
        TokenRole::SubcommandOrArg if candidate.kind == "subcommand" => 0.9,
        TokenRole::SubcommandOrArg if candidate.kind == "history" => 0.6,
        _ => 0.2,
    }
}

fn is_cd_path_context(parsed: &ParsedContext) -> bool {
    matches!(parsed.command.as_deref(), Some("cd"))
        || matches!(parsed.prev_token.as_deref(), Some("cd"))
}

fn source_prior(source: &str, kind: &str) -> f64 {
    match (source, kind) {
        ("path_index", "command") => 0.8,
        ("builtin-index", "subcommand") => 0.9,
        ("builtin-index", "module") => 0.9,
        ("help", "option") => 0.7,
        ("history", _) => 0.6,
        ("runtime_history", _) => 0.65,
        ("transition", _) => 0.7,
        ("path_cache", _) => 0.9,
        ("path_jump", _) => 0.85,
        _ => 0.4,
    }
}

fn doc_match_score(query: &str, description: Option<&str>) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    description
        .map(|text| fuzzy_match_score(query, text))
        .unwrap_or_default()
}

fn path_frecency_score(candidate: &Candidate, db: &AppDb, max_rank: f64) -> f64 {
    if candidate.kind != "path_jump" || max_rank <= 0.0 {
        return 0.0;
    }
    let abs = expand_with_home(&candidate.insert_text);
    let rank = db.path_rank(&abs).unwrap_or(0.0);
    (rank / max_rank).clamp(0.0, 1.0)
}

fn shorten_with_home(path: &str, home: Option<&Path>) -> String {
    if let Some(home) = home {
        let home_str = home.to_string_lossy();
        if path == home_str.as_ref() {
            return "~".to_string();
        }
        if let Some(stripped) = path.strip_prefix(home_str.as_ref()) {
            if let Some(rest) = stripped.strip_prefix('/') {
                return format!("~/{rest}");
            }
        }
    }
    path.to_string()
}

fn expand_with_home(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|p| p.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
    }
    path.to_string()
}

fn format_path_jump_description(is_git_repo: bool, last_visit: i64, now: i64) -> String {
    let age = (now - last_visit).max(0);
    let age_label = if last_visit <= 0 {
        "no recent visit".to_string()
    } else if age < 60 {
        "just now".to_string()
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86_400)
    };
    if is_git_repo {
        format!("git repo \u{00b7} last visit {age_label}")
    } else {
        format!("last visit {age_label}")
    }
}

fn sanitize_fts_query(query: &str) -> Option<String> {
    let tokens = query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '.')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

struct PythonModuleCandidate {
    name: &'static str,
    description: &'static str,
}

fn is_python_module_position(parsed: &ParsedContext) -> bool {
    matches!(parsed.command.as_deref(), Some("python" | "python3"))
        && matches!(parsed.prev_token.as_deref(), Some("-m"))
}

fn python_module_candidates() -> &'static [PythonModuleCandidate] {
    &[
        PythonModuleCandidate {
            name: "pytest",
            description: "Run the pytest test runner",
        },
        PythonModuleCandidate {
            name: "pip",
            description: "Run the Python package installer",
        },
        PythonModuleCandidate {
            name: "venv",
            description: "Create or manage virtual environments",
        },
        PythonModuleCandidate {
            name: "http.server",
            description: "Run a simple HTTP server",
        },
        PythonModuleCandidate {
            name: "unittest",
            description: "Run Python unit tests",
        },
        PythonModuleCandidate {
            name: "pdb",
            description: "Run the Python debugger",
        },
        PythonModuleCandidate {
            name: "pydoc",
            description: "Show Python documentation",
        },
        PythonModuleCandidate {
            name: "json.tool",
            description: "Validate and pretty-print JSON",
        },
        PythonModuleCandidate {
            name: "timeit",
            description: "Benchmark small Python snippets",
        },
        PythonModuleCandidate {
            name: "cProfile",
            description: "Run the Python profiler",
        },
        PythonModuleCandidate {
            name: "doctest",
            description: "Run examples embedded in docstrings",
        },
        PythonModuleCandidate {
            name: "compileall",
            description: "Byte-compile Python source files",
        },
        PythonModuleCandidate {
            name: "site",
            description: "Inspect Python site configuration",
        },
        PythonModuleCandidate {
            name: "ensurepip",
            description: "Bootstrap pip into an environment",
        },
    ]
}

fn contextual_history_prefix(parsed: &ParsedContext) -> String {
    if matches!(parsed.role, TokenRole::Command) {
        return parsed.active_token.clone();
    }
    if let Some(command) = parsed.command.as_deref() {
        if parsed.active_token.is_empty() {
            return format!("{command} ");
        }
        return format!("{command} {}", parsed.active_token);
    }
    parsed.active_token.clone()
}

fn project_history_candidate(
    entry: &str,
    parsed: &ParsedContext,
) -> Option<(String, String, String)> {
    if matches!(parsed.role, TokenRole::Command) {
        return Some((entry.to_string(), entry.to_string(), "history".to_string()));
    }
    let command = parsed.command.as_deref()?;
    let tokens = context::shell_split(entry);
    if tokens.first().map(String::as_str) != Some(command) {
        return None;
    }
    let token = tokens.get(parsed.active_index)?.to_string();
    let kind = match parsed.role {
        TokenRole::Option => "option",
        TokenRole::Path => "path",
        TokenRole::SubcommandOrArg => "subcommand",
        TokenRole::Command => "history",
    };
    Some((token.clone(), token, kind.to_string()))
}

fn contextual_candidate_key(parsed: &ParsedContext, candidate: &Candidate) -> String {
    if parsed.tokens.is_empty() {
        return candidate.insert_text.clone();
    }
    let mut tokens = parsed.tokens.clone();
    let index = parsed.active_index.min(tokens.len().saturating_sub(1));
    tokens[index] = candidate.insert_text.clone();
    tokens.join(" ").trim().to_string()
}

fn split_path_token(token: &str, cwd: &str) -> (PathBuf, String) {
    let token_path = if token.is_empty() {
        PathBuf::from(cwd)
    } else if token.starts_with("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(token.trim_start_matches("~/"))
    } else if token.starts_with('/') {
        PathBuf::from(token)
    } else {
        PathBuf::from(cwd).join(token)
    };

    let (dir, prefix) = if token.ends_with('/') || token.is_empty() {
        (token_path, String::new())
    } else {
        let prefix = token_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        let dir = token_path
            .parent()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| PathBuf::from(cwd));
        (dir, prefix)
    };

    (dir, prefix)
}

fn path_insertion_prefix(token: &str) -> String {
    if token.ends_with('/') {
        return token.to_string();
    }
    token
        .rfind('/')
        .map(|index| token[..=index].to_string())
        .unwrap_or_default()
}

fn read_dir_entries(dir: &Path) -> Result<Vec<String>> {
    let read_dir = read_dir_with_retry(dir)?;
    let mut entries = read_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

fn read_dir_with_retry(dir: &Path) -> Result<fs::ReadDir> {
    let mut last_error = None;
    for _ in 0..5 {
        match fs::read_dir(dir) {
            Ok(entries) => return Ok(entries),
            Err(err) if err.raw_os_error() == Some(35) => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(5));
            }
            Err(err) => return Err(err).with_context(|| format!("read dir {}", dir.display())),
        }
    }
    Err(last_error.expect("read_dir retry error"))
        .with_context(|| format!("read dir {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_score_handles_subsequence() {
        assert!(fuzzy_match_score("gt", "git") > 0.0);
        assert_eq!(fuzzy_match_score("zz", "git"), 0.0);
    }

    #[test]
    fn fuzzy_score_consecutive_beats_scattered() {
        // "gt" in "gtscript" (consecutive) > "gt" in "gxtscrip" (not consecutive)
        assert!(fuzzy_match_score("gt", "gtscript") > fuzzy_match_score("gt", "gxtscrip"));
    }

    #[test]
    fn fuzzy_score_boundary_beats_mid_word() {
        // same length: "g-clust" has 'c' at boundary, "gxcxxx0" does not
        assert!(fuzzy_match_score("gc", "g-clust") > fuzzy_match_score("gc", "gxcxxx0"));
    }

    #[test]
    fn fuzzy_score_space_boundary() {
        // "git pull" — 'p' after space is a boundary
        assert!(fuzzy_match_score("gp", "git pull") > 0.0);
    }

    #[test]
    fn prefix_score_prefers_exact_prefix() {
        assert_eq!(prefix_score("gi", "git"), 1.0);
        assert_eq!(prefix_score("zz", "git"), 0.0);
    }

    #[test]
    fn fts_query_sanitizer_drops_operator_only_queries() {
        assert_eq!(sanitize_fts_query("-"), None);
        assert_eq!(sanitize_fts_query("--"), None);
        assert_eq!(
            sanitize_fts_query("pytest -k"),
            Some("pytest k".to_string())
        );
    }

    struct TestDir {
        path: PathBuf,
    }
    impl TestDir {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("shac-engine-{label}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create test dir");
            Self { path }
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_engine(label: &str) -> (Engine, TestDir) {
        let dir = TestDir::new(label);
        let paths = AppPaths {
            config_file: dir.path.join("config.toml"),
            db_file: dir.path.join("shac.db"),
            socket_file: dir.path.join("shacd.sock"),
            pid_file: dir.path.join("shacd.pid"),
            shell_dir: dir.path.join("shell"),
            config_dir: dir.path.clone(),
            data_dir: dir.path.clone(),
            state_dir: dir.path.clone(),
        };
        let engine = Engine::new(&paths).expect("engine");
        (engine, dir)
    }

    fn make_request(line: &str, cwd: &str) -> CompletionRequest {
        CompletionRequest {
            shell: "zsh".to_string(),
            line: line.to_string(),
            cursor: line.len(),
            cwd: cwd.to_string(),
            env: std::collections::HashMap::new(),
            session: crate::protocol::SessionInfo {
                tty: Some("test".to_string()),
                pid: None,
            },
            history_hint: crate::protocol::HistoryHint {
                prev_command: None,
                runtime_commands: Vec::new(),
            },
        }
    }

    #[test]
    fn cd_with_global_path_emits_path_jump() {
        use std::fs;
        let (engine, _dir) = test_engine("path-jump");
        let cwd = std::env::temp_dir().join(format!(
            "shac-cd-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&cwd).unwrap();
        let target = std::env::temp_dir().join(format!(
            "shac-cd-target-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&target).unwrap();
        let target_str = target.to_string_lossy().to_string();
        engine
            .db
            .upsert_path_index_with_rank(&target_str, 5.0, 0, "test", false, None)
            .unwrap();

        let response = engine
            .complete(make_request("cd ", &cwd.to_string_lossy()))
            .expect("complete");
        let has_path_jump = response.items.iter().any(|i| i.kind == "path_jump");
        assert!(
            has_path_jump,
            "expected at least one path_jump candidate, got: {:?}",
            response.items
        );

        let _ = fs::remove_dir_all(&cwd);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn cd_with_no_global_paths_falls_back_to_children_only() {
        use std::fs;
        let (engine, _dir) = test_engine("no-global");
        let cwd = std::env::temp_dir().join(format!(
            "shac-cd-noglobal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(cwd.join("alpha")).unwrap();

        let response = engine
            .complete(make_request("cd ", &cwd.to_string_lossy()))
            .expect("complete");
        let any_path_jump = response.items.iter().any(|i| i.kind == "path_jump");
        assert!(
            !any_path_jump,
            "expected no path_jump candidates with empty paths_index, got: {:?}",
            response.items
        );

        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn contextual_candidate_key_rebuilds_command_line() {
        let parsed = ParsedContext {
            line_before_cursor: "git ch".to_string(),
            tokens: vec!["git".to_string(), "ch".to_string()],
            active_token: "ch".to_string(),
            active_index: 1,
            role: TokenRole::SubcommandOrArg,
            command: Some("git".to_string()),
            prev_token: Some("git".to_string()),
            project_markers: Vec::new(),
        };
        let candidate = Candidate {
            insert_text: "checkout".to_string(),
            display: "checkout".to_string(),
            kind: "subcommand".to_string(),
            source: "builtin-index".to_string(),
            description: None,
        };
        assert_eq!(
            contextual_candidate_key(&parsed, &candidate),
            "git checkout"
        );
    }
}
