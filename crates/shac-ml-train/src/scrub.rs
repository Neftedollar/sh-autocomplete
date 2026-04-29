//! Path / PII scrubbing rules. Applied to:
//!   - real history corpora before they enter training
//!   - the maintainer's local zsh history before it joins the synthetic dataset
//!
//! Rules are intentionally simple regex-replace; no context-sensitive parsing.
//! New rules go through tests/scrub_redlist.rs.

use std::sync::OnceLock;

use regex::Regex;

struct Rule {
    re: Regex,
    replacement: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
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
            // IPv6 (simplified: must contain at least one colon to avoid matching hex tokens)
            // Pattern: hex digits and colons, at least 7 chars, must have at least one colon
            Rule {
                re: Regex::new(r"\b[0-9a-fA-F]*:[0-9a-fA-F:]*[0-9a-fA-F]\b").unwrap(),
                replacement: "<IP>",
            },
            // AWS access key id (must come BEFORE generic hex token rule)
            Rule {
                re: Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
                replacement: "<AWS_KEY>",
            },
            // Long hex/base64-ish tokens (>=24 chars) — catches GH PATs, generic secrets
            Rule {
                re: Regex::new(r"\b[A-Za-z0-9_/+=-]{24,}\b").unwrap(),
                replacement: "<TOKEN>",
            },
        ]
    })
}

pub fn scrub_text(input: &str) -> String {
    let mut out = input.to_string();
    for rule in rules() {
        out = rule.re.replace_all(&out, rule.replacement).to_string();
    }
    out
}
