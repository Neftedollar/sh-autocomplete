# shac

![shac hero: shell completions in a terminal session](docs/assets/hero/shac-hero.gif)

`shac` is a local autocomplete engine for `bash` and `zsh` built around a lightweight Rust daemon.

Landing page: https://neftedollar.github.io/sh-autocomplete/

## Components

- `shacd`: daemon that serves completion requests over a Unix socket
- `shac`: CLI for daemon control, indexing, explain output, stats, config, and shell helpers
- `shell/`: completion adapters for `bash` and `zsh`

## Current MVP

- PATH command indexing
- local SQLite storage
- history and transition tracking
- completion impression/acceptance logging
- trust/provenance-aware event ingestion with soft legacy migration
- owned `zsh` Tab widget with mini-menu UI and exact accepted-item tracking
- confidence-aware `zsh` paste detection with exact and conservative heuristic paths
- project-aware heuristic reranking
- optional local ML reranking from a JSON model file
- on-demand path completion with cached hot directories
- branch-aware completion for `git checkout|switch|branch|merge|rebase` via `git for-each-ref` (200 ms timeout, no cache yet)
- bundled command priors (~60 grammar pairs for git, docker, kubectl, npm, ...) seeded on first install for cold-start
- live `npm run` / `pnpm run` / `yarn run` script completion parsed from the cwd's nearest `package.json` (walk-up bounded to 8 levels, stops at `.git` boundary)
- builtin docs for `git`, `docker`, `kubectl`, `npm`, `cargo`, `dotnet`, `python`, `pip`, `pytest`
- explain output for feature contributions

## Build

```bash
cargo build
```

## Homebrew

The formula always points to a stable tag.

```bash
brew install Neftedollar/shac/shac
```

[Leave beta feedback →](https://github.com/Neftedollar/sh-autocomplete/discussions)

For local formula testing from this checkout:

```bash
brew install --build-from-source ./Formula/shac.rb
```

## Beta quickstart

Recommended beta path is `zsh`; `bash` is supported as best-effort list completion.

```bash
brew install Neftedollar/shac/shac
shac install --shell zsh --edit-rc
brew services start shac
shac reindex
shac doctor
```

`shac install --shell zsh --edit-rc` runs a quick first-run pass: it
splices the shac block into your `~/.zshrc`, prompts (Y/n) before
importing your `~/.zsh_history`, imports your `zoxide` jump list if
present, and scans the standard project roots (`~/dev`,
`~/Documents/dev`, `~/code`, `~/src`, `~/projects`) for git repos.
Pass `--yes` to skip the history-import prompt, `--no-import` to skip
the import phase entirely. Output looks like:

```
✓ Hooking shac into zsh                          ~/.zshrc  [0.0s]
✓ Importing zsh history                          12,847 entries, 3 dup, 1 redacted  [1.8s]
✓ Importing zoxide                               156 destinations  [0.1s]
✓ Scanning project roots for git repos           23 found  [0.6s]

Try: cd <Tab>
  Run `shac doctor` if Tab feels off.
  Run `shac stats` to see what was learned.
```

Open a new shell or run:

```zsh
source ~/.config/shac/shell/shac.zsh
git commit -<Tab>
```

Useful smoke checks:

```bash
shac debug completion --shell zsh --line "git commit -" --cursor 12
shac debug completion --shell zsh --line "docker run --" --cursor 13
shac explain --shell zsh --line "kubectl get pods -n" --cursor 19 --cwd "$PWD"
shac recent-events --limit 20
```

For development builds, replace `shac` with `./target/debug/shac` after `cargo build --bins`.

## zsh menu UI

The owned `zsh` menu can be tuned from config:

```bash
shac config set ui.zsh.menu_detail compact      # minimal | compact | verbose | debug
shac config set ui.zsh.show_kind off
shac config set ui.zsh.show_source off
shac config set ui.zsh.show_description on
shac config set ui.zsh.max_description_width 72
shac config set ui.zsh.max_items 8
```

Defaults are optimized for daily use: compact descriptions are shown, internal `kind/source` metadata is hidden. Use `ui.zsh.menu_detail debug` when diagnosing ranking or candidate source issues.

Hybrid `cd` candidates surfaced from `paths_index` (`kind=path_jump`) render with a cyan-tinted leading `→` arrow so they're visually distinguishable from local cwd children. Set `SHAC_NO_COLOR=1` to disable the tint.

## Install and uninstall

```bash
cargo run --bin shac -- install --shell zsh
cargo run --bin shac -- install --shell zsh --edit-rc
cargo run --bin shac -- install --shell bash
cargo run --bin shac -- uninstall --shell zsh --edit-rc
```

Without `--edit-rc`, `install` only writes the adapter and prints the `source ...` line.
With `--edit-rc`, `shac` adds a managed block to `~/.zshrc` or `~/.bashrc`.
`uninstall --edit-rc` removes only that managed block.

Emergency disable:

```bash
export SHAC_DISABLE=1
```

Persistent disable:

```bash
shac config set enabled off
```

Re-enable:

```bash
shac config set enabled on
unset SHAC_DISABLE
```

## Reindex and inspect

```bash
cargo run --bin shac -- daemon start
cargo run --bin shac -- reindex
cargo run --bin shac -- doctor
cargo run --bin shac -- stats
cargo run --bin shac -- migration-status
cargo run --bin shac -- recent-events --limit 10
cargo run --bin shac -- debug completion --shell zsh --line "pyt" --cursor 3
cargo run --bin shac -- explain --shell zsh --line "git ch" --cursor 6 --cwd "$PWD"
cargo run --bin shac -- export-training-data --limit 1000
cargo run --bin shac -- train-model --output "$HOME/.config/shac/model.json"
cargo run --bin shac -- config set ml_model_file "$HOME/.config/shac/model.json"
cargo run --bin shac -- config set features.ml_rerank on
```

## Explicit indexing

`shac` does not scan the whole disk. You can explicitly opt into more index sources:

```bash
shac index add-command fzf
shac index add-path ../some/path --subpath --deep 10
shac index add-path ../some/path --subpath --full --deep 10
shac index status
```

The index stores compact metadata only:

- command name, kind, path, mtime;
- short option/subcommand docs;
- source references and index target metadata.

It does not store full `man` pages or full `--help` output.

## Diagnostics

```bash
shac doctor
shac doctor --json
shac debug completion --shell zsh --line "python3 -" --cursor 9
shac stats
shac recent-events --limit 20
```

`shac doctor` surfaces three cold-start telemetry checks alongside the usual config / daemon / shell-adapter checks:

- `cold_start_paths`: row count in `paths_index` (zsh-history replay + zoxide + project scan combined). Zero means hybrid `cd` will fall back to local cwd children.
- `cold_start_history`: imported zsh history events plus `import_coverage_pct` (imported / total history rows).
- `time_to_first_accept`: seconds between `shac install` and the first accepted completion. Recorded once and never overwritten.

If completion breaks, first try:

```bash
export SHAC_DISABLE=1
```

Then open a new shell. Native shell completion should remain usable.

## Data and privacy

All data stays local. There is no network path in the daemon or shell adapters.

Default paths:

- config: `~/.config/shac/config.toml`
- SQLite and caches: `~/.local/share/shac/`
- daemon socket/pid/state: `~/.local/state/shac/`

## Trust-aware migration

When this version starts on an existing database, old personalization data is kept and marked as `legacy`.

- `legacy` rows still help the heuristic ranker with a strong penalty
- `legacy` rows do not participate in ML training
- new `interactive` + `typed_manual` / `accepted_completion` events gradually replace old behavior
- `pasted` history in `zsh` is tracked with exact or heuristic confidence and only weakly influences heuristic ranking

Useful commands:

```bash
cargo run --bin shac -- migration-status
cargo run --bin shac -- stats
```

If you explicitly want to start over, there is also:

```bash
cargo run --bin shac -- reset-personalization
```