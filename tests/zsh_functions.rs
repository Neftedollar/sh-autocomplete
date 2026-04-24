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
