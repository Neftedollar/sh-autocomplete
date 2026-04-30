mod support;

use std::process::Command;

#[test]
fn zsh_preview_replaces_only_active_token() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
assert_preview "pyt" 3 "python3" "python3" 7
assert_preview "git ch" 6 "checkout" "git checkout" 12
assert_preview "python3 -" 9 "-m" "python3 -m" 10
assert_preview "cd src/fo" 9 "src/foo" "cd src/foo" 10
assert_preview "git ch -- file" 6 "checkout" "git checkout -- file" 12
"#,
    );
}

#[test]
fn zsh_menu_state_steps_and_restores_original_buffer() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
BUFFER="pyt"
CURSOR=3
_shac_last_request_id="42"
_shac_menu_item_keys=("python3" "python3-config")
_shac_menu_insert_texts=("python3" "python3-config")
_shac_menu_displays=("python3" "python3-config")
_shac_menu_kinds=("command" "command")
_shac_menu_sources=("path_index" "path_index")
_shac_menu_descriptions=("Python interpreter" "Python config")

_shac_open_menu
assert_eq "$BUFFER" "python3" "open menu applies first candidate"
assert_eq "$CURSOR" "7" "open menu cursor"
assert_eq "$_shac_menu_selected_index" "1" "open menu selected index"
assert_eq "$_shac_last_accepted_item_key" "python3" "open menu accepted key"
assert_eq "$_shac_last_accepted_rank" "0" "open menu accepted rank"
assert_eq "$_shac_input_provenance" "accepted_completion" "open menu provenance"

_shac_menu_step 1
assert_eq "$BUFFER" "python3-config" "tab moves to next candidate"
assert_eq "$CURSOR" "14" "next candidate cursor"
assert_eq "$_shac_menu_selected_index" "2" "next candidate selected index"
assert_eq "$_shac_last_accepted_item_key" "python3-config" "next candidate accepted key"
assert_eq "$_shac_last_accepted_rank" "1" "next candidate accepted rank"

_shac_menu_step -1
assert_eq "$BUFFER" "python3" "shift-tab moves to previous candidate"
assert_eq "$_shac_menu_selected_index" "1" "previous candidate selected index"

_shac_close_menu 1
assert_eq "$BUFFER" "pyt" "close menu restores original buffer"
assert_eq "$CURSOR" "3" "close menu restores original cursor"
assert_eq "$_shac_menu_open" "0" "close menu clears open flag"
"#,
    );
}

#[test]
fn zsh_menu_commit_accepts_without_running() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
BUFFER="python3 -"
CURSOR=9
_shac_last_request_id="42"
_shac_menu_item_keys=("python3 -m")
_shac_menu_insert_texts=("-m")
_shac_menu_displays=("-m")
_shac_menu_kinds=("option")
_shac_menu_sources=("builtin-index")
_shac_menu_descriptions=("Run a library module as a script")

_shac_open_menu
assert_eq "$BUFFER" "python3 -m" "open menu previews selected option"
_shac_space_widget
assert_eq "$BUFFER" "python3 -m " "enter commits -m and adds required space"
assert_eq "$CURSOR" "11" "commit cursor after inserted space"
assert_eq "$_shac_menu_open" "0" "commit closes menu"
assert_eq "$_shac_last_accepted_item_key" "python3 -m" "commit keeps accepted key"
assert_eq "$_shac_input_provenance" "accepted_completion" "commit keeps accepted provenance"

BUFFER="pyt"
CURSOR=3
_shac_last_request_id="43"
_shac_menu_item_keys=("python3")
_shac_menu_insert_texts=("python3")
_shac_menu_displays=("python3")
_shac_menu_kinds=("command")
_shac_menu_sources=("path_index")
_shac_menu_descriptions=("Python interpreter")

_shac_open_menu
_shac_forward_char_widget
assert_eq "$BUFFER" "python3" "right commits command without extra space"
assert_eq "$CURSOR" "7" "right commit cursor"
assert_eq "$_shac_menu_open" "0" "right commit closes menu"
"#,
    );
}

#[test]
fn zsh_menu_render_respects_ui_detail_settings() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
_shac_menu_open=1
_shac_menu_selected_index=1
_shac_menu_item_keys=("checkout")
_shac_menu_insert_texts=("checkout")
_shac_menu_displays=("checkout")
_shac_menu_kinds=("subcommand")
_shac_menu_sources=("builtin-index")
_shac_menu_descriptions=("Switch branches or restore working tree files")

_shac_ui_menu_detail="minimal"
_shac_ui_show_kind=1
_shac_ui_show_source=1
_shac_ui_show_description=1
_shac_render_menu
assert_contains "$POSTDISPLAY" "> checkout" "minimal render includes display"
assert_not_contains "$POSTDISPLAY" "subcommand" "minimal render hides kind"
assert_not_contains "$POSTDISPLAY" "Switch branches" "minimal render hides description"

_shac_ui_menu_detail="compact"
_shac_ui_show_kind=0
_shac_ui_show_source=0
_shac_ui_show_description=1
_shac_render_menu
assert_contains "$POSTDISPLAY" "> checkout  Switch branches" "compact render includes description"
assert_not_contains "$POSTDISPLAY" "builtin-index" "compact render hides source"

_shac_ui_menu_detail="debug"
_shac_ui_show_kind=0
_shac_ui_show_source=0
_shac_render_menu
assert_contains "$POSTDISPLAY" "[subcommand/builtin-index]" "debug render forces metadata"
"#,
    );
}

#[test]
fn zsh_menu_render_tints_path_jump_kind() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
unset SHAC_NO_COLOR
_shac_menu_open=1
_shac_menu_selected_index=1
_shac_menu_item_keys=("~/Documents/dev/sh-autocomplete")
_shac_menu_insert_texts=("~/Documents/dev/sh-autocomplete")
_shac_menu_displays=($'\xe2\x86\x92 ~/Documents/dev/sh-autocomplete')
_shac_menu_kinds=("path_jump")
_shac_menu_sources=("path_jump")
_shac_menu_descriptions=("git repo")

_shac_ui_menu_detail="debug"
_shac_ui_show_kind=0
_shac_ui_show_source=0
_shac_ui_show_description=0
_shac_render_menu
assert_contains "$POSTDISPLAY" $'\e[36m[path_jump/path_jump]\e[0m' "debug render tints path_jump label cyan"
assert_contains "$POSTDISPLAY" $'\e[36m\xe2\x86\x92\e[0m' "menu render tints path_jump arrow cyan"

# SHAC_NO_COLOR opt-out: no ANSI escapes anywhere in POSTDISPLAY.
SHAC_NO_COLOR=1 _shac_render_menu
assert_not_contains "$POSTDISPLAY" $'\e[36m' "SHAC_NO_COLOR disables cyan tint"

# Non-path_jump kinds remain uncolored even when color is on.
unset SHAC_NO_COLOR
_shac_menu_kinds=("subcommand")
_shac_menu_sources=("builtin-index")
_shac_menu_displays=("checkout")
_shac_render_menu
assert_not_contains "$POSTDISPLAY" $'\e[36m' "non-path_jump kind is not tinted"
"#,
    );
}

#[test]
fn zsh_menu_type_and_backspace_keep_selected_buffer() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
# Set up a menu with "cd Do" typed, "Documents" selected (already applied to BUFFER).
function setup_menu() {
  BUFFER="cd Documents"
  CURSOR=13
  _shac_menu_open=1
  _shac_menu_selected_index=1
  _shac_menu_original_buffer="cd Do"
  _shac_menu_original_cursor=5
  _shac_menu_item_keys=("cd Documents")
  _shac_menu_insert_texts=("Documents")
  _shac_menu_displays=("Documents")
  _shac_menu_kinds=("directory")
  _shac_menu_sources=("path")
  _shac_menu_descriptions=("")
}

# Typing "/" while menu open: commit_selected_item keeps the selected text.
# (_shac_self_insert_widget calls _shac_commit_selected_item never before inserting)
setup_menu
_shac_commit_selected_item never
assert_eq "$BUFFER" "cd Documents" "commit_selected_item never keeps selected buffer"
assert_eq "$_shac_menu_open" "0" "menu closed after commit"

# Backspace while menu open: note_manual_edit should NOT restore original buffer.
# Before the fix, close_menu 1 reverted BUFFER to "cd Do". Now close_menu 0 keeps it.
setup_menu
_shac_note_manual_edit
assert_eq "$BUFFER" "cd Documents" "note_manual_edit keeps selected buffer, not original"
assert_eq "$_shac_menu_open" "0" "menu closed after note_manual_edit"
"#,
    );
}

#[test]
fn zsh_version_mismatch_warning() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    run_zsh(
        r#"
# _shac_version_gt: basic ordering
_shac_version_gt "0.5.2" "0.5.1" && assert_eq "yes" "yes" "0.5.2 > 0.5.1" || { print -ru2 "FAIL: 0.5.2 should be > 0.5.1"; exit 1; }
_shac_version_gt "0.5.1" "0.5.2" && { print -ru2 "FAIL: 0.5.1 should NOT be > 0.5.2"; exit 1; } || true
_shac_version_gt "1.0.0" "0.9.9" && assert_eq "yes" "yes" "1.0.0 > 0.9.9" || { print -ru2 "FAIL: 1.0.0 should be > 0.9.9"; exit 1; }
_shac_version_gt "0.5.1" "0.5.1" && { print -ru2 "FAIL: equal versions should not be gt"; exit 1; } || true

# Daemon stale (client newer): expect "brew services restart shac"
_shac_version_warned=0
_shac_client_version="0.5.2"
_shac_daemon_version="0.5.1"
_shac_pending_tip_text=""
_shac_check_version_mismatch
assert_contains "$_shac_pending_tip_text" "brew services restart shac" "stale daemon shows restart command"
assert_contains "$_shac_pending_tip_text" "0.5.2" "stale daemon warning includes client version"
assert_eq "$_shac_version_warned" "1" "version_warned flag set after mismatch"

# Client outdated (daemon newer): expect "brew upgrade shac"
_shac_version_warned=0
_shac_client_version="0.5.1"
_shac_daemon_version="0.5.2"
_shac_pending_tip_text=""
_shac_check_version_mismatch
assert_contains "$_shac_pending_tip_text" "brew upgrade shac" "outdated client shows upgrade command"

# Same version: no warning
_shac_version_warned=0
_shac_client_version="0.5.2"
_shac_daemon_version="0.5.2"
_shac_pending_tip_text=""
_shac_check_version_mismatch
assert_eq "$_shac_pending_tip_text" "" "no warning when versions match"
assert_eq "$_shac_version_warned" "0" "version_warned not set when versions match"

# Already warned: tip not overwritten
_shac_version_warned=1
_shac_client_version="0.5.2"
_shac_daemon_version="0.5.1"
_shac_pending_tip_text="already set"
_shac_check_version_mismatch
assert_eq "$_shac_pending_tip_text" "already set" "already warned: tip not overwritten"
"#,
    );
}

fn run_zsh(body: &str) {
    let script_path = std::env::current_dir()
        .expect("current dir")
        .join("shell")
        .join("zsh")
        .join("shac.zsh");
    let harness = format!(
        r#"
set -e
function assert_eq() {{
  local actual="$1"
  local expected="$2"
  local message="$3"
  if [[ "$actual" != "$expected" ]]; then
    print -ru2 -- "assertion failed: $message"
    print -ru2 -- "expected: <$expected>"
    print -ru2 -- "actual:   <$actual>"
    exit 1
  fi
}}
function assert_preview() {{
  local base_buffer="$1"
  local base_cursor="$2"
  local insert_text="$3"
  local expected_buffer="$4"
  local expected_cursor="$5"
  _shac_preview_buffer_for_item "$base_buffer" "$base_cursor" "$insert_text"
  assert_eq "$REPLY" "$expected_buffer" "preview buffer for $base_buffer -> $insert_text"
  assert_eq "$REPLY2" "$expected_cursor" "preview cursor for $base_buffer -> $insert_text"
}}
function assert_contains() {{
  local haystack="$1"
  local needle="$2"
  local message="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    print -ru2 -- "assertion failed: $message"
    print -ru2 -- "missing: <$needle>"
    print -ru2 -- "haystack: <$haystack>"
    exit 1
  fi
}}
function assert_not_contains() {{
  local haystack="$1"
  local needle="$2"
  local message="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    print -ru2 -- "assertion failed: $message"
    print -ru2 -- "unexpected: <$needle>"
    print -ru2 -- "haystack: <$haystack>"
    exit 1
  fi
}}
source "{script_path}"
{body}
"#,
        script_path = script_path.display(),
        body = body
    );

    let output = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(harness)
        .env("SHAC_ZSH_TEST_MODE", "1")
        .output()
        .expect("run zsh function test");
    assert!(
        output.status.success(),
        "zsh function test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn zsh_render_menu_includes_tip_footer() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    let manifest = env!("CARGO_MANIFEST_DIR");
    let adapter = format!("{manifest}/shell/zsh/shac.zsh");
    let harness = format!("{manifest}/tests/zsh_render.sh");
    let out = std::process::Command::new("zsh")
        .arg(&harness)
        .arg(&adapter)
        .arg("hello tip world")
        .output()
        .expect("run zsh harness");
    assert!(
        out.status.success(),
        "harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("hello tip world"),
        "expected tip text in POSTDISPLAY, got:\n{stdout}"
    );
}

#[test]
fn zsh_render_menu_skips_tip_when_no_tips_env_set() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh function tests: zsh is unavailable");
        return;
    }

    let manifest = env!("CARGO_MANIFEST_DIR");
    let adapter = format!("{manifest}/shell/zsh/shac.zsh");
    let harness = format!("{manifest}/tests/zsh_render.sh");
    let out = std::process::Command::new("zsh")
        .arg(&harness)
        .arg(&adapter)
        .arg("hello tip world")
        .arg("1")
        .output()
        .expect("run zsh harness");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("hello tip world"),
        "expected no tip when SHAC_NO_TIPS=1, got:\n{stdout}"
    );
}
