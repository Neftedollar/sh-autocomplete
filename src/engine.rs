use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Hard timeout on the `git for-each-ref` invocation that backs
/// `collect_git_branch_candidates`. Branch completion is best-effort —
/// missing it never blocks completion of other candidate sources.
const GIT_REF_TIMEOUT: Duration = Duration::from_millis(200);

/// Cap on number of refs we consume from `git for-each-ref` output. Most
/// repos have < 50 branches; this is a defensive bound for repos with
/// thousands of remote branches (e.g. mirrors of large monorepos).
const GIT_REF_MAX: usize = 200;

/// Maximum number of ancestor directories we walk while searching for a
/// `package.json` in [`find_package_json_root`]. Caps work for pathological
/// nested layouts and prevents accidentally escaping the project tree.
const PACKAGE_JSON_WALK_LIMIT: usize = 8;

/// Cap on the size of `package.json` we'll read; anything larger is treated
/// as malformed (script lists are tiny — ~hundreds of bytes typical).
const PACKAGE_JSON_MAX_BYTES: u64 = 1_048_576; // 1 MiB

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
        _command: &str,
        active: &str,
        cwd: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        use crate::profiles::{self, ArgType};
        let arg_type = profiles::arg_type_for(parsed).unwrap_or(ArgType::None);
        match arg_type {
            ArgType::Directory => {
                // cd / pushd / popd: directories only, plus frecent jumps from
                // the global paths_index.
                self.collect_path_candidates(active, cwd, true, candidates, seen)?;
                self.collect_global_path_candidates(active, cwd, candidates, seen)?;
            }
            ArgType::Path => {
                // vim / cat / cp / etc.: files and directories under cwd.
                self.collect_path_candidates(active, cwd, false, candidates, seen)?;
            }
            ArgType::Branch => {
                self.collect_git_branch_candidates(active, cwd, candidates, seen)?;
            }
            ArgType::Script => {
                self.collect_npm_script_candidates(
                    active,
                    Path::new(cwd),
                    candidates,
                    seen,
                )?;
            }
            ArgType::Host
            | ArgType::Resource
            | ArgType::Image
            | ArgType::Workspace
            | ArgType::Target => {
                // Stubs for now — implementations land in subsequent PRs.
            }
            ArgType::Subcommand | ArgType::Flag | ArgType::None => {
                // Subcommand / flag completion is handled elsewhere in
                // collect_candidates.
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

    /// Emit git branch / ref candidates for `git checkout|switch|branch|merge|rebase`
    /// from the cwd's nearest git repo. Reads `git for-each-ref` over local
    /// branches and `origin` remotes, sorted by committerdate (most recent
    /// first). Bounded by a 200ms timeout; on any failure (no repo, git not
    /// installed, command timeout) we return Ok(()) silently — branch
    /// completion is best-effort.
    fn collect_git_branch_candidates(
        &self,
        active: &str,
        cwd: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let repo_root = match find_git_repo_root(Path::new(cwd)) {
            Some(root) => root,
            None => return Ok(()),
        };
        let refs = match list_git_refs(&repo_root, GIT_REF_TIMEOUT) {
            Some(refs) => refs,
            None => return Ok(()),
        };

        let limit = self.config.max_results.saturating_mul(2).max(8);
        let mut emitted = 0usize;
        for refname in refs {
            if emitted >= limit {
                break;
            }
            // Filter: prefix or fuzzy match. Empty `active` keeps everything.
            if !active.is_empty()
                && !refname.starts_with(active)
                && fuzzy_match_score(active, &refname) <= 0.0
            {
                continue;
            }
            let display = refname.clone();
            let description = if refname.starts_with("origin/") {
                Some("remote-tracking branch".to_string())
            } else {
                Some("local branch".to_string())
            };
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: refname,
                    display,
                    kind: "branch".to_string(),
                    source: "git_branch".to_string(),
                    description,
                },
            );
            emitted += 1;
        }
        Ok(())
    }

    /// Emit npm/pnpm/yarn script candidates for `<pm> run` from the cwd's
    /// nearest `package.json`. Walks up at most [`PACKAGE_JSON_WALK_LIMIT`]
    /// ancestors and stops at any directory containing `.git` (project
    /// boundary). On any failure (no `package.json`, malformed JSON,
    /// I/O error) we return Ok(()) silently — script completion is
    /// best-effort and must never block other candidate sources.
    fn collect_npm_script_candidates(
        &self,
        active: &str,
        cwd: &Path,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let pkg_json = match find_package_json_root(cwd) {
            Some(root) => root.join("package.json"),
            None => return Ok(()),
        };
        let scripts = parse_package_json_scripts(&pkg_json).unwrap_or_default();
        if scripts.is_empty() {
            return Ok(());
        }

        let limit = self.config.max_results.saturating_mul(2).max(8);
        let mut emitted = 0usize;
        for (name, command) in scripts {
            if emitted >= limit {
                break;
            }
            // Case-sensitive prefix match keeps the v1 contract crisp:
            // typing `de<Tab>` should match `dev` but not `Develop`.
            // Empty active still surfaces all scripts.
            if !active.is_empty() && !name.starts_with(active) {
                continue;
            }
            let description = format!("script \u{00b7} {}", truncate_command(&command, 60));
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: name.clone(),
                    display: name,
                    kind: "npm_script".to_string(),
                    source: "npm_script".to_string(),
                    description: Some(description),
                },
            );
            emitted += 1;
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
        _ if candidate.kind == "branch" => 1.0,
        _ if candidate.kind == "npm_script" => 1.0,
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
        ("git_branch", "branch") => 0.85,
        ("npm_script", _) => 0.85,
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

/// Walk up from `start` looking for a `.git` directory or file (worktree).
/// Returns the repo root (the parent of the `.git` entry) on success, `None`
/// if no repo is found before reaching `/`.
fn find_git_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let dot_git = dir.join(".git");
        if dot_git.exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

/// Run `git for-each-ref --format=%(refname:short) refs/heads refs/remotes
/// --sort=-committerdate` in `repo_root`, with a hard timeout. Returns
/// `None` on any failure (git not installed, command nonzero, timeout).
///
/// The returned vector is deduped (some refs appear under both heads and
/// remotes/origin) and capped at [`GIT_REF_MAX`].
fn list_git_refs(repo_root: &Path, timeout: Duration) -> Option<Vec<String>> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=-committerdate",
            "refs/heads",
            "refs/remotes",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let mut buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut buf).ok()?;
    }

    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for line in buf.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip `origin/HEAD` — it's an alias, not a real branch.
        if trimmed == "origin/HEAD" || trimmed.ends_with("/HEAD") {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            refs.push(trimmed.to_string());
            if refs.len() >= GIT_REF_MAX {
                break;
            }
        }
    }
    Some(refs)
}

/// Walk up at most [`PACKAGE_JSON_WALK_LIMIT`] ancestors from `start` looking
/// for a directory that contains `package.json`. Stops on either:
///
/// * the filesystem root,
/// * the walk limit, or
/// * the first ancestor whose `.git` exists, returning the package.json on
///   that boundary if present (we never escape past a project boundary).
///
/// Returns the **directory** containing `package.json`, not the file itself.
fn find_package_json_root(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    let mut steps = 0usize;
    while let Some(dir) = cur {
        if steps >= PACKAGE_JSON_WALK_LIMIT {
            return None;
        }
        if dir.join("package.json").is_file() {
            return Some(dir.to_path_buf());
        }
        // Project boundary: a `.git` here means we should not escape further.
        if dir.join(".git").exists() {
            return None;
        }
        cur = dir.parent();
        steps += 1;
    }
    None
}

/// Read `package.json` and extract `(name, command)` pairs from its `scripts`
/// object. Returns an empty Vec on any read or parse failure (caller is
/// expected to treat absence as "no candidates" rather than an error — script
/// completion must never block other sources).
fn parse_package_json_scripts(path: &Path) -> Result<Vec<(String, String)>> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return Ok(Vec::new()),
    };
    if meta.len() > PACKAGE_JSON_MAX_BYTES {
        return Ok(Vec::new());
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Ok(Vec::new()),
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(Vec::new()),
    };
    let scripts = match value.get("scripts").and_then(|v| v.as_object()) {
        Some(obj) => obj,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(scripts.len());
    for (name, cmd) in scripts {
        let command = cmd.as_str().unwrap_or("").to_string();
        out.push((name.clone(), command));
    }
    Ok(out)
}

/// Truncate a script command for display in the description column. Trims
/// whitespace and replaces newlines with spaces so multiline scripts render
/// inline. Adds a single ellipsis when truncated.
fn truncate_command(command: &str, max_len: usize) -> String {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_len {
        return normalized;
    }
    let truncated: String = normalized.chars().take(max_len).collect();
    format!("{truncated}\u{2026}")
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

    fn unique_tmp(prefix: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}-{}",
            std::process::id(),
            nanos,
            n
        ));
        std::fs::create_dir_all(&path).expect("create unique tmp");
        path
    }

    #[test]
    fn find_git_repo_root_walks_up_from_subdir() {
        let scratch = unique_tmp("shac-engine-fake-repo");
        let repo = scratch.join("repo");
        let nested = repo.join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir .git");
        let found = find_git_repo_root(&nested).expect("repo root");
        assert_eq!(found.canonicalize().unwrap(), repo.canonicalize().unwrap());
    }

    #[test]
    fn find_package_json_root_walks_up() {
        let scratch = unique_tmp("shac-engine-pkg-walk-up");
        let root = scratch.join("project");
        let nested = root.join("src/components");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        std::fs::write(root.join("package.json"), "{}").expect("seed package.json");
        let found = find_package_json_root(&nested).expect("package root");
        assert_eq!(
            found.canonicalize().unwrap(),
            root.canonicalize().unwrap(),
            "expected nearest package.json directory"
        );
    }

    #[test]
    fn find_package_json_root_stops_at_git_boundary() {
        // outer/package.json     <- should NOT be reached
        // outer/inner/.git
        // outer/inner/package.json (none here)
        // outer/inner/src        <- start here, must stop at inner/.git boundary
        let scratch = unique_tmp("shac-engine-pkg-git-boundary");
        let outer = scratch.join("outer");
        let inner = outer.join("inner");
        let leaf = inner.join("src");
        std::fs::create_dir_all(&leaf).expect("mkdir leaf");
        std::fs::create_dir_all(inner.join(".git")).expect("mkdir .git");
        std::fs::write(outer.join("package.json"), "{}").expect("seed outer pkg");
        // inner has no package.json, but it does have .git — walk must stop
        // at inner and refuse to escape into outer.
        let found = find_package_json_root(&leaf);
        assert!(
            found.is_none(),
            "walk should stop at .git boundary instead of escaping to {found:?}"
        );
    }

    #[test]
    fn find_package_json_root_returns_root_when_pkg_at_git_boundary() {
        // Same .git boundary, but package.json sits *at* the boundary —
        // we must still return it (the boundary itself is in-scope).
        let scratch = unique_tmp("shac-engine-pkg-at-boundary");
        let inner = scratch.join("project");
        let leaf = inner.join("src");
        std::fs::create_dir_all(&leaf).expect("mkdir leaf");
        std::fs::create_dir_all(inner.join(".git")).expect("mkdir .git");
        std::fs::write(inner.join("package.json"), "{}").expect("seed pkg");
        let found = find_package_json_root(&leaf).expect("found");
        assert_eq!(
            found.canonicalize().unwrap(),
            inner.canonicalize().unwrap()
        );
    }

    #[test]
    fn parse_package_json_scripts_extracts_keys() {
        let scratch = unique_tmp("shac-engine-pkg-parse");
        let pkg = scratch.join("package.json");
        std::fs::write(
            &pkg,
            r#"{"name":"x","scripts":{"dev":"vite","build":"vite build","test":"vitest"}}"#,
        )
        .expect("seed pkg");
        let scripts = parse_package_json_scripts(&pkg).expect("parse ok");
        let names: Vec<&str> = scripts.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"dev"), "missing dev: {names:?}");
        assert!(names.contains(&"build"), "missing build: {names:?}");
        assert!(names.contains(&"test"), "missing test: {names:?}");
        let dev_cmd = scripts
            .iter()
            .find(|(n, _)| n == "dev")
            .map(|(_, c)| c.as_str())
            .unwrap();
        assert_eq!(dev_cmd, "vite");
    }

    #[test]
    fn parse_package_json_scripts_tolerates_malformed_json() {
        let scratch = unique_tmp("shac-engine-pkg-bad-json");
        let pkg = scratch.join("package.json");
        std::fs::write(&pkg, "{ this is not json").expect("seed pkg");
        let scripts = parse_package_json_scripts(&pkg).expect("never errors");
        assert!(
            scripts.is_empty(),
            "malformed JSON must yield empty Vec, got {scripts:?}"
        );
    }

    #[test]
    fn parse_package_json_scripts_missing_file_returns_empty() {
        let scratch = unique_tmp("shac-engine-pkg-missing");
        let pkg = scratch.join("package.json");
        let scripts = parse_package_json_scripts(&pkg).expect("never errors");
        assert!(scripts.is_empty());
    }

    #[test]
    fn parse_package_json_scripts_no_scripts_object() {
        let scratch = unique_tmp("shac-engine-pkg-no-scripts");
        let pkg = scratch.join("package.json");
        std::fs::write(&pkg, r#"{"name":"x","version":"1.0.0"}"#).expect("seed");
        let scripts = parse_package_json_scripts(&pkg).expect("ok");
        assert!(scripts.is_empty());
    }

    #[test]
    fn truncate_command_truncates_long_strings() {
        let s = "node ./scripts/very-long-build-script-with-many-flags --foo --bar --baz";
        let out = truncate_command(s, 20);
        assert!(out.chars().count() <= 21, "{out:?}");
        assert!(out.ends_with('\u{2026}'), "expected ellipsis: {out:?}");
    }

    #[test]
    fn truncate_command_preserves_short_strings() {
        assert_eq!(truncate_command("vite", 60), "vite");
    }

    #[test]
    fn truncate_command_collapses_whitespace() {
        assert_eq!(truncate_command("a  b\n\tc", 60), "a b c");
    }

    #[test]
    fn find_git_repo_root_returns_none_when_no_dot_git_anywhere() {
        // Use a fresh scratch dir we control, point the start path at a
        // subdir that has no `.git` and whose parents we control too.
        let scratch = unique_tmp("shac-engine-no-repo");
        let inner = scratch.join("a/b");
        std::fs::create_dir_all(&inner).expect("mkdir nested");
        let result = find_git_repo_root(&inner);
        // Either None (clean) or — if some ancestor of /tmp happens to have
        // .git — a real ancestor with .git. Accept both.
        if let Some(root) = result {
            assert!(root.join(".git").exists(), "claimed repo root has no .git");
        }
    }
}
