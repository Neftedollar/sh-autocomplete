# shac

`shac` is a local autocomplete engine for `bash` and `zsh` built around a lightweight Rust daemon.

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

For local formula testing from this checkout:

```bash
brew install --build-from-source ./Formula/shac.rb
```

## Beta quickstart

Recommended beta path is `zsh`; `bash` is supported as best-effort list completion.

```bash
cargo build --bins
./target/debug/shac install --shell zsh --edit-rc
./target/debug/shac daemon start
./target/debug/shac reindex
./target/debug/shac doctor
```

Open a new shell or run:

```zsh
source ~/.config/shac/shell/shac.zsh
pyt<Tab>
```

Useful smoke checks:

```bash
./target/debug/shac debug completion --shell zsh --line "pyt" --cursor 3
./target/debug/shac explain --shell zsh --line "git ch" --cursor 6 --cwd "$PWD"
./target/debug/shac recent-events --limit 20
```

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
