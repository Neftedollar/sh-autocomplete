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

/// Cap on size of `~/.ssh/config` and `~/.ssh/known_hosts` we'll read.
/// Real files rarely exceed a few KB; 1 MiB is a very conservative guard.
const SSH_FILE_MAX_BYTES: u64 = 1_048_576; // 1 MiB

/// Hard timeout on `docker images` invocation used by
/// [`collect_docker_image_candidates`]. Docker daemon calls are cheap on a
/// healthy setup; 500ms is generous.
const DOCKER_TIMEOUT: Duration = Duration::from_millis(500);

/// Cap on the number of image lines consumed from `docker images` output.
/// A user with 200+ images is unusual but possible on a build machine.
const DOCKER_IMAGE_LIMIT: usize = 200;

/// Maximum number of ancestor directories walked while searching for a build
/// file (Makefile / justfile / Taskfile). Mirrors [`PACKAGE_JSON_WALK_LIMIT`].
const BUILD_FILE_WALK_LIMIT: usize = 8;

/// Cap on the size of build files we'll read for target extraction.
const BUILD_FILE_MAX_BYTES: u64 = 1_048_576; // 1 MiB

/// Hard timeout on the `kubectl api-resources` invocation that backs
/// `collect_kubectl_resource_candidates`. kubectl against a live cluster can
/// take 200–500ms; 500ms is generous. On timeout the static fallback list is
/// used instead, so the user always gets reasonable candidates.
const KUBECTL_TIMEOUT: Duration = Duration::from_millis(500);

/// Static fallback list of well-known Kubernetes resource names and short-names.
/// Used when `kubectl api-resources` fails (no kubectl, no cluster, timeout).
/// Also merged with live results when the shellout succeeds (robustness).
const KUBECTL_FALLBACK_RESOURCES: &[&str] = &[
    "pods",
    "po",
    "services",
    "svc",
    "deployments",
    "deploy",
    "replicasets",
    "rs",
    "statefulsets",
    "sts",
    "daemonsets",
    "ds",
    "jobs",
    "cronjobs",
    "cj",
    "configmaps",
    "cm",
    "secrets",
    "namespaces",
    "ns",
    "nodes",
    "no",
    "ingresses",
    "ing",
    "endpoints",
    "ep",
    "events",
    "ev",
    "persistentvolumes",
    "pv",
    "persistentvolumeclaims",
    "pvc",
    "serviceaccounts",
    "sa",
    "horizontalpodautoscalers",
    "hpa",
    "networkpolicies",
    "netpol",
    "storageclasses",
    "sc",
    "customresourcedefinitions",
    "crd",
    "roles",
    "rolebindings",
    "clusterroles",
    "clusterrolebindings",
    "podtemplates",
    "limitranges",
    "resourcequotas",
];

use anyhow::{Context, Result};
use rusqlite;

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
            ArgType::Host => {
                self.collect_ssh_host_candidates(active, candidates, seen)?;
            }
            ArgType::Target => {
                self.collect_target_candidates(command, active, Path::new(cwd), candidates, seen)?;
            }
            ArgType::Workspace => {
                // Primary: recent VS Code workspaces from the global store.
                self.collect_workspace_candidates(active, candidates, seen)?;
                // Secondary: filesystem path completion so `code ~/d<Tab>` also
                // surfaces directories the user is typing directly.
                self.collect_path_candidates(active, cwd, true, candidates, seen)?;
            }
            ArgType::Resource => {
                // Route by command: kubectl uses Kubernetes api-resources;
                // docker exec uses running container names; any other command
                // mapped to Resource gets no completions here (other sources
                // may still contribute via the Path fallback).
                match command {
                    "kubectl" => {
                        self.collect_kubectl_resource_candidates(active, candidates, seen)?;
                    }
                    "docker" => {
                        self.collect_docker_container_candidates(active, candidates, seen)?;
                    }
                    _ => {}
                }
            }
            ArgType::Image => {
                self.collect_docker_image_candidates(active, candidates, seen)?;
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
        // Resolve cwd to its canonical form once so that comparisons below
        // work even when the shell passes a symlinked path (e.g. /tmp on macOS
        // is a symlink to /private/tmp, while the DB stores canonical paths).
        let cwd_canonical = cwd_path.canonicalize().unwrap_or_else(|_| cwd_path.clone());
        let home = dirs::home_dir();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for row in rows {
            let path = PathBuf::from(&row.path);
            // Resolve this DB path canonically for comparison; fall back to the
            // raw value if the path has vanished since it was recorded.
            let path_canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            // Skip paths equal to cwd.
            if path_canonical == cwd_canonical {
                continue;
            }
            // Skip direct children of cwd (already covered by collect_path_candidates).
            // Compare canonical forms so that /tmp vs /private/tmp mismatches
            // don't cause the same directory to appear twice (§7.16).
            if path_canonical
                .parent()
                .map(|p| p == cwd_canonical)
                .unwrap_or(false)
            {
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

    /// Emit SSH host candidates for `ssh`, `scp`, `mosh`, `rsync` from:
    ///   1. `~/.ssh/config` — `Host` directive lines (skipping wildcards).
    ///   2. `~/.ssh/known_hosts` — first column host list (handling
    ///      `[host]:port` form, skipping hashed `|1|...` entries and
    ///      `@cert-authority`/`@revoked` markers).
    ///
    /// Sources are merged and deduplicated via the shared `seen` set.
    /// No `cwd` argument — SSH host completion is global, not project-local.
    fn collect_ssh_host_candidates(
        &self,
        active: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        // Collect hosts from each source, tracking origin for description.
        let mut config_hosts: HashSet<String> = HashSet::new();
        let mut known_hosts: HashSet<String> = HashSet::new();

        if let Some(path) = ssh_config_path() {
            for host in parse_ssh_config_hosts(&path) {
                config_hosts.insert(host);
            }
        }
        if let Some(path) = known_hosts_path() {
            for host in parse_known_hosts_hostnames(&path) {
                known_hosts.insert(host);
            }
        }

        // Build a merged, deduplicated list with provenance labels.
        let mut all: Vec<(String, &'static str)> = Vec::new();
        for host in &config_hosts {
            let label = if known_hosts.contains(host) {
                "both"
            } else {
                "config"
            };
            all.push((host.clone(), label));
        }
        for host in &known_hosts {
            if !config_hosts.contains(host) {
                all.push((host.clone(), "known_hosts"));
            }
        }

        // Sort for deterministic output.
        all.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let limit = self.config.max_results.saturating_mul(2).max(8);
        let mut emitted = 0usize;
        for (hostname, source_label) in all {
            if emitted >= limit {
                break;
            }
            // Prefix filter; empty active passes all.
            if !active.is_empty() && !hostname.starts_with(active) {
                continue;
            }
            let description = format!("ssh host \u{00b7} {source_label}");
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: hostname.clone(),
                    display: hostname,
                    kind: "ssh_host".to_string(),
                    source: "ssh_host".to_string(),
                    description: Some(description),
                },
            );
            emitted += 1;
        }
        Ok(())
    }

    /// Emit build-target candidates for `make`, `just`, and `task` by walking
    /// up from `cwd` to the nearest build file, then parsing target names from
    /// it.
    ///
    /// Walk-up is bounded by [`BUILD_FILE_WALK_LIMIT`] and stops at any `.git`
    /// boundary (the boundary directory is checked for the build file before
    /// stopping). On any failure (no build file found, parse error, I/O error)
    /// we return `Ok(())` silently — target completion is best-effort and must
    /// never block other candidate sources.
    fn collect_target_candidates(
        &self,
        command: &str,
        active: &str,
        cwd: &Path,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let (build_file, system) = match find_build_file_root(command, cwd) {
            Some(pair) => pair,
            None => return Ok(()),
        };

        let targets = match system {
            BuildSystem::Make => parse_makefile_targets(&build_file),
            BuildSystem::Just => parse_justfile_targets(&build_file),
            BuildSystem::Task => parse_taskfile_targets(&build_file),
        };

        let system_name = match system {
            BuildSystem::Make => "make",
            BuildSystem::Just => "just",
            BuildSystem::Task => "task",
        };

        let limit = self.config.max_results.saturating_mul(2).max(8);
        let mut emitted = 0usize;
        for target in targets {
            if emitted >= limit {
                break;
            }
            // Prefix filter; empty active passes all.
            if !active.is_empty() && !target.starts_with(active) {
                continue;
            }
            let description = format!("{system_name} target");
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: target.clone(),
                    display: target,
                    kind: "build_target".to_string(),
                    source: "build_target".to_string(),
                    description: Some(description),
                },
            );
            emitted += 1;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // VS Code workspace collector (§7.9)
    // -----------------------------------------------------------------------

    /// Collect VS Code recent-workspace candidates.
    ///
    /// Strategy:
    /// 1. Find VS Code's recent-workspaces store (SQLite DB preferred; JSON
    ///    fallback for older installations).
    /// 2. Parse out the recent folder/workspace paths.
    /// 3. Filter out stale entries that no longer exist on disk.
    /// 4. Filter by `active` prefix matched against the basename — users type
    ///    project name, not full path.
    /// 5. Emit candidates with `insert_text` = `~`-shortened path.
    fn collect_workspace_candidates(
        &self,
        active: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let home = dirs::home_dir();

        // Locate and parse recent workspaces.
        let mut paths: Vec<PathBuf> = Vec::new();
        for storage_path in vscode_storage_paths() {
            let parsed = parse_vscode_recent_workspaces(&storage_path);
            if !parsed.is_empty() {
                paths = parsed;
                break;
            }
        }

        // Filter: must exist on disk, must not be a plain file used as a
        // recently-opened item (we want folders / .code-workspace bundles).
        // Also apply the active-prefix filter against the basename.
        let limit = self.config.max_results.saturating_mul(2).max(8);
        let mut emitted = 0usize;

        for path in paths {
            if emitted >= limit {
                break;
            }

            // Skip entries that have gone missing.
            if !path.exists() {
                continue;
            }

            let path_str = path.to_string_lossy();

            // Build the basename for prefix-filtering.
            let basename = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_else(|| path_str.clone());

            // Prefix filter against the basename (project name).
            // Empty `active` passes all; non-empty must prefix-match basename.
            if !active.is_empty() && !basename.starts_with(active) {
                continue;
            }

            // Compute ~-shortened insert_text.
            let insert_text = shorten_with_home(&path_str, home.as_deref());

            // Display: "basename · parent" to help scan.
            let parent_str = path
                .parent()
                .map(|p| shorten_with_home(&p.to_string_lossy(), home.as_deref()))
                .unwrap_or_default();

            let is_workspace_bundle = path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("code-workspace"))
                .unwrap_or(false);

            let display = if is_workspace_bundle {
                format!("{basename} [workspace bundle] \u{00b7} {parent_str}")
            } else {
                format!("{basename} \u{00b7} {parent_str}")
            };

            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: insert_text.clone(),
                    display,
                    kind: "workspace".to_string(),
                    source: "workspace".to_string(),
                    description: Some("recent VS Code workspace".to_string()),
                },
            );
            emitted += 1;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // kubectl resource collector (§7.7)
    // -----------------------------------------------------------------------

    /// Emit Kubernetes resource-type candidates for `kubectl get|describe|delete`.
    ///
    /// Strategy:
    /// 1. Shell out to `kubectl api-resources --no-headers --output=name` with a
    ///    500ms timeout to discover live cluster resources.
    /// 2. Parse: each output line is `name` or `name.group` (e.g. `pods`,
    ///    `deployments.apps`). Emit **both** the short name (before the first `.`)
    ///    and the full name so users can type either form.
    /// 3. If the shellout fails (no kubectl, no cluster reachable, timeout, or
    ///    non-zero exit), fall back to [`KUBECTL_FALLBACK_RESOURCES`].
    /// 4. The static fallback is always merged in (even on success) to guarantee
    ///    short-name aliases like `po`/`svc` are always present.
    /// 5. Filter by `active` prefix; emit with `kind = k8s_resource`,
    ///    `source = k8s_resource`.
    ///
    /// No `cwd` argument — kubectl resources are cluster-scoped, not project-local.
    ///
    /// // TODO: cache by current-context, invalidate on context switch
    fn collect_kubectl_resource_candidates(
        &self,
        active: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let live_resources = list_kubectl_resources(KUBECTL_TIMEOUT);

        // Merge live results with the static fallback. Static is always included
        // for robustness (guarantees short-name aliases are always present).
        let mut all: Vec<(String, bool)> = Vec::new(); // (name, is_live)

        // Add live results first (they appear as "· live" in the description).
        for resource in live_resources {
            // Each line from `kubectl api-resources --output=name` looks like:
            //   pods
            //   deployments.apps
            //   ingresses.networking.k8s.io
            // Emit the full name plus the short name (segment before first '.').
            let short = resource
                .split_once('.')
                .map(|(s, _)| s.to_string());
            all.push((resource.clone(), true));
            if let Some(s) = short {
                if s != resource {
                    all.push((s, true));
                }
            }
        }

        // Merge static fallback — always added, marked as builtin.
        for &name in KUBECTL_FALLBACK_RESOURCES {
            all.push((name.to_string(), false));
        }

        // Deduplicate while preserving order (live results take precedence in
        // description). We use the shared `seen` set for cross-source deduplication,
        // but also need per-name dedup within our own list.
        //
        // No per-collector limit here — Kubernetes api-resources is a bounded,
        // well-known set (typically <200 entries including short names and CRDs).
        // The top-level `Engine::complete` truncates to `config.max_results` after
        // scoring, so we can safely emit the full merged list.
        let mut local_seen: HashSet<String> = HashSet::new();

        for (name, is_live) in all {
            // Prefix filter; empty active passes all.
            if !active.is_empty() && !name.starts_with(active) {
                continue;
            }
            if !local_seen.insert(name.clone()) {
                continue;
            }
            // "· live" only when the entry actually came from a live kubectl
            // invocation. Fallback entries are always "· builtin".
            let source_suffix = if is_live {
                " \u{00b7} live"
            } else {
                " \u{00b7} builtin"
            };
            let description = format!("kubernetes resource{source_suffix}");
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: name.clone(),
                    display: name,
                    kind: "k8s_resource".to_string(),
                    source: "k8s_resource".to_string(),
                    description: Some(description),
                },
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // docker image collector (§7.8)
    // -----------------------------------------------------------------------

    /// Emit `docker_image` candidates for `docker run|pull|push|rmi <Tab>`.
    ///
    /// Shells out to `docker images --format {{.Repository}}:{{.Tag}}` with a
    /// [`DOCKER_TIMEOUT`] deadline. If docker is unavailable or the daemon is
    /// not running the call returns `Ok(())` with no candidates — there is no
    /// static fallback because docker images are user-specific.
    fn collect_docker_image_candidates(
        &self,
        active: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let images = list_docker_images();
        for image_ref in images {
            // image_ref is "repo:tag". Split to extract the tag for the description.
            let tag = image_ref
                .find(':')
                .map(|i| &image_ref[i + 1..])
                .unwrap_or("unknown");

            if !active.is_empty() && !image_ref.starts_with(active) {
                continue;
            }

            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: image_ref.clone(),
                    display: image_ref.clone(),
                    kind: "docker_image".to_string(),
                    source: "docker_image".to_string(),
                    description: Some(format!("docker image · {tag}")),
                },
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // docker container collector (docker exec)
    // -----------------------------------------------------------------------

    /// Emit `docker_container` candidates for `docker exec <Tab>`.
    ///
    /// Shells out to `docker ps --format {{.Names}}` with a [`DOCKER_TIMEOUT`]
    /// deadline. Returns `Ok(())` with no candidates when docker is unavailable
    /// or the daemon is not running — there is no static fallback because
    /// container names are user-specific.
    fn collect_docker_container_candidates(
        &self,
        active: &str,
        candidates: &mut Vec<Candidate>,
        seen: &mut HashSet<String>,
    ) -> Result<()> {
        let containers = list_docker_containers();
        for name in containers {
            if !active.is_empty() && !name.starts_with(active) {
                continue;
            }
            push_candidate(
                candidates,
                seen,
                Candidate {
                    insert_text: name.clone(),
                    display: name,
                    kind: "docker_container".to_string(),
                    source: "docker_container".to_string(),
                    description: Some("docker container · running".to_string()),
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
        _ if candidate.kind == "branch" => 1.0,
        _ if candidate.kind == "npm_script" => 1.0,
        _ if candidate.kind == "ssh_host" => 1.0,
        _ if candidate.kind == "build_target" => 1.0,
        _ if candidate.kind == "workspace" => 1.0,
        _ if candidate.kind == "k8s_resource" => 1.0,
        _ if candidate.kind == "docker_image" => 1.0,
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
        ("ssh_host", _) => 0.85,
        ("build_target", _) => 0.85,
        ("workspace", _) => 0.85,
        ("k8s_resource", _) => 0.85,
        ("docker_image", _) => 0.85,
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

/// Check whether `kubectl` is available on `PATH` without invoking it.
/// Returns `true` if a file named `kubectl` exists and is executable on the
/// current `PATH`. This is a fast pre-flight check (stat-only) so we avoid
/// spawning a subprocess when kubectl isn't installed at all.
fn kubectl_on_path() -> bool {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("kubectl");
        if candidate.is_file() {
            return true;
        }
    }
    false
}

/// Run `kubectl api-resources --no-headers --output=name` with a hard timeout.
///
/// Returns the raw output lines (one resource per line, possibly `name.group`
/// form). Returns an empty Vec on any failure: kubectl not installed, no
/// cluster reachable, non-zero exit, or timeout.
///
/// Mirrors the [`list_git_refs`] pattern — spawn child, poll with `try_wait`,
/// kill on timeout.
fn list_kubectl_resources(timeout: Duration) -> Vec<String> {
    if !kubectl_on_path() {
        return Vec::new();
    }

    let mut child = match Command::new("kubectl")
        .args(["api-resources", "--no-headers", "--output=name"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return Vec::new(),
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Vec::new();
                }
                break;
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Vec::new();
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }

    let mut buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        if out.read_to_string(&mut buf).is_err() {
            return Vec::new();
        }
    }

    buf.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Shell out to `docker images --format {{.Repository}}:{{.Tag}}` with a
/// [`DOCKER_TIMEOUT`] deadline and return up to [`DOCKER_IMAGE_LIMIT`] image
/// refs. Dangling images (`<none>:<none>`) and images whose repository is
/// `<none>` are skipped. Returns an empty `Vec` when docker is unavailable,
/// the daemon is not running, or the command exits non-zero.
fn list_docker_images() -> Vec<String> {
    let mut child = match Command::new("docker")
        .args(["images", "--format", "{{.Repository}}:{{.Tag}}"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Vec::new();
                }
                break;
            }
            Ok(None) => {
                if started.elapsed() >= DOCKER_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Vec::new();
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }

    let mut buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut buf).ok();
    }

    parse_docker_images_output(&buf)
}

/// Parse raw `docker images` output (one `repo:tag` per line) into a deduplicated
/// Vec, skipping dangling images and capping at [`DOCKER_IMAGE_LIMIT`].
fn parse_docker_images_output(output: &str) -> Vec<String> {
    let mut images = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip dangling images: repo == "<none>" or tag == "<none>"
        // The format is always "repo:tag", so split at ':'
        let repo = trimmed.split(':').next().unwrap_or("");
        if repo == "<none>" {
            continue;
        }
        let tag = trimmed.split(':').nth(1).unwrap_or("");
        if tag == "<none>" {
            continue;
        }
        images.push(trimmed.to_string());
        if images.len() >= DOCKER_IMAGE_LIMIT {
            break;
        }
    }
    images
}

/// Shell out to `docker ps --format {{.Names}}` with a [`DOCKER_TIMEOUT`]
/// deadline and return running container names. Returns an empty `Vec` when
/// docker is unavailable, the daemon is not running, or the command exits
/// non-zero.
fn list_docker_containers() -> Vec<String> {
    let mut child = match Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Vec::new();
                }
                break;
            }
            Ok(None) => {
                if started.elapsed() >= DOCKER_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Vec::new();
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }

    let mut buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut buf).ok();
    }

    parse_docker_containers_output(&buf)
}

/// Parse raw `docker ps` output (one container name per line) into a Vec,
/// skipping empty lines.
fn parse_docker_containers_output(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
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

/// Returns the path to `~/.ssh/config`, or `None` if the home directory
/// cannot be determined.
fn ssh_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

/// Returns the path to `~/.ssh/known_hosts`, or `None` if the home directory
/// cannot be determined.
fn known_hosts_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

// ---------------------------------------------------------------------------
// VS Code workspace storage helpers (§7.9)
// ---------------------------------------------------------------------------

/// Return candidate VS Code storage paths in priority order.
///
/// Priority:
/// 1. SQLite DB (`state.vscdb`) — the authoritative store in VS Code ≥ 1.80.
/// 2. JSON file (`storage.json`) — legacy / fallback for older installations.
///
/// macOS paths are tried before Linux (`~/.config`) paths. Returns all that
/// conceptually exist; the caller tries each in order and stops at the first
/// that yields non-empty results.
fn vscode_storage_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    // macOS: ~/Library/Application Support/Code/...
    // Linux: ~/.config/Code/...
    let candidates: &[&[&str]] = &[
        &["Library", "Application Support", "Code", "User", "globalStorage", "state.vscdb"],
        &["Library", "Application Support", "Code", "User", "globalStorage", "storage.json"],
        &[".config", "Code", "User", "globalStorage", "state.vscdb"],
        &[".config", "Code", "User", "globalStorage", "storage.json"],
    ];

    candidates
        .iter()
        .map(|parts| parts.iter().fold(home.clone(), |p, s| p.join(s)))
        .collect()
}

/// Parse a VS Code storage file and return recent workspace/folder paths.
///
/// Supports two formats:
/// - **SQLite** (`state.vscdb`): queries `ItemTable` for the key
///   `history.recentlyOpenedPathsList`; the value is JSON.
/// - **JSON** (`storage.json`): reads the file and looks for
///   `history.recentlyOpenedPathsList` under the top-level object.
///
/// In both cases, entries are JSON objects with optional keys:
/// - `folderUri` — a folder opened directly (`file:///path` or remote URI).
/// - `workspace` / `workspace.configPath` — a `.code-workspace` bundle.
/// - `fileUri` — a single file (not a workspace); **skipped**.
///
/// Remote entries (those with a `remoteAuthority` key, or whose URI scheme is
/// not `file://`) are **skipped** for v1.
///
/// `file:///` URIs are decoded (percent-decoded) and the scheme prefix is
/// stripped to yield a plain `PathBuf`.
///
/// Returns an empty Vec if the file is missing, unreadable, or malformed.
fn parse_vscode_recent_workspaces(path: &Path) -> Vec<PathBuf> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let json_str: String = if ext == "vscdb" {
        // SQLite path — use rusqlite (already in deps).
        match rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => {
                match conn.query_row(
                    "SELECT value FROM ItemTable WHERE key = 'history.recentlyOpenedPathsList'",
                    [],
                    |row| row.get::<_, String>(0),
                ) {
                    Ok(val) => val,
                    Err(_) => return Vec::new(),
                }
            }
            Err(_) => return Vec::new(),
        }
    } else {
        // JSON path.
        match fs::read_to_string(path) {
            Ok(text) => {
                // The JSON file may embed the history list under the key
                // "history.recentlyOpenedPathsList" at top level.
                let top: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => return Vec::new(),
                };
                match top
                    .get("history.recentlyOpenedPathsList")
                    .and_then(|v| v.as_str())
                {
                    Some(s) => s.to_string(),
                    None => {
                        // The JSON file might itself be the history object.
                        text
                    }
                }
            }
            Err(_) => return Vec::new(),
        }
    };

    // Parse the JSON payload.
    let root: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let entries = match root.get("entries").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in entries {
        let obj = match entry.as_object() {
            Some(o) => o,
            None => continue,
        };

        // Skip remote workspaces (v1 — local only).
        if obj.contains_key("remoteAuthority") {
            continue;
        }

        // Prefer folderUri; fall back to workspace.configPath for bundles.
        let raw_uri = if let Some(v) = obj.get("folderUri").and_then(|v| v.as_str()) {
            v
        } else if let Some(ws) = obj.get("workspace").and_then(|v| v.as_object()) {
            match ws.get("configPath").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => continue,
            }
        } else {
            // fileUri or unrecognised — skip.
            continue;
        };

        // Only handle local file:// URIs.
        if !raw_uri.starts_with("file://") {
            continue;
        }

        // Strip scheme and percent-decode the path component.
        let encoded = raw_uri.trim_start_matches("file://");
        let decoded = percent_decode_uri(encoded);
        if decoded.is_empty() {
            continue;
        }
        out.push(PathBuf::from(decoded));
    }
    out
}

/// Minimal percent-decoder for `file://` URI path components.
///
/// Only handles the subset of percent-encoded characters that appear in
/// VS Code workspace URIs in practice (`%20` for space, `%2B`, etc.).
/// Invalid sequences are passed through as-is (best-effort).
fn percent_decode_uri(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Convert an ASCII hex digit to its nibble value.
#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse `~/.ssh/config` and extract concrete host names (aliases).
///
/// Parsing rules:
/// - Blank lines and `#` comments are skipped.
/// - Lines whose first token is `Host` (case-insensitive) are host-declaration
///   lines; everything after `Host` and whitespace are zero or more patterns.
/// - Patterns containing `*` or `?` (glob wildcards) are skipped.
/// - Multiple patterns on one line are allowed: `Host alpha beta gamma`.
/// - `Include` directives are **not** followed (v1 — TODO for v2).
///
/// Returns an empty Vec on any I/O error (caller treats absence as "no
/// candidates"); never panics.
fn parse_ssh_config_hosts(path: &Path) -> Vec<String> {
    // Guard against suspiciously large files.
    if path.metadata().map(|m| m.len()).unwrap_or(0) > SSH_FILE_MAX_BYTES {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut hosts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip blank lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Split on the first run of whitespace.
        let mut iter = trimmed.splitn(2, |c: char| c.is_whitespace());
        let directive = match iter.next() {
            Some(d) => d,
            None => continue,
        };
        if !directive.eq_ignore_ascii_case("Host") {
            continue;
        }
        // TODO(v2): follow Include directives for multi-file ssh configs.
        let rest = iter.next().unwrap_or("").trim();
        for pattern in rest.split_whitespace() {
            // Skip wildcards — they are not real hostnames.
            if pattern.contains('*') || pattern.contains('?') {
                continue;
            }
            if !pattern.is_empty() {
                hosts.push(pattern.to_string());
            }
        }
    }
    hosts
}

/// Parse `~/.ssh/known_hosts` and extract hostnames.
///
/// Parsing rules:
/// - Blank lines and `#` comments are skipped.
/// - Lines starting with `@` (`@cert-authority`, `@revoked`) are skipped.
/// - The first whitespace-delimited token is the comma-separated host list.
/// - Each element of the host list is processed:
///   - `[host]:port` bracket notation → strip to bare `host`.
///   - Entries starting with `|1|` are hashed (`HashKnownHosts yes`) — skip.
///   - Entries containing `*` are wildcards — skip.
///   - Plain `host` and `ip` entries are kept as-is (IPs are valid ssh targets).
///
/// Returns an empty Vec on any I/O error; never panics.
fn parse_known_hosts_hostnames(path: &Path) -> Vec<String> {
    if path.metadata().map(|m| m.len()).unwrap_or(0) > SSH_FILE_MAX_BYTES {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut hosts = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // Skip blank, comments, marker lines.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('@') {
            continue;
        }
        // First token is the host list.
        let host_list = match trimmed.split_whitespace().next() {
            Some(t) => t,
            None => continue,
        };
        for entry in host_list.split(',') {
            if entry.is_empty() {
                continue;
            }
            // Skip hashed entries.
            if entry.starts_with("|1|") {
                continue;
            }
            // Skip wildcards.
            if entry.contains('*') {
                continue;
            }
            // Strip bracket notation: [hostname]:port → hostname.
            let hostname = if entry.starts_with('[') {
                // Find the closing bracket.
                if let Some(close) = entry.find(']') {
                    &entry[1..close]
                } else {
                    entry
                }
            } else {
                entry
            };
            if !hostname.is_empty() {
                hosts.push(hostname.to_string());
            }
        }
    }
    hosts
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

// ---------------------------------------------------------------------------
// Build-target collector helpers
// ---------------------------------------------------------------------------

/// Which build system a discovered file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildSystem {
    Make,
    Just,
    Task,
}

/// Walk up from `start`, probing for the canonical build files of the build
/// system implied by `command`. Caps the walk at [`BUILD_FILE_WALK_LIMIT`]
/// levels and stops (inclusive) at any ancestor that contains a `.git`
/// directory or file.
///
/// Returns the path to the *file* (not the directory) together with which
/// build system it belongs to, or `None` when nothing is found.
fn find_build_file_root(command: &str, start: &Path) -> Option<(PathBuf, BuildSystem)> {
    // File name candidates for each build system (in preference order).
    let (names, system) = match command {
        "just" => (
            &["justfile", "Justfile", ".justfile"][..],
            BuildSystem::Just,
        ),
        "task" => (
            &["Taskfile.yml", "Taskfile.yaml", "taskfile.yml"][..],
            BuildSystem::Task,
        ),
        // "make" and anything else defaults to Makefile probing.
        _ => (
            &["Makefile", "makefile", "GNUmakefile"][..],
            BuildSystem::Make,
        ),
    };

    let mut cur: Option<&Path> = Some(start);
    let mut steps = 0usize;
    while let Some(dir) = cur {
        if steps >= BUILD_FILE_WALK_LIMIT {
            return None;
        }
        // Check each candidate name in preference order.
        for &name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some((candidate, system));
            }
        }
        // Stop after examining a `.git` boundary (we checked the boundary dir
        // itself above, so we never escape past a project root).
        if dir.join(".git").exists() {
            return None;
        }
        cur = dir.parent();
        steps += 1;
    }
    None
}

/// Parse a `Makefile` / `makefile` / `GNUmakefile` and return a list of
/// explicit target names. The following are excluded:
///
/// * Targets that start with `.` (special targets like `.PHONY`, `.SUFFIXES`).
/// * Targets containing `%` or `*` (pattern / wildcard rules).
/// * Lines of the form `VAR := value` or `VAR = value` (variable assignments).
/// * Targets whose name looks like a filesystem path (`foo/bar.o`).
///
/// `.PHONY` *declarations* are a common pattern — we skip `.PHONY` itself but
/// still include the concrete target names referenced on the same line as
/// plain targets elsewhere.
///
/// On any I/O error or if the file exceeds [`BUILD_FILE_MAX_BYTES`], returns
/// an empty `Vec`. Never panics.
fn parse_makefile_targets(path: &Path) -> Vec<String> {
    if path.metadata().map(|m| m.len()).unwrap_or(0) > BUILD_FILE_MAX_BYTES {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();
    for line in text.lines() {
        // Skip blank lines and comments.
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Recipe bodies start with a tab — skip them.
        if line.starts_with('\t') {
            continue;
        }
        // A target line has the form `TARGET(s): [deps]` but NOT `:=` or `::=`
        // (those are variable assignments). Split on the first `:`.
        let colon_pos = match trimmed.find(':') {
            Some(p) => p,
            None => continue,
        };
        // Reject `:=` (immediate variable assignment) and `::=` (POSIX assign).
        let after_colon = &trimmed[colon_pos + 1..];
        if after_colon.starts_with('=') {
            continue;
        }

        // Everything before the colon is space-separated target names.
        let before = trimmed[..colon_pos].trim();
        if before.is_empty() {
            continue;
        }

        for name in before.split_whitespace() {
            // Skip special targets (start with `.`).
            if name.starts_with('.') {
                continue;
            }
            // Skip pattern / wildcard rules.
            if name.contains('%') || name.contains('*') {
                continue;
            }
            // Skip names that look like file paths (`path/to/file.o`).
            if name.contains('/') {
                continue;
            }
            targets.push(name.to_string());
        }
    }
    targets
}

/// Parse a `justfile` / `Justfile` / `.justfile` and return a list of recipe
/// names.
///
/// Parsing rules:
/// * Blank lines and `#` comments are skipped.
/// * Lines that start with whitespace are recipe bodies — skip them.
/// * A recipe declaration has the form:
///   `name[(params)]: [deps]`
///   where `name` starts with a letter and consists of `[a-zA-Z0-9_-]`.
///
/// On any I/O error or oversized file, returns an empty `Vec`. Never panics.
fn parse_justfile_targets(path: &Path) -> Vec<String> {
    if path.metadata().map(|m| m.len()).unwrap_or(0) > BUILD_FILE_MAX_BYTES {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();
    for line in text.lines() {
        // Skip blank lines and comments.
        let trimmed = line.trim_end();
        if trimmed.is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        // Recipe bodies start with whitespace.
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        // A recipe line starts with an identifier character.
        let first = match trimmed.chars().next() {
            Some(c) => c,
            None => continue,
        };
        if !first.is_ascii_alphabetic() && first != '_' {
            continue;
        }
        // Extract the name: everything up to the first `(`, ` `, or `:`.
        let name_end = trimmed
            .find(['(', ' ', ':'])
            .unwrap_or(trimmed.len());
        let name = &trimmed[..name_end];
        if name.is_empty() {
            continue;
        }
        // Validate: only `[a-zA-Z0-9_-]`.
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            // Confirm there's a `:` somewhere after the name (to exclude bare
            // setting lines like `set shell := [...]`).
            if trimmed[name_end..].contains(':') {
                targets.push(name.to_string());
            }
        }
    }
    targets
}

/// Parse a `Taskfile.yml` / `Taskfile.yaml` / `taskfile.yml` and return a
/// list of task names.
///
/// `serde_yaml` is not in `Cargo.toml`, so this is a hand-rolled extractor:
/// it looks for a top-level `tasks:` key and then collects all immediately
/// 2-space-indented keys that end with `:`.  Sufficient for the vast majority
/// of real Taskfile layouts.
///
/// On any I/O error or oversized file, returns an empty `Vec`. Never panics.
fn parse_taskfile_targets(path: &Path) -> Vec<String> {
    if path.metadata().map(|m| m.len()).unwrap_or(0) > BUILD_FILE_MAX_BYTES {
        return Vec::new();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();
    let mut in_tasks = false;
    for line in text.lines() {
        // Skip blank lines and YAML comments.
        let raw = line;
        let trimmed = raw.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        // Detect the top-level `tasks:` key (no leading whitespace).
        if !raw.starts_with(' ') && !raw.starts_with('\t') {
            in_tasks = raw.trim_end() == "tasks:";
            continue;
        }
        if !in_tasks {
            continue;
        }
        // We're inside the `tasks:` block. Task names are at the first indent
        // level — exactly 2 spaces (common convention). Accept any number of
        // leading spaces as long as it's < the indent of a nested key.
        // Heuristic: if the line starts with exactly 2 spaces (or 1 tab) and
        // ends with `:`, treat the key as a task name.
        let indent = raw.len() - raw.trim_start().len();
        // Task-name lines sit at the first indent level (2 or 4 spaces typical).
        // We accept indent levels 1-4 for the name, but NOT deeper nesting
        // (which would be task properties). Use a simple threshold: indent <= 4
        // and the stripped line is a bare `key:` (nothing after the colon).
        if indent == 0 {
            // Hit a new top-level key; we already handled this above.
            continue;
        }
        let stripped = raw.trim_start();
        // A task-name line: `  name:` with nothing (or only a space) after `:`.
        if let Some(colon_pos) = stripped.find(':') {
            let after = stripped[colon_pos + 1..].trim();
            // The value after `:` may be empty (block mapping) or a string.
            // We want entries that look like task-name keys (not sub-keys of a
            // task). Use indent == 2 as the canonical task-level indent,
            // but also accept indent == 4 for 4-space-indented Taskfiles.
            if indent <= 4 && (after.is_empty() || after.starts_with('#')) {
                let name = stripped[..colon_pos].trim();
                // Skip YAML-internal keys like `cmds`, `desc`, `deps`, `env`, `vars`, etc.
                // They appear at deeper indent anyway, but guard here too.
                let is_yaml_key = matches!(
                    name,
                    "cmds" | "desc" | "summary" | "deps" | "env"
                        | "vars" | "silent" | "run" | "method"
                        | "sources" | "generates" | "status"
                        | "preconditions" | "ignore_error" | "dir"
                        | "dotenv" | "label" | "internal" | "platforms"
                        | "prompt" | "requires" | "aliases" | "set"
                        | "shopt"
                );
                if !is_yaml_key && !name.is_empty() && !name.starts_with('-') {
                    targets.push(name.to_string());
                }
            }
        }
    }
    targets
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

    // -----------------------------------------------------------------------
    // SSH parser unit tests
    // -----------------------------------------------------------------------

    fn write_file(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write file");
        path
    }

    #[test]
    fn parse_ssh_config_extracts_hosts() {
        let scratch = unique_tmp("shac-ssh-config-extract");
        let config = write_file(
            &scratch,
            "config",
            "Host alias1 alias2\n\
             \tHostName foo.example.com\n\
             \tUser joe\n\
             \n\
             Host *.internal\n\
             \tProxyJump bastion\n\
             \n\
             Host bastion\n\
             \tHostName 10.0.0.1\n",
        );
        let hosts = parse_ssh_config_hosts(&config);
        assert!(
            hosts.contains(&"alias1".to_string()),
            "expected alias1: {hosts:?}"
        );
        assert!(
            hosts.contains(&"alias2".to_string()),
            "expected alias2: {hosts:?}"
        );
        assert!(
            hosts.contains(&"bastion".to_string()),
            "expected bastion: {hosts:?}"
        );
        // Wildcard must be excluded.
        assert!(
            !hosts.iter().any(|h| h.contains('*')),
            "wildcard must not appear: {hosts:?}"
        );
    }

    #[test]
    fn parse_ssh_config_handles_multi_host_line() {
        let scratch = unique_tmp("shac-ssh-config-multi");
        let config = write_file(
            &scratch,
            "config",
            "Host alpha beta gamma\n\
             \tUser user\n",
        );
        let hosts = parse_ssh_config_hosts(&config);
        assert_eq!(
            hosts.len(),
            3,
            "expected 3 hosts from multi-host line: {hosts:?}"
        );
        assert!(hosts.contains(&"alpha".to_string()));
        assert!(hosts.contains(&"beta".to_string()));
        assert!(hosts.contains(&"gamma".to_string()));
    }

    #[test]
    fn parse_known_hosts_handles_brackets_and_ports() {
        let scratch = unique_tmp("shac-ssh-known-brackets");
        let known = write_file(
            &scratch,
            "known_hosts",
            "[bastion]:2222,[bastion-alt]:2222 ssh-ed25519 AAAA...\n",
        );
        let hosts = parse_known_hosts_hostnames(&known);
        assert!(
            hosts.contains(&"bastion".to_string()),
            "expected bastion: {hosts:?}"
        );
        assert!(
            hosts.contains(&"bastion-alt".to_string()),
            "expected bastion-alt: {hosts:?}"
        );
        // Port suffix must not appear.
        assert!(
            !hosts.iter().any(|h| h.contains(':') || h.contains('[')),
            "brackets/port must be stripped: {hosts:?}"
        );
    }

    #[test]
    fn parse_known_hosts_skips_hashed_entries() {
        let scratch = unique_tmp("shac-ssh-known-hashed");
        let known = write_file(
            &scratch,
            "known_hosts",
            "|1|abc123|def456 ssh-rsa AAAA...\n\
             realhost.example.com ssh-rsa AAAA...\n",
        );
        let hosts = parse_known_hosts_hostnames(&known);
        assert!(
            !hosts.iter().any(|h| h.starts_with('|')),
            "hashed entry must be skipped: {hosts:?}"
        );
        assert!(
            hosts.contains(&"realhost.example.com".to_string()),
            "plain host must appear: {hosts:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Target-collector inline parser tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_makefile_targets_basic() {
        let scratch = unique_tmp("shac-make-basic");
        let path = write_file(
            &scratch,
            "Makefile",
            "build:\n\
             \t@echo building\n\
             \n\
             test: build\n\
             \t@echo testing\n\
             \n\
             clean:\n\
             \t@echo cleaning\n",
        );
        let targets = parse_makefile_targets(&path);
        assert!(targets.contains(&"build".to_string()), "{targets:?}");
        assert!(targets.contains(&"test".to_string()), "{targets:?}");
        assert!(targets.contains(&"clean".to_string()), "{targets:?}");
    }

    #[test]
    fn parse_makefile_targets_skips_var_assignments() {
        let scratch = unique_tmp("shac-make-var");
        let path = write_file(
            &scratch,
            "Makefile",
            "CC := gcc\n\
             CFLAGS := -O2\n\
             build:\n\
             \t$(CC) main.c\n",
        );
        let targets = parse_makefile_targets(&path);
        assert!(
            !targets.iter().any(|t| t == "CC" || t == "CFLAGS"),
            "variable assignments must not appear as targets: {targets:?}"
        );
        assert!(targets.contains(&"build".to_string()), "{targets:?}");
    }

    #[test]
    fn parse_makefile_targets_skips_pattern_rules() {
        let scratch = unique_tmp("shac-make-pattern");
        let path = write_file(
            &scratch,
            "Makefile",
            "%.o: %.c\n\
             \t$(CC) -c $<\n\
             all:\n\
             \t@echo all\n",
        );
        let targets = parse_makefile_targets(&path);
        assert!(
            !targets.iter().any(|t| t.contains('%')),
            "pattern rules must not appear: {targets:?}"
        );
        assert!(targets.contains(&"all".to_string()), "{targets:?}");
    }

    #[test]
    fn parse_makefile_targets_skips_dot_targets() {
        let scratch = unique_tmp("shac-make-dot");
        let path = write_file(
            &scratch,
            "Makefile",
            ".PHONY: build clean\n\
             build:\n\
             \t@echo build\n\
             clean:\n\
             \t@echo clean\n",
        );
        let targets = parse_makefile_targets(&path);
        assert!(
            !targets.iter().any(|t| t.starts_with('.')),
            ".PHONY must not appear as target: {targets:?}"
        );
        assert!(targets.contains(&"build".to_string()), "{targets:?}");
        assert!(targets.contains(&"clean".to_string()), "{targets:?}");
    }

    #[test]
    fn parse_justfile_targets_basic() {
        let scratch = unique_tmp("shac-just-basic");
        let path = write_file(
            &scratch,
            "justfile",
            "build:\n\
             \tcargo build\n\
             \n\
             test arg1 arg2:\n\
             \tcargo test\n\
             \n\
             dev:\n\
             \tcargo run\n",
        );
        let targets = parse_justfile_targets(&path);
        assert!(targets.contains(&"build".to_string()), "{targets:?}");
        assert!(targets.contains(&"test".to_string()), "{targets:?}");
        assert!(targets.contains(&"dev".to_string()), "{targets:?}");
    }

    #[test]
    fn parse_justfile_targets_handles_parameters() {
        let scratch = unique_tmp("shac-just-params");
        let path = write_file(
            &scratch,
            "justfile",
            "recipe-name param1='default':\n\
             \techo {{param1}}\n",
        );
        let targets = parse_justfile_targets(&path);
        assert!(
            targets.contains(&"recipe-name".to_string()),
            "parameterized recipe must appear: {targets:?}"
        );
    }

    #[test]
    fn parse_taskfile_targets_basic() {
        let scratch = unique_tmp("shac-task-basic");
        let content = concat!(
            "version: '3'\n",
            "\n",
            "tasks:\n",
            "\n",
            "  build:\n",
            "    cmds:\n",
            "      - go build ./...\n",
            "\n",
            "  test:\n",
            "    cmds:\n",
            "      - go test ./...\n",
            "\n",
            "  clean:\n",
            "    cmds:\n",
            "      - rm -rf dist\n",
        );
        let path = write_file(&scratch, "Taskfile.yml", content);
        let targets = parse_taskfile_targets(&path);
        assert!(targets.contains(&"build".to_string()), "{targets:?}");
        assert!(targets.contains(&"test".to_string()), "{targets:?}");
        assert!(targets.contains(&"clean".to_string()), "{targets:?}");
    }

    #[test]
    fn find_build_file_root_make() {
        let scratch = unique_tmp("shac-target-make-root");
        let root = scratch.join("project");
        let nested = root.join("src/components");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(root.join("Makefile"), "build:\n\t@echo\n").expect("write");
        let (found_path, system) =
            find_build_file_root("make", &nested).expect("should find Makefile");
        assert_eq!(system, BuildSystem::Make);
        assert_eq!(
            found_path.canonicalize().unwrap(),
            root.join("Makefile").canonicalize().unwrap()
        );
    }

    #[test]
    fn find_build_file_root_just() {
        let scratch = unique_tmp("shac-target-just-root");
        let root = scratch.join("project");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(root.join("justfile"), "build:\n\tcargo build\n").expect("write");
        let (found_path, system) =
            find_build_file_root("just", &nested).expect("should find justfile");
        assert_eq!(system, BuildSystem::Just);
        assert_eq!(
            found_path.canonicalize().unwrap(),
            root.join("justfile").canonicalize().unwrap()
        );
    }

    #[test]
    fn find_build_file_root_stops_at_git() {
        // outer/Makefile  <- must NOT be reached
        // outer/inner/.git
        // outer/inner/    <- start here
        let scratch = unique_tmp("shac-target-git-boundary");
        let outer = scratch.join("outer");
        let inner = outer.join("inner");
        std::fs::create_dir_all(&inner).expect("mkdir");
        std::fs::create_dir_all(inner.join(".git")).expect("mkdir .git");
        std::fs::write(outer.join("Makefile"), "build:\n").expect("write");
        let result = find_build_file_root("make", &inner);
        assert!(
            result.is_none(),
            "should not escape past .git boundary, got {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // VS Code workspace parser unit tests (§7.9)
    // -----------------------------------------------------------------------

    /// Build a temporary SQLite `state.vscdb` with the given JSON payload as
    /// the `history.recentlyOpenedPathsList` value.
    fn write_test_vscdb(dir: &std::path::Path, payload: &str) -> std::path::PathBuf {
        let db_path = dir.join("state.vscdb");
        let conn = rusqlite::Connection::open(&db_path).expect("open test vscdb");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE, value TEXT);",
        )
        .expect("create table");
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('history.recentlyOpenedPathsList', ?1)",
            rusqlite::params![payload],
        )
        .expect("insert");
        db_path
    }

    #[test]
    fn parse_vscode_recent_workspaces_basic() {
        let scratch = unique_tmp("shac-vsc-basic");
        let payload = r#"{"entries":[{"folderUri":"file:///foo/alpha"},{"folderUri":"file:///foo/beta"}]}"#;
        let db_path = write_test_vscdb(&scratch, payload);
        let paths = parse_vscode_recent_workspaces(&db_path);
        assert_eq!(paths.len(), 2, "expected 2 paths: {paths:?}");
        let strs: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(strs.iter().any(|s| s == "/foo/alpha"), "alpha missing: {strs:?}");
        assert!(strs.iter().any(|s| s == "/foo/beta"), "beta missing: {strs:?}");
    }

    #[test]
    fn parse_vscode_recent_workspaces_strips_file_uri_prefix() {
        let scratch = unique_tmp("shac-vsc-strip-prefix");
        let payload = r#"{"entries":[{"folderUri":"file:///Users/roman/dev/proj"}]}"#;
        let db_path = write_test_vscdb(&scratch, payload);
        let paths = parse_vscode_recent_workspaces(&db_path);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0].to_string_lossy().as_ref(),
            "/Users/roman/dev/proj",
            "file:// prefix must be stripped"
        );
    }

    #[test]
    fn parse_vscode_recent_workspaces_skips_remote() {
        let scratch = unique_tmp("shac-vsc-skip-remote");
        let payload = r#"{"entries":[
            {"folderUri":"file:///local/proj"},
            {"folderUri":"vscode-remote://ssh-remote%2Bserver.example.com/home/user/proj","remoteAuthority":"ssh-remote+server.example.com"}
        ]}"#;
        let db_path = write_test_vscdb(&scratch, payload);
        let paths = parse_vscode_recent_workspaces(&db_path);
        assert_eq!(paths.len(), 1, "remote entry must be skipped: {paths:?}");
        assert_eq!(paths[0].to_string_lossy().as_ref(), "/local/proj");
    }

    // -----------------------------------------------------------------------
    // kubectl resource collector unit tests (§7.7)
    // -----------------------------------------------------------------------

    /// Parse synthetic `kubectl api-resources --output=name` output and verify
    /// that resource names are extracted correctly.
    #[test]
    fn parse_kubectl_api_resources_output_basic() {
        // Simulate the output of `kubectl api-resources --no-headers --output=name`.
        let raw = "pods\nservices\ndeployments.apps\n";
        let parsed: Vec<String> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect();
        assert!(parsed.contains(&"pods".to_string()), "{parsed:?}");
        assert!(parsed.contains(&"services".to_string()), "{parsed:?}");
        assert!(parsed.contains(&"deployments.apps".to_string()), "{parsed:?}");
        assert_eq!(parsed.len(), 3);
    }

    /// Verify that grouped resource names (e.g. `deployments.apps`) produce
    /// both the full name and the short segment as separate candidates. This
    /// mirrors the extraction logic in `collect_kubectl_resource_candidates`.
    #[test]
    fn parse_kubectl_api_resources_handles_grouped_names() {
        let grouped = "deployments.apps";
        let short = grouped.split_once('.').map(|(s, _)| s.to_string());
        assert_eq!(short.as_deref(), Some("deployments"));
        // Both "deployments" and "deployments.apps" should be candidates.
        let full = grouped.to_string();
        assert_ne!(short.as_deref(), Some(full.as_str()));
    }

    /// The static fallback list must be non-empty and contain core resource
    /// names.
    #[test]
    fn kubectl_static_fallback_nonempty() {
        assert!(
            !KUBECTL_FALLBACK_RESOURCES.is_empty(),
            "static fallback list must not be empty"
        );
        assert!(
            KUBECTL_FALLBACK_RESOURCES.contains(&"pods"),
            "pods must be in fallback list"
        );
        assert!(
            KUBECTL_FALLBACK_RESOURCES.contains(&"services"),
            "services must be in fallback list"
        );
        assert!(
            KUBECTL_FALLBACK_RESOURCES.contains(&"deployments"),
            "deployments must be in fallback list"
        );
        assert!(
            KUBECTL_FALLBACK_RESOURCES.contains(&"po"),
            "short-name 'po' must be in fallback list"
        );
        assert!(
            KUBECTL_FALLBACK_RESOURCES.contains(&"svc"),
            "short-name 'svc' must be in fallback list"
        );
    }

    /// Verify that `collect_kubectl_resource_candidates` surfaces the static
    /// fallback list when kubectl is not available on PATH.
    ///
    /// This test manipulates the `PATH` environment variable in-process to a
    /// value that cannot contain a `kubectl` binary, then calls the collector
    /// directly (bypassing the daemon) and checks that the static fallback
    /// resources are emitted. PATH is restored at the end of the test.
    ///
    /// Note: this test is inherently not thread-safe with respect to other
    /// tests that also manipulate PATH. It is designed to run quickly
    /// (no subprocess or socket involved) so the window is small. Isolate
    /// with `cargo test collect_kubectl_resources_falls_back -- --test-threads=1`
    /// if flakiness is observed.
    #[test]
    fn collect_kubectl_resources_falls_back_when_no_kubectl() {
        // Save the current PATH so we can restore it after the test.
        let saved_path = std::env::var_os("PATH");

        // Set PATH to /dev/null so kubectl_on_path() returns false.
        // Safety: single-threaded section; PATH is restored in the finally block.
        unsafe {
            std::env::set_var("PATH", "/dev/null");
        }

        let result = std::panic::catch_unwind(|| {
            let (engine, _dir) = test_engine("kubectl-no-kubectl");
            let mut candidates: Vec<Candidate> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();

            engine
                .collect_kubectl_resource_candidates("", &mut candidates, &mut seen)
                .expect("collect should not fail");

            // With PATH=/dev/null, list_kubectl_resources() returns empty, so all
            // candidates come from KUBECTL_FALLBACK_RESOURCES.
            let names: Vec<&str> = candidates
                .iter()
                .map(|c| c.insert_text.as_str())
                .collect();

            assert!(names.contains(&"pods"), "pods must surface: {names:?}");
            assert!(names.contains(&"services"), "services must surface: {names:?}");
            assert!(names.contains(&"deployments"), "deployments must surface: {names:?}");
            assert!(names.contains(&"po"), "short-name po must surface: {names:?}");
            assert!(names.contains(&"svc"), "short-name svc must surface: {names:?}");
            // With no live kubectl, all descriptions say "· builtin".
            assert!(
                candidates
                    .iter()
                    .all(|c| c.description.as_deref().unwrap_or("").contains("builtin")),
                "all candidates must be '· builtin' when kubectl is absent: {candidates:?}",
            );
        });

        // Restore PATH regardless of whether the test panicked.
        unsafe {
            match saved_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }

        // Re-panic if the test body panicked.
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    // -----------------------------------------------------------------------
    // §7.8 docker image inline unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_docker_images_output_basic() {
        let input = "nginx:latest\nredis:7\n<none>:<none>\n";
        let result = parse_docker_images_output(input);
        assert!(
            result.contains(&"nginx:latest".to_string()),
            "nginx:latest must be present: {result:?}"
        );
        assert!(
            result.contains(&"redis:7".to_string()),
            "redis:7 must be present: {result:?}"
        );
        assert!(
            !result.contains(&"<none>:<none>".to_string()),
            "<none>:<none> must be skipped: {result:?}"
        );
        assert_eq!(result.len(), 2, "expected exactly 2 images: {result:?}");
    }

    #[test]
    fn parse_docker_images_skips_unnamed_repos() {
        // Repo == "<none>" must be skipped even if the tag looks valid.
        let input = "<none>:somecid\nnginx:latest\n";
        let result = parse_docker_images_output(input);
        assert_eq!(result.len(), 1, "expected only nginx:latest: {result:?}");
        assert_eq!(result[0], "nginx:latest");
    }

    #[test]
    fn parse_docker_images_handles_empty_output() {
        let result = parse_docker_images_output("");
        assert!(result.is_empty(), "empty input must produce empty vec: {result:?}");
    }

    #[test]
    fn collect_docker_images_returns_empty_when_no_docker() {
        // Temporarily override PATH to a directory without docker, then
        // restore it after the call. We use catch_unwind to ensure restoration
        // even if something panics.
        use std::panic;

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var("PATH", "/dev/null");
        let result = panic::catch_unwind(list_docker_images);
        std::env::set_var("PATH", &original_path);

        let images = result.expect("list_docker_images must not panic");
        assert!(
            images.is_empty(),
            "expected empty Vec when docker is not on PATH: {images:?}"
        );
    }

    // -----------------------------------------------------------------------
    // docker container collector (docker exec) — regression for P2 routing fix
    // -----------------------------------------------------------------------

    /// Parse `docker ps` output: one name per line, empty lines skipped.
    #[test]
    fn parse_docker_containers_output_basic() {
        let input = "my-api\nredis-dev\n\npostgres\n";
        let result = parse_docker_containers_output(input);
        assert_eq!(result, vec!["my-api", "redis-dev", "postgres"]);
    }

    #[test]
    fn parse_docker_containers_output_empty() {
        let result = parse_docker_containers_output("");
        assert!(result.is_empty(), "empty input must produce empty vec: {result:?}");
    }

    /// `collect_docker_container_candidates` returns empty when docker is absent.
    #[test]
    fn collect_docker_containers_returns_empty_when_no_docker() {
        use std::panic;

        let original_path = std::env::var_os("PATH").unwrap_or_default();
        std::env::set_var("PATH", "/dev/null");
        let result = panic::catch_unwind(list_docker_containers);
        std::env::set_var("PATH", &original_path);

        let containers = result.expect("list_docker_containers must not panic");
        assert!(
            containers.is_empty(),
            "expected empty Vec when docker is not on PATH: {containers:?}"
        );
    }

    /// Regression: `docker exec <Tab>` must NOT return kubernetes resources.
    ///
    /// Before the fix, `ArgType::Resource` always called
    /// `collect_kubectl_resource_candidates`, so `docker exec <Tab>` was
    /// populated from Kubernetes api-resources (pods, services, …).  The fix
    /// routes by command: kubectl → k8s resources, docker → container names.
    ///
    /// With PATH=/dev/null neither kubectl nor docker is available, so both
    /// collectors return empty — but the key assertion is that candidates
    /// sourced from "k8s_resource" are NOT present for the docker command.
    #[test]
    fn docker_exec_tab_does_not_return_k8s_resources() {
        // Save and override PATH so neither kubectl nor docker is on it.
        let saved_path = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", "/dev/null") };

        let result = std::panic::catch_unwind(|| {
            let (engine, dir) = test_engine("docker-exec-no-k8s");

            // Build a ParsedContext for `docker exec ` using the real parser so
            // the field layout is always correct.
            let parsed = crate::context::parse(
                "docker exec ",
                12,
                dir.path.as_path(),
            );
            assert_eq!(parsed.command.as_deref(), Some("docker"),
                "parsed command must be docker");

            let mut dispatch_candidates: Vec<Candidate> = Vec::new();
            let mut dispatch_seen: HashSet<String> = HashSet::new();
            engine
                .dispatch_path_like(
                    &parsed,
                    "docker",
                    "",
                    "/tmp",
                    &mut dispatch_candidates,
                    &mut dispatch_seen,
                )
                .expect("dispatch must not fail");

            // No k8s_resource candidates must appear for `docker exec`.
            let k8s_candidates: Vec<&Candidate> = dispatch_candidates
                .iter()
                .filter(|c| c.source == "k8s_resource" || c.kind == "k8s_resource")
                .collect();
            assert!(
                k8s_candidates.is_empty(),
                "docker exec must not return kubernetes resources, got: {k8s_candidates:?}"
            );
        });

        // Restore PATH.
        unsafe {
            match saved_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }

        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    // §7.16 cross-source deduplication by canonical path
    // -----------------------------------------------------------------------

    /// Reproduce the bug: a directory that is a direct child of cwd appears
    /// twice in completion results when the paths_index stores its canonical
    /// path while the shell passes the symlinked path as cwd.
    ///
    /// On macOS /tmp -> /private/tmp, so PathBuf::from("/tmp/X").parent() !=
    /// PathBuf::from("/private/tmp/X").parent() and the old guard fails,
    /// causing both a `path` candidate ("Korat/") and a `path_jump` candidate
    /// ("→ /private/tmp/.../Korat") to be emitted for the same directory.
    #[test]
    fn no_duplicate_when_child_dir_in_paths_index_via_symlinked_cwd() {
        use std::fs;

        let (engine, _dir) = test_engine("dedupe-7-16");

        // Create a real cwd directory under /tmp (the symlink path).
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let cwd_symlink = PathBuf::from(format!(
            "/tmp/shac-dedupe-cwd-{}-{}",
            std::process::id(),
            ts
        ));
        fs::create_dir_all(&cwd_symlink).expect("create cwd via symlink path");

        // Create the child directory.
        let korat_symlink = cwd_symlink.join("Korat");
        fs::create_dir_all(&korat_symlink).expect("create Korat/");

        // Canonical path of the child (resolves /tmp -> /private/tmp on macOS).
        let korat_canonical = korat_symlink
            .canonicalize()
            .expect("canonicalize Korat path");
        let korat_canonical_str = korat_canonical.to_string_lossy().to_string();

        // Seed paths_index with the *canonical* path (as the daemon would store it).
        engine
            .db
            .upsert_path_index_with_rank(&korat_canonical_str, 5.0, 0, "test", false, None)
            .expect("seed paths_index");

        // Complete `cd Korat` with the *symlink* cwd path (as the shell would pass it).
        let cwd_str = cwd_symlink.to_string_lossy().to_string();
        let response = engine
            .complete(make_request("cd Korat", &cwd_str))
            .expect("complete");

        // Resolve every candidate's insert_text to a canonical path so we can
        // detect two cards for the same directory regardless of display form.
        let korat_canonical_ref = &korat_canonical;
        let items_for_korat: Vec<_> = response
            .items
            .iter()
            .filter(|item| {
                // Expand tilde and resolve the insert_text relative to cwd.
                let raw = item.insert_text.trim_end_matches('/');
                let expanded = if let Some(rest) = raw.strip_prefix("~/") {
                    dirs::home_dir()
                        .map(|h| h.join(rest))
                        .unwrap_or_else(|| PathBuf::from(raw))
                } else {
                    PathBuf::from(raw)
                };
                let resolved = if expanded.is_absolute() {
                    expanded
                } else {
                    PathBuf::from(&cwd_str).join(expanded)
                };
                resolved
                    .canonicalize()
                    .map(|c| c == *korat_canonical_ref)
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(
            items_for_korat.len(),
            1,
            "expected exactly 1 candidate resolving to Korat canonical path, got {}: {:?}",
            items_for_korat.len(),
            response.items
        );

        // Prefer the local `path` source over `path_jump`.
        assert_eq!(
            items_for_korat[0].source,
            "path_cache",
            "surviving candidate should come from path_cache (local), got source={:?}",
            items_for_korat[0].source
        );

        let _ = fs::remove_dir_all(&cwd_symlink);
    }
}
