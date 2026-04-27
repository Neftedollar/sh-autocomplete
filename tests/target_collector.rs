//! End-to-end integration tests for the `ArgType::Target` collector
//! (`make`, `just`, `task` target completion).
//!
//! Each test spins up a real [`shac::Engine`] backed by a temporary SQLite
//! database and a temporary working directory, then invokes
//! [`Engine::complete`] with a synthetic completion request. The returned
//! items are inspected for the expected `kind=build_target` candidates.

use std::path::PathBuf;

use shac::config::AppPaths;
use shac::engine::Engine;
use shac::protocol::{CompletionRequest, HistoryHint, SessionInfo};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("shac-target-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn make_engine(dir: &TestDir) -> Engine {
    let paths = AppPaths {
        config_file: dir.path.join("config.toml"),
        db_file: dir.path.join("shac.db"),
        socket_file: dir.path.join("shacd.sock"),
        pid_file: dir.path.join("shacd.pid"),
        shell_dir: dir.path.join("shell"),
        config_dir: dir.path.clone(),
        data_dir: dir.path.clone(),
        state_dir: dir.path.clone(),
    };
    Engine::new(&paths).expect("engine")
}

fn make_request(line: &str, cwd: &str) -> CompletionRequest {
    CompletionRequest {
        shell: "zsh".to_string(),
        line: line.to_string(),
        cursor: line.len(),
        cwd: cwd.to_string(),
        env: std::collections::HashMap::new(),
        session: SessionInfo {
            tty: Some("test".to_string()),
            pid: None,
        },
        history_hint: HistoryHint {
            prev_command: None,
            runtime_commands: Vec::new(),
        },
    }
}

// ---------------------------------------------------------------------------
// Test 1: make completes Makefile targets
// ---------------------------------------------------------------------------

#[test]
fn make_completion_lists_makefile_targets() {
    let dir = TestDir::new("make-basic");
    std::fs::write(
        dir.path.join("Makefile"),
        "build:\n\t@echo build\n\ntest:\n\t@echo test\n\nclean:\n\t@echo clean\n",
    )
    .expect("write Makefile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete");

    let build_targets: Vec<_> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .collect();

    let names: Vec<&str> = build_targets.iter().map(|i| i.display.as_str()).collect();
    assert!(names.contains(&"build"), "missing build: {names:?}");
    assert!(names.contains(&"test"), "missing test: {names:?}");
    assert!(names.contains(&"clean"), "missing clean: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 2: .PHONY targets are filtered out
// ---------------------------------------------------------------------------

#[test]
fn make_filters_targets_starting_with_dot() {
    let dir = TestDir::new("make-dot");
    std::fs::write(
        dir.path.join("Makefile"),
        ".PHONY: build\nbuild:\n\t@echo build\n",
    )
    .expect("write Makefile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete");

    let build_targets: Vec<_> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .collect();

    let names: Vec<&str> = build_targets.iter().map(|i| i.display.as_str()).collect();
    assert!(
        !names.iter().any(|n| n.starts_with('.')),
        ".PHONY must not appear: {names:?}"
    );
    assert!(names.contains(&"build"), "build must appear: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 3: pattern rules (%.o: %.c) are filtered
// ---------------------------------------------------------------------------

#[test]
fn make_skips_pattern_rules() {
    let dir = TestDir::new("make-pattern");
    std::fs::write(
        dir.path.join("Makefile"),
        "%.o: %.c\n\t$(CC) -c $<\nall:\n\t@echo all\n",
    )
    .expect("write Makefile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(
        !names.iter().any(|n| n.contains('%')),
        "pattern rule must not appear: {names:?}"
    );
    assert!(names.contains(&"all"), "all must appear: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 4: walk-up finds Makefile in project root when cwd is nested
// ---------------------------------------------------------------------------

#[test]
fn make_walks_up_to_project_root() {
    let dir = TestDir::new("make-walkup");
    // Makefile at project root.
    std::fs::write(
        dir.path.join("Makefile"),
        "build:\n\t@echo\ntest:\n\t@echo\n",
    )
    .expect("write Makefile");
    // cwd is a nested subdirectory.
    let nested = dir.path.join("src").join("components");
    std::fs::create_dir_all(&nested).expect("create nested");

    let engine = make_engine(&dir);
    let cwd = nested.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(names.contains(&"build"), "build must appear from walk-up: {names:?}");
    assert!(names.contains(&"test"), "test must appear from walk-up: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 5: variable assignments must not surface as targets
// ---------------------------------------------------------------------------

#[test]
fn make_skips_variable_assignments() {
    let dir = TestDir::new("make-vars");
    std::fs::write(
        dir.path.join("Makefile"),
        "CC := gcc\nCFLAGS := -O2\nbuild:\n\t$(CC) main.c\n",
    )
    .expect("write Makefile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(
        !names.iter().any(|&n| n == "CC" || n == "CFLAGS"),
        "variable assignments must not appear: {names:?}"
    );
    assert!(names.contains(&"build"), "build must appear: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 6: just completes justfile recipes
// ---------------------------------------------------------------------------

#[test]
fn just_completion_lists_recipes() {
    let dir = TestDir::new("just-basic");
    std::fs::write(
        dir.path.join("justfile"),
        "build:\n\tcargo build\n\ntest arg1 arg2:\n\tcargo test\n\ndev:\n\tcargo run\n",
    )
    .expect("write justfile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("just ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(names.contains(&"build"), "build must appear: {names:?}");
    assert!(names.contains(&"test"), "test must appear: {names:?}");
    assert!(names.contains(&"dev"), "dev must appear: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 7: just handles parameterized recipes
// ---------------------------------------------------------------------------

#[test]
fn just_handles_parameterized_recipe() {
    let dir = TestDir::new("just-params");
    std::fs::write(
        dir.path.join("justfile"),
        "recipe-name param1='default':\n\techo {{param1}}\n",
    )
    .expect("write justfile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("just ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(
        names.contains(&"recipe-name"),
        "parameterized recipe must appear: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 8: task completes Taskfile.yml tasks
// ---------------------------------------------------------------------------

#[test]
fn task_completion_lists_taskfile_targets() {
    let dir = TestDir::new("task-basic");
    let content = concat!(
        "version: '3'\n",
        "\n",
        "tasks:\n",
        "\n",
        "  build:\n",
        "    cmds:\n",
        "      - go build ./...\n",
        "\n",
        "  test:\n",
        "    cmds:\n",
        "      - go test ./...\n",
        "\n",
        "  clean:\n",
        "    cmds:\n",
        "      - rm -rf dist\n",
    );
    std::fs::write(dir.path.join("Taskfile.yml"), content).expect("write Taskfile.yml");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("task ", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(names.contains(&"build"), "build must appear: {names:?}");
    assert!(names.contains(&"test"), "test must appear: {names:?}");
    assert!(names.contains(&"clean"), "clean must appear: {names:?}");
}

// ---------------------------------------------------------------------------
// Test 9: no build file → no candidates, no error
// ---------------------------------------------------------------------------

#[test]
fn target_with_no_build_file_returns_empty() {
    let dir = TestDir::new("target-no-file");
    // No Makefile, no justfile, no Taskfile — just an empty directory.
    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("complete must not error when no build file found");

    let build_targets: Vec<_> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .collect();
    assert!(
        build_targets.is_empty(),
        "expected no build_target candidates: {build_targets:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 10: malformed build files do not panic; return empty
// ---------------------------------------------------------------------------

#[test]
fn target_handles_malformed_files() {
    let dir = TestDir::new("target-malformed");

    // Malformed Makefile
    std::fs::write(dir.path.join("Makefile"), "this is \0 not a valid \t makefile {{{}}}").expect("write");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    // Must not panic, must return Ok.
    let resp = engine
        .complete(make_request("make ", &cwd))
        .expect("no error on malformed file");

    // May or may not yield targets — the requirement is no panic.
    let _ = resp.items;
}

// ---------------------------------------------------------------------------
// Test 11: active-token prefix filter
// ---------------------------------------------------------------------------

#[test]
fn target_collector_active_prefix_filter() {
    let dir = TestDir::new("target-prefix");
    std::fs::write(
        dir.path.join("Makefile"),
        "build:\n\t@echo\nclean:\n\t@echo\nbenchmark:\n\t@echo\n",
    )
    .expect("write Makefile");

    let engine = make_engine(&dir);
    let cwd = dir.path.to_string_lossy().to_string();
    // Type "make bu<Tab>" — only "build" and "benchmark" should surface, not "clean".
    let resp = engine
        .complete(make_request("make bu", &cwd))
        .expect("complete");

    let names: Vec<&str> = resp
        .items
        .iter()
        .filter(|i| i.kind == "build_target")
        .map(|i| i.display.as_str())
        .collect();
    assert!(names.contains(&"build"), "build must match prefix 'bu': {names:?}");
    assert!(
        !names.contains(&"clean"),
        "clean must not match prefix 'bu': {names:?}"
    );
}
