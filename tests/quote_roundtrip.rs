//! Real-shell round-trip golden matrix for `shac::quote::quote_token`.
//!
//! For each payload x {zsh, bash, fish}: quote the payload for that shell,
//! then actually run the shell on `printf '%s' <quoted>` and assert the
//! shell's own expansion reproduces the original payload byte-for-byte.
//! This tests the real property (the target shell expands our literal back
//! to the input token), not a hand-maintained table of expected escapes.
//! A shell that is not installed on the runner is skipped, not failed.

use shac::quote::{quote_token, TokenContext};
use shac::shell::Shell;
use std::process::Command;

/// One class of shell-significant input per plan Task 16: space, single
/// quote, double quote, `$`, backtick, glob, `#`, `;`, `|`, parens, tab, and
/// a leading-dash token (plus a plain baseline). Each entry is
/// `(payload, expected shell-visible result)`; for every payload but the
/// leading-dash one, the expectation is the payload itself (true
/// round-trip). The leading-dash payload is the one designed exception: per
/// spec 4.2, `quote_token` deliberately prepends `./` so the inserted token
/// can never be parsed as an option flag, so the shell reproduces `./-rf`,
/// not the bare `-rf`.
///
/// The `{a,b}`, `weird[1`, and `a[x]b` payloads cover F1/F2: fish's escape
/// set previously omitted brace/bracket metacharacters, so a directory
/// named `{a,b}` brace-expanded into two words and a file named `weird[1`
/// made the fish command line unparseable. zsh/bash already escaped these
/// characters, so the same three payloads also exercise the (unchanged,
/// already-correct) zsh/bash behavior.
const PAYLOADS: &[(&str, &str)] = &[
    ("plain", "plain"),
    ("My Docs", "My Docs"),
    ("a'b", "a'b"),
    ("a\"b", "a\"b"),
    ("a$b", "a$b"),
    ("a`b`", "a`b`"),
    ("a*b", "a*b"),
    ("a#b", "a#b"),
    ("a;b", "a;b"),
    ("a|b", "a|b"),
    ("a(b)b", "a(b)b"),
    ("a\tb", "a\tb"),
    ("-rf", "./-rf"),
    ("{a,b}", "{a,b}"),
    ("weird[1", "weird[1"),
    ("a[x]b", "a[x]b"),
];

fn shell_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Runs `<bin> <invoke...> "printf '%s' <escaped>"` and checks stdout matches
/// `expect` exactly (no shell-added newline: `printf %s` emits none).
fn shell_roundtrip(bin: &str, invoke: &[&str], escaped: &str, expect: &str) -> Result<(), String> {
    let script = format!("printf '%s' {escaped}");
    let out = Command::new(bin)
        .args(invoke)
        .arg(&script)
        .output()
        .map_err(|e| format!("failed to spawn {bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bin} exited with {}\n  script: {script}\n  stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let got = String::from_utf8_lossy(&out.stdout);
    if got == expect {
        Ok(())
    } else {
        Err(format!(
            "{bin}: expected {expect:?}, got {got:?}\n  script: {script}"
        ))
    }
}

#[test]
fn roundtrip_matrix() {
    // (Shell variant, binary name, invocation flags to run a command string
    // without sourcing rc files where the shell supports it).
    let shells: &[(Shell, &str, &[&str])] = &[
        (Shell::Zsh, "zsh", &["-fc"]),
        (Shell::Bash, "bash", &["-c"]),
        (Shell::Fish, "fish", &["-c"]),
    ];

    let mut tested_any = false;
    for (shell, bin, invoke) in shells {
        if !shell_available(bin) {
            eprintln!("SKIP {bin}: not installed");
            continue;
        }
        tested_any = true;
        eprintln!("RUN {bin}: {} payloads", PAYLOADS.len());
        for (payload, expect) in PAYLOADS {
            let escaped = quote_token(*shell, &TokenContext::default(), payload);
            match shell_roundtrip(bin, invoke, &escaped, expect) {
                Ok(()) => eprintln!("  ok   {payload:?} -> {escaped:?}"),
                Err(e) => panic!("{bin} round-trip failed for {payload:?}: {e}"),
            }
        }
    }

    if !tested_any {
        eprintln!("no shells (zsh/bash/fish) installed; roundtrip_matrix is a no-op");
    }
}
