# Daemon-Boundary Escaping & Insertion Architecture

**Date:** 2026-07-06
**Status:** Design — awaiting review
**Scope:** Foundational layer. Makes the `shacd` daemon the single owner of shell quoting/escaping so the zsh/bash/fish widgets stop each deriving it (wrongly and differently). Closes ~12 of the 46 audit findings directly and provides the vocabulary (`quote_token`, the insert/display contract, wire format v3) that the remaining fixes build on.

---

## 1. Problem

Today the daemon emits a raw `insert_text` and each widget independently decides how to place it on the command line:

- **zsh** menu uses `compadd -Q` (which *disables* zsh's own quoting) and — separately — an inline path that string-splices a suffix into `BUFFER` with no quoter at all.
- **bash** uses a distinct `shell-metadata` format and pushes values into `COMPREPLY` with no `-o filenames` and no `printf %q`.
- **fish** inserts literally via `commandline -t --`, guarded only against embedded spaces.

The result is a whole family of confirmed defects: paths/args with spaces, quotes, `$`, backticks, globs, leading `-`, or control bytes splice in broken; the bash format inserts the decorated `display` string instead of `insert_text`; ANSI/control bytes in filenames reach the terminal raw; and the TSV wire format silently rewrites tab/newline in any field to a space (changing the value that gets inserted).

The daemon already receives the target shell (`CompletionRequest.shell`, `protocol.rs:40`), so it is the one place that *can* quote correctly and uniformly.

## 2. Core contract

> **The field a widget INSERTS is always the daemon-escaped `insert_text`. The `display` field is only ever shown, never inserted.**

Everything below follows from this single rule. A widget is a "dumb executor": it renders `display` for the eye and, on accept, replaces the active token with `insert_text` verbatim (its own shell quoting turned off). No widget re-quotes, re-escapes, or derives insertion text from `display`.

## 3. New Rust surface

### 3.1 `Shell` enum
Replace the free-form `shell: String` handling with an enum parsed once at the daemon boundary:

```
enum Shell { Zsh, Bash, Fish, Posix }
```

Unknown/absent `--shell` → `Posix` (conservative single-quote quoting). Bash and zsh share most rules but differ in a few escape forms (see §4), so they stay distinct variants.

### 3.2 `src/quote.rs` — the single source of truth
```
/// Produce a shell literal that, when the target shell evaluates it as the
/// replacement for the active word, expands back to exactly `token`.
pub fn quote_token(shell: Shell, ctx: &TokenContext, token: &str) -> String
```

`TokenContext` carries the two facts the quoter needs from `context::parse`: the **tilde/`$HOME` prefix state** (so `~`/`~/` stays bare) and the **opening-quote state** of the already-typed active token (§4.3). The daemon calls `quote_token` while building each candidate's `insert_text`; the TSV field goes out already-final.

## 4. Escaping rules (the hard cases)

### 4.1 Tilde / `$HOME`
`~`, `~/`, and a leading `~user/` must remain **outside** quoting or the shell won't expand them. `quote_token` escapes only the tail after the tilde-prefix boundary:
`~/My Docs` → `~/My\ Docs` (never `'~/My Docs'`, never `~/'My Docs'` unless the tail itself needs quote-style — see §4.3). The tilde boundary comes from `context::parse`, which is why §6 is a hard dependency.

### 4.2 Leading dash
A candidate whose inserted form would begin with `-` (a file `-rf`, a dir `--flag/`) must not be parseable as an option. Rule: if the escaped literal starts with `-`, prefix `./` → `./--flag/`, `./-rf`. This fixes `engine.rs:1414` at the boundary rather than per-widget.

### 4.3 Already-open quote in the active token
The user may have typed a quote before pressing Tab: `cd "My D<Tab>`. Rule:
- If the active token starts with `"` or `'`, escape the completion in **that same quote style** (backslash-for-`"`, `'\''`-splice-for-`'`) and leave the quote open/closed consistently with what was typed.
- Otherwise use backslash style.

This makes the replacement coherent with the on-line prefix instead of nesting a second quoting style inside the first.

### 4.4 Control bytes
Tab, newline, CR, ESC, and other C0 bytes in a filename escape to ANSI-C form: `$'\t'`, `$'\n'`, `$'\x1b'` in zsh/bash; `\t`, `\n`, `\x1b` (fish accepts backslash escapes in `commandline` when the value is a literal token) in fish. Side effect: ANSI-escape sequences embedded in filenames can no longer reach the terminal raw.

### 4.5 Empty token
`quote_token(_, "")` → `""` (empty string, not `''`), so an empty active token produces no artifact.

### 4.6 Non-UTF8 paths
Filenames are already handled via `to_string_lossy` upstream. Exact round-trip of invalid-UTF8 bytes is **out of scope** (unchanged best-effort behavior); the quoter operates on the lossy string.

## 5. Wire format v3 (`shell-tsv-v3`)

Keep the line-oriented TSV shape (widgets already `read`/`split` on `\t`), but replace the lossy `sanitize_shell_field` (tab/CR/LF → space) with **reversible field encoding** applied to every field:

| raw | encoded |
|-----|---------|
| `\` | `\\` |
| tab | `\t` |
| LF  | `\n` |
| CR  | `\r` |

Widgets decode each field after splitting. Framing can no longer be broken by field content and nothing is silently rewritten. Note that a daemon-escaped `insert_text` (§4.4) rarely contains raw control bytes anyway; the encoding is the belt-and-braces guarantee and is what makes `display`/`description` safe to carry verbatim.

**Retire `shell-metadata`.** All three widgets use `shell-tsv-v3`. Removing bash's separate format is the root fix for `shac.rs:1440` (it emitted `display`).

**Version is a hard cut.** Widgets ship in this repo and `shac install` rewrites the shell rc, so the daemon need not support v2 going forward. Operational consequence to document for users: **after upgrading the daemon, restart your shell** (a live session still running the old widget will request the old format). The daemon responds to an unknown `--format` with a clean error, not a malformed body.

### 5.1 `display` contract
`display` may contain daemon-authored decoration (e.g. the path_jump arrow). Any **user-derived substring** embedded in `display` (a filename, a history line) is stripped of control/ESC bytes before composition. Rendering decoration safely in the widget — notably replacing raw ANSI in the zsh `POSTDISPLAY` with `region_highlight` (`shac.zsh:443`) — is a widget change that lands in the later fixes phase but **depends on this contract**, so the contract is fixed here.

## 6. Hard dependency: correct token extraction

`quote_token` can only escape "the active word" and locate the tilde boundary (§4.1) and open-quote state (§4.3) if `context::parse` (`context.rs:23`) reports them correctly. Three confirmed `context.rs` defects therefore move **into this foundational layer** (not the "fixes on top" phase):

- **`context.rs:26`** — cursor is treated as a byte offset while zsh/fish send a character offset; multibyte text before the cursor truncates the active token.
- **`context.rs:41`** — command detection scans the whole line and treats `|`, `&&`, `;`, `>` as ordinary tokens, so completion after any operator is computed for the wrong command (and thus the wrong token is escaped).
- **`context.rs:33`** — a trailing escaped/quoted space is read as a token boundary, yielding an empty active token instead of the partial word.

`ParsedContext` gains the fields `quote_token` needs (tilde-prefix boundary, active-token opening-quote state) so the quoter is a pure function of context + token.

## 7. Insertion model per widget

All widgets: render `display`, and on accept replace the active token with the decoded, daemon-escaped `insert_text`.

- **zsh menu** — keep `compadd -Q` (now correct: the value is already a shell literal, `-Q` inserts it verbatim), but feed `insert_text`, not `_shac_menu_displays` (fixes `shac.zsh:578`).
- **zsh inline** — ghost text shows the **readable** (unescaped) remainder for legibility; on accept, **replace the whole active token** with the escaped `insert_text` rather than appending a suffix. Whole-token replacement is required because an escaped literal cannot be substring-sliced at the typed/untyped boundary.
- **fish** — `commandline -t --` with the decoded escaped literal; drop the space-only guard (escaping now makes multi-word values safe).
- **bash** — move off `COMPREPLY` to a `bind -x` function that edits `READLINE_LINE`/`READLINE_POINT` directly, inserting the literal — the bash analogue of zsh `zle` / fish `commandline`. `COMPREPLY` is unusable for this contract for two reasons: readline re-quotes its values (double-escaping a pre-escaped literal), and `compopt -o filenames` — the only readline quoting hook — also escapes a leading `~`, defeating §4.1. Consequence: candidate **display** for bash also moves under widget control (bash has no rich menu today — it relies on readline listing `COMPREPLY` — so `bind -x` inserts the top candidate and cycles on repeat, a deliberate, modest UX change, not parity with zsh's menu). **Decision:** `bind -x` is the mechanism. Explicit plan-time contingency if it proves too invasive: fall back to `COMPREPLY` with daemon-side **tilde-expanded absolute** `insert_text` for bash only (readline then quotes correctly because there is no `~` to preserve), accepting that bash users see expanded paths instead of `~/...`.

## 8. Testing

**Real-shell round-trip (golden).** For each payload in a matrix — space, single quote, double quote, `$VAR`, backtick, `*`/`?` glob, `#`, newline, tab, ESC, leading `-`, `~/` prefix, unicode/emoji, empty — and each shell in {zsh, bash, fish}: run `<shell> -c` that evaluates the escaped literal as an argument and echoes it, and assert the echo equals the original token. This tests the actual property (the real shell expands our string back to the path), not a table of what we *think* is right. Gated to run only when the shell binary is present.

**Unit table.** `quote_token` over the same matrix × shells, asserting exact output, runnable without any shell installed (fast CI lane).

**Context.** Unit tests for the three `context.rs` fixes (char-offset cursor, operator-aware command detection, escaped-trailing-space token) feeding representative lines.

**Wire v3.** Encode/decode round-trip in Rust; and a widget-side decode test per shell (a field containing `\t`/`\n`/`\\` decodes to the original bytes).

## 9. Rollout

1. This boundary lands first: `Shell` enum, `src/quote.rs`, the `context.rs` fixes, wire v3 + widget decode, the insert/display contract, and the per-widget insertion model.
2. Directly closed by landing it: `shac.rs:1440`, `shac.zsh:578`, `engine.rs:1414` (dash), the zsh/bash/fish unquoted-insert instances, TSV tab/newline loss, and the `context.rs` trio.
3. The remaining audit findings (perf blocker, daemon robustness, import corruption, scoring semantics, ANSI-in-POSTDISPLAY rendering, etc.) then land in batches on top of this vocabulary, each with adversarial verification.

## 10. Out of scope

- Exact round-trip of non-UTF8 filenames (best-effort/lossy, unchanged).
- Backward compatibility with `shell-tsv-v2` / `shell-metadata` (hard cut; shell restart after upgrade).
- The non-escaping audit findings enumerated in step 3 above (separate batches).
