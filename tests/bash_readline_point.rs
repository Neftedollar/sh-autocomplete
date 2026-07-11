mod support;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Drive `_shac_bash_active_region LINE POINT` and return the (before, after)
/// splice regions it assigns. Like `run_splice`, this exercises the function
/// directly by sourcing the script and reading the plain globals it sets.
fn run_active_region(line: &str, point: usize) -> (String, String) {
    let script_path = format!("{}/shell/bash/shac.bash", env!("CARGO_MANIFEST_DIR"));
    let script = format!(
        r#"
source "{script_path}"
_shac_bash_active_region {line:?} {point}
printf '%s\n%s\n' "$_shac_bash_before" "$_shac_bash_after"
"#,
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
    // Two `printf %s\n` lines; a trailing empty region yields an empty line, so
    // don't let `lines()` swallow it — index positionally.
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let parts: Vec<&str> = stdout.splitn(2, '\n').collect();
    let before = parts.first().copied().unwrap_or_default().to_string();
    let after = parts
        .get(1)
        .map(|rest| rest.strip_suffix('\n').unwrap_or(rest))
        .unwrap_or_default()
        .to_string();
    (before, after)
}

#[test]
fn active_region_splits_on_plain_whitespace() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // Cursor at end of the last word: `foo` is the active token, `cat ` is the
    // untouched prefix.
    assert_eq!(run_active_region("cat foo", 7), ("cat ".into(), "".into()));
    // Cursor inside a middle word: the trailing ` bar` is preserved in `after`.
    assert_eq!(
        run_active_region("cat foo bar", 5),
        ("cat ".into(), " bar".into())
    );
}

#[test]
fn active_region_is_quote_and_escape_aware() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // Whitespace inside double quotes must NOT split the token: the whole
    // `"my fi` span is active, so a splice replaces it wholesale (F6).
    assert_eq!(
        run_active_region("cat \"my fi", 10),
        ("cat ".into(), "".into())
    );
    // Same for single quotes...
    assert_eq!(
        run_active_region("echo 'a b", 9),
        ("echo ".into(), "".into())
    );
    // ...and for a backslash-escaped space.
    assert_eq!(
        run_active_region("cat My\\ fi", 9),
        ("cat ".into(), "".into())
    );
}

/// Drive `_shac_bash_request` against a fake `shac` on PATH that emits
/// `header_response`, and return the resulting `_shac_last_request_id`.
fn last_request_id_after(header_response: &str) -> String {
    let script_path = format!("{}/shell/bash/shac.bash", env!("CARGO_MANIFEST_DIR"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("shac-reqid-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).expect("mk fake bin dir");
    let respfile = dir.join("resp.txt");
    std::fs::write(&respfile, header_response).expect("write canned response");
    let fake = dir.join("shac");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\ncat {:?}\n", respfile.to_string_lossy()),
    )
    .expect("write fake shac");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod fake");

    let script = format!(
        r#"
export PATH="{dir}:$PATH"
source "{script_path}"
_shac_last_request_id=""
_shac_bash_request "foobar" 6
printf '%s' "$_shac_last_request_id"
"#,
        dir = dir.display(),
        script_path = script_path,
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "bash script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Create a temp dir with the given entries (name, is_dir), then run
/// `_shac_bash_default_fallback token` with `before`/`after` splice regions,
/// returning the resulting (READLINE_LINE, READLINE_POINT). READLINE_LINE is
/// preset to "PRESET" so a no-op (guarded token) is observable as unchanged.
fn run_default_fallback(
    entries: &[(&str, bool)],
    before: &str,
    after: &str,
    token: &str,
) -> (String, String) {
    let script_path = format!("{}/shell/bash/shac.bash", env!("CARGO_MANIFEST_DIR"));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("shac-fallback-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).expect("mk fallback dir");
    for (name, is_dir) in entries {
        if *is_dir {
            std::fs::create_dir_all(dir.join(name)).expect("mk entry dir");
        } else {
            std::fs::write(dir.join(name), "").expect("touch entry");
        }
    }
    let script = format!(
        r#"
cd "{dir}" || exit 1
source "{script_path}"
_shac_bash_before={before:?}
_shac_bash_after={after:?}
READLINE_LINE="PRESET"
READLINE_POINT="99"
_shac_bash_default_fallback {token:?}
printf '%s\n%s' "$READLINE_LINE" "$READLINE_POINT"
"#,
        dir = dir.display(),
        script_path = script_path,
    );
    let output = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        output.status.success(),
        "bash script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut parts = stdout.splitn(2, '\n');
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

#[test]
fn default_fallback_completes_unique_and_common_prefix() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // Unique dir match gets completed with a trailing slash, spliced between
    // the before/after regions.
    let (line, point) = run_default_fallback(&[("Documents", true)], "cat ", "", "Doc");
    assert_eq!(line, "cat Documents/");
    assert_eq!(point, "14"); // byte length of "cat Documents/"

    // Two matches: insert only the longest common prefix ("Do"), no slash.
    let (line, _) =
        run_default_fallback(&[("Documents", true), ("Downloads", true)], "cat ", "", "D");
    assert_eq!(line, "cat Do");

    // No match: leave the line untouched.
    let (line, point) = run_default_fallback(&[("Documents", true)], "cat ", "", "zzz");
    assert_eq!(line, "PRESET");
    assert_eq!(point, "99");
}

#[test]
fn default_fallback_skips_quoted_and_tilde_tokens() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // Tokens carrying quoting/escapes/tilde are left a no-op (can't safely
    // reconstruct intent) — READLINE_LINE stays untouched.
    for token in ["\"Doc", "~/Doc", "My\\ D"] {
        let (line, _) = run_default_fallback(&[("Documents", true)], "cat ", "", token);
        assert_eq!(line, "PRESET", "token {token:?} must be a no-op");
    }
}

#[test]
fn request_id_zero_sentinel_is_treated_as_no_request() {
    if !support::command_available("bash") {
        eprintln!("skipping: bash unavailable");
        return;
    }

    // A zero-candidate response keeps the TSV request-id field non-empty for
    // wire alignment (F2), sending the sentinel "0". The adapter must NOT adopt
    // it as a live request id, else a no-match Tab followed by running the line
    // records --accepted-request-id 0 and the server mis-attributes it (codex/#40).
    assert_eq!(
        last_request_id_after("__shac_request_id\t0\treplace_token\t0\n"),
        "",
        "the \"0\" sentinel must be neutralized to no-request"
    );

    // A genuine positive request id is still adopted normally.
    assert_eq!(
        last_request_id_after("__shac_request_id\t42\treplace_token\t0\n"),
        "42",
        "a real request id must still be recorded"
    );
}
