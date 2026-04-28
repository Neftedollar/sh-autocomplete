# shac Discoverability — Design Spec

**Date:** 2026-04-28
**Status:** approved, ready for implementation plan
**Target version:** v0.5.0

## Problem

Even shac's author cannot enumerate all features without grepping the codebase. New users have no chance — `brew install shac` finishes silently, the daemon starts, Tab works for the obvious cases, but the user never learns about hybrid `cd`, frecent paths, git branches, ssh hosts, npm scripts, kubectl resources, docker images, make/just targets, transitions, `shac index add-command`, configurable detail levels, etc.

Documentation does not solve this — nobody reads docs from inside the terminal. We need the system to **surface its own capabilities** at the moments they apply, with low noise and a clear opt-out path.

## Goals

1. A user who has never read the README can discover the most useful features within ~10 days of normal usage.
2. The author can ask "what can shac do here?" and get an answer without leaving the shell.
3. Power users who already know everything can mute the entire system with one config flag.
4. All user-facing text is English by default, with a clean extension point for community-contributed translations.

**Success metric:** after 2 weeks of dogfooding, Roman can name ≥3 features he "rediscovered" through tips, and naturally reaches for hybrid cd / git branches / ssh hosts in contexts where he previously typed paths manually.

## Non-goals

- Video tours, GIFs, web onboarding flows
- Push hints outside the menu (above the prompt, statusline, sidebar) — explicitly rejected during brainstorming as too intrusive
- A full TUI wizard (`shac tour`) with categorized feature browsing — YAGNI for v1
- Localization of existing shac UI strings (menu header `shac 2/7`, kind labels) — separate spec if requested
- Localized error messages and `shacd` logs — out of scope
- Bash adapter parity for hint footer — separate spec; v1 is zsh-only

## Architecture overview

Three discoverability channels:

1. **JIT footer in completion menu** — when the menu is open and the current context matches a hint trigger, the daemon attaches a single hint to its `/complete` response. The zsh adapter renders it as a footer line below the last candidate.
2. **Pull command `shac suggest`** — context-aware command. Reads cwd, recent commands, env. Prints capabilities applicable here, plus capabilities the user has not used recently. Plain text, pipeable.
3. **First-run greeter** — special hardcoded hint shown on the very first `/complete` response of a fresh daemon (no `tips_state` rows, empty `commands` table). Points to `shac suggest` and `shac config`. Never repeats.

State (per-tip show counts and mute flags) lives in a new SQLite table inside the existing `~/.cache/shac/shac.db`. Locale resolution and translation tables are handled by a new `i18n` module with bundled English defaults and a user-override extension point.

## Component: JIT footer in menu

### Visual

When the menu opens with ≥2 candidates and a tip is selected for this response, the last rendered line is the tip, separated from candidates by a blank line:

```
shac 2/7
> git checkout main         [branch/git_branches]  current branch
  git checkout dev          [branch/git_branches]
  git checkout feature/foo  [branch/git_branches]
  git checkout -            [option/cli_priors]    previous branch
  ...

  💡 branches of this repo are pulled automatically
```

The bullet glyph is `💡` by default; falls back to `tip:` when `SHAC_NO_COLOR=1` or no UTF-8 locale.

### Activation rules

A footer is rendered iff **all** of:
- Menu has ≥2 candidates (single-candidate auto-accept skips the menu entirely)
- A tip from the catalog matches the current context (Section "Hint catalog")
- That tip is not muted and `shows_count < max_shows`
- That tip has not been shown in this shell session yet
- This Tab-press has not already shown a tip (per-Tab budget = 1)
- `ui.show_tips=true` and `SHAC_NO_TIPS` is unset
- `ui.menu_detail` is not `minimal`

### Selection and behavior

When multiple tips match:
1. Filter out muted, exhausted, and already-shown-this-session.
2. Order by category: `capability` > `explanation` > `config`.
3. Within a category, prefer tips for features the user has 0 acceptances from (over the lifetime of `tips_state`).
4. Tie-break by oldest `last_shown_at` (round-robin).

Arrow keys (up/down) skip the footer — selection cycles only over candidates. The footer is non-interactive (cannot be activated). Closing the menu (Esc) does not mark the tip muted; only `shac tips mute <id>` does.

### Wire format

Existing `shell-tsv-v2` stream gains a sentinel line:
```
__shac_tip\t<tip_id>\t<localized_text>
```
Emitted at most once per response, after the candidate rows. The zsh adapter parses it into `_shac_pending_tip_id` and `_shac_pending_tip_text` and renders it in `_shac_render_menu`.

## Component: `shac suggest`

### Synopsis

```
shac suggest [--cwd PATH] [--all] [--json]
```

### Behavior

`shac suggest` (no flags) inspects current cwd and prints a grouped report:

```
$ shac suggest
shac is available in this directory:

  cd <Tab>                   global frecent paths (with → prefix), in addition to cwd children
  git checkout <Tab>         branches of this repo (3 local, 12 remote)
  npm run <Tab>              8 scripts from package.json
  ssh <Tab>                  14 hosts from ~/.ssh/config

Not used recently:
  shac index add-command <bin>     teach shac an unknown CLI via --help parsing
  shac config set ui.menu_detail verbose    more detail in menu

Full feature list: https://shac.dev/features
```

### Groups

- **Available here:** triggers that match current context AND user has used this source (≥1 acceptance from this source ever).
- **Not used recently:** triggers that match current context AND user has 0 acceptances in the last 7 days from this source.
- **Links:** static line(s) pointing to docs.

### Flags

- `--cwd PATH` — pretend cwd is PATH (useful for testing and scripts)
- `--all` — ignore context, list every catalog entry
- `--json` — machine-readable output: `{ groups: [{ title, items: [{ id, text, ... }] }] }`

### Trigger reuse

Triggers are the same predicates as for the JIT footer (Section "Hint catalog"). `suggest` shows all matching tips at once; the footer shows one at a time.

## Component: First-run greeter

### Trigger

When the daemon receives its first `/complete` request and detects the "fresh install" condition:
- `tips_state` table is empty
- `commands` table has 0 rows OR is missing the `meta_first_run_done` flag

The daemon includes a hardcoded greeter tip in the response:

```
💡 shac is ready. Run `shac suggest` to see what's available here. `shac config` to tune.
```

After emitting once, the daemon writes `meta_first_run_done=1` to the `meta` table. Never repeats.

### Why menu, not precmd

Alternative approaches considered and rejected:
- **Precmd echo at first shell load** — pollutes scrollback in IDEs and CI; fires before the user has signaled interest in shac UI
- **Echo on first `shell-init` source** — same problem
- **Daemon startup log** — invisible to the user

The first menu open is the earliest moment the user has actively engaged with shac UI. If they never press Tab, they are not a shac user and the greeter is unnecessary.

### Homebrew caveats

Add to the formula:

```ruby
def caveats
  <<~EOS
    Add to your shell config (.zshrc):
      eval "$(shac shell-init zsh)"

    After first Tab, run `shac suggest` to see what shac can do in your current directory.
  EOS
end
```

`brew install` prints caveats prominently — defense-in-depth for users who somehow miss the greeter.

## Component: Hint catalog

### Tip structure

```rust
struct Tip {
    id: &'static str,                    // stable identifier, used as PK in tips_state
    category: TipCategory,               // Capability | Explanation | Config
    text_key: &'static str,              // i18n key, never literal text
    max_shows: u32,                      // auto-mute threshold
    source_hint: Option<&'static str>,   // candidate source name (e.g. "git_branches")
                                         // used by "prefer features user hasn't accepted from" check
    trigger: fn(&Context) -> bool,
}

enum TipCategory { Capability, Explanation, Config }

struct Context<'a> {
    line: &'a str,                       // current command-line buffer
    cursor: usize,                       // cursor byte offset into `line`
    cwd: &'a Path,                       // resolved cwd from request
    tty: &'a str,                        // tty path (used as session id)
    db: &'a Database,                    // for queries (paths_index, transitions, etc.)
    response_candidates: &'a [Candidate], // candidates already chosen for this response
                                         // (used by triggers like `transitions` and `path_jump_cyan`)
}
```

`source_hint` is `None` for tips that do not correspond to a candidate source (e.g. `tips_off`, the meta-tip about disabling tips). When `None`, the "zero acceptances" priority filter is skipped for that tip.

The catalog is a `&[Tip]` constant in code. No user customization in v1.

### v1 catalog

| id | category | trigger | max_shows |
|---|---|---|---|
| `hybrid_cd` | Capability | command starts with `cd ` AND `paths_index` has matches outside cwd children | 3 |
| `git_branches` | Capability | inside git repo AND command matches `git (checkout|switch|merge|rebase) ` | 3 |
| `ssh_hosts` | Capability | command starts with `ssh ` AND `~/.ssh/config` is non-empty | 3 |
| `npm_scripts` | Capability | command matches `(npm|pnpm|yarn) run ` AND `package.json` exists in cwd | 3 |
| `kubectl_resources` | Capability | command matches `kubectl <verb> ` AND kubeconfig present | 3 |
| `docker_images` | Capability | command matches `docker (run|exec|rmi) ` | 3 |
| `make_targets` | Capability | command matches `(make|just) ` AND Makefile/Justfile exists | 3 |
| `transitions` | Explanation | a candidate in this response was sourced from `transitions` | 5 |
| `path_jump_cyan` | Explanation | response contains a `path_jump` candidate AND user has seen <3 such responses | 5 |
| `unknown_command` | Capability | user typed `<bin> ` AND 0 candidates AND `<bin>` exists in PATH AND `<bin>` not in `commands` table | 3 |
| `menu_detail_verbose` | Config | user has opened menu >50 times with `menu_detail=compact` | 2 |
| `tips_off` | Config | 5 different tips have been shown overall | 2 |

`max_shows` defaults: Capability=3, Explanation=5, Config=2.

### Selection algorithm

Implemented in `tips::select(catalog, context, state) -> Option<&Tip>`:

```
candidates = catalog.iter()
    .filter(|t| t.trigger(context))
    .filter(|t| !state.muted.contains(t.id))
    .filter(|t| state.shows_count.get(t.id).unwrap_or(0) < t.max_shows)
    .filter(|t| !state.shown_this_session.contains(t.id))
    .collect();

candidates.sort_by_key(|t| (
    t.category as u8,                                   // Capability=0, Explanation=1, Config=2
    match t.source_hint {                               // false (zero acceptances) sorts first
        Some(src) => !state.has_zero_acceptances(src),
        None => true,                                   // no source_hint → neutral priority
    },
    state.last_shown_at.get(t.id).unwrap_or(0),         // older first
));

candidates.first()
```

If `tips_per_session_max` (default 3) has already been hit this session, return `None`.
If `last_tab_at` is the same instant (e.g. very fast double-Tab), return `None`.

## Component: State persistence

### Schema

Add to existing migrations:

```sql
CREATE TABLE IF NOT EXISTS tips_state (
    tip_id          TEXT PRIMARY KEY,
    shows_count     INTEGER NOT NULL DEFAULT 0,
    last_shown_at   INTEGER,
    muted           INTEGER NOT NULL DEFAULT 0,
    muted_at        INTEGER,
    first_shown_at  INTEGER
);

-- Reuse existing meta table; add row meta_first_run_done=1 after greeter
```

In-memory per-session state (not persisted), keyed by tty path:

```rust
type SessionId = String;  // tty path, e.g. "/dev/ttys003"

struct TipSessionState {
    shown_this_session: HashSet<String>,    // tip ids
    last_tab_at: Option<Instant>,
}

struct TipsRuntime {
    sessions: HashMap<SessionId, TipSessionState>,  // GC entries idle >1h
}
```

Two zsh windows share the daemon but have distinct tty paths → distinct session state. A tip already shown in window A can still appear in window B. The shell already passes `TTY` env to `shac complete`; daemon reads it to key the session map. Sessions idle for >1h are evicted.

### Operations

- **Read on `/complete`:** one `SELECT * FROM tips_state` (small table, scan acceptable). Merge with in-memory session state. Run selection algorithm.
- **Write when tip emitted:** `INSERT INTO tips_state(tip_id, shows_count, last_shown_at, first_shown_at) VALUES(?, 1, ?, ?) ON CONFLICT(tip_id) DO UPDATE SET shows_count=shows_count+1, last_shown_at=excluded.last_shown_at`. Single statement, no transaction.
- **Mute:** `UPDATE tips_state SET muted=1, muted_at=? WHERE tip_id=?`.

### CLI surface

```
shac tips list [--all] [--muted]      # show all tips with state
shac tips mute <id>                    # set muted=1
shac tips unmute <id>                  # set muted=0, shows_count=0
shac tips reset                        # reset shows_count for all (preserves mutes)
shac tips reset --hard                 # reset everything including mutes
```

### Edge cases

- User deletes `~/.cache/shac/shac.db`: state lost, tips reappear. Acceptable — the system is designed to refresh memory.
- New machine without dotfile sync: state is local, intentionally.
- Daemon crash mid-session: `shown_this_session` resets. Acceptable risk of one duplicate within minutes.
- Multi-shell (multiple zsh windows): all share one daemon; daemon serializes via its IO loop. No write-race.

## Component: Localization (i18n)

### Principle

The catalog stores **keys**, never literal text. Translation tables map keys to localized strings, with English as the always-complete fallback.

### Localizable surface

- Tip texts (catalog)
- Group headers in `shac suggest` (`Available here:`, `Not used recently:`, `Full feature list:`)
- First-run greeter
- Static UI in `shac tips list` (column headers, status labels)

### Non-localizable

- Tip ids (`git_branches`, etc.)
- CLI flags, sub-commands, config keys
- Existing shac UI strings (menu header, kind labels)
- Error messages, `shacd` logs

### File format

TOML, located at the repo root and bundled via `include_str!("../../locales/<lang>.toml")` (or `rust-embed` if it grows):

```
shac/
  locales/
    en.toml    # always complete; source of truth
    ru.toml    # optional, may be partial
    de.toml    # future
  src/
    i18n.rs    # loader, resolution, lookup, interpolation
```

Adding a new bundled locale = add `<lang>.toml` to `locales/` and add one `include_str!` line in `src/i18n.rs`. No build-script magic; explicit and grep-able.

```toml
# locales/en.toml (excerpt)
[tips]
hybrid_cd        = "cd <Tab> works from anywhere — global frecent paths shown with → prefix"
git_branches     = "branches of this repo are pulled automatically"
ssh_hosts        = "hosts from ~/.ssh/config — partial match filters live"
unknown_command  = "shac doesn't know '{bin}'. Run `shac index add-command {bin}` to teach it"

[suggest]
header_available = "shac is available in this directory:"
header_unused    = "Not used recently:"
header_links     = "Full feature list:"

[greeter]
first_run = "shac is ready. Run `shac suggest` to see what's available here. `shac config` to tune."
```

### Resolution order

For each lookup, try in order:
1. `~/.config/shac/locales/<lang>.toml` (user override; extension point)
2. Bundled `locales/<lang>.toml` (if shipped)
3. Bundled `locales/en.toml` (always complete)

`<lang>` is determined once per process at startup:
1. `SHAC_LOCALE` env var (e.g. `ru`, `ru_RU.UTF-8`)
2. `ui.locale` config key (default `""` = auto-detect)
3. `LC_MESSAGES` env var
4. `LANG` env var
5. fallback `en`

Normalization: `ru_RU.UTF-8` → `ru` (take first 2 letters before `_` or `.`). If the resolved `<lang>.toml` does not exist or fails to parse, fall back to `en` for the entire process (logged once).

Per-key fallback: if `ru.toml` is missing key `tips.foo`, look it up in `en.toml`. A partial translation is fine — untranslated keys show the English text.

### Interpolation

Simple `{name}` substitution via `str::replace`. No ICU, no plural forms. Example:
```
"shac doesn't know '{bin}'. Run `shac index add-command {bin}` to teach it"
```
The trigger code passes `&[("bin", actual_bin)]` to the formatter.

### CLI surface

```
shac locale list                       # available locales (bundled + user-override)
shac locale current                    # resolved <lang> + source (env|config|auto-LC|auto-LANG|default)
shac locale set <lang>                 # write to ui.locale config
shac locale set --unset                # remove ui.locale, return to auto-detect
shac locale dump-keys [--missing <lang>]   # print all keys, or only those missing in <lang>
```

`dump-keys --missing` is the translator's tool: tells them what keys still need work.

### Lint guardrail

The `Tip` struct has `text_key: &'static str` and no `text` field, so the type system prevents inline literals at the catalog level — a developer cannot add a `text:` attribute that compiles. As a defence-in-depth check, CI runs a grep: any new key referenced in code that does not exist in `locales/en.toml` fails the build. This catches typos and keys added without a corresponding English string.

## Configuration

### New config keys (`~/.config/shac/config.toml`)

| key | default | meaning |
|---|---|---|
| `ui.show_tips` | `true` | global toggle for footer hints in menu |
| `ui.tips_per_session_max` | `3` | maximum distinct tips per shell session |
| `ui.tips_max_shows_default` | `3` | fallback for catalog entries without explicit `max_shows` |
| `ui.first_run_greeter` | `true` | first-run greeter (set false to skip on already-experienced installs) |
| `ui.locale` | `""` (auto) | force locale; empty = auto-detect via env |

Per-tip controls are NOT config keys (would explode the config surface). Use `shac tips mute/unmute` instead.

### Env var overrides

| var | effect |
|---|---|
| `SHAC_NO_TIPS=1` | disable footer hints (independent of `ui.show_tips`) |
| `SHAC_TIPS_DEBUG=1` | log selection decisions to `~/.cache/shac/shacd.log` |
| `SHAC_LOCALE=<lang>` | force locale (highest priority) |

## CLI surface summary

New commands:

```
shac suggest [--cwd PATH] [--all] [--json]

shac tips list [--all] [--muted]
shac tips mute <id>
shac tips unmute <id>
shac tips reset [--hard]

shac locale list
shac locale current
shac locale set <lang> | --unset
shac locale dump-keys [--missing <lang>]
```

No changes to existing commands except: `shac complete` may include a `__shac_tip\t<id>\t<text>` sentinel line in its response.

## zsh adapter changes

`shell/zsh/shac.zsh`:
- Parse `__shac_tip` sentinel in `_shac_fetch_candidates`, store into new globals `_shac_pending_tip_id` / `_shac_pending_tip_text`
- `_shac_render_menu` appends a blank line and the tip line if `_shac_pending_tip_text` is set
- Respect `SHAC_NO_TIPS=1` — skip parsing and rendering entirely
- No changes to key bindings; tip is non-interactive

Bash adapter is out of scope for v1 (separate spec).

## Testing strategy

### Unit tests (Rust)

- `tips::select` — synthetic catalog × contexts × state. Cover priority order, mute filter, exhaustion filter, session-dedup filter, tie-break.
- `tips::storage` — `INSERT`, `ON CONFLICT` upsert, `mute`, `unmute`, `reset`, `reset --hard`.
- `tips::session_state` — per-session dedup, per-Tab budget = 1.
- Each trigger function tested with positive and negative context (e.g. `git_branches` returns true inside a git repo, false outside).
- `i18n::resolve_locale` — all 5 resolution levels.
- `i18n::lookup` — present in `<lang>.toml`, missing key falls back to `en`, missing file falls back to `en` entirely.
- `i18n::interpolate` — `{bin}` replaced; missing placeholder leaves literal `{bin}` (logged once).

### Integration tests

New file `tests/tips.rs`:
- Fresh env (no `~/.cache/shac/shac.db`) → first `shac complete` includes greeter tip → second includes regular tip or none → third does not include greeter.
- `SHAC_NO_TIPS=1` → no `__shac_tip` line ever, regardless of context.
- `shac tips mute git_branches` then complete in git repo → no `git_branches` tip.
- `shac tips reset --hard` → all tips available again.
- `SHAC_LOCALE=ru` with bundled `ru.toml` containing `tips.git_branches` → tip text in Russian.
- `SHAC_LOCALE=jp` (no file) → falls back to English.
- User override at `~/.config/shac/locales/en.toml` with custom `tips.git_branches` → custom text wins.

zsh adapter rendering — the existing pattern in `tests/` is real-daemon integration tests, not mocks. So:
- Spawn daemon, set state to force a known tip (`shac tips reset --hard` then synthetic catalog with always-true trigger via test-only env `SHAC_TIPS_FORCE_ID=<id>`), invoke `shac complete`, assert response contains `__shac_tip\t<id>\t<expected_text>`.
- The `_shac_render_menu` zsh function itself is exercised by a small zsh harness script (`tests/zsh_render.sh`) that loads `shell/zsh/shac.zsh` in `SHAC_ZSH_TEST_MODE=1`, calls the function with a mock tip, and asserts the rendered `POSTDISPLAY` contains the tip text. Run from a Rust integration test via `Command::new("zsh").arg(harness_script)`.
- `SHAC_NO_TIPS=1` set on `shac complete`: response must omit `__shac_tip`. zsh harness with `SHAC_NO_TIPS=1` and a mock tip must NOT render the footer (defence-in-depth at both layers).

### Manual smoke (pre-release)

- Fresh `brew install shac` in clean VM → daemon starts → first Tab → greeter shown.
- Several Tabs in git repo → `git_branches` tip appears at least once.
- `shac suggest` in `~/dev/shac` → output lists applicable features.
- `shac tips mute git_branches; shac tips list` → marked muted.

## Rollout

- Version: v0.5.0 (minor bump — feature addition, no breaking changes)
- Default: `ui.show_tips=true` — feature is on for everyone, otherwise nobody discovers it
- CHANGELOG: section "New: contextual tips, `shac suggest`, and i18n scaffolding"
- README: new chapter "Discoverability"
- `docs/index.html`: section with screenshot of menu with footer

## Risks and mitigations

| risk | mitigation |
|---|---|
| Tips annoy power users | Auto-mute after `max_shows`; `SHAC_NO_TIPS=1` env; `ui.show_tips=false` config; `shac tips reset --hard` |
| Trigger fires on false positives | Catalog entries reviewed in code; Config-category tips have low `max_shows=2` and shut up fast |
| Menu latency grows | One small SELECT (PK lookup), in-memory session state; sub-millisecond |
| Local `tips_state` lost on cache wipe | By design — repeating tips after a year is acceptable, even useful |
| Hardcoded English string slips into a new tip | CI lint regex on `Tip { ... text: "..." }` rejects literals; only `text_key` allowed |
| Bundled locale files bloat binary | TOML stays small (~2 KB/lang); 10 langs = 20 KB, negligible |

## Open items (deferred to future iterations)

- Bash adapter parity for footer hints
- Plural forms / ICU formatting (only needed if a future tip text needs grammatical agreement)
- Localization of existing shac UI (menu header, kind labels)
- Localization of error messages and daemon logs
- Interactive `shac tour` TUI (categorized feature browser) — only if `shac suggest` proves insufficient in practice
