mod support;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use shac::engine::Engine;
use shac::protocol::{
    CompletionRequest, HistoryHint, RecordCommandRequest, SessionInfo, PROVENANCE_TYPED_MANUAL,
    TRUST_INTERACTIVE,
};

#[test]
fn ranking_regressions_cover_history_transitions_projects_and_runtime_hints() {
    let env = support::TestEnv::new("ranking");
    let paths = env.app_paths();
    let fake_bin = env.root.join("bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    for command in ["git", "cargo", "npm", "node", "pnpm", "nmap"] {
        write_fake_executable(&fake_bin.join(command));
    }

    let rust_project = env.root.join("rust-project");
    fs::create_dir_all(&rust_project).expect("create rust project");
    fs::write(
        rust_project.join("Cargo.toml"),
        "[package]\nname='fixture'\n",
    )
    .expect("write Cargo.toml");

    let node_project = env.root.join("node-project");
    fs::create_dir_all(&node_project).expect("create node project");
    fs::write(node_project.join("package.json"), "{}\n").expect("write package.json");

    let engine = Engine::new(&paths).expect("engine");
    engine
        .reindex(Some(fake_bin.to_string_lossy().as_ref()), false, false)
        .expect("reindex fake commands");

    engine
        .record_command(record("git status", &rust_project))
        .expect("record git status");
    engine
        .record_command(record("git checkout main", &rust_project))
        .expect("record git checkout");

    let git = engine
        .complete(request("git ch", &rust_project, Some("git status"), []))
        .expect("git completion");
    assert_top_display(
        &git.items,
        "checkout",
        "git transition should prefer checkout",
    );

    let cargo = engine
        .complete(request("cargo t", &rust_project, None, []))
        .expect("cargo completion");
    assert_top_display(
        &cargo.items,
        "test",
        "Rust project should prefer cargo test",
    );

    let npm = engine
        .complete(request("n", &node_project, None, []))
        .expect("node project completion");
    let top = npm.items.first().expect("npm completion item");
    assert!(
        matches!(top.display.as_str(), "node" | "npm"),
        "package.json should lift node/npm above unrelated n-prefixed tools, got: {:#?}",
        npm.items
    );

    let runtime = engine
        .complete(request(
            "npm run d",
            &node_project,
            None,
            ["npm run deploy"],
        ))
        .expect("runtime history completion");
    let deploy = runtime
        .items
        .iter()
        .find(|item| item.display == "deploy")
        .expect("runtime history candidate");
    assert_eq!(deploy.source, "runtime_history");

    let stats = engine.stats().expect("stats");
    assert_eq!(
        stats.history_events, 2,
        "runtime history hints must not be persisted as history_events"
    );
}

fn request<const N: usize>(
    line: &str,
    cwd: &Path,
    prev_command: Option<&str>,
    runtime_commands: [&str; N],
) -> CompletionRequest {
    CompletionRequest {
        shell: "zsh".to_string(),
        line: line.to_string(),
        cursor: line.len(),
        cwd: cwd.to_string_lossy().to_string(),
        env: HashMap::new(),
        session: SessionInfo {
            tty: Some("test-tty".to_string()),
            pid: Some(std::process::id()),
        },
        history_hint: HistoryHint {
            prev_command: prev_command.map(ToOwned::to_owned),
            runtime_commands: runtime_commands
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
        },
    }
}

fn record(command: &str, cwd: &Path) -> RecordCommandRequest {
    RecordCommandRequest {
        command: command.to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        shell: Some("zsh".to_string()),
        trust: Some(TRUST_INTERACTIVE.to_string()),
        provenance: Some(PROVENANCE_TYPED_MANUAL.to_string()),
        provenance_source: None,
        provenance_confidence: None,
        origin: Some("ranking_fixture".to_string()),
        tty_present: Some(true),
        exit_status: None,
        accepted_request_id: None,
        accepted_item_key: None,
        accepted_rank: None,
    }
}

fn write_fake_executable(path: &PathBuf) {
    fs::write(path, "#!/bin/sh\nexit 0\n").expect("write fake executable");
    let mut perms = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("set fake executable permissions");
}

fn assert_top_display(items: &[shac::protocol::CompletionItem], expected: &str, message: &str) {
    let top = items.first().expect("at least one completion item");
    assert_eq!(
        top.display, expected,
        "{message}; ranked items: {:#?}",
        items
    );
}
