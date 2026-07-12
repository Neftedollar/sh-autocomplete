# Changelog

## v0.6.13 — 2026-07-12

### Fixed
- **Upgrading shac now updates an already-open shell.** Every widget wrapped
  all of its function definitions in a one-time load guard
  (`_SHAC_ZSH_LOADED` / `_SHAC_BASH_LOADED` / `_SHAC_FISH_LOADED`), so the
  `source …` line that `shac install` prints — the documented way to pick up a
  new version — was a silent **no-op** in a shell that had already loaded shac.
  A `brew upgrade` therefore left the old widget running against the new daemon
  (visible as a stray `0`/`1` in the menu's description column: the new
  `full_line` protocol field parsed by an old adapter). The adapters are now
  reload-safe: function definitions run on every `source`, while only the
  one-time zle wiring in zsh (which saves the original widgets) stays guarded;
  bash and fish re-run their idempotent wiring directly.

  Note: this fix helps from 0.6.13 onward. Upgrading *to* 0.6.13 from an older
  version still has the old guard loaded, so open a new shell (or
  `unset _SHAC_ZSH_LOADED && source ~/.config/shac/shell/shac.zsh`) once to pick
  it up.

## v0.6.12 — 2026-07-11

The rest of the holistic Fable review, batched: security, resource safety, a
privacy-respecting history model, and the widget/daemon seam unified behind one
protocol flag.

### Security & resource safety
- **State is now owner-only (F5).** The config/data/state dirs are chmod'd
  `0700`, the history DB `0600`, and the **unauthenticated control socket**
  `0600` right after bind — previously any local user could `complete`, read
  `stats`, or poison learning over it. `SHAC_LOCALE` is sanitized before it
  reaches a path join (no traversal to an arbitrary `.toml`), and the per-locale
  catalog cache is bounded so a client can't grow daemon memory by spamming
  distinct locales.
- **Daemon shellouts can no longer deadlock or truncate (F10).** Every
  `git`/`kubectl`/`docker`/`--help`/`man` call went "poll for exit, then read
  stdout" — which deadlocks whenever the child writes more than one pipe buffer
  (~64 KiB) before exiting, as `man` and large `--help` routinely do: the child
  blocks on `write()`, the timeout fires, and the output is lost. All six sites
  now drain stdout on a reader thread concurrently with the timed wait. As a
  side effect `--help` is now reliably the primary doc source (it had been
  masked by `man` silently deadlocking), matching the documented intent.

### Privacy & history
- **Command history retention is configurable (F4).** The daemon prunes
  `history_events` older than `history_retention_days` (default 365) on its
  background tick, so the DB stays bounded over months of use. `0` keeps no
  persistent history.
- **Two commands are never recorded:** anything typed with a **leading space**
  (the `HISTCONTROL=ignorespace` / `HIST_IGNORE_SPACE` convention — so a one-off
  ` export TOKEN=…` or ` cd /private` leaves no trace in history, transitions, or
  the cd index), and everything while shac is disabled. Inline `cd`-frecency
  learning is gated on the same clean-interactive signal as transitions, so
  pasted/imported `cd`s no longer seed path-jump suggestions.

### Fixed — completion correctness
- **The widget and daemon agree on whole-line completions (F3/F7/F8).** Whether a
  candidate is a full command line (resurrected from history, replaces the whole
  buffer, runs on Enter) is now decided once by the daemon and shipped as a
  `full_line` flag, instead of the CLI keying off `kind` and each widget keying
  off `source` — two approximations that drifted and could escape a line
  per-token while treating it as runnable. zsh keys Enter/insert off the flag;
  bash and fish need no change.
- **Redirect targets complete as files, not commands (F3).** `cat > fi<Tab>` now
  offers filenames — the token after `>`/`<`/`>>` is classified as a path.
- **bash's active-token span is quote-aware (F6).** `cat "my fi<Tab>` completes
  the whole `"my fi` span instead of just the trailing `fi` and mangling the
  quote, matching the zsh widget.
- **`shac reindex` no longer reports a false failure on a large PATH.** The
  client capped the wait at 1.5 s, so a reindex that legitimately took longer
  (big PATH, slow CI) surfaced a "read daemon response" error while the daemon
  was still finishing. The ceiling for this explicit, non-latency-sensitive
  command is now 30 s.

### Fixed — follow-ups from prior-PR review comments
- **A no-match Tab no longer mis-attributes the next command.** v0.6.11's F2
  fix emitted `0` (instead of an empty field) for a zero-candidate response;
  clients then treated that non-empty sentinel as a live request id, so running
  the line within 30 s recorded `--accepted-request-id 0` and the server
  fell back to guessing a recent request. All three adapters now treat `0` as
  "no request".
- **A history flag no longer shadows a `-file` path candidate.** In a path
  context, a dash-prefixed history token (e.g. `vim -notes`) was classified as
  an option and could win over the real path candidate, dropping the leading-
  dash `./` guard; it now stays a path (`./-notes`).
- **bash Tab is no longer a dead key when shac has no candidate.** The bash≥4
  `bind -x` path returned without deferring to readline (which a `bind -x`
  handler can't invoke), so a down/disabled daemon left Tab inert. It now
  emulates default filesystem completion for simple tokens (unique match or
  longest common prefix), leaving quoted/escaped/`~` tokens untouched; at the
  command position it completes command names, not filenames.

### Fixed — from an adversarial (Fable) review of this batch
- **Configuring history retention no longer wipes your imported history.**
  Plain (non-`EXTENDED_HISTORY`) zsh history imports as `ts = 0` with the real
  clock in `imported_at`; the new prune keyed only on `ts`, so the first
  background tick deleted the entire imported corpus (and re-armed on
  re-import). Retention now keys off whichever of `ts`/`imported_at` is more
  recent.
- **The whole-line Enter feature actually works in zsh now.** The candidate
  TSV split elided the empty description field, shifting the `full_line` flag
  into the description slot — so Enter on a resurrected history line inserted
  instead of running it, and a stray `1`/`0` showed as the menu description.
- **A daemon shellout can no longer wedge completions.** A grandchild that
  inherited a `git`/`man`/`--help` process's stdout pipe kept the drain from
  seeing EOF, blocking the single-threaded daemon far past the timeout. Each
  shellout now runs in its own process group (killed as a unit on timeout) and
  the collect is bounded, so a stuck descendant can't stall other shells.
- **Resurrecting a history line after `&&`/`|`/`;` no longer breaks it.** The
  escape decision was tied to the Enter-runs-it flag, but a whole history line
  offered at a chained command position isn't a whole-buffer replacement, so it
  was per-token escaped into `git\ commit\ …`. Escaping now keys off a separate
  "already valid shell" signal.
- **`config set enabled false` now stops a running daemon from recording.** The
  daemon cached `enabled` at startup and the client never re-checked it, so
  recording continued after a disable while completions went quiet. The client
  now honors the kill-switch and the daemon reloads `enabled` per record.
- **Redirect targets complete files again.** `cat > <Tab>` / `cat > -n<Tab>`
  returned nothing (the path dispatch was gated on a command the redirect
  segment doesn't have); they now offer filesystem candidates.
- **bash multibyte cursor fix.** `mv Café tmp` with the cursor after `Café`
  selected the wrong token (a byte offset fed a character-indexed walker); the
  offset is now converted first.
- Locale cache keys are sanitized (a hostile `SHAC_LOCALE` can no longer bloat
  the per-locale cache); the control socket's actual mode is checked at startup
  and warned about if not owner-only. Note: a `--help` that prints then hangs
  past its deadline now indexes nothing from help (previously a truncated
  parse), with `man` as the fallback.

## v0.6.11 — 2026-07-11

Wire-protocol hardening from the holistic Fable review (the seam three focused
reviews each assumed the other owned).

### Fixed
- **A daemon-start status line no longer erases your typed token (F1).** When the
  daemon was down (first shell after boot, after a crash, or `shac daemon stop`),
  `complete` auto-started it and printed `started`/`running` to **stdout — the
  same stream the widgets parse as completion rows** — so `cd sr<Tab>` opened a
  menu whose blank first row was applied, deleting the `sr`. The auto-start path
  is now silent (`ensure_daemon` never prints); interactive `shac daemon start`
  still reports status. All three adapters additionally ignore any tab-less line
  as a non-candidate (defense in depth).
- **Empty `request_id` field no longer shifts wire parsing (F2).** A
  zero-candidate response emitted an empty field; zsh's `${(ps:\t:)}` elides
  empty elements and bash's `IFS=$'\t' read -a` collapses consecutive tabs, so
  both misread the following field — in bash that produced a non-numeric
  `--accepted-request-id` and **silently dropped the recorded command**. The id
  now defaults to `0` (a non-empty, non-matching sentinel).

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
