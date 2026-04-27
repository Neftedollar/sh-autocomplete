//! Integration tests for `Engine::collect_npm_script_candidates` —
//! the `ArgType::Script` collector that hooks `npm run` / `pnpm run` /
//! `yarn run` against the cwd's nearest `package.json`. PLAN §7.6.

mod support;

use std::fs;
use std::path::Path;

/// Write a `package.json` at `dir/package.json` with the given scripts
/// payload (must be a JSON object literal serialised as a string).
fn write_pkg_json(dir: &Path, scripts_obj: &str) {
    fs::create_dir_all(dir).expect("mkdir pkg dir");
    let body = format!(r#"{{"name":"shac-test","version":"1.0.0","scripts":{scripts_obj}}}"#);
    fs::write(dir.join("package.json"), body).expect("write package.json");
}

/// Parse the candidates emitted by a `complete` invocation, returning
/// `(insert_text, kind, source)` triples for every non-meta line. Filters
/// out the `__shac_request_id` header and lines that don't have the
/// expected shell-tsv-v2 column count.
fn parse_candidates(output: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 5 {
            continue;
        }
        out.push((
            fields[1].to_string(),
            fields[3].to_string(),
            fields[4].to_string(),
        ));
    }
    out
}

#[test]
fn npm_run_lists_scripts_from_package_json() {
    let env = support::TestEnv::new("npm-run-lists");
    let project = env.root.join("project");
    write_pkg_json(
        &project,
        r#"{"dev":"vite","build":"vite build","test":"vitest"}"#,
    );

    let _daemon = env.spawn_daemon();

    let line = "npm run ";
    let cursor = line.len().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            project.to_str().expect("utf8"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "npm_script" && source == "npm_script")
        .map(|(name, _, _)| name)
        .collect();

    assert!(
        names.iter().any(|n| n == "dev"),
        "expected `dev` script: {names:?}\n{output}"
    );
    assert!(
        names.iter().any(|n| n == "build"),
        "expected `build` script: {names:?}\n{output}"
    );
    assert!(
        names.iter().any(|n| n == "test"),
        "expected `test` script: {names:?}\n{output}"
    );
}

#[test]
fn npm_run_filters_by_prefix() {
    let env = support::TestEnv::new("npm-run-prefix");
    let project = env.root.join("project");
    write_pkg_json(
        &project,
        r#"{"dev":"vite","build":"vite build","test":"vitest"}"#,
    );

    let _daemon = env.spawn_daemon();

    let line = "npm run de";
    let cursor = line.len().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            project.to_str().expect("utf8"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "npm_script" && source == "npm_script")
        .map(|(name, _, _)| name)
        .collect();

    assert!(
        names.iter().any(|n| n == "dev"),
        "expected `dev` to match prefix `de`: {names:?}\n{output}"
    );
    assert!(
        !names.iter().any(|n| n == "build"),
        "expected `build` to be filtered out by prefix `de`: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "test"),
        "expected `test` to be filtered out by prefix `de`: {names:?}"
    );
}

#[test]
fn npm_run_with_no_package_json_silently_returns_empty() {
    let env = support::TestEnv::new("npm-run-no-pkg");
    let cwd = env.root.join("plain");
    fs::create_dir_all(&cwd).expect("mkdir cwd");

    let _daemon = env.spawn_daemon();

    let line = "npm run ";
    let cursor = line.len().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            cwd.to_str().expect("utf8"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let saw_npm_script = parse_candidates(&output)
        .into_iter()
        .any(|(_, _, source)| source == "npm_script");
    assert!(
        !saw_npm_script,
        "expected no npm_script candidates without package.json:\n{output}"
    );
}

#[test]
fn npm_run_walks_up_to_project_root() {
    let env = support::TestEnv::new("npm-run-walk-up");
    let project = env.root.join("project");
    write_pkg_json(&project, r#"{"dev":"vite","lint":"eslint ."}"#);
    let nested = project.join("src/components");
    fs::create_dir_all(&nested).expect("mkdir nested");

    let _daemon = env.spawn_daemon();

    let line = "npm run ";
    let cursor = line.len().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            nested.to_str().expect("utf8"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "npm_script" && source == "npm_script")
        .map(|(name, _, _)| name)
        .collect();

    assert!(
        names.iter().any(|n| n == "dev"),
        "walk-up should surface scripts from project root: {names:?}\n{output}"
    );
    assert!(
        names.iter().any(|n| n == "lint"),
        "walk-up should surface scripts from project root: {names:?}\n{output}"
    );
}

#[test]
fn pnpm_and_yarn_run_use_same_collector() {
    let env = support::TestEnv::new("pnpm-yarn-run");
    let project = env.root.join("project");
    write_pkg_json(&project, r#"{"dev":"vite","build":"vite build"}"#);

    let _daemon = env.spawn_daemon();

    for cmd in ["pnpm run ", "yarn run "] {
        let cursor = cmd.len().to_string();
        let output = support::run_ok(
            &env,
            [
                "complete",
                "--shell",
                "zsh",
                "--line",
                cmd,
                "--cursor",
                &cursor,
                "--cwd",
                project.to_str().expect("utf8"),
                "--format",
                "shell-tsv-v2",
            ],
        );
        let names: Vec<String> = parse_candidates(&output)
            .into_iter()
            .filter(|(_, kind, source)| kind == "npm_script" && source == "npm_script")
            .map(|(name, _, _)| name)
            .collect();

        assert!(
            names.iter().any(|n| n == "dev"),
            "{cmd:?} should surface `dev` script: {names:?}\n{output}"
        );
        assert!(
            names.iter().any(|n| n == "build"),
            "{cmd:?} should surface `build` script: {names:?}\n{output}"
        );
    }
}

#[test]
fn npm_run_handles_malformed_package_json() {
    let env = support::TestEnv::new("npm-run-malformed");
    let project = env.root.join("project");
    fs::create_dir_all(&project).expect("mkdir project");
    fs::write(project.join("package.json"), "{ this is not json").expect("seed bad pkg");

    let _daemon = env.spawn_daemon();

    let line = "npm run ";
    let cursor = line.len().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            project.to_str().expect("utf8"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let saw_npm_script = parse_candidates(&output)
        .into_iter()
        .any(|(_, _, source)| source == "npm_script");
    assert!(
        !saw_npm_script,
        "malformed JSON must not produce npm_script candidates:\n{output}"
    );
}
