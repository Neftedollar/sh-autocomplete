//! Single source of truth for shell quoting. `quote_token` produces a literal
//! that, when the target shell evaluates it as the replacement for the active
//! word, expands back to exactly the input token. See
//! docs/superpowers/specs/2026-07-06-daemon-boundary-escaping-design.md.

use crate::shell::Shell;

#[derive(Clone, Debug, Default)]
pub struct TokenContext {
    /// If the already-typed active token opened a quote, which char it was.
    pub open_quote: Option<char>,
    /// True when the candidate is itself a literal command-line flag (daemon
    /// `kind == "option"`, e.g. `-V`, `--release`) rather than a path/value
    /// that merely happens to start with `-`. The §4.2 leading-dash guard
    /// exists to stop a dash-prefixed *filename* from being parsed as a
    /// flag; it must not fire for a candidate that is deliberately a flag.
    pub is_option: bool,
    /// True only when the daemon itself built this `insert_text` by
    /// home-shortening an absolute path (`shorten_with_home` in engine.rs),
    /// e.g. a `path_jump` or recent-workspace candidate. Gates the bare
    /// tilde/`$HOME` prefix (`bare_prefix_len`): a raw filesystem entry that
    /// merely happens to start with `~` or `$HOME` must never be sniffed
    /// into a home reference, or a file literally named `~root` would
    /// tilde-expand to another user's home on insertion (F3/F4). Must be
    /// carried as an explicit signal from the candidate source, never
    /// inferred from the token text.
    pub home_ref: bool,
}

/// Characters that must be backslash-escaped in an unquoted word.
fn needs_escape(shell: Shell, c: char) -> bool {
    // C0 control bytes are handled separately (ANSI-C quoting), not here.
    let common = matches!(
        c,
        ' ' | '"'
            | '\''
            | '\\'
            | '$'
            | '`'
            | '&'
            | '|'
            | ';'
            | '<'
            | '>'
            | '('
            | ')'
            | '{'
            | '}'
            | '*'
            | '?'
            | '['
            | ']'
            | '#'
            | '~'
            | '!'
            | '='
    );
    match shell {
        // `{`/`}` trigger fish brace expansion, `[`/`]` are the variable
        // index/slice operator (`$var[1]`), and `%` introduces job/pid
        // expansion (`%1`, `%self`) — all omitted from the original set
        // (F1/F2), so e.g. a directory literally named `{a,b}` brace-expanded
        // into two words and `weird[1` broke the fish command line.
        Shell::Fish => matches!(
            c,
            ' ' | '"'
                | '\''
                | '\\'
                | '$'
                | '*'
                | '?'
                | '('
                | ')'
                | '~'
                | '#'
                | '&'
                | '|'
                | ';'
                | '<'
                | '>'
                | '{'
                | '}'
                | '['
                | ']'
                | '%'
        ),
        _ => common,
    }
}

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

pub fn quote_token(shell: Shell, ctx: &TokenContext, token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    match ctx.open_quote {
        Some('"') => {
            // A widget replaces the WHOLE active token, including the
            // opening quote the user already typed, so the returned literal
            // must be self-contained: prepend the opening quote too, not
            // just the closing one (F5), then escape " \ $ ` inside it.
            let mut out = String::with_capacity(token.len() + 2);
            out.push('"');
            for c in token.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            return out;
        }
        Some('\'') => {
            // Same self-containment requirement (F5): open with ', splice
            // out any embedded ' via close/escape/reopen, close with '.
            let mut out = String::with_capacity(token.len() + 2);
            out.push('\'');
            for c in token.chars() {
                if c == '\'' {
                    out.push_str("'\\''");
                } else {
                    out.push(c);
                }
            }
            out.push('\'');
            return out;
        }
        _ => {}
    }
    // Only a candidate the daemon explicitly tagged `home_ref` (see its
    // doc comment) gets its leading `~`/`$HOME` left bare for shell
    // expansion; otherwise the bare-prefix scan is skipped entirely and the
    // whole token — including a leading `~` or `$` — is escaped like any
    // other content (F3/F4).
    let prefix_len = if ctx.home_ref {
        bare_prefix_len(token)
    } else {
        0
    };
    let (prefix, tail) = token.split_at(prefix_len);
    let out = format!("{prefix}{}", escape_word(shell, tail));
    if prefix_len == 0 && ctx.open_quote.is_none() && !ctx.is_option && out.starts_with('-') {
        return format!("./{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;

    fn q(sh: Shell, t: &str) -> String {
        quote_token(sh, &TokenContext::default(), t)
    }

    fn q_home(sh: Shell, t: &str) -> String {
        let ctx = TokenContext {
            home_ref: true,
            ..Default::default()
        };
        quote_token(sh, &ctx, t)
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

    #[test]
    fn control_bytes_ansi_c() {
        // A filename with an embedded tab and ESC.
        assert_eq!(q(Shell::Bash, "a\tb"), "a$'\\t'b");
        assert_eq!(q(Shell::Zsh, "x\u{1b}y"), "x$'\\x1b'y");
        assert_eq!(q(Shell::Fish, "a\tb"), "a\\tb");
        assert_eq!(q(Shell::Fish, "x\u{1b}y"), "x\\x1by");
    }

    #[test]
    fn tilde_prefix_stays_bare_when_home_ref() {
        // Only candidates the daemon marks `home_ref` (a genuine
        // `shorten_with_home` result) get the tilde/`$HOME` prefix left bare.
        assert_eq!(q_home(Shell::Zsh, "~/My Docs"), "~/My\\ Docs");
        assert_eq!(q_home(Shell::Zsh, "~"), "~");
        assert_eq!(q_home(Shell::Bash, "~/a b/c"), "~/a\\ b/c");
        assert_eq!(q_home(Shell::Zsh, "$HOME/a b"), "$HOME/a\\ b");
        assert_eq!(q_home(Shell::Fish, "~/My Docs"), "~/My\\ Docs");
    }

    #[test]
    fn tilde_without_home_ref_is_escaped_literally() {
        // F3/F4: a raw filesystem entry that merely *looks* like a home
        // reference (no `home_ref` signal from the daemon) must be escaped
        // like any other content, not sniffed into tilde/$HOME expansion —
        // otherwise a file literally named `~root` or `$HOME` would expand
        // on insertion instead of inserting as a literal filename.
        assert_eq!(q(Shell::Zsh, "~root"), "\\~root");
        assert_eq!(q(Shell::Zsh, "~evil"), "\\~evil");
        assert_eq!(q(Shell::Bash, "$HOME"), "\\$HOME");
        assert_eq!(q(Shell::Zsh, "~"), "\\~");
    }

    #[test]
    fn leading_dash_gets_dot_slash() {
        assert_eq!(q(Shell::Zsh, "--flag/"), "./--flag/");
        assert_eq!(q(Shell::Bash, "-rf"), "./-rf");
        // Not a path-relative candidate if it already has ./ or a bare-prefix:
        assert_eq!(q_home(Shell::Zsh, "~/-x"), "~/-x"); // tail after ~/ is fine
        assert_eq!(q(Shell::Zsh, "a-b"), "a-b"); // dash not leading
    }

    #[test]
    fn is_option_candidates_keep_bare_dash() {
        // A genuine flag candidate (daemon kind == "option", e.g. python's
        // `-V`) must stay a bare flag, not be reinterpreted as a path.
        let ctx = TokenContext {
            is_option: true,
            ..Default::default()
        };
        assert_eq!(quote_token(Shell::Zsh, &ctx, "-V"), "-V");
        assert_eq!(quote_token(Shell::Bash, &ctx, "--release"), "--release");
    }

    #[test]
    fn open_quote_uses_same_style() {
        let dq = TokenContext {
            open_quote: Some('"'),
            ..Default::default()
        };
        let sq = TokenContext {
            open_quote: Some('\''),
            ..Default::default()
        };
        // User typed  cd "My D<Tab>  -> completion opens AND closes the
        // double quote (F5): the widget replaces the whole active token,
        // including the opening quote the user already typed, so the
        // returned literal must be self-contained.
        assert_eq!(quote_token(Shell::Zsh, &dq, "My Docs"), "\"My Docs\"");
        // A double-quote inside double quotes is backslash-escaped.
        assert_eq!(quote_token(Shell::Zsh, &dq, "a\"b"), "\"a\\\"b\"");
        // Single-quote style: open, close/splice/reopen for any embedded ', close.
        assert_eq!(quote_token(Shell::Zsh, &sq, "a'b"), "'a'\\''b'");
    }

    #[test]
    fn open_quote_result_is_self_contained() {
        // F5: quote_token's open_quote output must both open AND close the
        // quote style around the escaped content — never rely on an opening
        // quote already present elsewhere on the command line, because the
        // whole active token (including that opening quote) gets replaced.
        let dq = TokenContext {
            open_quote: Some('"'),
            ..Default::default()
        };
        let dq_result = quote_token(Shell::Zsh, &dq, "a b\"c");
        assert!(dq_result.starts_with('"') && dq_result.ends_with('"'));
        assert_eq!(dq_result, "\"a b\\\"c\"");

        let sq = TokenContext {
            open_quote: Some('\''),
            ..Default::default()
        };
        let sq_result = quote_token(Shell::Zsh, &sq, "a'b c");
        assert!(sq_result.starts_with('\'') && sq_result.ends_with('\''));
        assert_eq!(sq_result, "'a'\\''b c'");
    }

    #[test]
    fn fish_escapes_brace_bracket_and_percent_metachars() {
        // F1/F2: fish's escape set omitted brace/bracket metacharacters, so
        // a directory named `{a,b}` brace-expanded into two words and a
        // file named `weird[1` made the fish command line unparseable.
        assert_eq!(q(Shell::Fish, "{a,b}"), "\\{a,b\\}");
        assert_eq!(q(Shell::Fish, "weird[1"), "weird\\[1");
        assert_eq!(q(Shell::Fish, "a[x]b"), "a\\[x\\]b");
        assert_eq!(q(Shell::Fish, "a%b"), "a\\%b");
    }
}
