mod support;

#[test]
fn suggest_in_git_repo_lists_git_branches() {
    let env = support::TestEnv::new("suggest-git");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        [
            "suggest",
            "--cwd",
            cwd.to_string_lossy().as_ref(),
        ],
    );
    assert!(
        out.contains("git_branches") || out.contains("branches of this repo"),
        "expected git_branches mention, got:\n{out}"
    );
}

#[test]
fn suggest_all_lists_every_capability() {
    let env = support::TestEnv::new("suggest-all");
    let _daemon = env.spawn_daemon();
    let out = support::run_ok(&env, ["suggest", "--all"]);
    for id in [
        "git_branches",
        "ssh_hosts",
        "npm_scripts",
        "kubectl_resources",
        "docker_images",
        "make_targets",
        "hybrid_cd",
    ] {
        assert!(
            out.contains(id),
            "expected {id} listed under --all, got:\n{out}"
        );
    }
}

#[test]
fn suggest_json_returns_structured_output() {
    let env = support::TestEnv::new("suggest-json");
    let _daemon = env.spawn_daemon();
    let cwd = env.root.join("repo");
    std::fs::create_dir_all(cwd.join(".git")).unwrap();

    let out = support::run_ok(
        &env,
        ["suggest", "--cwd", cwd.to_string_lossy().as_ref(), "--json"],
    );
    let parsed: serde_json::Value =
        serde_json::from_str(out.trim()).expect("parse json");
    assert!(
        parsed.get("groups").is_some(),
        "expected groups field, got:\n{out}"
    );
}
