//! Integration tests for `Engine::collect_git_branch_candidates` —
//! the `ArgType::Branch` collector that hooks `git checkout|switch|...`
//! against `git for-each-ref` output. PLAN §7.4.

mod support;

use std::process::Command;

fn git(repo: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "shac-test")
        .env("GIT_AUTHOR_EMAIL", "shac@test.local")
        .env("GIT_COMMITTER_NAME", "shac-test")
        .env("GIT_COMMITTER_EMAIL", "shac@test.local")
        .status()
        .expect("git command");
    assert!(status.success(), "git {args:?} failed");
}

fn skip_if_no_git() -> bool {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping git_branch tests: git is unavailable");
        return true;
    }
    false
}

/// Stand up a test repo with a handful of branches and return its path.
fn make_repo_with_branches(env: &support::TestEnv) -> std::path::PathBuf {
    let repo = env.root.join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README.md"), "hello").expect("seed file");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    for branch in ["feature/alpha", "feature/beta", "release-1.0", "develop"] {
        git(&repo, &["branch", branch]);
    }
    repo
}

#[test]
fn git_checkout_returns_branch_candidates() {
    if skip_if_no_git() {
        return;
    }
    let env = support::TestEnv::new("git-branch-checkout");
    let repo = make_repo_with_branches(&env);

    let _daemon = env.spawn_daemon();

    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git checkout ",
            "--cursor",
            "13",
            "--cwd",
            repo.to_str().expect("utf8 repo"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let mut branches: Vec<&str> = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 5 {
            continue;
        }
        let kind = fields[3];
        let source = fields[4];
        let insert_text = fields[1];
        if kind == "branch" && source == "git_branch" {
            branches.push(insert_text);
        }
    }

    assert!(
        branches.contains(&"main"),
        "expected `main` branch in candidates: {branches:?}\n{output}"
    );
    assert!(
        branches.contains(&"feature/alpha"),
        "expected `feature/alpha` branch in candidates: {branches:?}\n{output}"
    );
    assert!(
        branches.contains(&"develop"),
        "expected `develop` branch in candidates: {branches:?}\n{output}"
    );
}

#[test]
fn git_checkout_filters_branches_by_active_prefix() {
    if skip_if_no_git() {
        return;
    }
    let env = support::TestEnv::new("git-branch-prefix");
    let repo = make_repo_with_branches(&env);

    let _daemon = env.spawn_daemon();

    // Active token "feat" should match feature/* branches and exclude main / develop / release-1.0.
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git checkout feat",
            "--cursor",
            "17",
            "--cwd",
            repo.to_str().expect("utf8 repo"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let mut branches: Vec<&str> = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 5 {
            continue;
        }
        if fields[3] == "branch" && fields[4] == "git_branch" {
            branches.push(fields[1]);
        }
    }

    assert!(
        branches.iter().any(|b| b.starts_with("feature/")),
        "expected feature/* branches when active='feat': {branches:?}\n{output}"
    );
    assert!(
        !branches.contains(&"main"),
        "main should be filtered out by prefix match on 'feat': {branches:?}"
    );
}

#[test]
fn git_checkout_outside_repo_returns_no_branch_candidates() {
    if skip_if_no_git() {
        return;
    }
    let env = support::TestEnv::new("git-branch-no-repo");

    // cwd that is decidedly NOT a git repo — env.root has no .git.
    let cwd = env.root.join("plain");
    std::fs::create_dir_all(&cwd).expect("mkdir plain");

    let _daemon = env.spawn_daemon();

    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git checkout ",
            "--cursor",
            "13",
            "--cwd",
            cwd.to_str().expect("utf8 cwd"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 5 {
            continue;
        }
        assert_ne!(
            fields[4], "git_branch",
            "expected no git_branch source candidates outside repo:\n{output}"
        );
    }
}

#[test]
fn git_switch_also_emits_branch_candidates() {
    if skip_if_no_git() {
        return;
    }
    let env = support::TestEnv::new("git-branch-switch");
    let repo = make_repo_with_branches(&env);

    let _daemon = env.spawn_daemon();

    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git switch ",
            "--cursor",
            "11",
            "--cwd",
            repo.to_str().expect("utf8 repo"),
            "--format",
            "shell-tsv-v2",
        ],
    );

    let saw_branch = output.lines().any(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        fields.len() >= 5 && fields[3] == "branch" && fields[4] == "git_branch"
    });
    assert!(
        saw_branch,
        "git switch should dispatch ArgType::Branch and emit candidates:\n{output}"
    );
}
