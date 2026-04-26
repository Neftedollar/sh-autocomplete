mod support;

use std::process::Command;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use serde_json::Value;

#[test]
fn zsh_pty_records_manual_accept_and_exact_paste() {
    if should_skip_pty_on_ci_linux() {
        eprintln!("skipping zsh PTY smoke on Linux CI");
        return;
    }
    if !support::command_available("zsh") || !support::command_available("python3") {
        eprintln!("skipping zsh PTY smoke: zsh or python3 is unavailable");
        return;
    }

    let env = support::TestEnv::new("zsh-pty");
    support::run_ok(&env, ["config", "set", "daemon_timeout_ms", "750"]);
    support::run_ok(&env, ["install", "--shell", "zsh"]);
    support::assert_path_exists(&env.zsh_script_path());
    let _daemon = env.spawn_daemon();
    let zsh_path = deterministic_zsh_path(&env);

    let script = r#"
import os
import pty
import select
import sys
import time

env = os.environ.copy()
env["PATH"] = env["ZSH_PATH"]
pid, fd = pty.fork()
if pid == 0:
    os.execvpe("zsh", ["zsh", "-f"], env)

def drain(timeout=0.15):
    end = time.time() + timeout
    out = b""
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.03)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
    return out

def send(text, delay=0.35):
    os.write(fd, text.encode())
    time.sleep(delay)
    return drain()

drain(0.4)
send("source " + env["SHAC_ZSH"] + "\n", 0.5)
send("shac reindex\n", 1.2)
send("echo pty-manual-check\n", 0.5)
tab_output = send("pyt\t", 1.1)
sys.stdout.buffer.write(tab_output)
send("\n\n", 1.1)
send("\x1b[200~echo pty-exact-paste\x1b[201~\n", 0.8)
send("exit\n", 0.5)

deadline = time.time() + 3.0
while time.time() < deadline:
    try:
        done, _ = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        sys.exit(0)
    if done:
        sys.exit(0)
    drain(0.1)

try:
    os.close(fd)
except OSError:
    pass
sys.exit(1)
"#;

    let mut python = Command::new("python3");
    env.apply_env(&mut python);
    python
        .arg("-c")
        .arg(script)
        .env("SHAC_ZSH", env.zsh_script_path())
        .env("ZSH_PATH", zsh_path)
        .env("TERM", "xterm-256color");
    let output = python.output().expect("run python pty smoke");
    assert!(
        output.status.success(),
        "zsh PTY smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pty_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !pty_stdout.contains("\\n>") && !pty_stdout.contains("\\n  "),
        "zsh menu rendered literal newline escapes instead of real newlines:\n{pty_stdout}"
    );
    assert!(
        pty_stdout.contains("> python3"),
        "zsh menu did not render selected python3 candidate:\n{pty_stdout}"
    );

    let recent: Value =
        serde_json::from_str(&support::run_ok(&env, ["recent-events", "--limit", "20"]))
            .expect("recent events json");
    let events = recent.as_array().expect("recent events array");

    assert!(
        events.iter().any(|event| {
            event["command"].as_str() == Some("echo pty-manual-check")
                && event["provenance"].as_str() == Some("typed_manual")
        }),
        "manual typed event missing from recent-events: {recent}"
    );
    assert!(
        events.iter().any(|event| {
            event["command"].as_str() == Some("python3")
                && event["provenance"].as_str() == Some("accepted_completion")
        }),
        "accepted completion event missing from recent-events: {recent}"
    );
    assert!(
        events.iter().any(|event| {
            event["command"].as_str() == Some("echo pty-exact-paste")
                && event["provenance"].as_str() == Some("pasted")
                && event["provenance_source"].as_str() == Some("zsh_bracketed_paste")
                && event["provenance_confidence"].as_str() == Some("exact")
        }),
        "exact paste event missing from recent-events: {recent}"
    );
}

#[test]
fn zsh_pty_cancel_menu_does_not_record_accept() {
    if should_skip_pty_on_ci_linux() {
        eprintln!("skipping zsh PTY smoke on Linux CI");
        return;
    }
    if !support::command_available("zsh") || !support::command_available("python3") {
        eprintln!("skipping zsh PTY smoke: zsh or python3 is unavailable");
        return;
    }

    let env = support::TestEnv::new("zsh-pty-cancel");
    support::run_ok(&env, ["config", "set", "daemon_timeout_ms", "750"]);
    support::run_ok(&env, ["install", "--shell", "zsh"]);
    support::assert_path_exists(&env.zsh_script_path());
    let _daemon = env.spawn_daemon();
    let zsh_path = deterministic_zsh_path(&env);

    let script = r#"
import os
import pty
import select
import sys
import time

env = os.environ.copy()
env["PATH"] = env["ZSH_PATH"]
pid, fd = pty.fork()
if pid == 0:
    os.execvpe("zsh", ["zsh", "-f"], env)

def drain(timeout=0.15):
    end = time.time() + timeout
    out = b""
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.03)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
    return out

def send(text, delay=0.35):
    os.write(fd, text.encode())
    time.sleep(delay)
    return drain()

drain(0.4)
send("source " + env["SHAC_ZSH"] + "\n", 0.5)
send("shac reindex\n", 1.2)
tab_output = send("pyt\t", 1.1)
sys.stdout.buffer.write(tab_output)
send("\x07", 0.4)
send("\x15", 0.2)
send("echo pty-after-cancel\n", 0.6)
send("exit\n", 0.5)

deadline = time.time() + 3.0
while time.time() < deadline:
    try:
        done, _ = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        sys.exit(0)
    if done:
        sys.exit(0)
    drain(0.1)

try:
    os.close(fd)
except OSError:
    pass
sys.exit(1)
"#;

    let mut python = Command::new("python3");
    env.apply_env(&mut python);
    python
        .arg("-c")
        .arg(script)
        .env("SHAC_ZSH", env.zsh_script_path())
        .env("ZSH_PATH", zsh_path)
        .env("TERM", "xterm-256color");
    let output = python.output().expect("run python pty smoke");
    assert!(
        output.status.success(),
        "zsh PTY cancel smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pty_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        pty_stdout.contains("> python3"),
        "zsh menu did not render before cancel:\n{pty_stdout}"
    );

    let recent: Value =
        serde_json::from_str(&support::run_ok(&env, ["recent-events", "--limit", "20"]))
            .expect("recent events json");
    let events = recent.as_array().expect("recent events array");

    assert!(
        events.iter().any(|event| {
            event["command"].as_str() == Some("echo pty-after-cancel")
                && event["provenance"].as_str() == Some("typed_manual")
        }),
        "manual command after cancel missing from recent-events: {recent}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event["provenance"].as_str() == Some("accepted_completion")),
        "cancelled menu should not record accepted completion: {recent}"
    );
}

#[test]
fn zsh_pty_completes_python_option_position() {
    if should_skip_pty_on_ci_linux() {
        eprintln!("skipping zsh PTY smoke on Linux CI");
        return;
    }
    if !support::command_available("zsh") || !support::command_available("python3") {
        eprintln!("skipping zsh PTY smoke: zsh or python3 is unavailable");
        return;
    }

    let env = support::TestEnv::new("zsh-pty-python-option");
    support::run_ok(&env, ["config", "set", "daemon_timeout_ms", "750"]);
    support::run_ok(&env, ["install", "--shell", "zsh"]);
    support::assert_path_exists(&env.zsh_script_path());
    let _daemon = env.spawn_daemon();
    let zsh_path = deterministic_zsh_path(&env);

    let script = r#"
import os
import pty
import select
import sys
import time

env = os.environ.copy()
env["PATH"] = env["ZSH_PATH"]
pid, fd = pty.fork()
if pid == 0:
    os.execvpe("zsh", ["zsh", "-f"], env)

def drain(timeout=0.15):
    end = time.time() + timeout
    out = b""
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], 0.03)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        out += chunk
    return out

def send(text, delay=0.35):
    os.write(fd, text.encode())
    time.sleep(delay)
    return drain()

drain(0.4)
send("source " + env["SHAC_ZSH"] + "\n", 0.5)
send("shac reindex\n", 1.2)
tab_output = send("python3 -\t", 1.1)
sys.stdout.buffer.write(tab_output)
send("\n\n", 1.1)
send("exit\n", 0.5)

deadline = time.time() + 3.0
while time.time() < deadline:
    try:
        done, _ = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        sys.exit(0)
    if done:
        sys.exit(0)
    drain(0.1)

try:
    os.close(fd)
except OSError:
    pass
sys.exit(1)
"#;

    let mut python = Command::new("python3");
    env.apply_env(&mut python);
    python
        .arg("-c")
        .arg(script)
        .env("SHAC_ZSH", env.zsh_script_path())
        .env("ZSH_PATH", zsh_path)
        .env("TERM", "xterm-256color");
    let output = python.output().expect("run python pty smoke");
    assert!(
        output.status.success(),
        "zsh PTY python option smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pty_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        pty_stdout.contains("> -"),
        "zsh menu did not render selected python option candidate:\n{pty_stdout}"
    );

    let recent: Value =
        serde_json::from_str(&support::run_ok(&env, ["recent-events", "--limit", "20"]))
            .expect("recent events json");
    let events = recent.as_array().expect("recent events array");

    assert!(
        events.iter().any(|event| {
            event["command"]
                .as_str()
                .is_some_and(|command| command.starts_with("python3 -"))
                && event["provenance"].as_str() == Some("accepted_completion")
        }),
        "accepted python option completion missing from recent-events: {recent}"
    );
}

#[test]
fn zsh_inline_ghost_text_show_and_accept() {
    if !support::command_available("zsh") {
        eprintln!("skipping zsh inline ghost-text test: zsh is unavailable");
        return;
    }

    let script_path = std::env::current_dir()
        .expect("current dir")
        .join("shell")
        .join("zsh")
        .join("shac.zsh");

    // Unit-style harness: deterministically exercise the ghost-text state
    // machine without needing a live daemon. We bypass `_shac_fetch_inline`
    // (which shells out to `shac complete`) and drive the renderer + accept
    // widget by setting state directly.
    let body = format!(
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
function assert_starts_with() {{
  local haystack="$1"
  local prefix="$2"
  local message="$3"
  if [[ "$haystack" != "$prefix"* ]]; then
    print -ru2 -- "assertion failed: $message"
    print -ru2 -- "missing prefix: <$prefix>"
    print -ru2 -- "haystack: <$haystack>"
    exit 1
  fi
}}
function assert_nonempty() {{
  local value="$1"
  local message="$2"
  if [[ -z "$value" ]]; then
    print -ru2 -- "assertion failed: $message (value was empty)"
    exit 1
  fi
}}
source "{script_path}"

# Enable the inline ghost-text feature flag the adapter reads.
_shac_ui_inline_zsh=1

# 1. _shac_show_inline writes the dim ANSI prefix and ends with reset.
POSTDISPLAY=""
_shac_show_inline "ckout"
assert_nonempty "$POSTDISPLAY" "show_inline must populate POSTDISPLAY"
assert_starts_with "$POSTDISPLAY" $'\e[2m' "POSTDISPLAY must start with the dim ANSI escape"
case "$POSTDISPLAY" in
  *$'\e[0m') ;;
  *)
    print -ru2 -- "POSTDISPLAY must end with the ANSI reset escape"
    print -ru2 -- "actual: <$POSTDISPLAY>"
    exit 1
    ;;
esac

# 2. With inline state primed and CURSOR at end-of-buffer, ^F (forward-char
#    widget) accepts the ghost suffix by appending it to BUFFER.
BUFFER="git che"
CURSOR=${{#BUFFER}}
_shac_inline_active=1
_shac_inline_suffix="ckout"
_shac_inline_item_key="git checkout"
_shac_inline_request_id="req-42"

before_len=${{#BUFFER}}
_shac_forward_char_widget
assert_eq "$BUFFER" "git checkout" "forward-char widget appends ghost suffix"
assert_eq "$CURSOR" "${{#BUFFER}}" "cursor moves to new end of buffer"
(( ${{#BUFFER}} > before_len )) || {{
  print -ru2 -- "BUFFER did not grow after accept: <$BUFFER>"
  exit 1
}}
assert_eq "$_shac_inline_active" "0" "inline state cleared after accept"
assert_eq "$_shac_last_accepted_item_key" "git checkout" "accepted item key recorded"
assert_eq "$_shac_input_provenance" "accepted_completion" "provenance set to accepted_completion"

# 3. _shac_clear_inline wipes state and POSTDISPLAY when the menu is closed.
_shac_inline_active=1
_shac_inline_suffix="something"
POSTDISPLAY=$'\e[2m\e[38;5;240msomething\e[0m'
_shac_menu_open=0
_shac_clear_inline
assert_eq "$_shac_inline_active" "0" "clear_inline resets active flag"
assert_eq "$_shac_inline_suffix" "" "clear_inline empties suffix"
assert_eq "$POSTDISPLAY" "" "clear_inline empties POSTDISPLAY when menu closed"
"#,
        script_path = script_path.display()
    );

    let output = Command::new("zsh")
        .arg("-f")
        .arg("-c")
        .arg(&body)
        .env("SHAC_ZSH_TEST_MODE", "1")
        .output()
        .expect("run zsh inline ghost-text test");
    assert!(
        output.status.success(),
        "zsh inline ghost-text test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn should_skip_pty_on_ci_linux() -> bool {
    std::env::var_os("CI").is_some() && cfg!(target_os = "linux")
}

fn deterministic_zsh_path(env: &support::TestEnv) -> String {
    let fake_bin = env.root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    write_executable(&fake_bin.join("python3"), "#!/bin/sh\nexit 0\n");
    write_executable(&fake_bin.join("python3-config"), "#!/bin/sh\nexit 0\n");
    format!(
        "{}:{}:/usr/bin:/bin",
        fake_bin.display(),
        env.bin_dir.display()
    )
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable bit");
}
