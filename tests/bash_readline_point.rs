mod support;

use std::process::Command;

/// MINOR fix: on the bash>=4 `bind -x` accept path, `_shac_bash_splice_candidate`
/// (shell/bash/shac.bash) must set `READLINE_POINT` to a BYTE offset -- that's
/// what readline itself expects (like `COMP_POINT`, see `_shac_bash_char_point`'s
/// own doc comment) -- not a bash `${#s}` CHARACTER count. A candidate whose
/// splice point sits after multibyte text previously mis-positioned the cursor.
///
/// `_shac_bash_splice_candidate` is defined unconditionally (only its
/// *registration* via `bind -x` is gated on `BASH_VERSINFO[0] >= 4`), so it can
/// be exercised directly here without an interactive bash>=4 terminal: source
/// the script, seed the globals it reads, call it, and inspect the plain shell
/// variables it assigns (`READLINE_LINE`/`READLINE_POINT` are ordinary
/// variables outside of an actual `bind -x` dispatch).
fn run_splice(before: &str, insert: &str, after: &str) -> (String, String) {
    let script_path = format!("{}/shell/bash/shac.bash", env!("CARGO_MANIFEST_DIR"));
    let script = format!(
        r#"
source "{script_path}"
_shac_bash_before={before:?}
_shac_bash_after={after:?}
_shac_bash_candidates=({insert:?})
_shac_bash_cycle_index=0
_shac_bash_splice_candidate
printf '%s\n%s\n' "$READLINE_LINE" "$READLINE_POINT"
"#,
        script_path = script_path,
        before = before,
        after = after,
        insert = insert,
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert!(
        output.status.success(),
        "bash script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut lines = stdout.lines();
    let line = lines.next().unwrap_or_default().to_string();
    let point = lines.next().unwrap_or_default().to_string();
    (line, point)
}

#[test]
fn splice_candidate_sets_byte_offset_readline_point_for_multibyte_prefix() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // "cd é" is 4 chars but 5 bytes (é is a 2-byte UTF-8 sequence). Splicing
    // an ASCII-only candidate after it means READLINE_POINT must land at the
    // BYTE offset of the end of the inserted text, not the character offset.
    let before = "cd é";
    let insert = "Documents/";
    let after = "";
    let (line, point) = run_splice(before, insert, after);

    assert_eq!(line, "cd éDocuments/");
    let expected_byte_point = before.len() + insert.len();
    assert_eq!(
        point.parse::<usize>().expect("numeric READLINE_POINT"),
        expected_byte_point,
        "READLINE_POINT must be a byte offset ({expected_byte_point}), not the \
         character count ({char_count}), so the cursor lands after the \
         spliced text when the prefix contains multibyte characters",
        char_count = before.chars().count() + insert.chars().count(),
    );
}

#[test]
fn splice_candidate_matches_byte_offset_for_ascii_only_prefix() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // Pure-ASCII control case: char count == byte count, so this must pass
    // both before and after the fix -- it guards against a byte-length
    // helper that's simply wrong (e.g. always 0), not just off in the
    // multibyte case.
    let before = "cd D";
    let insert = "ocuments/";
    let after = "";
    let (line, point) = run_splice(before, insert, after);

    assert_eq!(line, "cd Documents/");
    assert_eq!(
        point.parse::<usize>().expect("numeric READLINE_POINT"),
        before.len() + insert.len()
    );
}
