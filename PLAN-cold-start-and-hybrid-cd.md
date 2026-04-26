# PLAN: Cold-start & Hybrid cd

This plan covers three workstreams, sequenced as `base PR → A ∥ B ∥ C`.

Repo root: `/Users/roman/Documents/dev/sh-autocomplete` (single Cargo package; binaries `shac` + `shacd` share `src/lib.rs`).

## Project documentation references

- **`README.md`** — primary user-facing doc. Sections: Components, Current MVP, Build, Homebrew, Beta quickstart, zsh menu UI, Install/uninstall, Reindex/inspect, Explicit indexing, Diagnostics, Data and privacy, Trust-aware migration.
- **Landing page:** https://neftedollar.github.io/sh-autocomplete/ — sourced from `docs/index.html` + `docs/styles.css` + `docs/assets/`.
- **GitHub:** https://github.com/Neftedollar/sh-autocomplete (Discussions: https://github.com/Neftedollar/sh-autocomplete/discussions).
- **Homebrew formula:** `Formula/shac.rb` (tap `Neftedollar/shac`).
- **Existing implementation plans (`docs/superpowers/plans/`):**
  - `2026-04-26-background-indexation.md` — TDD plan for daemon-side background indexer with `full` + `skip_existing` knobs on `reindex_path_commands`. **Partially landed** (see §7.11): Task 1 (`AppDb::command_has_docs`) is on integration via base PR; Task 2 (signature extension) lives in `stash@{6}`; Tasks 3-4 (background thread in `shacd`, verification) not done.

---

## Decisions on open questions (answered up front)

1. **Zoxide format gate.** Read only `version_byte == 3`. On any other version, emit a one-line warning to stderr and skip (no fallback to `zoxide query --list` for v1).
2. **Cwd attribution for imported zsh history.** Use the **replay strategy**: maintain a `last_cd_target` register while parsing. Resolve `~`, `$HOME`, and absolute paths. Skip relative-cd resolution (too fragile) — those events get `cwd=""`.
3. **Default project-scan roots.** `~/dev`, `~/Documents/dev`, `~/code`, `~/src`, `~/projects`. Filter to existing. Do **not** include `~/Library/CloudStorage/*`. User can override via `--root` (repeatable).
4. **Privacy default.** Y/N prompt on `shac install` unless `--yes` is passed. `--no-import` skips the prompt and the import entirely.
5. **Profile data location.** Rust-static array (`src/profiles.rs`) for v1. Embedded TOML can come later.
6. **Branch/host/script/resource collectors.** Stub implementations (return empty) are fine for v1. `Directory`, `Path`, and `Subcommand` paths must be fully wired (those are what gate hybrid-cd activation).

---

## Section 0 — Architecture map

| Concern | File | Lines |
|---|---|---|
| CLI entry, all subcommands, `install`, `complete`, `explain`, `shell-env`, `recent-events`, `stats`, `migration-status` | `src/bin/shac.rs` | dispatch 213-273; `Commands` enum 27-46; `install` 579-612; `shell_env` 946-981; `complete` 758-767; `explain` 896-916 |
| Daemon process loop (Unix socket JSON-line protocol) | `src/bin/shacd.rs` | accept loop 37-49; `handle_client` action dispatch 73-114 |
| Engine, candidate generation, scoring (12 features) | `src/engine.rs` | `Engine::complete` 77-139; `Engine::explain` 141-179; `collect_candidates` 243-447; `collect_path_candidates` (the `path_cache` source) 449-511; `score_candidate` 513-623; `is_cd_path_context` 796-799; `source_prior` 801-813 |
| 12 scoring features (declared inline in `score_candidate`) | `src/engine.rs` | `prefix_score` 692-700, `fuzzy_match_score` 702-741, `global_usage_score` (via `history_usage` 625-633), `cwd_usage_score`, `recency_score` 635-649, `transition_score` 651-658, `project_affinity_score` 760-781, `position_score` 783-794, `source_prior` 801-813, `doc_match_score` 815-822, `heuristic_score`/`ml_model_score` blended 587-606 |
| Tokenizer & role classifier | `src/context.rs` | `parse` 23-57; `TokenRole` enum 3-9; `classify_role` 59-74; `looks_like_path` 76-89; `detect_project_markers` 91-109 |
| SQLite schema + migrations + queries | `src/db.rs` | `init` 95-315 (CREATE TABLEs at 101-238; `ensure_column` migrations 241-311); `record_history` 484-530; `frequent_history`/`weighted_history` 716-739, 1002-1057; `dir_cache` ops 781-799; `index_targets` 801-849; `stats` 851-910; trust-migration meta 1076-1115 |
| Wire protocol (`CompletionItem` has `kind`/`source`/`meta.description`) | `src/protocol.rs` | `CompletionItem` 56-64; `CompletionRequest` 38-48; `RecordCommandRequest` 97-111; `StatsResponse` 114-133 |
| PATH command indexer + static command docs | `src/indexer.rs` | `reindex_path_commands` 18-72; `static_docs` 309-455 |
| Config & XDG path discovery | `src/config.rs` | `FeatureFlags` 8-28; `RankingWeights` 30-60; `AppPaths::discover` 132-158; `AppPaths::ensure` 160-166 |
| Embedded shell scripts | `src/shell.rs` | all 3 lines |
| zsh adapter (menu rendering, calls `shac complete`, calls `shac record-command`) | `shell/zsh/shac.zsh` | invocation 177-181, 493-497; menu arrays 29-30, 488-489; `_shac_render_metadata_label` 352-381 |
| bash adapter | `shell/bash/shac.bash` | (similar, smaller) |
| fish adapter | `shell/fish/shac.fish` | |
| ML reranker | `src/ml.rs` | 149 lines |
| Test harness (sandboxes XDG dirs) | `tests/support/mod.rs` | `TestEnv` 12-138, `run_ok` 160-177, `spawn_daemon` 108-127 |
| Existing CD test fixtures (must update in workstream A) | `tests/cli_integration.rs` | 123-191 (asserts empty `cd ` returns dirs from cwd, no history bleed) |

`shac install` (lines 579-612 in `bin/shac.rs`) currently does only:
1. Writes the embedded shell script to `$XDG_CONFIG_HOME/shac/shell/shac.{zsh,bash,fish}`.
2. If `--edit-rc`, splices a `# >>> shac initialize >>>` block into the rc file.
3. Prints next-step hints.

It does **not** start the daemon, run reindex, or import anything.

---

## Section BASE — Base PR (sequential; lands FIRST)

Purpose: unblock A/B/C parallel work by landing the schema additions and the engine-dispatch indirection. **No behavior change.**

### Schema additions in `src/db.rs::init`

After the `dir_cache` block (~line 188), add:

```sql
CREATE TABLE IF NOT EXISTS paths_index (
    path TEXT PRIMARY KEY,            -- canonicalized absolute path
    rank REAL NOT NULL DEFAULT 0.0,   -- frecency score
    last_visit INTEGER NOT NULL DEFAULT 0,
    visit_count INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL,             -- 'cwd_event' | 'zoxide_import' | 'project_scan' | 'git_repo'
    is_git_repo INTEGER NOT NULL DEFAULT 0,
    project_marker TEXT,
    created_ts INTEGER NOT NULL,
    updated_ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_paths_index_rank ON paths_index(rank DESC);
CREATE INDEX IF NOT EXISTS idx_paths_index_last_visit ON paths_index(last_visit DESC);
```

Also add via `ensure_column` on `history_events`:
- `import_hash TEXT` (nullable)
- `imported_at INTEGER` (nullable)

Plus: `CREATE UNIQUE INDEX IF NOT EXISTS idx_history_import_hash ON history_events(import_hash) WHERE import_hash IS NOT NULL;`

### Engine refactor in `src/engine.rs::collect_candidates`

Extract the cd-hardcoded branch (lines 377-388) into a new private method:

```rust
fn dispatch_path_like(
    &self,
    parsed: &ParsedContext,
    active: &str,
    cwd: &str,
    candidates: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    // For now, preserve existing behavior exactly: cd-hardcoded path completion.
    if is_cd_path_context(parsed) || /* existing role-based check */ {
        self.collect_path_candidates(active, cwd, candidates, seen)?;
    }
    Ok(())
}
```

Replace the inline branch with `self.dispatch_path_like(...)`. Verify all existing tests pass unchanged.

### Tests

- All existing tests must pass without modification. Run full suite: `cargo test`.
- Add `tests/base_pr_smoke.rs` that just opens the DB, queries `SELECT name FROM sqlite_master WHERE type='table' AND name='paths_index'`, asserts the table exists.

### File-level change list (BASE)

- `src/db.rs` — schema only.
- `src/engine.rs` — `dispatch_path_like` extraction. No new behavior.
- `tests/base_pr_smoke.rs` — new.

---

## Section 1 — Workstream A: Hybrid `cd`

### Diagnosis

`Engine::collect_candidates` triggers `collect_path_candidates` when the active token is path-shaped, prev token is `cd`, or command is `cd`. `collect_path_candidates` only reads the parent dir of the active token (via `split_path_token`) and emits its children. There is **no global frecency source for paths today**. `cd_empty_path_context` (engine.rs:251, 391-393) actively short-circuits other sources for empty `cd ` — that early return must be loosened.

### New methods on `AppDb` (in `src/db.rs`)

- `pub fn upsert_path_index(&self, path: &str, source: &str, is_git_repo: bool, project_marker: Option<&str>) -> Result<()>` — increments `visit_count`, sets `last_visit = now`, frecency formula: `rank = old_rank + 1.0` clamped, with decay applied lazily on read.
- `pub fn upsert_path_index_with_rank(&self, path: &str, rank: f64, last_visit: i64, source: &str, is_git_repo: bool, project_marker: Option<&str>) -> Result<()>` — for zoxide import where rank is supplied.
- `pub fn top_paths(&self, prefix_filter: Option<&str>, limit: usize) -> Result<Vec<PathFrecency>>` — returns ranked paths, optionally filtered by basename or substring match.
- Hook into `record_history` (db.rs:484-530): detect `cd <path>` lines and call `upsert_path_index(target_dir, "cwd_event", ...)`.

### Engine: new candidate source

Inside `dispatch_path_like` (created by base PR):

```rust
if is_cd_path_context(parsed) {
    self.collect_path_candidates(active, cwd, candidates, seen)?;
    self.collect_global_path_candidates(active, cwd, candidates, seen)?;
}
```

Loosen the `cd_empty_path_context` short-circuit at engine.rs:391-393: replace early return with **per-source filter** that suppresses only `history` and `runtime_history` sources, allowing the new `path_jump` source through.

New method `collect_global_path_candidates`:
- Calls `db.top_paths(if active.is_empty() { None } else { Some(active) }, max_results * 2)`.
- Filters out paths equal to `cwd` or direct children of `cwd` (those will already be emitted by `collect_path_candidates` with `source=path_cache`, `kind=path`). Dedupe via existing `seen` set on `kind::insert_text`.
- For each remaining path:
  - `insert_text`: `~/`-shortened path (use `dirs::home_dir()` to shorten `/Users/roman/...` → `~/...`).
  - `display`: `→ ~/Documents/dev/sh-autocomplete` (arrow as visual marker).
  - `kind`: `"path_jump"` (NEW, distinct from existing `"path"`).
  - `source`: `"path_jump"` (NEW, distinct from `"path_cache"`).
  - `description`: `"git repo · last visited 3d ago"` (from `last_visit` and `is_git_repo`).

### Scoring integration

In `src/engine.rs::score_candidate`:
- `position_score` (line 783-794): add `_ if is_cd_path_context(parsed) && candidate.kind == "path_jump" => 1.0`.
- `source_prior` (line 801-813): add `("path_jump", _) => 0.85` (just below `path_cache`'s 0.9).
- New ranking feature `path_frecency_score`: introduce in `RankingWeights` (`src/config.rs:30-60`) with default `0.10`. Only nonzero for `kind=path_jump` candidates; value = `paths_index.rank / max_rank` clamped to `[0,1]`. Add `feature(...)` line to `score_candidate`'s features vec around line 527-585. Update all `get_key`/`set_key` matches in `src/config.rs:185-258`.

### Wire format

`CompletionItem.kind` and `.source` are already first-class on the wire and `shell-tsv-v2` already emits both. The zsh adapter already populates `_shac_menu_kinds`/`_shac_menu_sources` arrays. **The "→" arrow appears via `display` and works without zsh changes.** Optional cosmetic: in `_shac_render_metadata_label` add a cyan tint when `kind=path_jump`.

### `--explain` output

Already covered: new candidates show as `→ ~/Documents/dev/sh-autocomplete [0.823] via path_jump` followed by feature breakdown including the new `path_frecency_score`. No code change beyond the new ranking feature.

### Tests

- `tests/cli_integration.rs:128-192` — adjust empty-`cd ` assertions: keep no-history/no-runtime_history check but allow `kind=path_jump`. Add fixture: pre-populate `paths_index` with row pointing to `<env.root>/elsewhere`, run `complete --line "cd " --cwd <cwd-not-elsewhere>`, assert `elsewhere/` appears with `kind=path_jump`, `source=path_jump`. Add negative fixture: empty `paths_index` → no `path_jump` items.
- New test `tests/hybrid_cd.rs`: full daemon round-trip — `cd shac<tab>` from `~/` returns the global `~/Documents/dev/sh-autocomplete` candidate even though it isn't a direct child.
- DB unit tests: `upsert_path_index` updates rank/last_visit; `top_paths` honors prefix filter and limit.

### File-level change list (A)

- `src/db.rs` — 3 new methods + `record_history` hook.
- `src/engine.rs` — `collect_global_path_candidates`, `dispatch_path_like` body update, score_candidate additions.
- `src/config.rs` — add `path_frecency_score` weight + key getters/setters.
- `tests/cli_integration.rs` — relax/extend existing cd-empty test.
- `tests/hybrid_cd.rs` — new.
- `shell/zsh/shac.zsh` — optional cosmetic tweak (line 352-381).

---

## Section 2 — Workstream B: Cold-start imports

### CLI surface

Extend `Commands` enum at `src/bin/shac.rs:27-46`:

```rust
Import(ImportArgs),
ScanProjects(ScanProjectsArgs),
```

```rust
#[derive(Debug, Args)]
struct ImportArgs { #[command(subcommand)] action: ImportAction }
#[derive(Debug, Subcommand)]
enum ImportAction {
    ZshHistory { #[arg(long)] path: Option<String>, #[arg(long)] dry_run: bool },
    Zoxide     { #[arg(long)] path: Option<String>, #[arg(long)] dry_run: bool },
    All        { #[arg(long)] yes: bool },
}

#[derive(Debug, Args)]
struct ScanProjectsArgs {
    #[arg(long)] root: Vec<String>,
    #[arg(long, default_value_t = 3)] depth: usize,
}
```

### New module `src/import.rs`

```rust
pub struct ImportSummary {
    pub source: &'static str,
    pub seen: usize,
    pub inserted: usize,
    pub skipped_dup: usize,
    pub skipped_redacted: usize,
    pub elapsed: Duration,
}

pub fn import_zsh_history(db: &AppDb, path: &Path, redactor: &Redactor) -> Result<ImportSummary>;
pub fn import_zoxide(db: &AppDb, path: &Path) -> Result<ImportSummary>;
pub fn scan_projects(db: &AppDb, roots: &[PathBuf], max_depth: usize) -> Result<ImportSummary>;
pub fn detect_tools() -> ToolDetection;
pub fn run_full_import(db: &AppDb, opts: ImportOpts) -> Result<Vec<ImportSummary>>;
```

Add `pub mod import;` to `src/lib.rs`.

### Zsh history parser

`~/.zsh_history` lines:
- Plain: `git status\n`
- Extended: `: 1700000000:0;git status\n`
- Multi-line entries use `\` continuation lines.

Implementation:
1. Open with `BufReader`. Read raw bytes, lossy-decode (zsh's metafication uses `0x83` byte; pass `bytes.iter().filter(|b| **b != 0x83)` before utf8 lossy).
2. Detect extended format by `^: [0-9]+:[0-9]+;` regex (build once).
3. Extract `(timestamp_or_none, command)`.
4. **Idempotency**: `dedupe_hash = sha256(format!("{ts}|{command}"))`. Insert with `INSERT OR IGNORE` against the partial unique index from base PR.
5. **Cwd attribution (replay strategy)**: maintain `last_cd_target: Option<String>`. When parsing a `cd <path>` line, resolve:
   - Absolute paths: use as-is
   - `~` or `~/...`: expand via `dirs::home_dir()`
   - `$HOME` / `$HOME/...`: expand
   - Relative paths: skip, don't update `last_cd_target` (too fragile)
   - When updating, set future events' cwd to `last_cd_target.unwrap_or_default()` until next `cd`.
6. **Trust/provenance**: set `trust = TRUST_LEGACY`, `provenance = PROVENANCE_LEGACY`. The legacy weighting at db.rs:1206-1215 (`legacy => 0.15`) ensures imported history doesn't dominate fresh interactive events.
7. New `AppDb::insert_imported_history(ts, cwd, command, shell, import_hash)` — uses parsed timestamp, not `unix_ts()`.
8. **Redaction** (cross-cutting, see §4): drop events matching redaction regex.
9. **Side-effect during import**: derived `cd <path>` events → `upsert_path_index(path, "cwd_event", ...)` with rank-bump per occurrence. Critical bootstrap so hybrid cd works after `shac install` even before new shell activity.

Performance budget: 200k lines in <2.5s. Mandatory: wrap inserts in a single transaction (`conn.execute_batch("BEGIN")` / `COMMIT`).

### Zoxide DB format (version 3)

zoxide stores DB at `~/.local/share/zoxide/db.zo`. Format:
- 4-byte little-endian version (`[3, 0, 0, 0]`).
- Then bincode-serialized `Vec<DirEntry>` where `DirEntry { path: String, rank: f64, last_accessed: u64 }`. Strings are `u64` LE length + UTF-8 bytes.

Hand-roll a small reader (~80 lines):
```rust
fn read_u32_le(r: &mut impl Read) -> io::Result<u32>;
fn read_u64_le(r: &mut impl Read) -> io::Result<u64>;
fn read_f64_le(r: &mut impl Read) -> io::Result<f64>;
fn read_string(r: &mut impl Read) -> io::Result<String>;
```

If `version_byte != 3`, log warning and return `Ok(ImportSummary { seen: 0, ... })`.

For each entry: `db.upsert_path_index_with_rank(path, rank, last_visit, "zoxide_import", is_git_repo: PathBuf::from(&path).join(".git").exists(), None)`.

If file missing, return empty summary (normal for users without zoxide).

### Project scanner

Default roots (from §Decisions): `~/dev`, `~/Documents/dev`, `~/code`, `~/src`, `~/projects`. Filter to existing.

Hand-roll stack-based DFS, depth ≤ 3, prune-list: `node_modules`, `target`, `.git`, `dist`, `build`, `.venv`, `__pycache__`, `.next`, `vendor`. Stop descending into a directory once `.git` is found.

For each `.git` parent: `upsert_path_index_with_rank(path, rank=0.5, last_visit=parent_mtime, "project_scan", is_git_repo=1, project_marker=detect_marker(path))`.

Budget: ~30 repos × ~10ms stat ≈ 0.8s.

### `shac install` extension

After existing logic in `src/bin/shac.rs::install`:

```rust
if !args.no_import {
    let summaries = run_full_import(&db, ImportOpts {
        yes: args.yes,
        roots: default_roots(),
        depth: 3,
        shell,
    })?;
    print_first_run_summary(&summaries);  // Workstream C owns the printer
}
```

Add `--no-import` and `--yes` flags to `InstallArgs` (line 48-54).

### Telemetry

Track per-import counts in `app_meta`:
- `import_zsh_history.count`, `import_zoxide.count`, `project_scan.count`
- `install_ts`, `first_accept_ts` (for `time_to_first_accept`)

Surface in `shac stats` via new fields on `StatsResponse`:
```rust
pub imported_history_events: i64,
pub imported_zoxide_paths: i64,
pub scanned_project_paths: i64,
pub paths_index_rows: i64,
pub time_to_first_accept_seconds: Option<i64>,
pub import_coverage_pct: f64,
```

### Tests

- `src/import.rs#mod tests`:
  - `parses_extended_history_format` — synthetic file, both formats.
  - `idempotent_double_import` — second import inserts 0.
  - `cd_replay_populates_paths_index` — `cd /tmp/foo` from history → `paths_index` has `/tmp/foo`.
  - `redactor_drops_aws_key`.
  - `zoxide_v3_parser` — fake binary fixture.
  - `zoxide_v4_skipped` — wrong version byte → empty summary.
- `tests/cold_start_import.rs` — end-to-end: stage `~/.zsh_history` and zoxide db in TestEnv, run `shac install --shell zsh --edit-rc --yes`, assert stats deltas, `assert!(elapsed < Duration::from_secs(5))`.
- `tests/cli_integration.rs` — assert `shac install --no-import` does NOT touch `paths_index`.

### File-level change list (B)

- `src/lib.rs` — `pub mod import;`.
- `src/import.rs` — NEW (~600 lines).
- `src/db.rs` — `insert_imported_history`, `record_import_summary`. (Schema lives in base PR; `upsert_path_index_with_rank` lives in workstream A.)
- `src/bin/shac.rs` — `Commands::Import`, `Commands::ScanProjects`, install flow extension, `--no-import`/`--yes` flags.
- `src/protocol.rs` — new `StatsResponse` fields.
- `src/engine.rs::stats` (line 190-192) — wire new fields through `db.stats()`.
- `Cargo.toml` — add `sha2 = "0.10"`, `regex = "1"`.
- `tests/cold_start_import.rs` — NEW.

---

## Section 3 — Workstream C: Command profiles + first-run UX

### Profile registry

New file `src/profiles.rs`:

```rust
#[derive(Debug, Clone, Copy)]
pub enum ArgType {
    Directory, Branch, Host, Resource, Script, Image, Subcommand, Flag, Path, Workspace, Target, None,
}

#[derive(Debug, Clone, Copy)]
pub struct CommandProfile {
    pub command: &'static str,
    pub default_arg: ArgType,
    pub subcommands: &'static [(&'static str, ArgType)],
}

pub fn lookup(command: &str) -> Option<&'static CommandProfile>;
pub fn arg_type_for(parsed: &ParsedContext) -> ArgType;
```

Static registry of ~30 profiles:

| command | default_arg | subcommand mappings |
|---|---|---|
| `cd`, `pushd`, `popd` | Directory | — |
| `git` | Subcommand | `checkout`/`switch` → Branch; `worktree add` → Path; `clone` → Path |
| `ssh`, `scp`, `mosh`, `rsync` | Host | — |
| `npm` | Subcommand | `run` → Script |
| `pnpm`, `yarn` | Subcommand | `run` → Script |
| `kubectl` | Subcommand | `get` → Resource; `apply` → Path |
| `docker` | Subcommand | `run`/`pull`/`push` → Image |
| `cargo` | Subcommand | `run --bin` → Target |
| `make`, `just`, `task` | Target | — |
| `code`, `subl`, `idea`, `nvim`, `vim` | Workspace | — |
| `python`, `python3` | Path | (already handled separately via `-m` at engine.rs:841-844) |
| `brew` | Subcommand | `install`/`uninstall` → existing brew formula list |
| `gh` | Subcommand | — |
| `dotnet` | Subcommand | — |
| `pytest` | Path | — |
| `bash`, `sh`, `zsh` | Script | — |
| `which`, `type`, `man`, `help` | Subcommand (= command name) | — |
| `tmux` | Subcommand | — |
| `aws` | Subcommand | — |

### Engine integration

Replace the `dispatch_path_like` body (created by base PR) with:

```rust
fn dispatch_path_like(&self, parsed: &ParsedContext, active: &str, cwd: &str, candidates: &mut Vec<CompletionItem>, seen: &mut HashSet<String>) -> Result<()> {
    let arg_type = profiles::arg_type_for(parsed)
        .unwrap_or_else(|| infer_arg_type_from_role(parsed.role));
    match arg_type {
        ArgType::Directory => {
            self.collect_path_candidates(active, cwd, /*dirs_only=*/ true, candidates, seen)?;
            self.collect_global_path_candidates(active, cwd, candidates, seen)?;
        }
        ArgType::Path => self.collect_path_candidates(active, cwd, /*dirs_only=*/ false, candidates, seen)?,
        ArgType::Branch => self.collect_git_branch_candidates(active, cwd, candidates, seen)?,
        ArgType::Host => self.collect_ssh_host_candidates(active, candidates, seen)?,
        ArgType::Script => self.collect_npm_script_candidates(active, cwd, candidates, seen)?,
        ArgType::Resource => self.collect_kubectl_resource_candidates(active, candidates, seen)?,
        ArgType::Subcommand => { /* falls through to docs_for_command path already at line 339-355 */ }
        _ => {}
    }
    Ok(())
}
```

Stub implementations of `collect_git_branch_candidates`, `collect_ssh_host_candidates`, `collect_npm_script_candidates`, `collect_kubectl_resource_candidates` — return `Ok(())` with `// TODO: implement` comments.

### Profile DB hydration (optional fast path)

`seed_profiles_into_docs(db, &PROFILES)` expands `subcommands` into `StoredDoc { item_type: "subcommand", ... }` and bulk-inserts via existing `replace_docs_for_command`. Called once during install flow.

### First-run UX printer

Owns `print_first_run_summary(&[ImportSummary])`:

```
Hooking shac into zsh...                      ✓
Importing zsh history... (12,847 entries)     ✓ [1.8s]
Importing zoxide... (156 destinations)        ✓ [0.1s]
Scanning ~/Documents/dev for git repos...     ✓ (23 found) [0.6s]
Loading 31 command profiles...                ✓
Reindexing $PATH (847 commands)...            ✓ [0.4s]

Try: cd <Tab>
  Run `shac doctor` if Tab feels off.
  Run `shac stats` to see what was learned.
```

Helper `print_step(label: &str, op: impl FnOnce() -> Result<String>)` emits `label...` then on completion overwrites with `\r{label} ✓ {detail}`. If `!stdout.is_terminal()`, print sequentially without `\r`.

Consent prompt for zsh-history: if `~/.zsh_history` exists and `--yes` not passed, prompt `Import 12,847 zsh history entries? [Y/n] `. Skip on `--no-import`.

### Tests

- `tests/profiles.rs` — `profiles::lookup("git").is_some()`, `arg_type_for(parsed("cd "))` == `Directory`, `arg_type_for(parsed("git checkout "))` == `Branch`, `arg_type_for(parsed("ssh "))` == `Host`.
- `tests/cli_integration.rs` — assert `cd <Tab>` after `shac install` shows `path_jump` items (verifies A+B+C end-to-end).

### File-level change list (C)

- `src/profiles.rs` — NEW (~250 lines).
- `src/lib.rs` — `pub mod profiles;`.
- `src/engine.rs` — replace `dispatch_path_like` body. Add stub collector methods.
- `src/bin/shac.rs::install` — `print_first_run_summary` and helpers (`print_step`).
- `tests/profiles.rs` — NEW.

---

## Section 4 — Cross-cutting

### Performance budgets

| Source | 200k zsh history | Budget | Approach |
|---|---|---|---|
| Zsh parse + insert | 200k entries | 2.5s | Single `BEGIN/COMMIT` txn; pre-prepared statement; SHA dedupe in-memory `HashSet<[u8;32]>` |
| Zoxide import | typical 200-2000 | 0.1s | Trivial |
| Project scan | typical ~30 repos | 0.8s | Bounded depth, prune-list |
| PATH reindex (existing) | ~1000 commands | 0.4s | Already optimized |
| **Total install budget** | — | **<5s** | |

### Idempotency

`history_events.import_hash = sha256(format!("{ts}|{cmd}"))` with partial unique index. Re-running `shac install --edit-rc` is safe (`INSERT OR IGNORE` short-circuits).

`paths_index.path` is primary key. `upsert_path_index` on conflict bumps `visit_count`/`updated_ts` but does NOT increment `rank` for the same call (rank only grows via genuine `cd` events). For zoxide imports, set rank directly. For project scan, set rank to `0.5`.

### Privacy / redaction

Module `src/import::redact`. Default patterns (compile to `regex::RegexSet`):
```
\bAKIA[0-9A-Z]{16}\b              # AWS access key
\bASIA[0-9A-Z]{16}\b              # AWS temp
\bxox[abprs]-[A-Za-z0-9-]{10,}\b  # Slack
\beyJ[A-Za-z0-9_-]{20,}\.eyJ      # JWT
\bgithub_pat_[A-Za-z0-9_]{82}\b   # GitHub PAT
\bghp_[A-Za-z0-9]{36}\b           # GitHub classic
\bsk-[A-Za-z0-9]{32,}\b           # OpenAI/Anthropic-shaped
postgres(?:ql)?://[^@]+@         # connection strings
```

Behavior: events whose `command` matches ANY pattern dropped at import time, counted in `ImportSummary.skipped_redacted`. `recent_events` CLI gains `--show-redacted` flag (default off).

### Telemetry counters

- `time_to_first_accept` — record at first `mark_completion_accepted` per install. Surface as `time_to_first_accept_seconds` in `shac stats`.
- `import_coverage_pct` — `(history_events_with_import_hash) / (history_events)`. Surface in stats.
- Add two `doctor_check` entries in `shac doctor` (bin/shac.rs:362-461).

### Daemon protocol

No new `shacd` actions required for v1. SQLite WAL handles concurrent writes safely.

---

## Section 5 — Test plan

### Existing conventions
- `tests/support/mod.rs::TestEnv` provides per-test temp `HOME`/`XDG_*`, `spawn_daemon`, `run_ok`. Use it.
- Integration tests: `#[test] fn <descriptive_snake_case>()` calling `support::run_ok(&env, [...])` and asserting on TSV/JSON.
- Unit tests: inline `#[cfg(test)] mod tests` blocks.

### Per-workstream
**A:**
- DB unit tests: `paths_index_upsert_and_top_paths`, `top_paths_filters_by_prefix`, `cd_history_event_populates_paths_index`.
- Engine unit tests: `cd_with_global_path_emits_path_jump`, `cd_with_no_global_paths_falls_back_to_children_only`.
- Patch existing `cd_completion` test in `cli_integration.rs`; add `path_jump_appears_when_paths_index_seeded`.
- New `tests/hybrid_cd.rs` — full daemon round-trip.

**B:**
- `import.rs` unit tests: parse fixtures, idempotency, redaction, zoxide v3/v4.
- `tests/cold_start_import.rs`: end-to-end install with planted history+zoxide; assert <5s.

**C:**
- `tests/profiles.rs`: lookup, arg-type-for across 10 representative parsed lines.
- `tests/cli_integration.rs`: assert `cd <Tab>` after install shows path_jump (E2E A+B+C).

---

## Section 6 — Sequencing & parallelization

### Conflict surface

| File | Base | A | B | C |
|---|---|---|---|---|
| `src/db.rs` (schema in `init`) | ✓ paths_index + import_hash | — | — | — |
| `src/db.rs` (methods) | — | upsert_path_index, top_paths, record_cd_event | insert_imported_history, upsert_path_index_with_rank | — |
| `src/engine.rs::collect_candidates` | ✓ extract `dispatch_path_like` | extends `dispatch_path_like` for cd | — | replaces `dispatch_path_like` body for profile dispatch |
| `src/engine.rs::score_candidate` | — | new path_frecency_score | — | — |
| `src/bin/shac.rs::install` | — | — | imports orchestration | first-run printer |
| `src/lib.rs` | — | — | `pub mod import;` | `pub mod profiles;` |
| `src/protocol.rs` | — | — | new StatsResponse fields | — |
| `Cargo.toml` | — | — | sha2, regex | — |

### Sequencing

1. **Base PR** (sequential, ~30min agent run): schema scaffolding only + `dispatch_path_like` extraction. **No behavior change.** Lands first.

2. **A, B, C in parallel** after base PR. Conflicts resolved as follows:
   - A and C both touch `dispatch_path_like` body — C "owns" the final body (profile dispatch); A's `collect_global_path_candidates` is called from within C's `Directory` branch. Coordination: A implements `collect_global_path_candidates` and exports it; C imports and calls it.
   - B and C both touch `bin/shac.rs::install` — B adds the orchestration call (`run_full_import`), C adds the printer. Coordination: B exposes `run_full_import` returning `Vec<ImportSummary>`; C consumes via `print_first_run_summary`. They edit different lines.
   - A and B both touch `src/db.rs` for new methods — different methods, no conflict.

3. **Integration step** after A/B/C: run `cargo test`, fix any cross-cutting test breakage, commit final integration PR.

---

## Critical Files for Implementation

- `/Users/roman/Documents/dev/sh-autocomplete/src/engine.rs`
- `/Users/roman/Documents/dev/sh-autocomplete/src/db.rs`
- `/Users/roman/Documents/dev/sh-autocomplete/src/bin/shac.rs`
- `/Users/roman/Documents/dev/sh-autocomplete/src/import.rs` (new in B)
- `/Users/roman/Documents/dev/sh-autocomplete/src/profiles.rs` (new in C)

---

## Section 7 — Follow-up workstreams (post-integration)

Status as of integration commit `e31edde` + cleanup/tests `f68ac43` + Section 7 batch 1 (`ac1497a` + `ffbacc1` + `24a1183` + `a5f916e` + `5fd30f5`):

### ✅ Done
- Base PR, Workstreams A / B / C, integration wiring, cleanup, dedicated CLI tests for `shac import` / `shac scan-projects`. 83 tests green.
- **7.1** First-run UX printer (`feat/section-7-1-first-run-ux` `ffbacc1`, +5 tests). `print_first_run_summary` + `print_step` helpers in `src/bin/shac.rs`; per-source labels with thousands separators and `[1.8s]` timing; TTY uses `\r` overwrite + ANSI green check, non-TTY plain fallback. README Beta quickstart updated.
- **7.4** Branch collector (`feat/section-7-4-branch-collector` `5fd30f5`, +6 tests). `collect_git_branch_candidates` + `find_git_repo_root` + `list_git_refs` in `src/engine.rs`; 200ms `GIT_REF_TIMEOUT` and 200-ref cap; `source_prior` / `position_score` updated for branch kind. `dispatch_path_like` carved out a separate `Branch =>` arm (sets the structural pattern for 7.5-7.10).
- **7.12** Doctor cold-start telemetry (`feat/section-7-12-doctor-telemetry` `a5f916e`, +1 test). 3 new checks in `shac doctor`: `cold_start_paths`, `cold_start_history` (with `import_coverage_pct`), `time_to_first_accept`. Doctor name column widened `{:<18}` → `{:<22}`. README Diagnostics updated.
- **7.13** zsh cosmetic tint for `kind=path_jump` (`feat/section-7-13-zsh-tint` `24a1183`, +1 test). Cyan tint on `[path_jump]` label and leading `→` arrow in `_shac_render_metadata_label` / `_shac_render_menu`. Respects `SHAC_NO_COLOR=1`. README zsh menu UI section updated.
- **7.15** Clippy clean (`feat/section-7-15-clippy` `ac1497a`). All 4 pre-existing lib warnings cleared (`Ok(x?)`, manual suffix stripping, `clone() → slice::from_ref`, borrowed-expression-in-test) + 9 auto-fixed `needless_borrows_for_generic_args` test warnings. `cargo clippy --all-targets` zero-warning on this branch tip.
- **7.2** Bundled command priors (`feat/section-7-2-priors`, +6 tests). `src/priors.rs` ships a hand-curated static corpus of ~60 `(command, item_type, item_value, description)` rows covering `git`, `npm`, `pnpm`, `yarn`, `cargo`, `docker`, `kubectl`, `gh`, `brew`, `make`, `python`/`python3`, `pip`. `seed_priors_into_docs(&db)` writes them with `source = "priors"` via `replace_docs_for_command` (idempotent). `shac install` seeds priors after `run_full_import` and prints `Loaded N command priors`. README Current MVP updated.

### ⏳ Pending — High priority (cold-start activation)

~~**7.1 First-run UX printer (polished)**~~ — ✅ Done (see above).

~~**7.2 Bundled command priors**~~ — ✅ Done (see above).

**7.3 `detect_tools()` + tool-aware profile loading**
B intentionally skipped this (PLAN §B5). Detect installed CLIs (homebrew, cargo, dotnet, nvm, etc.) and load only relevant profiles into `command_docs`. Reduces noise for users without those tools. **File:** `src/import.rs::detect_tools`.

### ⏳ Pending — Medium priority (extend dispatch coverage)

Stubs in `Engine::dispatch_path_like` for non-Directory/Path arg types. Each is an independent collector method:

| # | ArgType | Implementation source | Status |
|---|---|---|---|
| 7.4 | `Branch` | `git for-each-ref refs/heads refs/remotes` cached per-repo | ✅ Done |
| 7.5 | `Host` | parse `~/.ssh/config` + `~/.ssh/known_hosts` | ⏳ Pending |
| 7.6 | `Script` | parse `package.json` of cwd's nearest project root | ⏳ Pending |
| 7.7 | `Resource` | cache `kubectl api-resources` output | ⏳ Pending |
| 7.8 | `Image` | cache `docker images` output | ⏳ Pending |
| 7.9 | `Workspace` | parse VS Code `~/Library/Application Support/Code/storage.json` for recent workspaces | ⏳ Pending |
| 7.10 | `Target` | parse `Makefile` / `justfile` targets | ⏳ Pending |

**Priority within this group (by frequency × user impact):** 7.4 Branch → 7.6 Script → 7.5 Host → 7.10 Target → 7.9 Workspace → 7.7 Resource → 7.8 Image.

### ⏳ Pending — Background indexer (graduates `stash@{6}` per existing TDD plan)

**7.11 Background reindexer with incremental mode**

**Authoritative spec:** `docs/superpowers/plans/2026-04-26-background-indexation.md` — TDD plan with checkbox tasks. Reuse it, don't rewrite it. Goal: `shacd` auto-indexes `--help` for every PATH executable in a detached background thread (own DB connection, 2s settle, 6h re-loop), so tools like `terraform` / `gh` / `shac` itself get completions without manual `shac reindex`.

**Status of the TDD plan's 4 tasks:**

| Task | Spec section | Status |
|---|---|---|
| **Task 1**: `AppDb::command_has_docs(&str) -> bool` + 2 unit tests | `docs/superpowers/plans/2026-04-26-background-indexation.md` §"Task 1" | ✅ **DONE** — landed on integration via base PR (`6409d7e`). Method exists at `src/db.rs`; tests pass. |
| **Task 2**: `reindex_path_commands` gains `full: bool` + `skip_existing: bool` args, guard `if !skip_existing \|\| !db.command_has_docs(&name) { maybe_upsert_docs(db, &name, full)? }` (`src/indexer.rs:54-58`) + 2 unit tests | `docs/superpowers/plans/2026-04-26-background-indexation.md` §"Task 2" | 🟡 **STASHED** — `stash@{6}` "WIP feat/background-indexation engine+indexer changes before BASE PR". Near-complete, both tests included, applies cleanly onto `integration/cold-start-and-hybrid-cd` (no conflict with hybrid-cd's engine.rs edits — they live in `dispatch_path_like` / `collect_global_path_candidates`, far from `reindex_path_commands`). |
| **Task 3**: spawn background thread in `src/bin/shacd.rs` after `Engine::new()`; thread opens own `AppDb`, sleeps 2s, calls `reindex_path_commands(.., true, true)`, loops every 6h | `docs/superpowers/plans/2026-04-26-background-indexation.md` §"Task 3" | ❌ **NOT DONE** |
| **Task 4**: verify full flow + clippy + manual `shac complete --line "shac "` end-to-end | `docs/superpowers/plans/2026-04-26-background-indexation.md` §"Task 4" | ❌ **NOT DONE** |

**Sequence to graduate:**

1. Land integration (current `e31edde` + `f68ac43`) into `main` first — it's the larger, thematically distinct PR.
2. Branch fresh `feat/background-indexation` off merged main.
3. `git stash pop stash@{6}` — confirmed clean (verified by stash inspection agent).
4. Execute remaining Tasks 3 & 4 per the TDD plan's exact steps. The plan is checkbox-formatted for `superpowers:executing-plans` / `superpowers:subagent-driven-development` skills — invoke the appropriate skill rather than re-deriving the steps.

**Adjacent extensions worth bundling here (pulled from this Section 7):**

- **7.5b** Add `shacd` action `invalidate-caches` so CLI imports nudge the daemon to refresh in-memory caches without restart. (PLAN §4 deferred for v1; natural fit with the background-indexer work since both touch `shacd`'s long-lived state.)
- **7.5c** Per-CLI knob: `shac reindex [--full] [--skip-existing]` so the user can manually trigger either mode.

**Conflict surface with integration:** none. Cold-start (`integration/cold-start-and-hybrid-cd`) solves *first-run* import; background indexer solves *cheap repeat* indexing. Orthogonal.

### Polish / observability

~~**7.12** `shac doctor` checks for `time_to_first_accept_seconds` and `import_coverage_pct`~~ — ✅ Done (see above).

~~**7.13** zsh cosmetic tint for `kind=path_jump`~~ — ✅ Done (see above).

**7.14** Performance tuning of zsh-history import: 200k lines extrapolated ≈ 7s vs target 2.5s. Switch SHA-256 to blake3, batch into multi-VALUES `INSERT OR IGNORE` statements. Not blocking. ⏳ Pending.

~~**7.15** Resolve 4 clippy warnings~~ — ✅ Done (see above).

### Sequencing recommendation

```
   ┌──── 7.1 printer polish ───┐    (independent UX win)
   │                           │
7.2 priors ─┬── 7.3 tool-detect ─┤    (cold-start completeness)
            │                    │
            └─ 7.4 Branch ─┬─ 7.6 Script ─ 7.5 Host ─ ...    (dispatch coverage)
                          │
   7.11 background-indexer (parallel — totally orthogonal)
```

7.1, 7.11, and 7.2 are independent and can run in parallel after the integration lands. 7.3 depends on 7.2 (both hydrate `command_docs`). 7.4–7.10 are independent siblings; do in priority order.

7.12–7.15 are polish — slot in whenever convenient.
