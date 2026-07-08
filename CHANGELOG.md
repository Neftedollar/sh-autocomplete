# Changelog

## v0.6.4 — 2026-07-08

### Fixed
- **Stale-daemon detection.** A `brew upgrade` swaps the binary on disk but
  leaves the old long-running `shacd` in memory, so completions keep being
  served by outdated code with no visible symptom until behavior drifts (a
  0.6.3 client talking to a 0.5.3 daemon produced confusing multi-Tab behavior).
  Two safety nets now catch this:
  - `shac doctor` gains a `daemon_version` check that live-probes the running
    daemon and flags a version mismatch with the exact fix (`shac daemon
    restart`).
  - The completion response now carries the client version on every request, so
    the zsh widget's version-mismatch tip fires even when `shac shell-env` was
    not re-sourced after the upgrade (previously the client version was only
    learned at shell startup, so a bare `brew upgrade` in an existing shell left
    it blank and the warning silent).
- **Tilde no longer escaped in learned path transitions.** A learned `cd ~/proj/`
  transition (`source: transition`, `kind: subcommand`) was quoted as
  `cd \~/proj/`, which cd's into a literal `~` directory instead of expanding
  home. Candidates from the user's own history/transitions now keep a genuine
  bare `~`/`$HOME` prefix, while raw filesystem entries stay guarded against the
  `~root` masquerade (F3/F4).

## v0.6.3 — 2026-07-08

### Fixed
- **The `shac-macos-universal` asset is now a genuine universal binary.** It was
  built with a plain `cargo build` on an Apple Silicon runner, so it was
  arm64-only despite the name — fine on Apple Silicon, but the v0.6.2 binary
  formula would hand that arm64 binary to Intel Macs, where it cannot run.
  Release CI now builds both `aarch64-apple-darwin` and `x86_64-apple-darwin`
  and `lipo`-merges them into one fat binary, so `brew install` works on both
  Apple Silicon and Intel Macs.

## v0.6.2 — 2026-07-08

### Fixed
- **Homebrew installs no longer build from source.** The v0.6.1 bottle pipeline
  never actually delivered a usable bottle (a glob missed `.bottle.1.tar.gz`
  files and the bottle was built from the tap before the tap url was bumped, so
  it carried the wrong version), so `brew install/upgrade shac` still fell back
  to `cargo install` and pulled the entire Rust + LLVM toolchain (~600 MB) just
  to compile a binary that is already published in the release.
- The tap formula is now a **binary-install formula**: it downloads the prebuilt
  release tarball (`shac-macos-universal` — a universal arm64+Intel binary — or
  `shac-linux-x86_64`) and runs `bin.install`, with **no `depends_on "rust"`**
  and no build step. A normal `brew install neftedollar/shac/shac` pulls zero
  compiler dependencies. `brew install --HEAD shac` still builds from source.

### Changed
- Release CI renders the tap formula from the repo's canonical
  `Formula/shac.rb` at tag time (`.github/scripts/render_tap_formula.py`),
  injecting each platform's release url + sha256 with block-scoped substitution.
- Removed the bottle-publish workflow (`bottle.yml`) and `merge_bottle.py`; they
  are obsolete now that installs use the release tarball directly.

## v0.6.1 — 2026-07-06

### Fixed
- Homebrew installs now use a pre-built bottle instead of compiling from source.
  The bottle-publish CI blocked on a single slow runner (one platform's queue
  stalled the tap update for everyone), so `brew install/upgrade shac` fell back
  to `cargo install`, which drags in the whole Rust + LLVM toolchain (~600 MB:
  llvm, rust, z3, python, libgit2, openssl) that shac never needs at runtime.
  Bottles are now built and published per-platform independently, the arm64
  bottle is built on the oldest supported macOS so it covers sonoma/sequoia/
  tahoe, and the tap formula merges each platform's sha additively.

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
