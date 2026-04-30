# shac ML — maintainer rebuild guide

This directory holds the data and committed artifacts for shac's ML
next-command predictor. The pipeline is **maintainer-only**: end users
do not run any of these commands.

## Layout

- `data/personas.toml` — synthetic persona definitions (committed)
- `data/synthetic-{darwin,linux}.jsonl` — Qwen-generated sessions (NOT committed)
- `data/scrubbed-{darwin,linux}.jsonl` — same, after PII scrub (NOT committed)
- `data/distill-{darwin,linux}.jsonl` — with Qwen-teacher soft targets (NOT committed)
- `models/shac-ml-{darwin,linux}.bpk` — trained student weights (committed)
- `models/vocab.json` — 2000-token vocab (committed)
- `models/feature-spec.json` — model architecture + schema version (committed)

The `.jsonl` files in `data/` are intentionally not committed (gitignored).
Only the `models/` artifacts ship.

## Full rebuild (one OS at a time)

Approximate wall-clock on M1 Mac (CPU only — Metal is currently disabled
in the maintainer crate's mistralrs dependency for portability; re-enable
locally for a real run by editing `crates/shac-ml-train/Cargo.toml` to
add `features = ["metal"]` to the mistralrs dep):

| step | time (CPU) | time (Metal, if enabled) |
|---|---|---|
| `gen-synthetic` (one OS, 6 personas × 50 sessions × ~12 cmds) | ~3 hours | ~30 min |
| `scrub` | <1 min | <1 min |
| `distill` | ~3 hours | ~40 min |
| `train` (10 epochs) | ~15 min | ~5 min |
| `eval` | <1 min | <1 min |
| **total per OS** | **~6 hours** | **~75 min** |

```bash
# 1. Generate synthetic sessions
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-gen-synthetic -- \
  --os darwin \
  --out-dir ml/data

# 2. Scrub (and merge any local real history if maintainer wants)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-scrub -- \
  --input ml/data/synthetic-darwin.jsonl \
  --output ml/data/scrubbed-darwin.jsonl

# 3. Distill (teacher pass; builds vocab.json on first run)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-distill -- \
  --input ml/data/scrubbed-darwin.jsonl \
  --vocab ml/models/vocab.json \
  --output ml/data/distill-darwin.jsonl

# 4. Train student (writes .bpk + feature-spec.json)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-train -- \
  --input ml/data/distill-darwin.jsonl \
  --output ml/models/shac-ml-darwin.bpk

# 5. Evaluate
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-eval -- \
  --model ml/models/shac-ml-darwin.bpk \
  --input ml/data/distill-darwin.jsonl
```

Repeat steps 1–5 with `--os linux` and `linux` filenames.

## Acceptance gate before committing new `.bpk`

1. `eval` reports top-3 ≥ 0.45 on the distilled held-out (sanity check;
   the real bar is "+5pp over baseline" measured in runtime integration
   tests, not here)
2. `cargo test -p shac-ml-train --features full --test pipeline_smoke` passes
3. Roundtrip equivalence in `pipeline_smoke` holds (training-side and
   inference-side outputs match within ε=1e-6)
4. Manual inspection of `vocab.json`: confirm no PII tokens slipped past
   scrubbing:
   ```bash
   jq -r '.tokens[]' ml/models/vocab.json | grep -iE 'roman|@gmail|192\.|10\.0|/users/' \
     || echo "OK no PII"
   ```

## Updating the architecture

If you change `StudentModelConfig` defaults, you MUST:
1. Bump `feature_spec.version` in `crates/shac-ml-train/src/bin/train.rs`
2. Regenerate both `.bpk` files
3. Update the runtime crate's `expect_feature_spec_version` constant (separate plan)

## Privacy

- Real shell history is **never** committed. The maintainer may add a
  *local-only* scrubbed copy of `~/.zsh_history` to
  `data/real-{os}.jsonl` for personal training experiments, but `data/`
  is gitignored except for `personas.toml`.
- Even the maintainer's local zsh history MUST go through `scrub`
  before joining any persisted dataset. The scrub red-list test
  (`tests/scrub_redlist.rs`) is the safety net.
