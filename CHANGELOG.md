# Changelog

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
