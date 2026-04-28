//! Integration tests for `Engine::collect_ssh_host_candidates` —
//! the `ArgType::Host` collector that parses `~/.ssh/config` and
//! `~/.ssh/known_hosts` for SSH host completion. PLAN §7.5.

mod support;

use std::fs;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `~/.ssh/config` inside the test env's HOME.
fn write_ssh_config(env: &support::TestEnv, content: &str) {
    let ssh_dir = env.home.join(".ssh");
    fs::create_dir_all(&ssh_dir).expect("create .ssh dir");
    fs::write(ssh_dir.join("config"), content).expect("write ssh config");
}

/// Write `~/.ssh/known_hosts` inside the test env's HOME.
fn write_known_hosts(env: &support::TestEnv, content: &str) {
    let ssh_dir = env.home.join(".ssh");
    fs::create_dir_all(&ssh_dir).expect("create .ssh dir");
    fs::write(ssh_dir.join("known_hosts"), content).expect("write known_hosts");
}

/// Parse shell-tsv-v2 output from a `complete` invocation, returning
/// `(insert_text, kind, source)` triples for every non-meta data line.
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

/// Run `shac complete` with `--line LINE --cursor CURSOR --cwd /tmp` and return
/// the full parsed candidate list.
fn complete(env: &support::TestEnv, line: &str) -> Vec<(String, String, String)> {
    let cursor = line.len().to_string();
    let output = support::run_ok(
        env,
        [
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
        ],
    );
    parse_candidates(&output)
}

fn ssh_hosts(candidates: &[(String, String, String)]) -> Vec<&str> {
    candidates
        .iter()
        .filter(|(_, kind, source)| kind == "ssh_host" && source == "ssh_host")
        .map(|(name, _, _)| name.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn ssh_completion_lists_config_hosts() {
    let env = support::TestEnv::new("ssh-config-hosts");
    write_ssh_config(
        &env,
        "Host prod-server\n\
         \tHostName 203.0.113.1\n\
         \n\
         Host staging\n\
         \tHostName 203.0.113.2\n\
         \n\
         Host bastion\n\
         \tHostName 10.0.0.1\n",
    );

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.contains(&"prod-server"),
        "expected prod-server in ssh candidates: {hosts:?}"
    );
    assert!(
        hosts.contains(&"staging"),
        "expected staging in ssh candidates: {hosts:?}"
    );
    assert!(
        hosts.contains(&"bastion"),
        "expected bastion in ssh candidates: {hosts:?}"
    );
}

#[test]
fn ssh_completion_lists_known_hosts() {
    let env = support::TestEnv::new("ssh-known-hosts");
    write_known_hosts(
        &env,
        "server1.example.com,server1-alt.example.com ssh-rsa AAAA...\n\
         db.internal ssh-ed25519 BBBB...\n",
    );

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.contains(&"server1.example.com"),
        "expected server1.example.com: {hosts:?}"
    );
    assert!(
        hosts.contains(&"server1-alt.example.com"),
        "expected server1-alt.example.com: {hosts:?}"
    );
    assert!(
        hosts.contains(&"db.internal"),
        "expected db.internal: {hosts:?}"
    );
}

#[test]
fn ssh_completion_dedupes_across_sources() {
    let env = support::TestEnv::new("ssh-dedup");
    // "bastion" appears in both config and known_hosts — must surface only once.
    write_ssh_config(
        &env,
        "Host bastion\n\
         \tHostName 10.0.0.1\n",
    );
    write_known_hosts(&env, "bastion ssh-rsa AAAA...\n");

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    let count = hosts.iter().filter(|&&h| h == "bastion").count();
    assert_eq!(
        count, 1,
        "bastion should appear exactly once when in both config and known_hosts: {hosts:?}"
    );
}

#[test]
fn ssh_completion_filters_by_prefix() {
    let env = support::TestEnv::new("ssh-prefix");
    write_ssh_config(
        &env,
        "Host bastion\n\
         \tHostName 10.0.0.1\n\
         \n\
         Host backup-server\n\
         \tHostName 10.0.0.2\n\
         \n\
         Host prod-api\n\
         \tHostName 10.0.0.3\n",
    );

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ba");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.contains(&"bastion"),
        "expected bastion to match prefix 'ba': {hosts:?}"
    );
    assert!(
        hosts.contains(&"backup-server"),
        "expected backup-server to match prefix 'ba': {hosts:?}"
    );
    assert!(
        !hosts.contains(&"prod-api"),
        "prod-api should be filtered out by prefix 'ba': {hosts:?}"
    );
}

#[test]
fn ssh_completion_skips_wildcards() {
    let env = support::TestEnv::new("ssh-wildcards");
    write_ssh_config(
        &env,
        "Host *.internal\n\
         \tProxyJump bastion\n\
         \n\
         Host real-host\n\
         \tHostName 10.0.0.5\n",
    );

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        !hosts.iter().any(|h| h.contains('*')),
        "wildcard patterns must not appear in candidates: {hosts:?}"
    );
    assert!(
        hosts.contains(&"real-host"),
        "real-host must still appear: {hosts:?}"
    );
}

#[test]
fn ssh_completion_skips_hashed_known_hosts() {
    let env = support::TestEnv::new("ssh-hashed");
    write_known_hosts(
        &env,
        // hashed entry (HashKnownHosts yes output) — must be skipped.
        "|1|abc123def456|xyz789 ssh-rsa AAAA...\n\
         visible-host.example.com ssh-rsa BBBB...\n",
    );

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        !hosts.iter().any(|h| h.starts_with('|')),
        "hashed known_hosts entries must be skipped: {hosts:?}"
    );
    assert!(
        hosts.contains(&"visible-host.example.com"),
        "plain host must appear: {hosts:?}"
    );
}

#[test]
fn ssh_completion_handles_missing_files() {
    // No ~/.ssh directory at all — must produce no candidates and no error.
    let env = support::TestEnv::new("ssh-missing-files");
    // Explicitly ensure .ssh does NOT exist.
    let ssh_dir = env.home.join(".ssh");
    let _ = fs::remove_dir_all(&ssh_dir);

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.is_empty(),
        "expected no ssh_host candidates when no ~/.ssh: {hosts:?}"
    );
}

#[test]
fn mosh_uses_same_collector() {
    let env = support::TestEnv::new("mosh-host-collector");
    write_ssh_config(
        &env,
        "Host deploy-box\n\
         \tHostName 203.0.113.50\n",
    );

    let _daemon = env.spawn_daemon();

    // mosh maps to ArgType::Host via profiles (same as ssh).
    let candidates = complete(&env, "mosh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.contains(&"deploy-box"),
        "mosh should dispatch through ArgType::Host and emit config hosts: {hosts:?}"
    );
}

#[test]
fn ssh_completion_brackets_stripped_from_known_hosts() {
    // [host]:port notation must produce bare hostname.
    let env = support::TestEnv::new("ssh-brackets-strip");
    write_known_hosts(&env, "[jump-host]:2222 ssh-ed25519 CCCC...\n");

    let _daemon = env.spawn_daemon();

    let candidates = complete(&env, "ssh ");
    let hosts = ssh_hosts(&candidates);

    assert!(
        hosts.contains(&"jump-host"),
        "bracket/port stripped: expected jump-host: {hosts:?}"
    );
    assert!(
        !hosts.iter().any(|h| h.contains('[')),
        "brackets must not appear in candidates: {hosts:?}"
    );
    assert!(
        !hosts
            .iter()
            .any(|h| h.contains(':') && !h.contains("::") /* IPv6 */),
        "port suffix must not appear: {hosts:?}"
    );
}
