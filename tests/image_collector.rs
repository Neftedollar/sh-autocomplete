//! Integration tests for `Engine::collect_docker_image_candidates` —
//! the `ArgType::Image` collector that shells out to `docker images` for
//! `docker run|pull|push|rmi <Tab>` completion. PLAN §7.8.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse shell-tsv-v2 output, returning `(insert_text, kind, source)` triples.
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
            fields[1].to_string(), // insert_text
            fields[3].to_string(), // kind
            fields[4].to_string(), // source
        ));
    }
    out
}

/// Write a fake `docker` shell script into `bin_dir`. When `images_output` is
/// `Some(text)` the script prints that text to stdout and exits 0 when called
/// as `docker images …`. When `images_output` is `None` the script always
/// exits with `exit_code` (used to simulate "no docker / daemon down").
fn write_fake_docker(bin_dir: &Path, images_output: Option<&str>, exit_code: i32) {
    fs::create_dir_all(bin_dir).expect("create fake docker bin dir");
    let script = match images_output {
        Some(output) => {
            // Respond to "docker images …" with the controlled output; anything
            // else exits 1 so we don't accidentally satisfy other commands.
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"images\" ]; then\n  printf '%s\\n' {quoted}\n  exit 0\nfi\nexit 1\n",
                quoted = shell_single_quote(output)
            )
        }
        None => format!("#!/bin/sh\nexit {exit_code}\n"),
    };
    let path = bin_dir.join("docker");
    fs::write(&path, &script).expect("write fake docker");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod fake docker");
}

/// Wrap `s` in POSIX single quotes, escaping embedded `'` as `'\''`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A running shacd process that is killed on drop.
struct TestDaemon {
    child: Option<Child>,
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Spawn shacd with `extra_path` prepended to the PATH that the daemon sees.
/// The daemon is the process that actually shells out to `docker`, so its PATH
/// must contain the fake binary.
fn spawn_daemon_with_path(env: &support::TestEnv, extra_path: &Path) -> TestDaemon {
    let path_str = env.path_with_prefix(extra_path);
    let socket = env.state_home.join("shac").join("shacd.sock");

    let mut command = Command::new(&env.shacd);
    env.apply_env(&mut command);
    // Override PATH to include the fake bin dir first.
    command.env("PATH", &path_str);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn shacd with custom PATH");

    for _ in 0..50 {
        if socket.exists() {
            return TestDaemon { child: Some(child) };
        }
        thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("shacd did not create socket: {}", socket.display());
}

/// Run `shac complete --shell zsh --line LINE --cursor CURSOR --cwd /tmp` and
/// return parsed candidates. The PATH for the `shac` client is set to include
/// `extra_path` first (so the correct daemon socket can be found).
fn complete_line(
    env: &support::TestEnv,
    line: &str,
    extra_path: &Path,
) -> Vec<(String, String, String)> {
    let path_str = env.path_with_prefix(extra_path);
    let cursor = line.len().to_string();
    let mut cmd = env.shac_cmd();
    cmd.env("PATH", &path_str);
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
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    parse_candidates(&stdout)
}

fn docker_image_candidates(candidates: &[(String, String, String)]) -> Vec<&str> {
    candidates
        .iter()
        .filter(|(_, kind, source)| kind == "docker_image" && source == "docker_image")
        .map(|(name, _, _)| name.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// Test 1: fake docker exits non-zero → no docker_image candidates
// ---------------------------------------------------------------------------

#[test]
fn docker_run_returns_empty_when_no_docker() {
    let env = support::TestEnv::new("docker-no-docker");
    let fake_bin = env.root.join("fake-bin");
    write_fake_docker(&fake_bin, None, 1);

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker run ", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.is_empty(),
        "expected no docker_image candidates when docker exits non-zero: {images:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: fake docker outputs known images → both surface
// ---------------------------------------------------------------------------

#[test]
fn docker_run_lists_images_from_fake_docker() {
    let env = support::TestEnv::new("docker-lists-images");
    let fake_bin = env.root.join("fake-bin");
    write_fake_docker(&fake_bin, Some("nginx:latest\nredis:7-alpine"), 0);

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker run ", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.contains(&"nginx:latest"),
        "expected nginx:latest in candidates: {images:?}"
    );
    assert!(
        images.contains(&"redis:7-alpine"),
        "expected redis:7-alpine in candidates: {images:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: docker pull uses the same collector
// ---------------------------------------------------------------------------

#[test]
fn docker_pull_uses_same_collector() {
    let env = support::TestEnv::new("docker-pull");
    let fake_bin = env.root.join("fake-bin");
    write_fake_docker(&fake_bin, Some("alpine:3.18\nubuntu:22.04"), 0);

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker pull ", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.contains(&"alpine:3.18"),
        "expected alpine:3.18 in docker pull candidates: {images:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: docker push uses the same collector
// ---------------------------------------------------------------------------

#[test]
fn docker_push_uses_same_collector() {
    let env = support::TestEnv::new("docker-push");
    let fake_bin = env.root.join("fake-bin");
    write_fake_docker(&fake_bin, Some("myapp:v1.0\nmyapp:latest"), 0);

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker push ", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.contains(&"myapp:v1.0"),
        "expected myapp:v1.0 in docker push candidates: {images:?}"
    );
    assert!(
        images.contains(&"myapp:latest"),
        "expected myapp:latest in docker push candidates: {images:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: docker rmi uses the same collector
// ---------------------------------------------------------------------------

#[test]
fn docker_rmi_uses_same_collector() {
    let env = support::TestEnv::new("docker-rmi");
    let fake_bin = env.root.join("fake-bin");
    write_fake_docker(&fake_bin, Some("old-image:v0.1"), 0);

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker rmi ", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.contains(&"old-image:v0.1"),
        "expected old-image:v0.1 in docker rmi candidates: {images:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: prefix filtering — only matching images surfaced
// ---------------------------------------------------------------------------

#[test]
fn docker_filters_by_active_prefix() {
    let env = support::TestEnv::new("docker-prefix");
    let fake_bin = env.root.join("fake-bin");
    // Mixed set; only "ng*" images should survive the "ng" prefix filter.
    write_fake_docker(
        &fake_bin,
        Some("nginx:latest\nnginx:1.25\nredis:7\nalpine:3"),
        0,
    );

    let _daemon = spawn_daemon_with_path(&env, &fake_bin);

    let candidates = complete_line(&env, "docker run ng", &fake_bin);
    let images = docker_image_candidates(&candidates);

    assert!(
        images.contains(&"nginx:latest"),
        "expected nginx:latest with prefix 'ng': {images:?}"
    );
    assert!(
        images.contains(&"nginx:1.25"),
        "expected nginx:1.25 with prefix 'ng': {images:?}"
    );
    assert!(
        !images.contains(&"redis:7"),
        "redis:7 must be filtered out by prefix 'ng': {images:?}"
    );
    assert!(
        !images.contains(&"alpine:3"),
        "alpine:3 must be filtered out by prefix 'ng': {images:?}"
    );
}
