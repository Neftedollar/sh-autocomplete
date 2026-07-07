# Changelog

## v0.6.0 — 2026-07-06

### Changed
- **Daemon-owned shell quoting.** The daemon now escapes `insert_text` per target
  shell (`--format shell-tsv-v3`) and the zsh/bash/fish widgets insert it
  literally, instead of each deriving quoting independently. Paths/args with
  spaces, quotes, `$()`, backticks, globs, braces/brackets, leading `-`, control
  bytes and tildes now insert correctly across all three shells.
- **Heuristic-only ranking.** Removed the experimental ML reranker; suggestions
  are ranked purely by frecency, transitions, fuzzy match and priors over your
  real local history. One ranking path, no synthetic model.
- **Daemon robustness.** Request reads are capped (1 MiB) and time out (500 ms);
  `daemon stop` verifies the PID is really shacd before signalling; startup won't
  unlink a live daemon's socket; the model file is written atomically.
- Path completion is bounded so a huge directory no longer wedges the daemon.

### Added
- `config set telemetry_retention_days <n>` (default 30, `0` = prune each cycle)
  and completion-telemetry counts in `shac stats` — a clear, controllable answer
  to "what is kept about me, and for how long".
- Man-page indexation with an env-sanitized `--help` shellout; client/daemon
  version-mismatch detection; `shac daemon restart` + a `shac-update` helper;
  grouped `shac help`; Homebrew-tap auto-update on tagged release.

### Removed
- The `shac-ml-train` training crate and the `ml_rerank` / `ml_model_file` /
  `ml_blend_weight` config keys, along with the `train-model` /
  `export-training-data` subcommands. The mistralrs/burn dependency tree is gone
  (Cargo.lock: 878 → 110 packages).

### Fixed
- zsh history import no longer corrupts non-ASCII commands (metafication is
  decoded correctly); multiline history entries and quoted/spaced `cd` targets
  are parsed intact; a corrupt zoxide DB is skipped instead of aborting import.
- SQL `LIKE` wildcards (`_` `%`) in a typed token now match literally; assorted
  scoring/dedup/cache correctness fixes.

### Backward compatibility
- Config files carrying the removed `ml_*` keys still load (unknown keys are
  ignored).
- After upgrading, restart your shell so the new widgets/wire format take effect
  (the daemon speaks `shell-tsv-v3`; a live old widget requests the old format).

## v0.5.0 — 2026-04-28

### Added
- Contextual tips in the zsh completion menu — when context matches a feature
  (git repo + `git checkout`, `~/.ssh/config` + `ssh `, `package.json` + `npm run`,
  etc.), a hint footer appears below the candidate list.
- `shac suggest [--cwd <path>] [--all] [--json]` — list features applicable to
  the current directory, grouped by "available here" vs "not used recently".
- `shac tips list [--all|--muted]`, `shac tips mute <id>`, `shac tips unmute <id>`,
  `shac tips reset [--hard]` — manage per-tip show counts and mute state.
- `shac locale list/current/set/dump-keys` — i18n controls. Resolution priority:
  `SHAC_LOCALE` env > `ui.locale` config > `LC_MESSAGES` > `LANG` > en default.
- First-run greeter on the first completion menu after install — points to
  `shac suggest` and `shac config`. Atomic claim, never repeats.
- i18n scaffolding: bundled `locales/en.toml` (always complete) + extension point
  at `~/.config/shac/locales/<lang>.toml` for community translations. `LANG=C`
  and `LANG=POSIX` resolve to English.
- New config keys (all optional, defaults preserve existing behavior):
  `ui.show_tips` (default true), `ui.tips_per_session_max` (3),
  `ui.tips_max_shows_default` (3), `ui.first_run_greeter` (true), `ui.locale` (auto).
- New env vars: `SHAC_NO_TIPS=1` (suppress tips), `SHAC_LOCALE=<lang>` (force locale),
  `SHAC_TIPS_DEBUG=1` (reserved for future debug logging).

### Changed
- `shac complete --format shell-tsv-v2` may now emit one optional
  `__shac_tip\t<id>\t<text>` line after the items. Old shells and parsers ignore
  unknown sentinel lines — fully backward compatible.
- New SQLite table `tips_state` is created automatically on daemon start.

### Backward compatibility
- No breaking changes. Tips default ON; opt out via
  `shac config set ui.show_tips false` or `SHAC_NO_TIPS=1`.

## v0.4.0

See git log.
