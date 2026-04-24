mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::Value;

#[test]
fn index_cli_adds_commands_paths_and_compact_help_docs() {
    let env = support::TestEnv::new("index-cli");
    let fake_bin = env.root.join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    write_executable(
        &fake_bin.join("fzf"),
        r#"#!/bin/sh
if [ "$1" = "--help" ]; then
  printf '%s\n' '  --preview  Preview command for the current line'
  printf '%s\n' '  --bind     Custom key bindings'
fi
"#,
    );
    write_executable(&fake_bin.join("rg"), "#!/bin/sh\nexit 0\n");

    let mut add_command = env.shac_cmd();
    add_command
        .args(["index", "add-command", "fzf"])
        .env("PATH", env.path_with_prefix(&fake_bin));
    let output = add_command.output().expect("run add-command");
    assert!(
        output.status.success(),
        "add-command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let add_command_json: Value =
        serde_json::from_slice(&output.stdout).expect("add-command json output");
    assert_eq!(add_command_json["indexed_commands"].as_i64(), Some(1));

    let add_path = support::run_ok(
        &env,
        [
            "index",
            "add-path",
            fake_bin.to_string_lossy().as_ref(),
            "--subpath",
            "--deep",
            "1",
        ],
    );
    let add_path_json: Value = serde_json::from_str(&add_path).expect("add-path json output");
    assert!(
        add_path_json["indexed_commands"]
            .as_i64()
            .unwrap_or_default()
            >= 2,
        "expected add-path to index fake executables: {add_path_json}"
    );

    let status: Value =
        serde_json::from_str(&support::run_ok(&env, ["index", "status"])).expect("status json");
    let targets = status.as_array().expect("status array");
    assert!(targets.iter().any(|target| {
        target["type"].as_str() == Some("command") && target["value"].as_str() == Some("fzf")
    }));
    assert!(targets.iter().any(|target| {
        target["type"].as_str() == Some("path")
            && target["value"]
                .as_str()
                .is_some_and(|value| value.ends_with("/fake-bin"))
    }));

    let _daemon = env.spawn_daemon();
    let completion: Value = serde_json::from_str(&support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "fzf --",
            "--cursor",
            "6",
            "--cwd",
            env.root.to_string_lossy().as_ref(),
            "--format",
            "json",
        ],
    ))
    .expect("completion json");
    let items = completion["items"].as_array().expect("completion items");
    assert!(
        items.iter().any(|item| item["display"].as_str() == Some("--preview")),
        "expected compact --help docs to provide --preview without storing full help text: {completion}"
    );
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).expect("write executable");
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set executable bit");
}
