# shac ML — maintainer rebuild guide

This directory holds the data and committed artifacts for shac's ML
next-command predictor. The pipeline is **maintainer-only**: end users
do not run any of these commands.

CI reflects that split: it builds/clippies/tests the rest of the workspace
plus `shac-ml-train`'s lightweight `--no-default-features --features
"model-only scrub"` lane (model/tokenizer/scrub/data/personas, no teacher
stack); the `full` feature below (mistralrs + burn-autodiff) is compiled and
tested locally by the maintainer only, per the rebuild steps below.

## Layout

- `data/personas.toml` — synthetic persona definitions (committed)
- `data/synthetic-{darwin,linux}.jsonl` — Qwen-generated sessions (NOT committed)
- `data/scrubbed-{darwin,linux}.jsonl` — same, after PII scrub (NOT committed)
- `data/distill-{darwin,linux}.jsonl` — train split, with Qwen-teacher soft
  targets (NOT committed)
- `data/distill-{darwin,linux}-val.jsonl` — held-out val split, produced by
  `shac-ml-distill --val-output` (chronological, per-persona — see step 3).
  This is the **only** file the acceptance-gate `eval` in step 5 may read
  (NOT committed)
- `models/shac-ml-{darwin,linux}.bpk` — trained student weights (committed)
- `models/vocab-{darwin,linux}.json` — per-OS vocab, up to `--max-vocab`
  tokens (default 2000) (committed). One file **per OS** — darwin and linux
  have different command vocabularies, so they must not share a vocab file;
  see "Per-OS vocab files" below
- `models/feature-spec.json` — model architecture + schema version, written
  by `shac-ml-train` from the actual `--vocab`/`--cwd-buckets` it was run
  with, not a hardcoded literal (committed)

The `.jsonl` files in `data/` are intentionally not committed (gitignored).
Only the `models/` artifacts ship.

## Per-OS vocab files

`shac-ml-distill --vocab` builds a fresh vocab on first run (when the path
doesn't exist yet) and reuses it on subsequent runs (when it does). Because
darwin and linux command vocabularies differ, run each OS against its own
vocab path (`ml/models/vocab-darwin.json` / `ml/models/vocab-linux.json`) —
never point both OSes' distill runs at the same vocab file.

If you reuse an existing `--vocab` file whose corpus doesn't fit it well
(e.g. you accidentally pointed linux's `--input` at darwin's vocab),
`shac-ml-distill` aborts by default once the hard-label `<UNK>` fraction
exceeds 5%. Pass `--allow-vocab-reuse` only if you've deliberately decided to
share a vocab and accept the fallout on the OS it wasn't built from.

`shac-ml-train` and `shac-ml-eval` both take the same `--vocab` file used to
build `--input`; they derive `vocab_size` from it and refuse to run (fail
fast) if any token id in the dataset doesn't fit that vocab.

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

# 3. Distill (teacher pass; builds vocab-darwin.json on first run; splits
#    off a chronological, per-persona held-out val set)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-distill -- \
  --input ml/data/scrubbed-darwin.jsonl \
  --vocab ml/models/vocab-darwin.json \
  --output ml/data/distill-darwin.jsonl \
  --val-output ml/data/distill-darwin-val.jsonl

# 4. Train student on the TRAIN split only (writes .bpk + feature-spec.json;
#    vocab_size/cwd_buckets in feature-spec.json come from --vocab and
#    --cwd-buckets below, not a hardcoded default — keep them matched to the
#    --distill run above)
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-train -- \
  --input ml/data/distill-darwin.jsonl \
  --vocab ml/models/vocab-darwin.json \
  --output ml/models/shac-ml-darwin.bpk

# 5. Evaluate on the VAL split only — never the file used to train in step 4,
#    or the reported accuracy is training-set accuracy, not a real gate.
cargo run --release -p shac-ml-train --features full \
  --bin shac-ml-eval -- \
  --model ml/models/shac-ml-darwin.bpk \
  --input ml/data/distill-darwin-val.jsonl \
  --vocab ml/models/vocab-darwin.json
```

Repeat steps 1–5 with `--os linux` and `linux` filenames (including
`ml/models/vocab-linux.json` — do not reuse the darwin vocab file).

## Acceptance gate before committing new `.bpk`

1. `eval` (step 5 above) reports top-3 ≥ 0.45 on the **held-out val split**
   (`ml/data/distill-<os>-val.jsonl`) — never on the file `shac-ml-train
   --input` was trained on, or the number is training-set accuracy, not a
   generalization measurement (sanity check; the real bar is "+5pp over
   baseline" measured in runtime integration tests, not here)
2. `cargo test -p shac-ml-train --features full --test pipeline_smoke` passes
3. Roundtrip equivalence in `pipeline_smoke` holds (training-side and
   inference-side outputs match within ε=1e-6)
4. Manual inspection of each OS's vocab: confirm no PII tokens slipped past
   scrubbing:
   ```bash
   for f in ml/models/vocab-darwin.json ml/models/vocab-linux.json; do
     jq -r '.tokens[]' "$f" | grep -iE 'roman|@gmail|192\.|10\.0|/users/' \
       && echo "PII FOUND in $f" || echo "OK no PII in $f"
   done
   ```

## Updating the architecture

If you change `StudentModelConfig` defaults (`n_layers`, `n_heads`,
`hidden_dim`, `intermediate_dim`, `context_len`, `dropout`), you MUST:
1. Bump `feature_spec.version` in `crates/shac-ml-train/src/bin/train.rs`
2. Regenerate both `.bpk` files
3. Update the runtime crate's `expect_feature_spec_version` constant (separate plan)

`vocab_size` and `cwd_buckets` are the exception: they are never a hardcoded
default — `shac-ml-train` always derives them from the actual `--vocab` file
and `--cwd-buckets` flag, so changing `shac-ml-distill --max-vocab` or
`--cwd-buckets` doesn't require touching `StudentModelConfig` at all.

## Privacy

- Real shell history is **never** committed. The maintainer may add a
  *local-only* scrubbed copy of `~/.zsh_history` to
  `data/real-{os}.jsonl` for personal training experiments, but `data/`
  is gitignored except for `personas.toml`.
- Even the maintainer's local zsh history MUST go through `scrub`
  before joining any persisted dataset. The scrub red-list test
  (`tests/scrub_redlist.rs`) is the safety net.
