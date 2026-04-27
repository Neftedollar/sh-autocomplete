//! Integration tests for `Engine::collect_kubectl_resource_candidates` —
//! the `ArgType::Resource` collector that hooks `kubectl get|describe|delete`
//! against `kubectl api-resources` output. PLAN §7.7.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Parse TSV candidates from `shac complete --format shell-tsv-v2` output.
/// Returns `(insert_text, kind, source)` triples, skipping the
/// `__shac_request_id` header and short lines.
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

/// Write a fake `kubectl` script to `bin_dir/kubectl` that exits with the
/// given exit code. Used to test the "no cluster" failure path.
fn write_fake_kubectl(bin_dir: &Path, exit_code: i32) {
    fs::create_dir_all(bin_dir).expect("create fake kubectl bin dir");
    let script = format!("#!/bin/sh\nexit {exit_code}\n");
    let path = bin_dir.join("kubectl");
    fs::write(&path, script).expect("write fake kubectl");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("chmod fake kubectl");
}

// ---------------------------------------------------------------------------
// Test 1: no kubectl on PATH → static fallback surfaces
// ---------------------------------------------------------------------------

#[test]
fn kubectl_get_lists_static_fallback_when_no_kubectl() {
    let env = support::TestEnv::new("kubectl-no-kubectl");

    let _daemon = env.spawn_daemon();

    // We deliberately do NOT add kubectl to PATH. The test PATH is controlled
    // by TestEnv::test_path() which prepends the Cargo target/debug dir.
    // We override PATH to exclude any real kubectl by using a minimal PATH that
    // contains only the shac binary dir (which has shac/shacd, not kubectl).
    // Run completion with PATH restricted to just our bin_dir.
    let bin_dir_str = env.bin_dir.to_str().expect("bin_dir utf8");
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", bin_dir_str);
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            "kubectl get ",
            "--cursor",
            "12",
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .map(|(name, _, _)| name)
        .collect();

    assert!(
        k8s_names.iter().any(|n| n == "pods"),
        "expected `pods` from static fallback: {k8s_names:?}\n{output}"
    );
    assert!(
        k8s_names.iter().any(|n| n == "services"),
        "expected `services` from static fallback: {k8s_names:?}\n{output}"
    );
    assert!(
        k8s_names.iter().any(|n| n == "deployments"),
        "expected `deployments` from static fallback: {k8s_names:?}\n{output}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: prefix filtering — "kubectl get po" should surface only "po*"
// ---------------------------------------------------------------------------

#[test]
fn kubectl_get_filters_by_active_prefix() {
    let env = support::TestEnv::new("kubectl-prefix");

    let _daemon = env.spawn_daemon();

    // Restrict PATH to exclude any real kubectl so we use the static fallback,
    // which is deterministic.
    let bin_dir_str = env.bin_dir.to_str().expect("bin_dir utf8");
    let line = "kubectl get po";
    let cursor = line.len().to_string();
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", bin_dir_str);
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .map(|(name, _, _)| name)
        .collect();

    // "po" and "pods" and "podtemplates" match the "po" prefix.
    assert!(
        k8s_names.iter().any(|n| n == "po" || n == "pods"),
        "expected `po` or `pods` to match prefix `po`: {k8s_names:?}\n{output}"
    );
    // "services" must not appear.
    assert!(
        !k8s_names.iter().any(|n| n == "services"),
        "expected `services` to be filtered out by prefix `po`: {k8s_names:?}"
    );
    // "deployments" must not appear.
    assert!(
        !k8s_names.iter().any(|n| n == "deployments"),
        "expected `deployments` to be filtered out by prefix `po`: {k8s_names:?}"
    );
    // All candidates must start with "po".
    for name in &k8s_names {
        assert!(
            name.starts_with("po"),
            "candidate `{name}` does not match prefix `po`"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: kubectl describe uses the same collector
// ---------------------------------------------------------------------------

#[test]
fn kubectl_describe_uses_same_collector() {
    let env = support::TestEnv::new("kubectl-describe");

    let _daemon = env.spawn_daemon();

    let bin_dir_str = env.bin_dir.to_str().expect("bin_dir utf8");
    let line = "kubectl describe ";
    let cursor = line.len().to_string();
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", bin_dir_str);
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_candidates: Vec<(String, String, String)> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .collect();

    assert!(
        !k8s_candidates.is_empty(),
        "kubectl describe should emit k8s_resource candidates:\n{output}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: kubectl delete uses the same collector
// ---------------------------------------------------------------------------

#[test]
fn kubectl_delete_uses_same_collector() {
    let env = support::TestEnv::new("kubectl-delete");

    let _daemon = env.spawn_daemon();

    let bin_dir_str = env.bin_dir.to_str().expect("bin_dir utf8");
    let line = "kubectl delete ";
    let cursor = line.len().to_string();
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", bin_dir_str);
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_candidates: Vec<(String, String, String)> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .collect();

    assert!(
        !k8s_candidates.is_empty(),
        "kubectl delete should emit k8s_resource candidates:\n{output}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: kubectl exists but returns non-zero (no cluster) → fallback surfaces
// ---------------------------------------------------------------------------

#[test]
fn kubectl_get_handles_no_cluster() {
    let env = support::TestEnv::new("kubectl-no-cluster");

    let _daemon = env.spawn_daemon();

    // Stage a fake kubectl that exits with code 1 (simulates "no cluster").
    let fake_bin = env.root.join("fake-bin");
    write_fake_kubectl(&fake_bin, 1);

    // Build a PATH that puts our fake-bin first, then the real bin_dir.
    let path = env.path_with_prefix(&fake_bin);

    let line = "kubectl get ";
    let cursor = line.len().to_string();
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.env("PATH", &path);
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        // Must succeed — errors from kubectl are never surfaced to the user.
        assert!(
            result.status.success(),
            "shac complete must succeed even when kubectl fails:\nstatus: {}\nstderr:\n{}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .map(|(name, _, _)| name)
        .collect();

    assert!(
        k8s_names.iter().any(|n| n == "pods"),
        "static fallback must surface `pods` when kubectl fails: {k8s_names:?}\n{output}"
    );
    assert!(
        k8s_names.iter().any(|n| n == "services"),
        "static fallback must surface `services` when kubectl fails: {k8s_names:?}\n{output}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: kubectl get returns top core resources (daemon path)
// ---------------------------------------------------------------------------
//
// The daemon truncates results to max_results (default 12). All k8s_resource
// candidates share the same position_score=1.0, so the top 12 come from the
// beginning of the merged fallback list: pods, po, services, svc, deployments,
// deploy, replicasets, rs, statefulsets, sts, daemonsets, ds.
// Only assert on resources that realistically land in that top-12 window.
// For a comprehensive fallback-list check, see `kubectl_static_fallback_nonempty`
// in `src/engine.rs#mod tests`.
#[test]
fn kubectl_get_returns_core_resources() {
    let env = support::TestEnv::new("kubectl-static-core");

    let _daemon = env.spawn_daemon();

    let line = "kubectl get ";
    let cursor = line.len().to_string();
    let output = {
        let mut cmd = env.shac_cmd();
        cmd.args([
            "complete",
            "--shell",
            "zsh",
            "--line",
            line,
            "--cursor",
            &cursor,
            "--cwd",
            "/tmp",
            "--format",
            "shell-tsv-v2",
        ]);
        let result = cmd.output().expect("run shac complete");
        String::from_utf8_lossy(&result.stdout).to_string()
    };

    let k8s_names: Vec<String> = parse_candidates(&output)
        .into_iter()
        .filter(|(_, kind, source)| kind == "k8s_resource" && source == "k8s_resource")
        .map(|(name, _, _)| name)
        .collect();

    // These three are consistently in the top-12 regardless of cluster state.
    for expected in &["pods", "services", "deployments"] {
        assert!(
            k8s_names.iter().any(|n| n == *expected),
            "expected `{expected}` in kubectl get candidates: {k8s_names:?}"
        );
    }
}
