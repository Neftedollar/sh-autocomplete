# Daemon-Boundary Escaping & Insertion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `shac` process the single owner of shell quoting/escaping so the zsh/bash/fish widgets stop each deriving insertion text (wrongly, differently), closing the ~12 escaping-class audit findings at their root.

**Architecture:** The `shac complete` CLI already knows the target shell (`args.shell`) and formats the daemon's shell-agnostic JSON into a shell-specific wire format. We add a pure library quoter `shac::quote::quote_token(Shell, &TokenContext, token)` and call it in the formatter so `insert_text` leaves the process as a ready shell literal. The widgets become dumb executors: render `display`, and on accept insert the decoded `insert_text` verbatim with their own quoting disabled. Correct token extraction (`context::parse`) is a hard dependency and is fixed in the same layer. The wire format gains reversible field encoding (v3) so no field content can break framing or be silently rewritten.

**Tech Stack:** Rust (root `shac` crate: lib + `shac`/`shacd` bins), zsh/bash/fish widget scripts, SQLite unchanged. Tests: `cargo test -p shac`; shell round-trip tests shell out to real `zsh`/`bash`/`fish` when present.

## Global Constraints

- Escaping lives in ONE place: `shac::quote::quote_token`. No widget re-quotes or derives insertion text from `display`. (spec §2)
- The field a widget INSERTS is always the escaped `insert_text`; `display` is only shown. (spec §2)
- `~`, `~/`, `~user/`, `$HOME`, `$HOME/` stay OUTSIDE quoting so the shell expands them; only the tail is escaped. (spec §4.1)
- Unknown/absent `--shell` → `Shell::Posix` (single-quote quoting). (spec §3.1)
- Wire format v3 field encoding: `\`→`\\`, tab→`\t`, LF→`\n`, CR→`\r`, applied to every field, decoded by every widget. (spec §5)
- `shell-metadata` is removed; all three widgets use `shell-tsv-v3`. (spec §5)
- Non-UTF8 filenames are best-effort/lossy (unchanged). No v2 backward compat (hard cut; shell restart after upgrade). (spec §4.6, §10)
- TDD: write the failing test first, watch it fail, implement minimally, watch it pass, commit. Do NOT commit unrelated working-tree changes — stage only files named in the task.

---

## File Structure

- `src/shell.rs` (currently a 3-line stub) — home of the shared `Shell` enum + `Shell::parse`. Consumed by the lib quoter and both bins. Replaces the ad-hoc `ShellKind` matching in `src/bin/shac.rs`.
- `src/quote.rs` (new) — `TokenContext`, `quote_token`, per-shell escapers. Pure, no I/O. The single source of truth.
- `src/wire.rs` (new) — `encode_field` / `decode_field` for wire v3, plus the `Shell`-free record helpers. Pure.
- `src/context.rs` (modify) — cursor char-offset fix, operator-aware command detection, escaped-trailing-space token; add `open_quote` + `bare_prefix` info to `ParsedContext`.
- `src/lib.rs` (modify) — `pub mod quote; pub mod wire;` (and keep `pub mod shell;`).
- `src/bin/shac.rs` (modify) — `print_completion_response` takes `Shell`; calls `quote_token`; emits `shell-tsv-v3`; `shell-metadata`/`shell-tsv-v2` branches removed.
- `shell/zsh/shac.zsh`, `shell/bash/shac.bash`, `shell/fish/shac.fish` (modify) — decode v3, insert `insert_text` literally, per spec §7.
- `tests/quote_roundtrip.rs` (new) — real-shell round-trip golden matrix.

---

## Task 1: `Shell` enum

**Files:**
- Modify: `src/shell.rs`
- Modify: `src/lib.rs` (ensure `pub mod shell;`)
- Test: `src/shell.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `enum Shell { Zsh, Bash, Fish, Posix }` (derives `Clone, Copy, Debug, PartialEq, Eq`); `Shell::parse(s: Option<&str>) -> Shell`.

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_known_and_unknown() {
        assert_eq!(Shell::parse(Some("zsh")), Shell::Zsh);
        assert_eq!(Shell::parse(Some("bash")), Shell::Bash);
        assert_eq!(Shell::parse(Some("fish")), Shell::Fish);
        assert_eq!(Shell::parse(Some("nu")), Shell::Posix);
        assert_eq!(Shell::parse(None), Shell::Posix);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac shell::tests::parse_known_and_unknown`
Expected: FAIL (compile error: `Shell` not found).

- [ ] **Step 3: Write minimal implementation**
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
    Posix,
}

impl Shell {
    pub fn parse(s: Option<&str>) -> Shell {
        match s.map(str::to_ascii_lowercase).as_deref() {
            Some("zsh") => Shell::Zsh,
            Some("bash") => Shell::Bash,
            Some("fish") => Shell::Fish,
            _ => Shell::Posix,
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac shell::tests::parse_known_and_unknown`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/shell.rs src/lib.rs
git commit -m "feat(shell): shared Shell enum with conservative Posix fallback"
```

---

## Task 2: `quote_token` — base word quoting

Handles the common case: a token with no tilde prefix and no open quote. Backslash-escapes shell-significant characters for zsh/bash/posix; fish uses its own significant set.

**Files:**
- Create: `src/quote.rs`
- Modify: `src/lib.rs` (`pub mod quote;`)
- Test: `src/quote.rs` inline tests

**Interfaces:**
- Consumes: `shac::shell::Shell`.
- Produces:
  - `pub struct TokenContext { pub open_quote: Option<char> }` (`Default` = `{ open_quote: None }`).
  - `pub fn quote_token(shell: Shell, ctx: &TokenContext, token: &str) -> String`.

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;

    fn q(sh: Shell, t: &str) -> String {
        quote_token(sh, &TokenContext::default(), t)
    }

    #[test]
    fn base_backslash_escaping() {
        assert_eq!(q(Shell::Zsh, "My Docs"), "My\\ Docs");
        assert_eq!(q(Shell::Bash, "a'b"), "a\\'b");
        assert_eq!(q(Shell::Zsh, "$(x)"), "\\$\\(x\\)");
        assert_eq!(q(Shell::Zsh, "a*b?"), "a\\*b\\?");
        assert_eq!(q(Shell::Zsh, "plain"), "plain");
        assert_eq!(q(Shell::Fish, "My Docs"), "My\\ Docs");
        assert_eq!(q(Shell::Fish, "a*b"), "a\\*b");
        assert_eq!(q(Shell::Posix, ""), "");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac quote::tests::base_backslash_escaping`
Expected: FAIL (compile error: module not found).

- [ ] **Step 3: Write minimal implementation**
```rust
//! Single source of truth for shell quoting. `quote_token` produces a literal
//! that, when the target shell evaluates it as the replacement for the active
//! word, expands back to exactly the input token. See
//! docs/superpowers/specs/2026-07-06-daemon-boundary-escaping-design.md.

use crate::shell::Shell;

#[derive(Clone, Debug, Default)]
pub struct TokenContext {
    /// If the already-typed active token opened a quote, which char it was.
    pub open_quote: Option<char>,
}

/// Characters that must be backslash-escaped in an unquoted word.
fn needs_escape(shell: Shell, c: char) -> bool {
    // C0 control bytes are handled separately (ANSI-C quoting) in a later task.
    let common = matches!(
        c,
        ' ' | '\t' | '"' | '\'' | '\\' | '$' | '`' | '&' | '|' | ';'
            | '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | ']'
            | '#' | '~' | '!' | '=' | '\n'
    );
    match shell {
        Shell::Fish => matches!(c, ' ' | '\t' | '"' | '\'' | '\\' | '$' | '*' | '?' | '(' | ')' | '~' | '#' | '\n' | '&' | '|' | ';' | '<' | '>'),
        _ => common,
    }
}

fn backslash_escape(shell: Shell, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if needs_escape(shell, c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn quote_token(shell: Shell, _ctx: &TokenContext, token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    backslash_escape(shell, token)
}
```
> Note: `~` and `!` are escaped here in the base case; Task 4 re-exempts a leading tilde *prefix* so expansion still works. `=` is escaped to defuse zsh `foo=bar` global-alias/assignment parsing.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac quote::tests::base_backslash_escaping`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/quote.rs src/lib.rs
git commit -m "feat(quote): base per-shell word escaping"
```

---

## Task 3: `quote_token` — control bytes via ANSI-C quoting

**Files:**
- Modify: `src/quote.rs`
- Test: `src/quote.rs` inline tests

**Interfaces:**
- Produces: no signature change; `quote_token` now emits `$'...'` (zsh/bash/posix) or `\xHH` (fish) for C0 bytes.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn control_bytes_ansi_c() {
    // A filename with an embedded tab and ESC.
    assert_eq!(q(Shell::Bash, "a\tb"), "a$'\\t'b");
    assert_eq!(q(Shell::Zsh, "x\u{1b}y"), "x$'\\x1b'y");
    assert_eq!(q(Shell::Fish, "a\tb"), "a\\tb");
    assert_eq!(q(Shell::Fish, "x\u{1b}y"), "x\\x1by");
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac quote::tests::control_bytes_ansi_c`
Expected: FAIL (current code backslash-escapes the literal control char instead of encoding it).

- [ ] **Step 3: Write minimal implementation**
Replace `backslash_escape` with a scanner that emits ANSI-C spans for C0 bytes and backslash-escapes the rest:
```rust
fn is_control(c: char) -> bool {
    (c as u32) < 0x20 || c == '\u{7f}'
}

fn ansi_c_byte(shell: Shell, c: char) -> String {
    match (shell, c) {
        (Shell::Fish, '\t') => "\\t".into(),
        (Shell::Fish, '\n') => "\\n".into(),
        (Shell::Fish, '\r') => "\\r".into(),
        (Shell::Fish, _) => format!("\\x{:02x}", c as u32),
        (_, '\t') => "$'\\t'".into(),
        (_, '\n') => "$'\\n'".into(),
        (_, '\r') => "$'\\r'".into(),
        (_, _) => format!("$'\\x{:02x}'", c as u32),
    }
}

fn escape_word(shell: Shell, s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if is_control(c) {
            out.push_str(&ansi_c_byte(shell, c));
        } else {
            if needs_escape(shell, c) {
                out.push('\\');
            }
            out.push(c);
        }
    }
    out
}
```
Update `quote_token` to call `escape_word`, and drop `'\n'`/`'\t'` from `needs_escape` (now handled as control). Keep `' '` in `needs_escape`.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac quote::tests::control_bytes_ansi_c && cargo test -p shac quote::tests::base_backslash_escaping`
Expected: PASS (both).

- [ ] **Step 5: Commit**
```bash
git add src/quote.rs
git commit -m "feat(quote): ANSI-C encode control bytes (neutralizes ANSI injection)"
```

---

## Task 4: `quote_token` — bare tilde/`$HOME` prefix

**Files:**
- Modify: `src/quote.rs`
- Test: `src/quote.rs` inline tests

**Interfaces:** no signature change.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn tilde_prefix_stays_bare() {
    assert_eq!(q(Shell::Zsh, "~/My Docs"), "~/My\\ Docs");
    assert_eq!(q(Shell::Zsh, "~"), "~");
    assert_eq!(q(Shell::Bash, "~/a b/c"), "~/a\\ b/c");
    assert_eq!(q(Shell::Zsh, "$HOME/a b"), "$HOME/a\\ b");
    // A literal file that merely starts with ~ but is NOT a home ref:
    // engine only ever emits ~ as a home prefix, so treat leading ~/ or bare ~.
    assert_eq!(q(Shell::Fish, "~/My Docs"), "~/My\\ Docs");
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac quote::tests::tilde_prefix_stays_bare`
Expected: FAIL (`~` currently escaped to `\~`; `$` escaped).

- [ ] **Step 3: Write minimal implementation**
```rust
/// Return the byte length of a leading home-expansion prefix that must stay
/// bare: `~`, `~/`, `~user/`, `$HOME`, `$HOME/`. 0 if none.
fn bare_prefix_len(token: &str) -> usize {
    if let Some(rest) = token.strip_prefix("$HOME") {
        return token.len() - rest.len() + rest.strip_prefix('/').map(|_| 1).unwrap_or(0);
    }
    if token == "~" {
        return 1;
    }
    if let Some(rest) = token.strip_prefix('~') {
        // ~/... or ~user/... : bare through the first '/'.
        if let Some(idx) = rest.find('/') {
            return 1 + idx + 1;
        }
        // bare ~name with no slash: only ~ itself is the expansion trigger.
        return 1;
    }
    0
}
```
In `quote_token`, split the token at `bare_prefix_len(token)`: emit the prefix verbatim, `escape_word` only the tail.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac quote::tests::tilde_prefix_stays_bare && cargo test -p shac quote::tests`
Expected: PASS (all quote tests).

- [ ] **Step 5: Commit**
```bash
git add src/quote.rs
git commit -m "feat(quote): keep tilde/\$HOME expansion prefix outside escaping"
```

---

## Task 5: `quote_token` — leading-dash `./` guard

**Files:**
- Modify: `src/quote.rs`
- Test: `src/quote.rs` inline tests

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn leading_dash_gets_dot_slash() {
    assert_eq!(q(Shell::Zsh, "--flag/"), "./--flag/");
    assert_eq!(q(Shell::Bash, "-rf"), "./-rf");
    // Not a path-relative candidate if it already has ./ or a bare-prefix:
    assert_eq!(q(Shell::Zsh, "~/-x"), "~/-x");        // tail after ~/ is fine
    assert_eq!(q(Shell::Zsh, "a-b"), "a-b");          // dash not leading
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac quote::tests::leading_dash_gets_dot_slash`
Expected: FAIL (no `./` prepended).

- [ ] **Step 3: Write minimal implementation**
In `quote_token`, after computing the escaped result but only when `bare_prefix_len(token) == 0` and `ctx.open_quote.is_none()`: if the escaped string starts with `-`, prepend `./`.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac quote::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/quote.rs
git commit -m "feat(quote): ./ guard so dash-named paths are not parsed as options"
```

---

## Task 6: `quote_token` — open-quote active token

**Files:**
- Modify: `src/quote.rs`
- Test: `src/quote.rs` inline tests

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn open_quote_uses_same_style() {
    let dq = TokenContext { open_quote: Some('"') };
    let sq = TokenContext { open_quote: Some('\'') };
    // User typed  cd "My D<Tab>  -> completion "My Docs" closes in double quotes.
    assert_eq!(quote_token(Shell::Zsh, &dq, "My Docs"), "My Docs\"");
    // A double-quote inside double quotes is backslash-escaped.
    assert_eq!(quote_token(Shell::Zsh, &dq, "a\"b"), "a\\\"b\"");
    // Single-quote style: close, splice, reopen for any embedded '.
    assert_eq!(quote_token(Shell::Zsh, &sq, "a'b"), "a'\\''b'");
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac quote::tests::open_quote_uses_same_style`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**
Add branches at the top of `quote_token` for `ctx.open_quote`:
```rust
match ctx.open_quote {
    Some('"') => {
        // inside "...": escape " \ $ ` and close the quote.
        let mut out = String::new();
        for c in token.chars() {
            if matches!(c, '"' | '\\' | '$' | '`') { out.push('\\'); }
            out.push(c);
        }
        out.push('"');
        return out;
    }
    Some('\'') => {
        // inside '...': only ' is special -> '\'' splice; close at end.
        let mut out = String::new();
        for c in token.chars() {
            if c == '\'' { out.push_str("'\\''"); } else { out.push(c); }
        }
        out.push('\'');
        return out;
    }
    _ => {}
}
```
(Leave the existing unquoted path for `open_quote == None`.)

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac quote::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/quote.rs
git commit -m "feat(quote): honor an already-open quote in the active token"
```

---

## Task 7: wire v3 field encode/decode

**Files:**
- Create: `src/wire.rs`
- Modify: `src/lib.rs` (`pub mod wire;`)
- Test: `src/wire.rs` inline tests

**Interfaces:**
- Produces: `pub fn encode_field(s: &str) -> String`; `pub fn decode_field(s: &str) -> String` (inverse).

- [ ] **Step 1: Write the failing test**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_control_and_backslash() {
        for raw in ["plain", "a\tb", "a\nb", "a\\b", "a\\tb", "\r", "→ ~/x"] {
            assert_eq!(decode_field(&encode_field(raw)), raw);
        }
        assert_eq!(encode_field("a\tb"), "a\\tb");
        assert_eq!(encode_field("a\\b"), "a\\\\b");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac wire::tests::roundtrip_control_and_backslash`
Expected: FAIL (module not found).

- [ ] **Step 3: Write minimal implementation**
```rust
//! Reversible TSV field encoding for shell-tsv-v3. Order matters: backslash
//! first on encode, so decode is unambiguous.
pub fn encode_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

pub fn decode_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(other) => { out.push('\\'); out.push(other); }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac wire::tests::roundtrip_control_and_backslash`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/wire.rs src/lib.rs
git commit -m "feat(wire): reversible field encoding for shell-tsv-v3"
```

---

## Task 8: `context::parse` — char-offset cursor

**Files:**
- Modify: `src/context.rs`
- Test: `src/context.rs` inline tests

**Interfaces:**
- Consumes/Produces: `parse(line, cursor, cwd)` now interprets `cursor` as a CHARACTER index (matching zsh `$CURSOR` / fish), not a byte offset.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn cursor_is_character_offset() {
    // "привет " has 7 chars; cursor 7 = end. Byte length is 13.
    let cwd = std::path::Path::new("/tmp");
    let ctx = parse("привет ls", 8, cwd); // 8 chars = after "привет l"
    assert_eq!(ctx.active_token, "l");
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac context::tests::cursor_is_character_offset`
Expected: FAIL (byte slicing truncates the multibyte prefix).

- [ ] **Step 3: Write minimal implementation**
Replace the byte-based `max`/`safe_cursor`/`line[..safe_cursor]` (context.rs:26-31) with a char-based prefix:
```rust
let before: String = line.chars().take(cursor).collect();
```
Remove the now-dead `safe_cursor` codepoint-boundary scan.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac context::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/context.rs
git commit -m "fix(context): treat cursor as character offset, not byte offset"
```

---

## Task 9: `context::parse` — operator-aware command detection

**Files:**
- Modify: `src/context.rs`
- Test: `src/context.rs` inline tests

**Interfaces:**
- Produces: `ParsedContext.command` reflects the command of the pipeline/list segment the cursor is in; `active_index` is relative to that segment.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn command_after_operator() {
    let cwd = std::path::Path::new("/tmp");
    let ctx = parse("git log | grep fo", 17, cwd);
    assert_eq!(ctx.command.as_deref(), Some("grep"));
    assert_eq!(ctx.active_token, "fo");
    let ctx2 = parse("a && b -x", 9, cwd);
    assert_eq!(ctx2.command.as_deref(), Some("b"));
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac context::tests::command_after_operator`
Expected: FAIL (command resolves to "git").

- [ ] **Step 3: Write minimal implementation**
Before tokenizing for the command, split `before` on the last unquoted segment separator in `{ "|", "||", "&&", ";", "&" }` and also `>`/`<` redirection; take the trailing segment as the command context. Reuse the existing quote-aware scanner (context.rs:147) to avoid splitting inside quotes. Compute `command`, `active_token`, `active_index` from that trailing segment.

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac context::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/context.rs
git commit -m "fix(context): resolve command per pipeline/list segment"
```

---

## Task 10: `context::parse` — escaped/quoted trailing space + expose quote/tilde state

**Files:**
- Modify: `src/context.rs`
- Test: `src/context.rs` inline tests

**Interfaces:**
- Produces: `ParsedContext` gains `pub open_quote: Option<char>` (the unterminated quote of the active token, if any). The escaped/quoted trailing space no longer ends the token.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn escaped_trailing_space_keeps_token() {
    let cwd = std::path::Path::new("/tmp");
    let ctx = parse("cd My\\ ", 7, cwd);
    assert_eq!(ctx.active_token, "My ");
    assert_eq!(ctx.open_quote, None);
    let q = parse("cd \"My ", 7, cwd);
    assert_eq!(q.active_token, "My ");
    assert_eq!(q.open_quote, Some('"'));
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac context::tests::escaped_trailing_space_keeps_token`
Expected: FAIL (`open_quote` field missing; token split at the space).

- [ ] **Step 3: Write minimal implementation**
Extend the quote/escape state machine (context.rs:147) so a space that is backslash-escaped or inside an open quote does NOT start a new token; carry the final open-quote char out as `open_quote`; add the field to `ParsedContext` (default `None` on all other construction sites).

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac context::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/context.rs
git commit -m "fix(context): keep escaped/quoted trailing space in the active token; expose open_quote"
```

---

## Task 11: formatter emits `shell-tsv-v3` with escaped insert_text

**Files:**
- Modify: `src/bin/shac.rs` (`complete`, `print_completion_response`, `disabled_completion_response`, remove `sanitize_shell_field`, `shell-metadata`, `shell-tsv-v2` branches)
- Modify: `src/bin/shac.rs` (thread `Shell` + `open_quote` into the formatter)
- Test: none new here (behavior covered by Task 16 integration); this task is a refactor gated by existing tests + compile.

**Interfaces:**
- Consumes: `shac::shell::Shell`, `shac::quote::{quote_token, TokenContext}`, `shac::wire::encode_field`.
- Produces: `print_completion_response(response, shell: Shell, open_quote: Option<char>, format: &str)` emitting `shell-tsv-v3`.

- [ ] **Step 1: Change the call site**
In `complete` (shac.rs:1413): compute `let shell = Shell::parse(args.shell_str.as_deref());` and pass the daemon-returned `open_quote` (add it to the JSON response in the engine, or recompute via `context::parse` in the CLI from `args.line`/`args.cursor`) into `print_completion_response`.

- [ ] **Step 2: Rewrite the v3 branch**
Replace `sanitize_shell_field(field)` everywhere in the formatter with `encode_field(field)`, and set the per-item insert field to `encode_field(&quote_token(shell, &TokenContext { open_quote }, raw_insert_text))`. Keep the header row `__shac_request_id\t<id>\t<mode>\t<n>`; keep the tip row but `encode_field` its fields. Delete the `shell-metadata` and `shell-tsv-v2` branches and the `sanitize_shell_field` fn.

- [ ] **Step 3: Verify it compiles and existing tests pass**
Run: `cargo test -p shac`
Expected: PASS (no reference to removed symbols).

- [ ] **Step 4: Manual smoke**
Run: `printf 'ok\n'` — then `cargo build -p shac` and, in a scratch dir with a file named `My Docs`, `target/debug/shac complete --shell zsh --line 'cat My' --cursor 6 --cwd "$PWD" --format shell-tsv-v3` and confirm the insert field is `My\ Docs` (encoded).

- [ ] **Step 5: Commit**
```bash
git add src/bin/shac.rs
git commit -m "feat(shac): emit shell-tsv-v3 with daemon-side quoted insert_text"
```

---

## Task 12: display contract — strip control/ESC from user-derived display

**Files:**
- Modify: `src/engine.rs` (where `display` is composed for path/history candidates, e.g. engine.rs:831 arrow, and the raw `insert_text`-as-display cases)
- Test: `src/engine.rs` inline test or `tests/`

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn display_strips_control_bytes_from_filename() {
    // a filename containing an ESC must not carry it into `display`.
    let cleaned = super::sanitize_display("na\u{1b}[31mme");
    assert_eq!(cleaned, "na[31mme");
}
```

- [ ] **Step 2: Run test to verify it fails**
Run: `cargo test -p shac engine::tests::display_strips_control_bytes_from_filename`
Expected: FAIL (`sanitize_display` not defined).

- [ ] **Step 3: Write minimal implementation**
Add `fn sanitize_display(s: &str) -> String` that drops C0/DEL bytes (keeps printable + whitespace as a single space). Apply it to every user-derived substring placed into a candidate's `display` (NOT to daemon decoration like the arrow prefix).

- [ ] **Step 4: Run test to verify it passes**
Run: `cargo test -p shac engine::tests`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add src/engine.rs
git commit -m "fix(engine): strip control/ESC bytes from user-derived display text"
```

---

## Task 13: zsh widget — decode v3, insert insert_text, whole-token inline accept

**Files:**
- Modify: `shell/zsh/shac.zsh` (request `--format shell-tsv-v3`; decode fields; feed `insert_text` to `compadd -Q`; inline accept replaces the whole active token)
- Test: manual + Task 16 integration

- [ ] **Step 1: Add a decode helper**
Add a zsh function `_shac_decode` that reverses v3 (`\\t`→tab, `\\n`→newline, `\\r`→CR, `\\\\`→`\\`) for each field after `IFS=$'\t'` splitting.

- [ ] **Step 2: Switch the format + fields**
Change all `--format shell-tsv-v2` / `shell-metadata` invocations to `shell-tsv-v3`. Where the menu builds `_shac_menu_displays` for `compadd` (shac.zsh:579), feed the decoded `insert_text` array instead; keep `display` only for the visible menu label.

- [ ] **Step 3: Whole-token inline accept**
In the inline accept path (shac.zsh:196-210), on accept set the buffer by replacing the active token `${BUFFER##*[[:space:]]}`-delimited region with the decoded `insert_text`, rather than appending `suffix`. Keep the ghost-text preview (unescaped `display` remainder) for rendering only.

- [ ] **Step 4: Manual verify**
Run: source the widget in a scratch zsh, create dir `My Docs`, type `cd My` + Tab, confirm the buffer becomes `cd My\ Docs/` and the dir changes on Enter.

- [ ] **Step 5: Commit**
```bash
git add shell/zsh/shac.zsh
git commit -m "feat(zsh): shell-tsv-v3 decode, insert insert_text, whole-token inline accept"
```

---

## Task 14: fish widget — decode v3, insert escaped literal

**Files:**
- Modify: `shell/fish/shac.fish`
- Test: manual + Task 16

- [ ] **Step 1: Add decode + switch format**
Add a fish `_shac_decode` (reverse v3) and change the `shac complete` call to `--format shell-tsv-v3`; decode `insert_text` after `string split \t`.

- [ ] **Step 2: Drop the space guard, insert literally**
Remove the `not string match -q '* *'` guard (shac.fish:41) — the daemon-escaped literal is safe. `commandline -t -- $insert_text` with the decoded value; keep the `__shac_tip` skip (add a `__shac_*` sentinel filter mirroring zsh shac.zsh:172-177 so tip rows never insert).

- [ ] **Step 3: Manual verify**
Run: in fish, scratch dir with `My Docs`, `cd My`+Tab → `cd My\ Docs/`.

- [ ] **Step 4: Commit**
```bash
git add shell/fish/shac.fish
git commit -m "feat(fish): shell-tsv-v3 decode, insert escaped literal, skip tip sentinels"
```

---

## Task 15: bash widget — bind -x insertion via READLINE_LINE

**Files:**
- Modify: `shell/bash/shac.bash`
- Test: manual + Task 16

- [ ] **Step 1: Replace COMPREPLY path**
Bind Tab (or a dedicated key) to a `bind -x` function `_shac_bash_complete` that: calls `shac complete --shell bash --format shell-tsv-v3 --line "$READLINE_LINE" --cursor "$READLINE_POINT"`, decodes fields, and splices the decoded `insert_text` of the top candidate into `READLINE_LINE` at the active-token boundary, updating `READLINE_POINT`. Repeat-press cycles to the next candidate.

- [ ] **Step 2: Remove the sed pipelines**
Delete the GNU-only `[0-9]\+` sed recorder (shac.bash:31,37) or replace the extraction with bash parameter expansion (`${line%% *}` etc.) so recording works on BSD/macOS. (Covers the macOS-dead-recording finding.)

- [ ] **Step 3: Manual verify (bash on macOS)**
Run: bash, scratch dir with `My Docs`, type `cd My` + the bound key → `READLINE_LINE` becomes `cd My\ Docs/`.

- [ ] **Step 4: Commit**
```bash
git add shell/bash/shac.bash
git commit -m "feat(bash): READLINE_LINE insertion via bind -x; POSIX-safe recording"
```

---

## Task 16: real-shell round-trip golden test

**Files:**
- Create: `tests/quote_roundtrip.rs`
- Test: itself

- [ ] **Step 1: Write the test**
```rust
use shac::quote::{quote_token, TokenContext};
use shac::shell::Shell;
use std::process::Command;

fn shell_expands(bin: &str, invoke: &[&str], escaped: &str, expect: &str) -> bool {
    // Runs: <bin> -c 'printf %s\n -- <escaped>' and compares stdout to expect.
    let script = format!("printf '%s' {escaped}");
    let out = Command::new(bin).args(invoke).arg(&script).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout) == expect,
        Err(_) => true, // shell not installed -> skip (treated as pass)
    }
}

#[test]
fn roundtrip_matrix() {
    let payloads = [
        "plain", "My Docs", "a'b", "a\"b", "a$b", "a`b`", "a*b",
        "a#b", "a;b", "a|b", "a(b)b", "a\tb",
    ];
    for p in payloads {
        let z = quote_token(Shell::Zsh, &TokenContext::default(), p);
        assert!(shell_expands("zsh", &["-fc"], &z, p), "zsh: {p:?} -> {z:?}");
        let b = quote_token(Shell::Bash, &TokenContext::default(), p);
        assert!(shell_expands("bash", &["-c"], &b, p), "bash: {p:?} -> {b:?}");
        let f = quote_token(Shell::Fish, &TokenContext::default(), p);
        assert!(shell_expands("fish", &["-c"], &f, p), "fish: {p:?} -> {f:?}");
    }
}
```

- [ ] **Step 2: Run**
Run: `cargo test -p shac --test quote_roundtrip`
Expected: PASS on any installed shells (skips absent ones).

- [ ] **Step 3: Commit**
```bash
git add tests/quote_roundtrip.rs
git commit -m "test(quote): real-shell round-trip golden matrix"
```

---

## Task 17: CI runs the shell round-trip lane

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Ensure shells present**
Add a step installing `zsh fish` (bash is present) on the ubuntu runner before `cargo test`, so `tests/quote_roundtrip.rs` actually exercises all three rather than skipping.

- [ ] **Step 2: Commit**
```bash
git add .github/workflows/ci.yml
git commit -m "ci: install zsh/fish so the quote round-trip lane runs"
```

---

## Self-review notes (spec coverage)

- spec §2 contract → Tasks 11 (insert=escaped insert_text), 12 (display), 13/14/15 (widgets never re-quote).
- spec §3 Shell enum + quote_token → Tasks 1, 2.
- spec §4.1 tilde → Task 4; §4.2 dash → Task 5; §4.3 open quote → Tasks 6, 10; §4.4 control → Task 3; §4.5 empty → Task 2; §4.6 non-UTF8 → inherent (lossy upstream).
- spec §5 wire v3 → Task 7; retire shell-metadata → Task 11; decode in widgets → Tasks 13/14/15; §5.1 display → Task 12.
- spec §6 context deps → Tasks 8, 9, 10.
- spec §7 insertion model → Tasks 13 (zsh incl. whole-token), 14 (fish), 15 (bash bind -x).
- spec §8 testing → Tasks 16 (round-trip), 17 (CI), plus per-task unit tests.

---

## Follow-on plans (the remaining audit findings, NOT in this plan)

Per the writing-plans scope check these are independent subsystems and each gets its own spec→plan when this foundation lands. Findings that dissolve into the foundation above are struck from these counts.

- **Batch A — perf blocker:** `engine.rs:1406` unbounded path candidates + O(dir×SQL) scoring; cap like the other collectors; concurrency (`shacd.rs:104`). (1 blocker + related)
- **Batch B — daemon robustness:** unbounded `read_line` (`shacd.rs:143`), read timeout, pid-kill verification (`shac.rs:1390`), telemetry-table pruning (`db.rs:602`), atomic model write + graceful degrade (`ml.rs:59`), pid/socket lifecycle (`shacd.rs:35`).
- **Batch C — import corruption:** zsh metafication XOR (`import.rs:173`), multiline history (`import.rs:290`), cd-target quote/space truncation (`db.rs:1613`), zoxide length-field guard (`import.rs:422`).
- **Batch D — scoring/DB semantics:** LIKE wildcard escaping (`db.rs:938,1421`), transition scoping (`db.rs:536`), dedup key (`engine.rs:1606`), top_paths recency (`db.rs:935`), token-filter/projection (`engine.rs:557,682,2676`), per-candidate project walk (`engine.rs:1484`), dir-cache newline + mtime granularity (`engine.rs:1383,1389`), replace_docs transaction (`db.rs:386`), priors reinstall (`priors.rs:677`), acceptance prefix (`db.rs:1740`), kubectl tip (`tips.rs:185`).
- **Batch E — remaining widget UX:** ANSI-in-POSTDISPLAY via region_highlight (`shac.zsh:443`), TSV interior-empty-field split (`shac.zsh:526`), multiline history display (`shac.rs:1566`).

Each batch: fresh spec if it has design choices (e.g. transition scoping, cap sizes), else a direct plan; implement with subagent-driven-development; adversarially verify per the established workflow.
