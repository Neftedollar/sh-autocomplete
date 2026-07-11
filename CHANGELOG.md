# Changelog

## v0.6.10 — 2026-07-11

Fixes from a three-lens Fable review (engine, release pipeline, usability).

### Fixed — correctness
- **Accepting a full command line from history no longer runs broken escaped
  text.** A row like `cd ..` was inserted token-escaped (`cd\ ..`) and executed
  as a single word (`command not found: cd ..`), which then re-poisoned the
  history. Whole command lines (multi-word `history`/`command` candidates) now
  insert raw; single tokens (paths, options) are still escaped.
- **`cd <Tab>` no longer suggests deleted directories via a kind hole.** The
  v0.6.8 existence filter only ran for `kind == "path"`, but single-segment
  history tokens arrive as `subcommand`; every cd-argument candidate is now
  existence-checked. The check is also hardened: `$VAR`/`~user`/glob tokens and
  absolute paths outside `$HOME` (possible dead network mounts) are kept rather
  than stat'd, and it uses `is_dir`.
- **`--help`/man option parsing** — a metavar between flags no longer drops the
  long form (`-o FILE, --output FILE` → both), and `--color[=WHEN]` / `-i[SUFFIX]`
  no longer produce corrupt `--color[` tokens. The man-page doc cap actually
  bounds output again (a mis-scoped `break` had disabled it).
- **`doc_search` is scoped to the current command** — `git c<Tab>` no longer
  leaks mdfind's `-case_sensitive` etc.; doc `insert_text` is stripped of control
  bytes (roff overstrike) so accepting never injects them into the buffer.
- **Release pipeline can't ship an empty checksum** — an asset-sha fetch failure
  now fails the job (plain assignment vs `echo "$(...)"`), and the tap renderer
  refuses any sha256 that isn't 64 hex chars.
- **`shac doctor` version probe** is read-only (`SHAC_NO_TIPS`, so it no longer
  burns the one-shot first-run greeter) and now flags a pre-0.5.2 daemon that
  answers without a version field instead of waving it through.

### Fixed — usability (ranking & noise)
- **Transitions ("what you run after X") only appear at command position** — no
  more `git c<Tab>` → `clr`, or a once-pasted sentence offered as an argument.
- **Subcommand indexing** now reads aliased rows (cargo's `build, b` → `build`)
  and underscore names (`s_client`).
- **Git branch completion** collapses each `origin/foo` twin when the local
  `foo` exists and drops the bare `origin` remote name.
- **Path completion is case-insensitive** — `cd d<Tab>` surfaces
  `Documents/`/`Desktop/` on a case-insensitive macOS filesystem.
- Redundant per-row descriptions ("Previously executed command", "Provided by
  current shell context", "Frequently used after X") are dropped as clutter.

## v0.6.9 — 2026-07-08

### Fixed
- **Duplicate rows in completion menus.** A history line stored with trailing
  whitespace (e.g. `clr `) produced a candidate whose `insert_text` differed
  from the clean `clr` only by an invisible space, so dedup-by-insert_text let
  both through and the menu showed two identical-looking rows (one "Provided by
  current shell context", one "Previously executed command"). History,
  runtime-history and transition candidates are now trimmed before dedup.

## v0.6.8 — 2026-07-08

### Fixed
- **Command completion no longer times out on a short prefix.** `c<Tab>` (and
  any 1-2 char prefix) fuzzy-matched almost all ~1700 indexed commands; ranking
  that many candidates blew the 80 ms daemon budget, so the request timed out
  and the widget showed nothing. Command candidates are now cheap-ranked
  (prefix beats fuzzy, shorter beats longer) and capped before the expensive
  scoring, and fuzzy matching is skipped for 0-1 char prefixes (where it only
  ever produced noise).
- **`cd` no longer suggests a deleted directory.** A `cd dev/sh-autocomplete/`
  resurrected from history/transitions was offered even after the folder was
  removed, so accepting it failed with "no such file or directory". cd-path
  candidates from history and transitions are now dropped when the target
  directory no longer exists (a cheap stat, cd-context only).

## v0.6.7 — 2026-07-08

### Fixed
- **A flag typed as an argument no longer completes as `./--flag`.** A history
  entry like `bash --help` was offered for `bash <Tab>` as a `subcommand`
  candidate, and since a real subcommand never starts with `-`, the leading-dash
  path guard rewrote it to `bash ./--help` (which fails). Flag-shaped history/
  transition arguments are now classified as options, so they insert bare.
- **`shac index`/`shac daemon` subcommands were undocumented.** `shac index
  --help` listed `add-command` / `add-path` / `status` with blank descriptions;
  those (and the `daemon` start/stop/restart/status verbs) now carry help text.

## v0.6.6 — 2026-07-08

### Fixed
- **Options completed as a whole flags column.** A `--help`/man row like
  `-h, --help  Print help` was indexed with the entire `-h, --help` string as
  the insert value, so accepting it typed `cmd -h,\ --help`. Each flag is now a
  separate candidate (`-h`, `--help`), with any metavar (`<MSG>`, `=<WHEN>`)
  stripped, so short- and long-form prefixes both complete correctly.
- **Subcommands were never indexed.** The help parser only read option rows, so
  `shac <Tab>` (and any CLI's subcommands) fell through to history/path noise.
  It now also extracts subcommand rows — indented single-token names under a
  group header — which clap emits under custom sections (`Setup:`, `Index:`,
  …) rather than a single `Commands:` block.

### Note
- The background indexer never re-shells `--help` for already-indexed commands,
  so existing help/man docs keep the old parsing until refreshed with
  `shac index add-command <cmd>`.

## v0.6.5 — 2026-07-08

### Fixed
- **Phantom blank completion after upgrade.** v0.6.4 made `shac complete` emit a
  new `__shac_client_version` protocol line, but `brew upgrade` refreshes the
  binary without re-running `shac install`, so the still-installed older zsh
  adapter did not recognize the line and mis-parsed it as a completion candidate
  — a blank menu entry whose insertion was the version string (`cd 0.6.4`).
  - Reverted the `__shac_client_version` emission and the widget's live parse of
    it; stale-daemon detection already lives in the `shac doctor daemon_version`
    check (v0.6.4), which needs no protocol change.
  - The zsh adapter now **ignores any unrecognized `__shac_*` control line**
    instead of rendering it as a candidate, so a future binary can add protocol
    lines without breaking an older adapter.
- **Stale-adapter detection.** `shac doctor` gains a `zsh_adapter_current` check
  that compares the installed adapter against the one embedded in this binary
  and tells you to run `shac install --shell zsh` when a `brew upgrade` left it
  outdated (the adapter counterpart to the `daemon_version` check).

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
