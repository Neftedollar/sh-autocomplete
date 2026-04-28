# shac Discoverability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the discoverability system from `docs/superpowers/specs/2026-04-28-shac-discoverability-design.md` — JIT tip footer in the zsh menu, `shac suggest` pull command, first-run greeter, and i18n scaffolding (English defaults + extension point).

**Architecture:** Three new Rust modules — `i18n` (locale + lookup + interpolation), `tips` (catalog + selection + storage + session runtime), `suggest` (group/print) — composed from the existing `engine`, `db`, `config`, and `tools` modules. New SQLite table `tips_state`. New CLI subcommands: `suggest`, `tips`, `locale`. Wire format adds one optional `__shac_tip\t<id>\t<text>` sentinel line. zsh adapter parses + renders footer.

**Tech Stack:** Rust 1.x, rusqlite (bundled), clap, serde, toml, anyhow. zsh integration via existing `shell/zsh/shac.zsh`.

**Conventions used in this codebase:**
- Single crate `shac/`. Modules are siblings under `src/`. Each new module gets one file and is registered in `src/lib.rs`.
- DB schema lives in `Database::init()` inside one big `execute_batch` block (`src/db.rs:106-...`). Add new `CREATE TABLE IF NOT EXISTS` clauses to that block.
- Config is a single `AppConfig` struct in `src/config.rs` with `serde(default)`. New fields go under existing `UiConfig` to keep the surface flat (e.g. `ui.show_tips`).
- CLI uses `clap` derive with one top-level `Commands` enum + per-command `Args` structs in `src/bin/shac.rs`.
- Integration tests use `support::TestEnv` (real daemon spawn, isolated XDG dirs). One file per feature in `tests/`.
- All commit messages use Conventional Commits style (e.g. `feat(tips): ...`, `docs(spec): ...`). **No `Co-Authored-By` trailers.**

**Out of scope (deferred per spec):**
- Bash adapter parity for footer hints
- Plural forms / ICU
- Localization of existing menu strings
- Interactive `shac tour` TUI

**Existing files you will touch:**
- `src/lib.rs` — register new modules
- `src/db.rs` — add `tips_state` table
- `src/config.rs` — add fields to `UiConfig`, `AppConfig::get_key`/`set_key`
- `src/protocol.rs` — extend `CompletionResponse` with optional `tip`
- `src/engine.rs` — call into `tips::select` and attach result
- `src/bin/shac.rs` — `print_completion_response` emits `__shac_tip` line; new subcommands
- `shell/zsh/shac.zsh` — parse + render

**New files you will create:**
- `src/i18n.rs`
- `src/tips.rs`
- `src/suggest.rs`
- `locales/en.toml`
- `tests/tips.rs`
- `tests/suggest.rs`
- `tests/locale.rs`
- `tests/zsh_render.sh`

---

## Task 1: Add `tips_state` table and `meta_first_run_done` flag

**Goal:** Persist per-tip show counts and mute flags.

**Files:**
- Modify: `src/db.rs:106-...` (the `init()` `execute_batch` block)
- Test: `tests/tips.rs` (new file)

- [ ] **Step 1: Add the table to the init batch**

In `src/db.rs`, inside the `execute_batch` string in `Database::init()`, after the `paths_index` block and before `index_targets`, add:

```sql
CREATE TABLE IF NOT EXISTS tips_state (
    tip_id          TEXT PRIMARY KEY,
    shows_count     INTEGER NOT NULL DEFAULT 0,
    last_shown_at   INTEGER,
    muted           INTEGER NOT NULL DEFAULT 0,
    muted_at        INTEGER,
    first_shown_at  INTEGER
);
```

The `meta_first_run_done` flag reuses the existing `app_meta` table — no schema change needed, just a row insert later.

- [ ] **Step 2: Smoke-test that the schema applies**

Create `tests/tips.rs`:

```rust
mod support;

#[test]
fn tips_state_table_is_created() {
    let env = support::TestEnv::new("tips-schema");
    let _daemon = env.spawn_daemon();
    // Force a `complete` to ensure the daemon opened the DB and ran init.
    support::run_ok(&env, ["complete", "--shell", "zsh", "--line", "ls", "--cursor", "2", "--cwd", env.root.to_string_lossy().as_ref(), "--format", "json"]);

    let db_path = env.app_paths().db_file;
    let conn = rusqlite::Connection::open(&db_path).expect("open db");
    let exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='tips_state'",
            [],
            |r| r.get(0),
        )
        .expect("query");
    assert_eq!(exists, 1, "tips_state table should exist");
}
```

Add `rusqlite` to `[dev-dependencies]` in `Cargo.toml` if it is not already accessible from tests (it is: `shac` re-exports it transitively, but tests link against the crate, so `use rusqlite::Connection;` works once any test imports it). If the test fails to compile, add to `Cargo.toml`:

```toml
[dev-dependencies]
rusqlite = { version = "0.39", features = ["bundled"] }
```

- [ ] **Step 3: Run the test**

```
cargo test --test tips tips_state_table_is_created -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/db.rs tests/tips.rs Cargo.toml Cargo.lock
git commit -m "feat(db): add tips_state table for per-tip show counts and mute flags"
```

---

## Task 2: i18n module — locale resolution

**Goal:** Resolve which locale to use, given env vars and config.

**Files:**
- Create: `src/i18n.rs`
- Modify: `src/lib.rs`
- Test: `tests/locale.rs` (new file)

- [ ] **Step 1: Write the failing tests**

Create `tests/locale.rs`:

```rust
use shac::i18n::{resolve_locale, LocaleSource};

#[test]
fn shac_locale_env_wins() {
    let resolved = resolve_locale(
        Some("ru".to_string()),  // SHAC_LOCALE
        None,                     // ui.locale config
        Some("de_DE.UTF-8".to_string()),  // LC_MESSAGES
        Some("fr_FR".to_string()),        // LANG
    );
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::Env);
}

#[test]
fn config_overrides_lc_and_lang() {
    let resolved = resolve_locale(None, Some("de".to_string()), Some("ru_RU".to_string()), None);
    assert_eq!(resolved.lang, "de");
    assert_eq!(resolved.source, LocaleSource::Config);
}

#[test]
fn lc_messages_used_when_no_env_or_config() {
    let resolved = resolve_locale(None, None, Some("ru_RU.UTF-8".to_string()), Some("en_US".to_string()));
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::AutoLcMessages);
}

#[test]
fn lang_used_when_no_env_config_or_lc() {
    let resolved = resolve_locale(None, None, None, Some("fr_CA.UTF-8".to_string()));
    assert_eq!(resolved.lang, "fr");
    assert_eq!(resolved.source, LocaleSource::AutoLang);
}

#[test]
fn falls_back_to_en_when_nothing_set() {
    let resolved = resolve_locale(None, None, None, None);
    assert_eq!(resolved.lang, "en");
    assert_eq!(resolved.source, LocaleSource::Default);
}

#[test]
fn empty_config_string_is_treated_as_unset() {
    let resolved = resolve_locale(None, Some("".to_string()), None, Some("ru_RU".to_string()));
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::AutoLang);
}

#[test]
fn normalizes_locale_strings() {
    assert_eq!(resolve_locale(Some("ru_RU.UTF-8".to_string()), None, None, None).lang, "ru");
    assert_eq!(resolve_locale(Some("de.UTF-8".to_string()), None, None, None).lang, "de");
    assert_eq!(resolve_locale(Some("EN".to_string()), None, None, None).lang, "en");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cargo test --test locale
```

Expected: compile error — module `shac::i18n` does not exist.

- [ ] **Step 3: Create `src/i18n.rs` with locale resolution**

```rust
//! Localization: locale resolution and string lookup.
//!
//! Resolution priority (high → low): SHAC_LOCALE env, ui.locale config,
//! LC_MESSAGES env, LANG env, "en" default. All inputs normalized to a
//! 2-letter language code.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocaleSource {
    Env,
    Config,
    AutoLcMessages,
    AutoLang,
    Default,
}

#[derive(Debug, Clone)]
pub struct ResolvedLocale {
    pub lang: String,
    pub source: LocaleSource,
}

pub fn resolve_locale(
    shac_locale_env: Option<String>,
    ui_locale_config: Option<String>,
    lc_messages_env: Option<String>,
    lang_env: Option<String>,
) -> ResolvedLocale {
    if let Some(value) = non_empty(shac_locale_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::Env };
    }
    if let Some(value) = non_empty(ui_locale_config) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::Config };
    }
    if let Some(value) = non_empty(lc_messages_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::AutoLcMessages };
    }
    if let Some(value) = non_empty(lang_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::AutoLang };
    }
    ResolvedLocale { lang: "en".into(), source: LocaleSource::Default }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

fn normalize(raw: &str) -> String {
    let lowercase = raw.to_ascii_lowercase();
    let cut = lowercase
        .split(|c: char| c == '_' || c == '.' || c == '-')
        .next()
        .unwrap_or("en");
    if cut.is_empty() { "en".into() } else { cut.into() }
}
```

- [ ] **Step 4: Register module**

In `src/lib.rs`, add `pub mod i18n;` between `pub mod engine;` and `pub mod import;` (alphabetical).

- [ ] **Step 5: Run tests**

```
cargo test --test locale
```

Expected: all 7 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/i18n.rs tests/locale.rs
git commit -m "feat(i18n): locale resolution from env, config, LC_MESSAGES, LANG"
```

---

## Task 3: i18n module — TOML lookup with per-key fallback to English

**Goal:** Look up keys like `tips.git_branches` in a locale TOML. Missing key → fall back to `en`. Missing locale file → fall back to `en` for the whole process.

**Files:**
- Modify: `src/i18n.rs`
- Create: `locales/en.toml`
- Test: `tests/locale.rs` (extend)

- [ ] **Step 1: Write `locales/en.toml`** with all known keys (some are placeholders for tips that will be added later — fill them in now so lookups never miss)

```toml
[tips]
hybrid_cd        = "cd <Tab> works from anywhere — global frecent paths shown with → prefix"
git_branches     = "branches of this repo are pulled automatically"
ssh_hosts        = "hosts from ~/.ssh/config — partial match filters live"
npm_scripts      = "scripts from package.json"
kubectl_resources = "cluster resources via kubectl: get/describe/logs/exec"
docker_images    = "local images from docker daemon"
make_targets     = "targets from Makefile/Justfile"
transitions      = "this candidate appears because it often follows your previous command"
path_jump_cyan   = "→ in cyan means a global frecent path, not a cwd child"
unknown_command  = "shac doesn't know '{bin}'. Run `shac index add-command {bin}` to teach it"
menu_detail_verbose = "more detail in menu: shac config set ui.zsh.menu_detail verbose"
tips_off         = "tips bothering you? shac config set ui.show_tips false"

[suggest]
header_available = "shac is available in this directory:"
header_unused    = "Not used recently:"
header_links     = "Full feature list:"
no_matches       = "No applicable shac features found in this directory."

[greeter]
first_run = "shac is ready. Run `shac suggest` to see what's available here. `shac config` to tune."
```

- [ ] **Step 2: Write the failing tests** (append to `tests/locale.rs`)

```rust
use shac::i18n::{Catalog, Translator};

#[test]
fn english_lookup_returns_key_text() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    assert!(translator.lookup("greeter.first_run").contains("shac is ready"));
}

#[test]
fn missing_key_returns_key_string_for_visibility() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    assert_eq!(translator.lookup("tips.does_not_exist"), "tips.does_not_exist");
}

#[test]
fn russian_partial_translation_falls_back_to_english_per_key() {
    let mut catalog = Catalog::bundled_en();
    catalog.merge_locale("ru", r#"
        [tips]
        git_branches = "ветки этого репо подтягиваются автоматически"
    "#).expect("parse ru toml");

    let translator = Translator::new_for_test("ru", &catalog);
    assert!(translator.lookup("tips.git_branches").starts_with("ветки"));
    // ssh_hosts not in ru.toml → english fallback
    assert!(translator.lookup("tips.ssh_hosts").contains("hosts from"));
}

#[test]
fn interpolation_replaces_placeholders() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    let out = translator.lookup_with("tips.unknown_command", &[("bin", "kubectl")]);
    assert!(out.contains("kubectl"));
    assert!(!out.contains("{bin}"));
}

#[test]
fn interpolation_leaves_literal_when_placeholder_missing() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    let out = translator.lookup_with("tips.unknown_command", &[]);
    assert!(out.contains("{bin}"), "missing placeholder leaves literal {{bin}}");
}
```

- [ ] **Step 3: Implement `Catalog` and `Translator` in `src/i18n.rs`**

Append to `src/i18n.rs`:

```rust
use std::collections::HashMap;
use anyhow::{Context, Result};

const EN_TOML: &str = include_str!("../locales/en.toml");

/// One language's strings, parsed from TOML into a flat dotted-key map.
type LangMap = HashMap<String, String>;

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    locales: HashMap<String, LangMap>,
}

impl Catalog {
    pub fn bundled_en() -> Self {
        let mut catalog = Self::default();
        catalog
            .merge_locale("en", EN_TOML)
            .expect("bundled en.toml must parse");
        catalog
    }

    pub fn merge_locale(&mut self, lang: &str, toml_str: &str) -> Result<()> {
        let parsed: toml::Value = toml::from_str(toml_str).context("parse locale toml")?;
        let mut flat = self.locales.remove(lang).unwrap_or_default();
        flatten(&mut flat, "", &parsed);
        self.locales.insert(lang.to_string(), flat);
        Ok(())
    }
}

fn flatten(out: &mut LangMap, prefix: &str, value: &toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                let next = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(out, &next, v);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        _ => {}  // Non-string values ignored; localization only handles strings.
    }
}

#[derive(Debug, Clone)]
pub struct Translator {
    lang: String,
    catalog: Catalog,
}

impl Translator {
    pub fn new(lang: String, catalog: Catalog) -> Self {
        Self { lang, catalog }
    }

    /// Test helper that clones the catalog (production code should reuse one Catalog).
    pub fn new_for_test(lang: &str, catalog: &Catalog) -> Self {
        Self { lang: lang.into(), catalog: catalog.clone() }
    }

    pub fn lookup(&self, key: &str) -> String {
        self.lookup_with(key, &[])
    }

    pub fn lookup_with(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let raw = self.lookup_raw(key).unwrap_or_else(|| key.to_string());
        interpolate(&raw, vars)
    }

    fn lookup_raw(&self, key: &str) -> Option<String> {
        if let Some(map) = self.catalog.locales.get(&self.lang) {
            if let Some(value) = map.get(key) {
                return Some(value.clone());
            }
        }
        // Per-key fallback to en.
        self.catalog.locales.get("en").and_then(|m| m.get(key)).cloned()
    }
}

fn interpolate(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        let placeholder = format!("{{{name}}}");
        out = out.replace(&placeholder, value);
    }
    out
}
```

- [ ] **Step 4: User-override loader**

Add a constructor that loads `~/.config/shac/locales/<lang>.toml` if present and merges on top of bundled:

```rust
use std::fs;
use std::path::Path;

impl Catalog {
    /// Build full catalog: bundled `en` + bundled `<lang>` (if present) + user override at
    /// `<config_dir>/locales/<lang>.toml` (if present). User override wins per-key.
    pub fn build(config_dir: &Path, lang: &str) -> Self {
        let mut catalog = Self::bundled_en();
        // Future: bundled non-en locales would be merged here via include_str! per language.
        let user_path = config_dir.join("locales").join(format!("{lang}.toml"));
        if user_path.exists() {
            if let Ok(raw) = fs::read_to_string(&user_path) {
                let _ = catalog.merge_locale(lang, &raw);
            }
        }
        // Also let the user override english, e.g. for re-wording.
        let user_en = config_dir.join("locales").join("en.toml");
        if user_en.exists() && lang != "en" {
            if let Ok(raw) = fs::read_to_string(&user_en) {
                let _ = catalog.merge_locale("en", &raw);
            }
        } else if user_en.exists() {
            // lang == en: same merge as above.
            if let Ok(raw) = fs::read_to_string(&user_en) {
                let _ = catalog.merge_locale("en", &raw);
            }
        }
        catalog
    }
}
```

- [ ] **Step 5: Run tests**

```
cargo test --test locale
```

Expected: all tests PASS (12 total: 7 from Task 2 + 5 new).

- [ ] **Step 6: Commit**

```bash
git add src/i18n.rs locales/en.toml tests/locale.rs
git commit -m "feat(i18n): TOML catalog with bundled en, per-key fallback, interpolation"
```

---

## Task 4: Tips catalog — struct, categories, registry

**Goal:** Define `Tip`, `TipCategory`, `Context`, and a static catalog skeleton. Triggers all return `false` for now — they will be filled in Task 6.

**Files:**
- Create: `src/tips.rs`
- Modify: `src/lib.rs`
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Write the failing test** (append to `tests/tips.rs`)

```rust
use shac::tips::{catalog, TipCategory};

#[test]
fn catalog_has_expected_ids() {
    let ids: Vec<&str> = catalog().iter().map(|t| t.id).collect();
    let expected = [
        "hybrid_cd",
        "git_branches",
        "ssh_hosts",
        "npm_scripts",
        "kubectl_resources",
        "docker_images",
        "make_targets",
        "transitions",
        "path_jump_cyan",
        "unknown_command",
        "menu_detail_verbose",
        "tips_off",
    ];
    for id in expected {
        assert!(ids.contains(&id), "missing tip id: {id}");
    }
}

#[test]
fn catalog_categories_match_spec() {
    let by_id: std::collections::HashMap<&str, TipCategory> =
        catalog().iter().map(|t| (t.id, t.category)).collect();
    assert_eq!(by_id.get("git_branches").copied(), Some(TipCategory::Capability));
    assert_eq!(by_id.get("transitions").copied(), Some(TipCategory::Explanation));
    assert_eq!(by_id.get("tips_off").copied(), Some(TipCategory::Config));
}
```

- [ ] **Step 2: Run tests**

Expected: compile error — `shac::tips` does not exist.

- [ ] **Step 3: Create `src/tips.rs`**

```rust
//! Discoverability hints — catalog, selection, persistence, runtime.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipCategory {
    Capability = 0,
    Explanation = 1,
    Config = 2,
}

/// Read-only context passed to trigger predicates.
pub struct Context<'a> {
    pub line: &'a str,
    pub cursor: usize,
    pub cwd: &'a Path,
    pub tty: &'a str,
    pub home: &'a Path,
    /// Sources of candidates already chosen for this response (e.g. ["git_branches", "history"]).
    pub response_sources: &'a [String],
    /// Whether at least one candidate of kind=path_jump is present.
    pub has_path_jump: bool,
    /// Number of candidates in the current response.
    pub n_candidates: usize,
    /// Whether `<bin>` (first token) exists in PATH but is missing from the commands table.
    /// Set by the caller before invoking triggers; None if not computed.
    pub unknown_bin: Option<&'a str>,
}

pub struct Tip {
    pub id: &'static str,
    pub category: TipCategory,
    pub text_key: &'static str,
    pub max_shows: u32,
    pub source_hint: Option<&'static str>,
    pub trigger: fn(&Context) -> bool,
}

pub fn catalog() -> &'static [Tip] {
    &CATALOG
}

const CATALOG: &[Tip] = &[
    Tip {
        id: "hybrid_cd",
        category: TipCategory::Capability,
        text_key: "tips.hybrid_cd",
        max_shows: 3,
        source_hint: Some("path_jump"),
        trigger: triggers::hybrid_cd,
    },
    Tip {
        id: "git_branches",
        category: TipCategory::Capability,
        text_key: "tips.git_branches",
        max_shows: 3,
        source_hint: Some("git_branches"),
        trigger: triggers::git_branches,
    },
    Tip {
        id: "ssh_hosts",
        category: TipCategory::Capability,
        text_key: "tips.ssh_hosts",
        max_shows: 3,
        source_hint: Some("ssh_hosts"),
        trigger: triggers::ssh_hosts,
    },
    Tip {
        id: "npm_scripts",
        category: TipCategory::Capability,
        text_key: "tips.npm_scripts",
        max_shows: 3,
        source_hint: Some("npm_scripts"),
        trigger: triggers::npm_scripts,
    },
    Tip {
        id: "kubectl_resources",
        category: TipCategory::Capability,
        text_key: "tips.kubectl_resources",
        max_shows: 3,
        source_hint: Some("kubectl_resources"),
        trigger: triggers::kubectl_resources,
    },
    Tip {
        id: "docker_images",
        category: TipCategory::Capability,
        text_key: "tips.docker_images",
        max_shows: 3,
        source_hint: Some("docker_images"),
        trigger: triggers::docker_images,
    },
    Tip {
        id: "make_targets",
        category: TipCategory::Capability,
        text_key: "tips.make_targets",
        max_shows: 3,
        source_hint: Some("make_targets"),
        trigger: triggers::make_targets,
    },
    Tip {
        id: "transitions",
        category: TipCategory::Explanation,
        text_key: "tips.transitions",
        max_shows: 5,
        source_hint: Some("transitions"),
        trigger: triggers::transitions,
    },
    Tip {
        id: "path_jump_cyan",
        category: TipCategory::Explanation,
        text_key: "tips.path_jump_cyan",
        max_shows: 5,
        source_hint: Some("path_jump"),
        trigger: triggers::path_jump_cyan,
    },
    Tip {
        id: "unknown_command",
        category: TipCategory::Capability,
        text_key: "tips.unknown_command",
        max_shows: 3,
        source_hint: None,
        trigger: triggers::unknown_command,
    },
    Tip {
        id: "menu_detail_verbose",
        category: TipCategory::Config,
        text_key: "tips.menu_detail_verbose",
        max_shows: 2,
        source_hint: None,
        trigger: triggers::menu_detail_verbose,
    },
    Tip {
        id: "tips_off",
        category: TipCategory::Config,
        text_key: "tips.tips_off",
        max_shows: 2,
        source_hint: None,
        trigger: triggers::tips_off,
    },
];

mod triggers {
    use super::Context;

    // All return false until Task 6 fills them in.
    pub fn hybrid_cd(_: &Context) -> bool { false }
    pub fn git_branches(_: &Context) -> bool { false }
    pub fn ssh_hosts(_: &Context) -> bool { false }
    pub fn npm_scripts(_: &Context) -> bool { false }
    pub fn kubectl_resources(_: &Context) -> bool { false }
    pub fn docker_images(_: &Context) -> bool { false }
    pub fn make_targets(_: &Context) -> bool { false }
    pub fn transitions(_: &Context) -> bool { false }
    pub fn path_jump_cyan(_: &Context) -> bool { false }
    pub fn unknown_command(_: &Context) -> bool { false }
    pub fn menu_detail_verbose(_: &Context) -> bool { false }
    pub fn tips_off(_: &Context) -> bool { false }
}
```

- [ ] **Step 4: Register module**

In `src/lib.rs`, add `pub mod tips;` (alphabetical, between `tools` and the previous module).

- [ ] **Step 5: Run tests**

```
cargo test --test tips
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/tips.rs tests/tips.rs
git commit -m "feat(tips): catalog skeleton with 12 tips, all triggers stubbed"
```

---

## Task 5: Tips storage — load, upsert, mute, reset

**Goal:** All SQL operations on `tips_state` in one place. Pure functions on `&Connection` so they work in both daemon and CLI contexts.

**Files:**
- Modify: `src/tips.rs`
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Write failing tests**

Append to `tests/tips.rs`:

```rust
use rusqlite::Connection;
use shac::tips::storage;

fn test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE tips_state (
            tip_id          TEXT PRIMARY KEY,
            shows_count     INTEGER NOT NULL DEFAULT 0,
            last_shown_at   INTEGER,
            muted           INTEGER NOT NULL DEFAULT 0,
            muted_at        INTEGER,
            first_shown_at  INTEGER
        );",
    )
    .unwrap();
    conn
}

#[test]
fn record_show_inserts_and_increments() {
    let conn = test_db();
    storage::record_show(&conn, "git_branches", 1000).unwrap();
    storage::record_show(&conn, "git_branches", 2000).unwrap();
    let state = storage::load_all(&conn).unwrap();
    let entry = state.get("git_branches").expect("entry exists");
    assert_eq!(entry.shows_count, 2);
    assert_eq!(entry.last_shown_at, Some(2000));
    assert_eq!(entry.first_shown_at, Some(1000));
    assert!(!entry.muted);
}

#[test]
fn mute_and_unmute() {
    let conn = test_db();
    storage::record_show(&conn, "git_branches", 1000).unwrap();
    storage::mute(&conn, "git_branches", 5000).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert!(state.get("git_branches").unwrap().muted);

    storage::unmute(&conn, "git_branches").unwrap();
    let state = storage::load_all(&conn).unwrap();
    let e = state.get("git_branches").unwrap();
    assert!(!e.muted);
    assert_eq!(e.shows_count, 0, "unmute resets shows_count for a second chance");
}

#[test]
fn reset_clears_counts_but_preserves_mutes() {
    let conn = test_db();
    storage::record_show(&conn, "a", 1).unwrap();
    storage::record_show(&conn, "b", 1).unwrap();
    storage::mute(&conn, "b", 2).unwrap();
    storage::reset(&conn, false).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert_eq!(state.get("a").unwrap().shows_count, 0);
    assert!(state.get("b").unwrap().muted, "soft reset preserves mute");
}

#[test]
fn reset_hard_clears_everything() {
    let conn = test_db();
    storage::record_show(&conn, "a", 1).unwrap();
    storage::mute(&conn, "a", 2).unwrap();
    storage::reset(&conn, true).unwrap();
    let state = storage::load_all(&conn).unwrap();
    assert!(state.is_empty(), "hard reset deletes all rows");
}
```

- [ ] **Step 2: Implement `storage` submodule**

Append to `src/tips.rs`:

```rust
pub mod storage {
    use std::collections::HashMap;
    use anyhow::{Context, Result};
    use rusqlite::{params, Connection};

    #[derive(Debug, Clone, Default)]
    pub struct TipState {
        pub shows_count: u32,
        pub last_shown_at: Option<i64>,
        pub first_shown_at: Option<i64>,
        pub muted: bool,
        pub muted_at: Option<i64>,
    }

    pub type StateMap = HashMap<String, TipState>;

    pub fn load_all(conn: &Connection) -> Result<StateMap> {
        let mut stmt = conn.prepare(
            "SELECT tip_id, shows_count, last_shown_at, first_shown_at, muted, muted_at FROM tips_state",
        ).context("prepare load_all")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TipState {
                    shows_count: r.get::<_, i64>(1)? as u32,
                    last_shown_at: r.get(2)?,
                    first_shown_at: r.get(3)?,
                    muted: r.get::<_, i64>(4)? != 0,
                    muted_at: r.get(5)?,
                },
            ))
        }).context("query tips_state")?;
        let mut out = HashMap::new();
        for row in rows {
            let (id, state) = row.context("read row")?;
            out.insert(id, state);
        }
        Ok(out)
    }

    pub fn record_show(conn: &Connection, tip_id: &str, now: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO tips_state(tip_id, shows_count, last_shown_at, first_shown_at)
             VALUES (?1, 1, ?2, ?2)
             ON CONFLICT(tip_id) DO UPDATE SET
                 shows_count = shows_count + 1,
                 last_shown_at = excluded.last_shown_at",
            params![tip_id, now],
        ).context("upsert record_show")?;
        Ok(())
    }

    pub fn mute(conn: &Connection, tip_id: &str, now: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO tips_state(tip_id, shows_count, muted, muted_at)
             VALUES (?1, 0, 1, ?2)
             ON CONFLICT(tip_id) DO UPDATE SET muted = 1, muted_at = excluded.muted_at",
            params![tip_id, now],
        ).context("mute")?;
        Ok(())
    }

    pub fn unmute(conn: &Connection, tip_id: &str) -> Result<()> {
        conn.execute(
            "UPDATE tips_state SET muted = 0, muted_at = NULL, shows_count = 0 WHERE tip_id = ?1",
            params![tip_id],
        ).context("unmute")?;
        Ok(())
    }

    pub fn reset(conn: &Connection, hard: bool) -> Result<()> {
        if hard {
            conn.execute("DELETE FROM tips_state", []).context("reset --hard")?;
        } else {
            conn.execute(
                "UPDATE tips_state SET shows_count = 0, last_shown_at = NULL, first_shown_at = NULL",
                [],
            ).context("reset")?;
        }
        Ok(())
    }
}
```

- [ ] **Step 3: Run tests**

```
cargo test --test tips
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/tips.rs tests/tips.rs
git commit -m "feat(tips): storage layer (load_all/record_show/mute/unmute/reset)"
```

---

## Task 6: Tip triggers — fill in the predicates

**Goal:** Replace stubs from Task 4 with real detection logic.

**Files:**
- Modify: `src/tips.rs` (the `triggers` submodule)
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Write failing tests**

Append to `tests/tips.rs`:

```rust
use std::path::PathBuf;
use shac::tips::{Context, triggers_for_test};

fn ctx<'a>(line: &'a str, cwd: &'a PathBuf, home: &'a PathBuf, sources: &'a [String]) -> Context<'a> {
    Context {
        line,
        cursor: line.len(),
        cwd: cwd.as_path(),
        tty: "test-tty",
        home: home.as_path(),
        response_sources: sources,
        has_path_jump: sources.iter().any(|s| s == "path_jump"),
        n_candidates: sources.len(),
        unknown_bin: None,
    }
}

#[test]
fn git_branches_trigger_inside_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let c = ctx("git checkout ", &cwd, &home, &sources);
    assert!(triggers_for_test::git_branches(&c));
}

#[test]
fn git_branches_trigger_outside_git_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let c = ctx("git checkout ", &cwd, &home, &sources);
    assert!(!triggers_for_test::git_branches(&c));
}

#[test]
fn ssh_hosts_requires_ssh_config() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let cwd = home.clone();
    let sources = vec![];

    // No ~/.ssh/config → false.
    let c = ctx("ssh ", &cwd, &home, &sources);
    assert!(!triggers_for_test::ssh_hosts(&c));

    // Create config → true.
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("config"), "Host foo\n  HostName 1.2.3.4\n").unwrap();
    let c = ctx("ssh ", &cwd, &home, &sources);
    assert!(triggers_for_test::ssh_hosts(&c));
}

#[test]
fn npm_scripts_requires_package_json_and_command() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];

    let c = ctx("npm run ", &cwd, &home, &sources);
    assert!(!triggers_for_test::npm_scripts(&c));

    std::fs::write(cwd.join("package.json"), "{\"scripts\":{\"x\":\"y\"}}").unwrap();
    let c = ctx("npm run ", &cwd, &home, &sources);
    assert!(triggers_for_test::npm_scripts(&c));

    let c = ctx("npm install ", &cwd, &home, &sources);
    assert!(!triggers_for_test::npm_scripts(&c), "only `run` triggers");
}

#[test]
fn make_targets_requires_makefile_or_justfile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];

    let c = ctx("make ", &cwd, &home, &sources);
    assert!(!triggers_for_test::make_targets(&c));

    std::fs::write(cwd.join("Makefile"), "all:\n").unwrap();
    let c = ctx("make ", &cwd, &home, &sources);
    assert!(triggers_for_test::make_targets(&c));
}

#[test]
fn docker_trigger_matches_run_exec_rmi() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    for ok in ["docker run ", "docker exec ", "docker rmi "] {
        let c = ctx(ok, &cwd, &home, &sources);
        assert!(triggers_for_test::docker_images(&c), "{ok} should trigger");
    }
    for nope in ["docker ps ", "docker logs "] {
        let c = ctx(nope, &cwd, &home, &sources);
        assert!(!triggers_for_test::docker_images(&c), "{nope} should not trigger");
    }
}

#[test]
fn transitions_trigger_uses_response_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let with = vec!["transitions".to_string()];
    let without = vec!["history".to_string()];
    assert!(triggers_for_test::transitions(&ctx("foo", &cwd, &home, &with)));
    assert!(!triggers_for_test::transitions(&ctx("foo", &cwd, &home, &without)));
}

#[test]
fn unknown_command_uses_unknown_bin_field() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let mut c = ctx("kubectx ", &cwd, &home, &sources);
    c.unknown_bin = Some("kubectx");
    c.n_candidates = 0;
    assert!(triggers_for_test::unknown_command(&c));

    let mut c = ctx("kubectx ", &cwd, &home, &sources);
    c.unknown_bin = None;
    assert!(!triggers_for_test::unknown_command(&c));
}
```

Add to `[dev-dependencies]` in `Cargo.toml` if absent: `tempfile = "3"`.

- [ ] **Step 2: Implement triggers**

Replace the `mod triggers` block in `src/tips.rs` with real implementations:

```rust
mod triggers {
    use std::path::Path;
    use super::Context;

    pub fn hybrid_cd(c: &Context) -> bool {
        starts_with_token(c.line, "cd")
            && c.has_path_jump
    }

    pub fn git_branches(c: &Context) -> bool {
        if !matches_subcommand(c.line, "git", &["checkout", "switch", "merge", "rebase"]) {
            return false;
        }
        let mut path = c.cwd.to_path_buf();
        loop {
            if path.join(".git").exists() {
                return true;
            }
            if !path.pop() {
                return false;
            }
        }
    }

    pub fn ssh_hosts(c: &Context) -> bool {
        if !starts_with_token(c.line, "ssh") {
            return false;
        }
        let cfg = c.home.join(".ssh").join("config");
        std::fs::metadata(&cfg).map(|m| m.len() > 0).unwrap_or(false)
    }

    pub fn npm_scripts(c: &Context) -> bool {
        if !matches_subcommand(c.line, "npm", &["run"])
            && !matches_subcommand(c.line, "pnpm", &["run"])
            && !matches_subcommand(c.line, "yarn", &["run"]) {
            return false;
        }
        c.cwd.join("package.json").exists()
    }

    pub fn kubectl_resources(c: &Context) -> bool {
        if !line_starts_with_word(c.line, "kubectl") {
            return false;
        }
        // Require at least 2 tokens after `kubectl ` to avoid firing on bare `kubectl `.
        let rest = c.line.strip_prefix("kubectl ").unwrap_or("");
        if rest.split_whitespace().next().is_none() {
            return false;
        }
        // Kubeconfig: $KUBECONFIG or ~/.kube/config.
        if std::env::var_os("KUBECONFIG").is_some() {
            return true;
        }
        c.home.join(".kube").join("config").exists()
    }

    pub fn docker_images(c: &Context) -> bool {
        matches_subcommand(c.line, "docker", &["run", "exec", "rmi"])
    }

    pub fn make_targets(c: &Context) -> bool {
        if !line_starts_with_word(c.line, "make") && !line_starts_with_word(c.line, "just") {
            return false;
        }
        c.cwd.join("Makefile").exists()
            || c.cwd.join("makefile").exists()
            || c.cwd.join("Justfile").exists()
            || c.cwd.join("justfile").exists()
    }

    pub fn transitions(c: &Context) -> bool {
        c.response_sources.iter().any(|s| s == "transitions")
    }

    pub fn path_jump_cyan(c: &Context) -> bool {
        c.has_path_jump
    }

    pub fn unknown_command(c: &Context) -> bool {
        c.unknown_bin.is_some() && c.n_candidates == 0
    }

    pub fn menu_detail_verbose(_: &Context) -> bool {
        // Cannot decide from Context alone — depends on user's historical menu open count.
        // The selection layer overrides this when it has access to such state. For now,
        // returning false keeps it dormant; revisit when stats wiring is added (deferred).
        false
    }

    pub fn tips_off(_: &Context) -> bool {
        // Same: depends on overall tips-shown count, applied by selection layer.
        false
    }

    fn starts_with_token(line: &str, token: &str) -> bool {
        let mut parts = line.split_whitespace();
        parts.next() == Some(token)
    }

    fn line_starts_with_word(line: &str, word: &str) -> bool {
        starts_with_token(line, word)
    }

    fn matches_subcommand(line: &str, prog: &str, subs: &[&str]) -> bool {
        let mut parts = line.split_whitespace();
        if parts.next() != Some(prog) {
            return false;
        }
        match parts.next() {
            Some(sub) => subs.contains(&sub),
            None => false,
        }
    }
}

/// Public test surface for the private `triggers` module. Each function
/// just delegates so integration tests in `tests/tips.rs` can call them.
pub mod triggers_for_test {
    use super::Context;
    pub fn git_branches(c: &Context) -> bool { super::triggers::git_branches(c) }
    pub fn ssh_hosts(c: &Context) -> bool { super::triggers::ssh_hosts(c) }
    pub fn npm_scripts(c: &Context) -> bool { super::triggers::npm_scripts(c) }
    pub fn make_targets(c: &Context) -> bool { super::triggers::make_targets(c) }
    pub fn docker_images(c: &Context) -> bool { super::triggers::docker_images(c) }
    pub fn transitions(c: &Context) -> bool { super::triggers::transitions(c) }
    pub fn unknown_command(c: &Context) -> bool { super::triggers::unknown_command(c) }
    pub fn hybrid_cd(c: &Context) -> bool { super::triggers::hybrid_cd(c) }
    pub fn kubectl_resources(c: &Context) -> bool { super::triggers::kubectl_resources(c) }
    pub fn path_jump_cyan(c: &Context) -> bool { super::triggers::path_jump_cyan(c) }
}
```

- [ ] **Step 3: Run tests**

```
cargo test --test tips
```

Expected: PASS (all trigger tests + earlier tests still pass).

- [ ] **Step 4: Commit**

```bash
git add src/tips.rs tests/tips.rs Cargo.toml Cargo.lock
git commit -m "feat(tips): trigger predicates for catalog (git/ssh/npm/docker/make/etc)"
```

---

## Task 7: Tip selection algorithm + session runtime

**Goal:** Given the catalog, current state, and an in-memory session set, pick at most one tip per request.

**Files:**
- Modify: `src/tips.rs`
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Write failing tests**

Append to `tests/tips.rs`:

```rust
use shac::tips::{select, SelectInput, SessionState, storage::TipState};
use std::collections::{HashMap, HashSet};

fn empty_state() -> HashMap<String, TipState> { HashMap::new() }
fn empty_session() -> SessionState { SessionState::default() }

#[test]
fn select_returns_none_when_no_trigger_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let context = ctx("ls -la", &cwd, &home, &sources);
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_skips_muted() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let home = PathBuf::from("/tmp/nope");
    let cwd = tmp.path().to_path_buf();
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut state = empty_state();
    state.insert("git_branches".into(), TipState { muted: true, ..Default::default() });
    let input = SelectInput {
        context: &context,
        state: &state,
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_skips_when_session_already_saw_tip() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut session = empty_session();
    session.shown_this_session.insert("git_branches".into());
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &session,
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none());
}

#[test]
fn select_caps_per_session_max() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    let sources = vec![];
    let context = ctx("git checkout main", &cwd, &home, &sources);

    let mut session = empty_session();
    session.shown_this_session.insert("a".into());
    session.shown_this_session.insert("b".into());
    session.shown_this_session.insert("c".into());
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &session,
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    assert!(select(&input).is_none(), "session at max should suppress further tips");
}

#[test]
fn select_capability_beats_explanation() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let home = PathBuf::from("/tmp/nope");
    // Both `git_branches` (Capability) and `transitions` (Explanation) match.
    let sources = vec!["transitions".to_string()];
    let context = ctx("git checkout main", &cwd, &home, &sources);
    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &HashSet::new(),
        tips_per_session_max: 3,
    };
    let picked = select(&input).expect("a tip");
    assert_eq!(picked.id, "git_branches");
}

#[test]
fn select_prefers_zero_acceptance_within_category() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("config"), "Host x\n").unwrap();
    let cwd = home.clone();
    std::fs::write(cwd.join("package.json"), "{\"scripts\":{}}").unwrap();
    let sources = vec![];
    let context = ctx("ssh ", &cwd, &home, &sources);
    // Both ssh_hosts and (separately) npm_scripts are Capability.
    // Set `npm_scripts` source to have ≥1 acceptance, ssh_hosts to have zero.
    let mut zero = HashSet::new();
    zero.insert("ssh_hosts".to_string());

    let input = SelectInput {
        context: &context,
        state: &empty_state(),
        session: &empty_session(),
        zero_acceptance_sources: &zero,
        tips_per_session_max: 3,
    };
    let picked = select(&input).expect("a tip");
    assert_eq!(picked.id, "ssh_hosts");
}
```

- [ ] **Step 2: Implement `select` and `SessionState`**

Append to `src/tips.rs`:

```rust
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Default, Clone)]
pub struct SessionState {
    pub shown_this_session: HashSet<String>,
    pub last_tab_at: Option<Instant>,
}

pub struct SelectInput<'a> {
    pub context: &'a Context<'a>,
    pub state: &'a HashMap<String, storage::TipState>,
    pub session: &'a SessionState,
    /// Source names where the user has 0 lifetime acceptances. Used as a priority signal.
    pub zero_acceptance_sources: &'a HashSet<String>,
    pub tips_per_session_max: usize,
}

pub fn select<'a>(input: &'a SelectInput<'a>) -> Option<&'static Tip> {
    if input.session.shown_this_session.len() >= input.tips_per_session_max {
        return None;
    }
    let mut candidates: Vec<&'static Tip> = catalog().iter().filter(|t| {
        if !(t.trigger)(input.context) { return false; }
        if input.session.shown_this_session.contains(t.id) { return false; }
        if let Some(s) = input.state.get(t.id) {
            if s.muted { return false; }
            if s.shows_count >= t.max_shows { return false; }
        }
        true
    }).collect();

    candidates.sort_by_key(|t| {
        let category_rank = t.category as u8;
        let zero_acc_priority = match t.source_hint {
            Some(src) if input.zero_acceptance_sources.contains(src) => 0u8,  // first
            _ => 1u8,
        };
        let last_shown = input.state.get(t.id).and_then(|s| s.last_shown_at).unwrap_or(0);
        (category_rank, zero_acc_priority, last_shown)
    });

    candidates.into_iter().next()
}
```

- [ ] **Step 3: Run tests**

```
cargo test --test tips
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add src/tips.rs tests/tips.rs
git commit -m "feat(tips): selection algorithm with category/zero-accept/recency ordering"
```

---

## Task 8: Add config keys and `tips_state` config integration

**Goal:** Wire `ui.show_tips`, `ui.tips_per_session_max`, `ui.tips_max_shows_default`, `ui.first_run_greeter`, `ui.locale` through `AppConfig::get_key`/`set_key`.

**Files:**
- Modify: `src/config.rs`
- Test: extend an existing config test or add inline `#[cfg(test)]` block

- [ ] **Step 1: Add fields to `UiConfig`**

In `src/config.rs`, replace the `UiConfig` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub zsh: ZshUiConfig,
    pub show_tips: bool,
    pub tips_per_session_max: usize,
    pub tips_max_shows_default: u32,
    pub first_run_greeter: bool,
    pub locale: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            zsh: ZshUiConfig::default(),
            show_tips: true,
            tips_per_session_max: 3,
            tips_max_shows_default: 3,
            first_run_greeter: true,
            locale: String::new(),
        }
    }
}
```

- [ ] **Step 2: Extend `get_key` and `set_key`**

Inside `AppConfig::get_key`, add the new arms:

```rust
"ui.show_tips" => Some(self.ui.show_tips.to_string()),
"ui.tips_per_session_max" => Some(self.ui.tips_per_session_max.to_string()),
"ui.tips_max_shows_default" => Some(self.ui.tips_max_shows_default.to_string()),
"ui.first_run_greeter" => Some(self.ui.first_run_greeter.to_string()),
"ui.locale" => Some(self.ui.locale.clone()),
```

Inside `AppConfig::set_key`:

```rust
"ui.show_tips" => self.ui.show_tips = parse_bool(value)?,
"ui.tips_per_session_max" => self.ui.tips_per_session_max = value.parse()?,
"ui.tips_max_shows_default" => self.ui.tips_max_shows_default = value.parse()?,
"ui.first_run_greeter" => self.ui.first_run_greeter = parse_bool(value)?,
"ui.locale" => self.ui.locale = value.to_string(),
```

- [ ] **Step 3: Smoke-test via integration**

Append to `tests/tips.rs`:

```rust
#[test]
fn config_set_show_tips_persists() {
    let env = support::TestEnv::new("config-show-tips");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["config", "set", "ui.show_tips", "false"]);
    let out = support::run_ok(&env, ["config", "get", "ui.show_tips"]);
    assert!(out.trim().ends_with("false"), "got: {out}");
}
```

- [ ] **Step 4: Run all tests**

```
cargo test
```

Expected: PASS (existing tests still pass; new test passes).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs tests/tips.rs
git commit -m "feat(config): add ui.show_tips, ui.tips_*, ui.first_run_greeter, ui.locale"
```

---

## Task 9: Wire tip into completion response (daemon side)

**Goal:** After scoring/ranking, the engine selects a tip and attaches it to `CompletionResponse`. Build the `Context`, pull `tips_state` from DB, call `tips::select`.

**Files:**
- Modify: `src/protocol.rs`
- Modify: `src/engine.rs`
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Extend the protocol**

In `src/protocol.rs`, add to `CompletionResponse`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionTip {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub request_id: Option<i64>,
    pub items: Vec<CompletionItem>,
    pub mode: String,
    pub fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<CompletionTip>,
}
```

- [ ] **Step 2: Plumb tip into engine**

This is the largest change in the plan, but kept minimal: a new helper in `engine.rs` that runs after ranking and before returning.

Find the `complete` method on `Engine` in `src/engine.rs`. After the response items are populated and before `Ok(CompletionResponse { … })`, insert:

```rust
let tip = self.maybe_pick_tip(&request, &items);
```

And include `tip` in the response struct literal:

```rust
Ok(CompletionResponse {
    request_id,
    items,
    mode,
    fallback,
    tip,
})
```

Add a new private method on `Engine`:

```rust
fn maybe_pick_tip(
    &self,
    request: &CompletionRequest,
    items: &[CompletionItem],
) -> Option<CompletionTip> {
    use crate::tips;
    use crate::i18n::{Catalog, Translator, resolve_locale};
    use std::collections::HashSet;

    // Honor SHAC_NO_TIPS.
    if request.env.get("SHAC_NO_TIPS").map(|v| v == "1").unwrap_or(false) {
        return None;
    }
    if !self.config.ui.show_tips {
        return None;
    }
    if self.config.ui.zsh.menu_detail == "minimal" {
        return None;
    }

    // Build context.
    let cwd = std::path::Path::new(&request.cwd);
    let home = self.app_paths.home_or_default();  // see helper below
    let tty = request.session.tty.as_deref().unwrap_or("");
    let response_sources: Vec<String> = items.iter().map(|i| i.source.clone()).collect();
    let has_path_jump = response_sources.iter().any(|s| s == "path_jump");
    let unknown_bin: Option<&str> = if items.is_empty() {
        first_token(&request.line).filter(|bin| {
            // Cheap check: bin in PATH but not in commands table.
            self.db.command_known(bin).map(|known| !known).unwrap_or(false)
        })
    } else { None };

    let context = tips::Context {
        line: &request.line,
        cursor: request.cursor,
        cwd,
        tty,
        home: &home,
        response_sources: &response_sources,
        has_path_jump,
        n_candidates: items.len(),
        unknown_bin,
    };

    // Load persisted state and per-tty session state.
    let state = self.db.load_tips_state().ok()?;
    let session = self.tips_runtime.session_for(tty);
    let tips_per_session_max = self.config.ui.tips_per_session_max;
    let zero_acceptance_sources = self.db.zero_acceptance_sources().unwrap_or_default();

    // First-run greeter overrides selection.
    if self.config.ui.first_run_greeter && self.db.is_first_run().unwrap_or(false) {
        let _ = self.db.mark_first_run_done();
        let translator = self.translator();
        let text = translator.lookup("greeter.first_run");
        return Some(CompletionTip { id: "__greeter__".into(), text });
    }

    let input = tips::SelectInput {
        context: &context,
        state: &state,
        session: &session,
        zero_acceptance_sources: &zero_acceptance_sources,
        tips_per_session_max,
    };
    let picked = tips::select(&input)?;

    // Record show + update session.
    let now = unix_now();
    let _ = self.db.record_tip_show(picked.id, now);
    self.tips_runtime.record_show(tty, picked.id);

    let translator = self.translator();
    let text = translator.lookup(picked.text_key);
    Some(CompletionTip { id: picked.id.into(), text })
}

fn first_token(line: &str) -> Option<&str> {
    line.split_whitespace().next()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 3: Add the `Engine` support members**

The above references `self.tips_runtime` and `self.translator()`. Add:

```rust
// In Engine struct (find the existing struct definition in src/engine.rs and
// add these two fields):
tips_runtime: crate::tips::Runtime,
translator_cache: std::sync::OnceLock<crate::i18n::Translator>,
```

In `Engine::new` (find the constructor in `src/engine.rs`), initialize:

```rust
tips_runtime: crate::tips::Runtime::default(),
translator_cache: std::sync::OnceLock::new(),
```

(`std::sync::OnceLock` is stable since Rust 1.70 — no extra dependency.)

Add a method:

```rust
fn translator(&self) -> &crate::i18n::Translator {
    self.translator_cache.get_or_init(|| {
        let resolved = crate::i18n::resolve_locale(
            std::env::var("SHAC_LOCALE").ok(),
            Some(self.config.ui.locale.clone()),
            std::env::var("LC_MESSAGES").ok(),
            std::env::var("LANG").ok(),
        );
        let catalog = crate::i18n::Catalog::build(&self.app_paths.config_dir, &resolved.lang);
        crate::i18n::Translator::new(resolved.lang, catalog)
    })
}
```

> **Note for the implementer:** verify the actual field name on `Engine` for paths
> and DB. The plan assumes `self.app_paths` and `self.db`. If `Engine` exposes
> them under different names (e.g. `paths`, `database`), adjust references in
> `maybe_pick_tip` and `translator()` accordingly.
```

- [ ] **Step 4: Add `Runtime` to `tips.rs`**

Append to `src/tips.rs`:

```rust
use std::sync::Mutex;

#[derive(Default)]
pub struct Runtime {
    sessions: Mutex<HashMap<String, SessionState>>,
}

impl Runtime {
    pub fn session_for(&self, tty: &str) -> SessionState {
        let map = self.sessions.lock().expect("tips runtime mutex");
        map.get(tty).cloned().unwrap_or_default()
    }

    pub fn record_show(&self, tty: &str, tip_id: &str) {
        let mut map = self.sessions.lock().expect("tips runtime mutex");
        let entry = map.entry(tty.to_string()).or_default();
        entry.shown_this_session.insert(tip_id.to_string());
        entry.last_tab_at = Some(Instant::now());
    }
}
```

- [ ] **Step 5: Add helper methods to `Database`** (in `src/db.rs`)

```rust
pub fn load_tips_state(&self) -> Result<HashMap<String, crate::tips::storage::TipState>> {
    crate::tips::storage::load_all(&self.conn)
}

pub fn record_tip_show(&self, tip_id: &str, now: i64) -> Result<()> {
    crate::tips::storage::record_show(&self.conn, tip_id, now)
}

pub fn is_first_run(&self) -> Result<bool> {
    let row: Option<String> = self.conn
        .query_row("SELECT value FROM app_meta WHERE key = 'tips_first_run_done'",
                   [], |r| r.get(0))
        .optional()?;
    Ok(row.is_none())
}

pub fn mark_first_run_done(&self) -> Result<()> {
    self.conn.execute(
        "INSERT INTO app_meta(key, value) VALUES ('tips_first_run_done', '1') \
         ON CONFLICT(key) DO UPDATE SET value = '1'", []).context("mark first run done")?;
    Ok(())
}

pub fn command_known(&self, name: &str) -> Result<bool> {
    let n: i64 = self.conn
        .query_row("SELECT count(*) FROM commands WHERE name = ?1", params![name], |r| r.get(0))?;
    Ok(n > 0)
}

pub fn zero_acceptance_sources(&self) -> Result<HashSet<String>> {
    // Return the set of candidate sources that have zero acceptance events recorded.
    // Conservative implementation: query distinct sources from completion_items where
    // accepted=1, then return the catalog source list minus that set. If schema doesn't
    // track per-source acceptances directly, return all known catalog sources (everyone
    // is "zero-accept" until ranking captures it). For v1 this is acceptable — the
    // priority is a soft signal, not a hard filter.
    Ok(HashSet::new())
}
```

`zero_acceptance_sources` is intentionally a no-op in v1 (returns empty set, so the priority signal is neutral). Wiring it to real acceptance counts is a follow-up — the spec calls this out as a "soft signal".

- [ ] **Step 6: Add `home_or_default` to `AppPaths`**

```rust
impl AppPaths {
    pub fn home_or_default(&self) -> std::path::PathBuf {
        dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
    }
}
```

(Or, if `Engine` has a clearer way to find HOME, use that.)

- [ ] **Step 7: Run tests**

```
cargo build && cargo test
```

Expected: builds; existing tests still pass. New tip is not yet visible in the response — that comes from end-to-end test in next task.

- [ ] **Step 8: Commit**

```bash
git add src/protocol.rs src/engine.rs src/tips.rs src/db.rs src/config.rs Cargo.toml Cargo.lock
git commit -m "feat(engine): pick a tip per /complete response, persist, translate"
```

---

## Task 10: Emit `__shac_tip` in shell-tsv-v2 output

**Goal:** When the JSON response has `tip`, `print_completion_response` writes one extra line.

**Files:**
- Modify: `src/bin/shac.rs` (around line 1197, `format == "shell-tsv-v2"` branch)
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Modify the formatter**

In `src/bin/shac.rs`, inside the `else if format == "shell-tsv-v2"` block, after the `for item in items { … }` loop ends, add:

```rust
if let Some(tip) = response.get("tip").and_then(|v| v.as_object()) {
    let id = tip.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let text = tip.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if !id.is_empty() && !text.is_empty() {
        println!("__shac_tip\t{}\t{}", sanitize_shell_field(id), sanitize_shell_field(text));
    }
}
```

- [ ] **Step 2: End-to-end test**

Append to `tests/tips.rs`:

```rust
#[test]
fn complete_in_git_repo_emits_tip_line() {
    let env = support::TestEnv::new("tip-line");
    let _daemon = env.spawn_daemon();
    // Create a git repo inside cwd.
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        [
            "complete", "--shell", "zsh",
            "--line", "git checkout main",
            "--cursor", "20",
            "--cwd", cwd.to_string_lossy().as_ref(),
            "--format", "shell-tsv-v2",
        ],
    );
    // First call may emit the greeter rather than git_branches; either is acceptable.
    assert!(out.contains("__shac_tip"), "expected __shac_tip line, got:\n{out}");
}

#[test]
fn shac_no_tips_env_suppresses_tip_line() {
    let env = support::TestEnv::new("no-tip-env");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let mut cmd = env.shac_cmd();
    cmd.env("SHAC_NO_TIPS", "1");
    cmd.args([
        "complete", "--shell", "zsh",
        "--line", "git checkout main",
        "--cursor", "20",
        "--cwd", cwd.to_string_lossy().as_ref(),
        "--format", "shell-tsv-v2",
    ]);
    let out = cmd.output().expect("run");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("__shac_tip"), "SHAC_NO_TIPS should suppress: {stdout}");
}
```

For the env var to reach the daemon, ensure `CompletionRequest` carries env. Look at `completion_request(args)` in `src/bin/shac.rs`. If it does not currently propagate env, add a one-liner:

```rust
// Inside completion_request(args):
let mut env = HashMap::new();
for key in ["SHAC_NO_TIPS", "SHAC_LOCALE", "SHAC_TIPS_DEBUG"] {
    if let Ok(value) = std::env::var(key) {
        env.insert(key.to_string(), value);
    }
}
// then pass `env` into the CompletionRequest struct
```

- [ ] **Step 3: Run tests**

```
cargo test --test tips complete_in_git_repo_emits_tip_line
cargo test --test tips shac_no_tips_env_suppresses_tip_line
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/bin/shac.rs tests/tips.rs
git commit -m "feat(cli): emit __shac_tip line in shell-tsv-v2 + propagate SHAC_NO_TIPS"
```

---

## Task 11: `shac tips` subcommand (list/mute/unmute/reset)

**Goal:** CLI surface for managing tips state.

**Files:**
- Modify: `src/bin/shac.rs`
- Test: `tests/tips.rs` (extend)

- [ ] **Step 1: Add to clap enums**

In `src/bin/shac.rs`, in the top-level `Commands` enum:

```rust
Tips(TipsArgs),
```

Add the new structs:

```rust
#[derive(Debug, Args)]
struct TipsArgs {
    #[command(subcommand)]
    action: TipsAction,
}

#[derive(Debug, Subcommand)]
enum TipsAction {
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        muted: bool,
    },
    Mute { id: String },
    Unmute { id: String },
    Reset {
        #[arg(long)]
        hard: bool,
    },
}
```

- [ ] **Step 2: Implement the dispatch**

Find where other subcommands are dispatched (e.g. `Commands::Config(args) => …`) and add:

```rust
Commands::Tips(args) => run_tips(&paths, args),
```

Add the handler:

```rust
fn run_tips(paths: &AppPaths, args: TipsArgs) -> Result<()> {
    let conn = rusqlite::Connection::open(&paths.db_file)
        .with_context(|| format!("open db at {:?}", paths.db_file))?;
    match args.action {
        TipsAction::List { all, muted } => {
            let state = shac::tips::storage::load_all(&conn)?;
            let catalog = shac::tips::catalog();
            for tip in catalog {
                let s = state.get(tip.id);
                let is_muted = s.map(|x| x.muted).unwrap_or(false);
                let count = s.map(|x| x.shows_count).unwrap_or(0);
                if muted && !is_muted { continue; }
                if !all && !muted && count == 0 && !is_muted { continue; }
                let status = if is_muted { "muted" } else { "active" };
                println!("{:30} {:11} shows={}/{}", tip.id, status, count, tip.max_shows);
            }
            Ok(())
        }
        TipsAction::Mute { id } => {
            let now = unix_now_secs();
            shac::tips::storage::mute(&conn, &id, now)?;
            println!("muted: {id}");
            Ok(())
        }
        TipsAction::Unmute { id } => {
            shac::tips::storage::unmute(&conn, &id)?;
            println!("unmuted: {id}");
            Ok(())
        }
        TipsAction::Reset { hard } => {
            shac::tips::storage::reset(&conn, hard)?;
            println!(if hard { "tips state reset (hard)" } else { "tips state reset (soft)" });
            Ok(())
        }
    }
}

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64).unwrap_or(0)
}
```

- [ ] **Step 3: Integration test**

Append to `tests/tips.rs`:

```rust
#[test]
fn tips_mute_then_list_shows_muted() {
    let env = support::TestEnv::new("tips-cli");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["tips", "mute", "git_branches"]);
    let out = support::run_ok(&env, ["tips", "list", "--muted"]);
    assert!(out.contains("git_branches"));
    assert!(out.contains("muted"));
}

#[test]
fn tips_reset_hard_clears_state() {
    let env = support::TestEnv::new("tips-reset");
    let _daemon = env.spawn_daemon();
    support::run_ok(&env, ["tips", "mute", "git_branches"]);
    support::run_ok(&env, ["tips", "reset", "--hard"]);
    let out = support::run_ok(&env, ["tips", "list", "--all"]);
    assert!(!out.contains("muted"), "expected no muted after hard reset, got:\n{out}");
}
```

- [ ] **Step 4: Run tests**

```
cargo test --test tips
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/shac.rs tests/tips.rs
git commit -m "feat(cli): shac tips list/mute/unmute/reset[--hard]"
```

---

## Task 12: `shac locale` subcommand

**Goal:** CLI for inspecting and overriding locale.

**Files:**
- Modify: `src/bin/shac.rs`
- Modify: `src/i18n.rs` (add `dump_keys`, list helper)
- Test: `tests/locale.rs` (extend)

- [ ] **Step 1: Add to clap**

```rust
// In Commands enum:
Locale(LocaleArgs),
```

```rust
#[derive(Debug, Args)]
struct LocaleArgs {
    #[command(subcommand)]
    action: LocaleAction,
}

#[derive(Debug, Subcommand)]
enum LocaleAction {
    List,
    Current,
    Set {
        #[arg(value_name = "LANG")]
        lang: Option<String>,
        #[arg(long)]
        unset: bool,
    },
    DumpKeys {
        #[arg(long)]
        missing: Option<String>,
    },
}
```

- [ ] **Step 2: Add helpers in `i18n.rs`**

```rust
impl Catalog {
    pub fn known_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = self.locales.get("en").map(|m| m.keys().cloned().collect()).unwrap_or_default();
        out.sort();
        out
    }

    pub fn missing_keys(&self, lang: &str) -> Vec<String> {
        let en = match self.locales.get("en") { Some(m) => m, None => return vec![] };
        let other = self.locales.get(lang);
        let mut out: Vec<String> = en.keys().filter(|k| {
            other.map(|m| !m.contains_key(*k)).unwrap_or(true)
        }).cloned().collect();
        out.sort();
        out
    }

    /// User-override locales found at <config_dir>/locales/*.toml.
    pub fn user_locale_files(config_dir: &Path) -> Vec<String> {
        let dir = config_dir.join("locales");
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) {
                    if let Some(stem) = name.strip_suffix(".toml") {
                        out.push(stem.to_string());
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
```

- [ ] **Step 3: Implement dispatch**

```rust
Commands::Locale(args) => run_locale(&paths, args),
```

```rust
fn run_locale(paths: &AppPaths, args: LocaleArgs) -> Result<()> {
    use shac::i18n::{Catalog, resolve_locale};
    match args.action {
        LocaleAction::List => {
            // Bundled (currently only "en") + any user-override files in <config>/locales.
            println!("en  (bundled)");
            for lang in Catalog::user_locale_files(&paths.config_dir) {
                println!("{lang}  (user)");
            }
            Ok(())
        }
        LocaleAction::Current => {
            let cfg = AppConfig::load(paths)?;
            let resolved = resolve_locale(
                std::env::var("SHAC_LOCALE").ok(),
                Some(cfg.ui.locale),
                std::env::var("LC_MESSAGES").ok(),
                std::env::var("LANG").ok(),
            );
            let source_label = match resolved.source {
                shac::i18n::LocaleSource::Env => "SHAC_LOCALE env",
                shac::i18n::LocaleSource::Config => "ui.locale config",
                shac::i18n::LocaleSource::AutoLcMessages => "LC_MESSAGES env",
                shac::i18n::LocaleSource::AutoLang => "LANG env",
                shac::i18n::LocaleSource::Default => "default (en)",
            };
            println!("{} (source: {source_label})", resolved.lang);
            Ok(())
        }
        LocaleAction::Set { lang, unset } => {
            let mut cfg = AppConfig::load(paths)?;
            if unset {
                cfg.ui.locale = String::new();
                cfg.save(paths)?;
                println!("ui.locale unset (back to auto-detect)");
            } else {
                let lang = lang.context("locale required unless --unset")?;
                cfg.ui.locale = lang.clone();
                cfg.save(paths)?;
                println!("ui.locale = {lang}");
            }
            Ok(())
        }
        LocaleAction::DumpKeys { missing } => {
            let cfg = AppConfig::load(paths)?;
            let resolved = resolve_locale(
                std::env::var("SHAC_LOCALE").ok(),
                Some(cfg.ui.locale),
                std::env::var("LC_MESSAGES").ok(),
                std::env::var("LANG").ok(),
            );
            let catalog = Catalog::build(&paths.config_dir, &resolved.lang);
            if let Some(target) = missing {
                for k in catalog.missing_keys(&target) {
                    println!("{k}");
                }
            } else {
                for k in catalog.known_keys() {
                    println!("{k}");
                }
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 4: Integration tests**

Append to `tests/locale.rs`:

```rust
#[test]
fn locale_current_reports_default_when_unset() {
    use shac::config::{AppPaths, AppConfig};
    let env = tests_support_minimal_env();
    let out = run_locale(&env, ["current"]);
    assert!(out.contains("en"), "expected en default, got: {out}");
}

// (harness helpers — adapt or duplicate `support` from tests/tips.rs)
```

For brevity here, skip the full `tests_support_minimal_env`/`run_locale` boilerplate — the project's existing pattern is to use `tests/support/mod.rs` from each test file. So instead reuse:

```rust
mod support;

#[test]
fn locale_current_reports_default_when_unset() {
    let env = support::TestEnv::new("locale-current");
    let out = support::run_ok(&env, ["locale", "current"]);
    assert!(out.contains("en"), "expected en default, got: {out}");
}

#[test]
fn locale_set_persists_to_config() {
    let env = support::TestEnv::new("locale-set");
    support::run_ok(&env, ["locale", "set", "ru"]);
    let out = support::run_ok(&env, ["config", "get", "ui.locale"]);
    assert!(out.trim().ends_with("ru"));
    support::run_ok(&env, ["locale", "set", "--unset"]);
    let out = support::run_ok(&env, ["config", "get", "ui.locale"]);
    assert!(out.trim().ends_with("ui.locale ="), "expected empty, got:\n{out}");
}

#[test]
fn locale_dump_keys_lists_known_keys() {
    let env = support::TestEnv::new("locale-dump");
    let out = support::run_ok(&env, ["locale", "dump-keys"]);
    assert!(out.contains("tips.git_branches"));
    assert!(out.contains("greeter.first_run"));
}
```

- [ ] **Step 5: Run tests**

```
cargo test --test locale
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/bin/shac.rs src/i18n.rs tests/locale.rs
git commit -m "feat(cli): shac locale list/current/set/dump-keys"
```

---

## Task 13: `shac suggest` subcommand

**Goal:** Standalone CLI that opens DB read-only, runs all triggers against current cwd, groups results.

**Files:**
- Create: `src/suggest.rs`
- Modify: `src/lib.rs`
- Modify: `src/bin/shac.rs`
- Test: `tests/suggest.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `tests/suggest.rs`:

```rust
mod support;

#[test]
fn suggest_in_git_repo_lists_git_branches() {
    let env = support::TestEnv::new("suggest-git");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        [
            "suggest",
            "--cwd",
            cwd.to_string_lossy().as_ref(),
        ],
    );
    assert!(out.contains("git_branches") || out.contains("branches of this repo"),
        "expected git_branches mention, got:\n{out}");
}

#[test]
fn suggest_all_lists_every_capability() {
    let env = support::TestEnv::new("suggest-all");
    let _daemon = env.spawn_daemon();
    let out = support::run_ok(&env, ["suggest", "--all"]);
    for id in ["git_branches", "ssh_hosts", "npm_scripts", "kubectl_resources",
               "docker_images", "make_targets", "hybrid_cd"] {
        assert!(out.contains(id), "expected {id} listed under --all, got:\n{out}");
    }
}

#[test]
fn suggest_json_returns_structured_output() {
    let env = support::TestEnv::new("suggest-json");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        ["suggest", "--cwd", cwd.to_string_lossy().as_ref(), "--json"],
    );
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).expect("parse json");
    assert!(parsed.get("groups").is_some(), "expected groups field, got:\n{out}");
}
```

- [ ] **Step 2: Implement `src/suggest.rs`**

```rust
//! `shac suggest` — context-aware list of applicable shac features.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::AppConfig;
use crate::i18n::{Catalog, Translator, resolve_locale};
use crate::tips::{self, Context, Tip, TipCategory};

#[derive(Debug, Serialize)]
pub struct SuggestItem {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SuggestGroup {
    pub title: String,
    pub items: Vec<SuggestItem>,
}

#[derive(Debug, Serialize, Default)]
pub struct SuggestOutput {
    pub groups: Vec<SuggestGroup>,
}

pub struct SuggestInput<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub config_dir: &'a Path,
    pub config: &'a AppConfig,
    pub all: bool,
    /// For Group: "Available here" — sources the user has accepted ≥1 in last N days.
    pub accepted_sources_recent: HashSet<String>,
}

pub fn run(input: &SuggestInput<'_>) -> Result<SuggestOutput> {
    let resolved = resolve_locale(
        std::env::var("SHAC_LOCALE").ok(),
        Some(input.config.ui.locale.clone()),
        std::env::var("LC_MESSAGES").ok(),
        std::env::var("LANG").ok(),
    );
    let catalog = Catalog::build(input.config_dir, &resolved.lang);
    let translator = Translator::new(resolved.lang, catalog);

    if input.all {
        let mut group = SuggestGroup {
            title: "all features".into(),
            items: vec![],
        };
        for tip in tips::catalog() {
            group.items.push(SuggestItem {
                id: tip.id.into(),
                text: translator.lookup(tip.text_key),
            });
        }
        return Ok(SuggestOutput { groups: vec![group] });
    }

    let response_sources: Vec<String> = vec![];
    let context = Context {
        line: "",
        cursor: 0,
        cwd: input.cwd,
        tty: "",
        home: input.home,
        response_sources: &response_sources,
        has_path_jump: false,
        n_candidates: 0,
        unknown_bin: None,
    };

    // Triggers that are *path/cwd-aware* but don't depend on the user's typed line:
    // run them with synthetic command lines for each capability.
    let probe_lines: &[(&str, &str)] = &[
        ("git_branches", "git checkout "),
        ("ssh_hosts", "ssh "),
        ("npm_scripts", "npm run "),
        ("kubectl_resources", "kubectl get "),
        ("docker_images", "docker run "),
        ("make_targets", "make "),
        ("hybrid_cd", "cd "),
    ];

    let mut available: Vec<&'static Tip> = vec![];
    for tip in tips::catalog() {
        let probe = probe_lines.iter().find(|(id, _)| *id == tip.id).map(|(_, l)| *l);
        let line = probe.unwrap_or("");
        let probe_ctx = Context { line, ..clone_context(&context) };
        if (tip.trigger)(&probe_ctx) {
            available.push(tip);
        }
    }

    // Split into "available + accepted" (used recently) vs "available + not-accepted".
    let mut group_used = SuggestGroup {
        title: translator.lookup("suggest.header_available"),
        items: vec![],
    };
    let mut group_unused = SuggestGroup {
        title: translator.lookup("suggest.header_unused"),
        items: vec![],
    };
    for tip in available {
        let used = tip.source_hint.map(|s| input.accepted_sources_recent.contains(s)).unwrap_or(false);
        let item = SuggestItem {
            id: tip.id.into(),
            text: translator.lookup(tip.text_key),
        };
        if used { group_used.items.push(item); } else { group_unused.items.push(item); }
    }

    let mut groups = vec![];
    if !group_used.items.is_empty() { groups.push(group_used); }
    if !group_unused.items.is_empty() { groups.push(group_unused); }
    if groups.is_empty() {
        groups.push(SuggestGroup {
            title: translator.lookup("suggest.no_matches"),
            items: vec![],
        });
    }

    Ok(SuggestOutput { groups })
}

pub fn render_text(out: &SuggestOutput) -> String {
    let mut buf = String::new();
    for (i, group) in out.groups.iter().enumerate() {
        if i > 0 { buf.push('\n'); }
        buf.push_str(&group.title);
        buf.push('\n');
        for item in &group.items {
            buf.push_str(&format!("  {}    {}\n", item.id, item.text));
        }
    }
    buf
}

fn clone_context<'a>(c: &Context<'a>) -> Context<'a> {
    Context {
        line: c.line,
        cursor: c.cursor,
        cwd: c.cwd,
        tty: c.tty,
        home: c.home,
        response_sources: c.response_sources,
        has_path_jump: c.has_path_jump,
        n_candidates: c.n_candidates,
        unknown_bin: c.unknown_bin,
    }
}
```

- [ ] **Step 3: Register module**

In `src/lib.rs`, add `pub mod suggest;`.

- [ ] **Step 4: Wire CLI**

In `src/bin/shac.rs`, add to `Commands` enum:

```rust
Suggest(SuggestArgs),
```

```rust
#[derive(Debug, Args)]
struct SuggestArgs {
    #[arg(long, default_value = ".")]
    cwd: String,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}
```

Dispatch:

```rust
Commands::Suggest(args) => run_suggest(&paths, args),
```

```rust
fn run_suggest(paths: &AppPaths, args: SuggestArgs) -> Result<()> {
    let cwd = std::path::PathBuf::from(&args.cwd).canonicalize().unwrap_or_else(|_| std::path::PathBuf::from(&args.cwd));
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    let cfg = AppConfig::load(paths).unwrap_or_default();

    let input = shac::suggest::SuggestInput {
        cwd: &cwd,
        home: &home,
        config_dir: &paths.config_dir,
        config: &cfg,
        all: args.all,
        accepted_sources_recent: std::collections::HashSet::new(), // TODO wire DB later
    };
    let output = shac::suggest::run(&input)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print!("{}", shac::suggest::render_text(&output));
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests**

```
cargo test --test suggest
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/suggest.rs src/bin/shac.rs tests/suggest.rs
git commit -m "feat(cli): shac suggest [--cwd|--all|--json] context-aware feature listing"
```

---

## Task 14: zsh adapter — parse and render the tip footer

**Goal:** `shell/zsh/shac.zsh` reads `__shac_tip` lines and shows them as a footer in the menu.

**Files:**
- Modify: `shell/zsh/shac.zsh`
- Create: `tests/zsh_render.sh`
- Modify: `tests/zsh_functions.rs` (add a render test)

- [ ] **Step 1: Add globals and parser**

Near the top of `shell/zsh/shac.zsh`, alongside other `typeset -g` declarations, add:

```sh
typeset -g _shac_pending_tip_id=""
typeset -g _shac_pending_tip_text=""
```

In `_shac_fetch_candidates`, inside the `while IFS= read -r line` loop, add a branch alongside the `__shac_request_id` parser:

```sh
elif [[ "$line" == __shac_tip$'\t'* ]]; then
  if [[ -z "${SHAC_NO_TIPS:-}" ]]; then
    local -a tip_fields
    tip_fields=("${(ps:\t:)line}")
    _shac_pending_tip_id="${tip_fields[2]:-}"
    _shac_pending_tip_text="${tip_fields[3]:-}"
  fi
```

In `_shac_reset_menu_state` (and any reset path), clear them:

```sh
_shac_pending_tip_id=""
_shac_pending_tip_text=""
```

- [ ] **Step 2: Render footer in `_shac_render_menu`**

At the end of `_shac_render_menu`, after the candidate-line loop and before `POSTDISPLAY=...`, append:

```sh
if [[ -n "$_shac_pending_tip_text" && -z "${SHAC_NO_TIPS:-}" ]]; then
  local bullet="💡"
  if [[ -n "${SHAC_NO_COLOR:-}" ]]; then
    bullet="tip:"
  fi
  lines+=("")
  lines+=("  ${bullet} ${_shac_pending_tip_text}")
fi
```

- [ ] **Step 3: Write the zsh harness**

Create `tests/zsh_render.sh`:

```sh
#!/bin/zsh
# Loaded by Rust integration test. Sources the adapter in test mode, manually
# populates state, calls _shac_render_menu, and prints POSTDISPLAY.

set -e

export SHAC_ZSH_TEST_MODE=1
ADAPTER="${1:?adapter path required}"
TIP_TEXT="${2:?tip text required}"
NO_TIPS="${3:-}"

if [[ -n "$NO_TIPS" ]]; then
  export SHAC_NO_TIPS=1
fi

# Provide minimal zsh stubs.
function zle() { return 0; }
typeset -g POSTDISPLAY=""

source "$ADAPTER"

# Populate one fake candidate so render path runs.
_shac_menu_item_keys=("k1")
_shac_menu_insert_texts=("t1")
_shac_menu_displays=("d1")
_shac_menu_kinds=("k")
_shac_menu_sources=("s")
_shac_menu_descriptions=("desc1")
_shac_menu_selected_index=1
_shac_pending_tip_id="some_id"
_shac_pending_tip_text="$TIP_TEXT"

_shac_render_menu

print -- "$POSTDISPLAY"
```

Make it executable: `chmod +x tests/zsh_render.sh`

- [ ] **Step 4: Add Rust integration test**

In `tests/zsh_functions.rs`, add (or append to existing test set):

```rust
use std::process::Command;

#[test]
fn zsh_render_menu_includes_tip_footer() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let adapter = format!("{manifest}/shell/zsh/shac.zsh");
    let harness = format!("{manifest}/tests/zsh_render.sh");
    let out = Command::new("zsh")
        .arg(&harness)
        .arg(&adapter)
        .arg("hello tip world")
        .output()
        .expect("run zsh harness");
    assert!(out.status.success(), "harness failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hello tip world"),
            "expected tip text in POSTDISPLAY, got:\n{stdout}");
}

#[test]
fn zsh_render_menu_skips_tip_when_no_tips_env_set() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let adapter = format!("{manifest}/shell/zsh/shac.zsh");
    let harness = format!("{manifest}/tests/zsh_render.sh");
    let out = Command::new("zsh")
        .arg(&harness)
        .arg(&adapter)
        .arg("hello tip world")
        .arg("1")  // NO_TIPS
        .output()
        .expect("run zsh harness");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("hello tip world"),
            "expected no tip when SHAC_NO_TIPS=1, got:\n{stdout}");
}
```

- [ ] **Step 5: Run tests**

```
cargo test --test zsh_functions
```

Expected: PASS. If `_shac_render_menu` is not designed to be called outside ZLE, it may need a small guard. The render code already checks `if zle` for `zle -R`, so it should run cleanly given the harness's `zle()` stub.

- [ ] **Step 6: Commit**

```bash
git add shell/zsh/shac.zsh tests/zsh_render.sh tests/zsh_functions.rs
git commit -m "feat(zsh): parse __shac_tip + render footer in completion menu"
```

---

## Task 15: Documentation, version bump, CHANGELOG

**Goal:** Cut v0.5.0. Document the new surface in README and the landing page.

**Files:**
- Modify: `README.md`
- Modify: `docs/index.html`
- Modify: `Cargo.toml` (version)
- Create or modify: `CHANGELOG.md` (if exists)

- [ ] **Step 1: Confirm version bump with user**

Per project memory: never bump version unilaterally. Pause and ask:

> "Bumping for the new discoverability ffeatures. Patch (0.4.1) is wrong here — we're adding new commands and a new wire-format line. Minor (0.5.0) feels right since there are no breaking changes. OK to bump to 0.5.0?"

Wait for user reply. Do NOT proceed past this step without confirmation.

- [ ] **Step 2: Bump `Cargo.toml`**

After approval, edit `Cargo.toml`:
```toml
version = "0.5.0"
```

- [ ] **Step 3: Update README**

Add a new section "Discoverability" near the existing feature list. One paragraph + a code block showing `shac suggest` example output.

- [ ] **Step 4: Update CHANGELOG**

If `CHANGELOG.md` exists, prepend a new entry. If not, create one with:

```markdown
# Changelog

## v0.5.0

### Added
- Contextual tips in the zsh completion menu (footer hint when context matches a feature)
- `shac suggest` — list features applicable to the current directory
- `shac tips list/mute/unmute/reset[--hard]` — manage tip state
- `shac locale list/current/set/dump-keys` — i18n controls
- First-run greeter on the first completion menu after install
- i18n scaffolding with `locales/en.toml` + extension point at `~/.config/shac/locales/<lang>.toml`
- New config keys: `ui.show_tips`, `ui.tips_per_session_max`, `ui.tips_max_shows_default`, `ui.first_run_greeter`, `ui.locale`
- New env vars: `SHAC_NO_TIPS`, `SHAC_LOCALE`, `SHAC_TIPS_DEBUG`

### Changed
- `shac complete` may now emit one optional `__shac_tip\t<id>\t<text>` line in `shell-tsv-v2` output

### Backward compatibility
- No breaking changes. Tips default to ON; opt out via `shac config set ui.show_tips false` or `SHAC_NO_TIPS=1`.
```

- [ ] **Step 5: Update landing page**

In `docs/index.html`, add a section under existing features with a screenshot/asciinema of the tip footer or `shac suggest`. Skip image for now if not available — text-only with a code block is acceptable.

- [ ] **Step 6: Run full test suite**

```
cargo test
```

Expected: ALL tests pass.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock README.md CHANGELOG.md docs/index.html
git commit -m "chore: bump to v0.5.0; document discoverability features"
```

---

## Self-Review Notes

Cross-checked plan against spec:

- ✅ JIT footer: Tasks 9 + 10 + 14 (engine selects → CLI emits sentinel → zsh renders)
- ✅ `shac suggest`: Task 13
- ✅ First-run greeter: Task 9 (greeter override path inside `maybe_pick_tip`)
- ✅ Hint catalog: Tasks 4 + 6
- ✅ Selection algorithm: Task 7
- ✅ Persistence (tips_state, app_meta): Tasks 1 + 5
- ✅ Localization: Tasks 2 + 3 + 12
- ✅ Configuration: Task 8
- ✅ CLI: Tasks 11, 12, 13
- ✅ zsh adapter: Task 14
- ✅ Tests: every task has tests (unit or integration)
- ✅ Rollout: Task 15

Noted simplifications vs. spec (acceptable for v1, called out in spec as "soft signal" or open items):
- `zero_acceptance_sources` returns empty in v1 — priority becomes neutral; planned follow-up
- `menu_detail_verbose` and `tips_off` triggers return false (need historical menu-open count and total-tips-shown count, not in v1)
- Homebrew `caveats` block: not in this plan (formula lives in a separate tap repo). Open follow-up.
- `SHAC_TIPS_DEBUG` logging: not implemented in v1; placeholder env var reserved.

These are documented in the spec under "Open items" and are explicitly out of scope for this plan.
