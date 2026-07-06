//! Path / PII scrubbing rules. Applied to:
//!   - real history corpora before they enter training
//!   - the maintainer's local zsh history before it joins the synthetic dataset
//!
//! Rules are intentionally simple regex-replace; no context-sensitive parsing.
//! New rules go through tests/scrub_redlist.rs.

use std::sync::OnceLock;

use regex::Regex;
use shac::import::SECRET_PATTERNS;

struct Rule {
    re: Regex,
    replacement: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        // Known secret shapes come from the shared red-list in the root crate
        // (same patterns the history importer uses to DROP commands; here the
        // match is replaced instead). They run before the structural rules so
        // canonical secrets are gone before anything else rewrites the line.
        let mut rules: Vec<Rule> = SECRET_PATTERNS
            .iter()
            .map(|pattern| Rule {
                re: Regex::new(pattern).unwrap(),
                replacement: "<TOKEN>",
            })
            .collect();
        rules.extend(vec![
            // Key-context assignment: `<key>=<value>` where the key contains
            // a secret-ish word. The word must end at a `_`/`-`/`=` boundary,
            // so CARGO_TARGET_DIR and `tokenizer` never match, and a literal
            // value is required, so bare flags like `--password-stdin` never
            // match. Catches secrets regardless of the value's alphabet
            // (slashes, dots, '=' padding, …).
            Rule {
                re: Regex::new(
                    r"(?ix)
                    ( [A-Za-z0-9_-]*
                      (?: secret | token | password | passwd | api[_-]?key
                        | access[_-]?key | private[_-]?key | credentials? )
                      (?: [_-] [A-Za-z0-9_-]* )? = )
                    \S+
                    ",
                )
                .unwrap(),
                replacement: "${1}<TOKEN>",
            },
            // Key-context, space-separated form: `aws configure set
            // aws_secret_access_key <value>`. Stricter than the `=` form —
            // the key must END with the secret-ish word and the value must
            // be a >=16-char non-flag token — so `--password-stdin`,
            // `cargo test tokenizer` and `rg token src/import.rs` survive.
            Rule {
                re: Regex::new(
                    r"(?ix)
                    ( [A-Za-z0-9_-]*
                      (?: secret | token | password | passwd | api[_-]?key
                        | access[_-]?key | private[_-]?key | credentials? )
                      \s+ )
                    [^\s-] \S{15,}
                    ",
                )
                .unwrap(),
                replacement: "${1}<TOKEN>",
            },
            // /Users/<name>/... → <HOME>/...
            Rule {
                re: Regex::new(r"/Users/[^/\s]+(/[^\s]*)?").unwrap(),
                replacement: "<HOME>$1",
            },
            // /home/<name>/... → <HOME>/...
            Rule {
                re: Regex::new(r"/home/[^/\s]+(/[^\s]*)?").unwrap(),
                replacement: "<HOME>$1",
            },
            // /var/folders/aa/bb/T/file → <TMPDIR>/file (macOS per-session tmp)
            Rule {
                re: Regex::new(r"/var/folders/[^/\s]+/[^/\s]+/T(/[^\s]*)?").unwrap(),
                replacement: "<TMPDIR>$1",
            },
            // /tmp/<random-8-or-more> → <TMPDIR>/<id>
            Rule {
                re: Regex::new(r"/tmp/[A-Za-z0-9_.-]{8,}").unwrap(),
                replacement: "<TMPDIR>",
            },
            // emails
            Rule {
                re: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap(),
                replacement: "<EMAIL>",
            },
            // IPv4
            Rule {
                re: Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap(),
                replacement: "<IP>",
            },
            // IPv6, three deliberate shapes: full 8-group form (7 colons), or
            // `::` compression with a required hex group on the right. Single
            // colons never match, so host:port, -p 8080:80, and HH:MM(:SS)
            // survive. Trailing-`::` prefixes (`2001:db8::`) are deliberately
            // NOT matched — that shape is indistinguishable from Rust/C++
            // `mod::` path prefixes. The bare `::b` shape requires a
            // non-alphanumeric left context so `scrub::add`-style paths with
            // an all-hex right segment survive while `::1` / `[::1]` scrub.
            Rule {
                re: Regex::new(
                    r"(?x)
                    \b (?: [0-9A-Fa-f]{1,4} : ){7} [0-9A-Fa-f]{1,4} \b        # full form
                    | \b (?: [0-9A-Fa-f]{1,4} : )+ :
                        [0-9A-Fa-f]{1,4} (?: : [0-9A-Fa-f]{1,4} )* \b         # a::b
                    | ( ^ | [^A-Za-z0-9] )
                        :: [0-9A-Fa-f]{1,4} (?: : [0-9A-Fa-f]{1,4} )* \b      # ::b
                    ",
                )
                .unwrap(),
                // `$1` is empty for the first two branches.
                replacement: "${1}<IP>",
            },
            // '='-padded base64 blobs (e.g. `Authorization: Basic …==`),
            // slash-containing or not. The generic rule below treats '/' as
            // a delimiter, so this is the only structural rule catching
            // slash-containing base64; the trailing-'=' requirement is what
            // keeps benign paths and `--flag=value` shapes safe (their runs
            // never end in '=').
            Rule {
                re: Regex::new(r"(^|[^A-Za-z0-9+/])[A-Za-z0-9+/]{24,}={1,2}").unwrap(),
                replacement: "${1}<TOKEN>",
            },
            // Generic secret blobs: runs of >=24 alnum/'+' chars delimited by
            // anything else. '/', '-', '_', '=' and '.' are delimiters, not
            // run chars, so paths, kebab-case names, env-var assignments and
            // long flags survive. Tradeoff: a blob containing '/' escapes
            // this rule and is only scrubbed when a key-context rule above
            // applies, a SECRET_PATTERNS prefix matches, or it carries '='
            // padding (rule above). A bare unpadded slash-containing blob
            // with no secret-ish key word — e.g. an AWS secret access key
            // passed positionally — is NOT scrubbed.
            Rule {
                re: Regex::new(r"(^|[^A-Za-z0-9+])[A-Za-z0-9+]{24,}").unwrap(),
                replacement: "${1}<TOKEN>",
            },
        ]);
        rules
    })
}

pub fn scrub_text(input: &str) -> String {
    let mut out = input.to_string();
    for rule in rules() {
        out = rule.re.replace_all(&out, rule.replacement).to_string();
    }
    out
}
