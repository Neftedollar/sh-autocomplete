//! End-to-end check that `shac install` seeds the bundled command priors
//! corpus into `command_docs`, and that the daemon then surfaces those
//! priors as candidates for cold-start completions like `git `.
//!
//! Uses `--no-import` to keep the test isolated from `~/.zsh_history` and
//! zoxide — priors must surface even on a completely empty user profile.

mod support;

use support::TestEnv;

#[test]
fn install_seeds_priors_and_completions_surface_them() {
    let env = TestEnv::new("priors-install");

    // Plant an empty HOME, install with no imports — only priors should
    // populate `command_docs`.
    let install_out = support::run_ok(
        &env,
        ["install", "--shell", "zsh", "--edit-rc", "--yes", "--no-import"],
    );
    assert!(
        install_out.contains("Loaded command priors"),
        "expected priors line in install output, got:\n{install_out}"
    );

    // Sanity: the DB should contain our priors source rows for at least git.
    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");
    let git_docs = db.docs_for_command("git").expect("docs for git");
    let priors_for_git: Vec<_> = git_docs
        .iter()
        .filter(|d| d.source == shac::priors::PRIORS_SOURCE)
        .collect();
    assert!(
        !priors_for_git.is_empty(),
        "expected at least one git prior row, got: {git_docs:?}"
    );

    // Spawn daemon and ask for completions for `git ` (cursor at end).
    let _daemon = env.spawn_daemon();
    let cwd = env.home.to_string_lossy().to_string();
    let output = support::run_ok(
        &env,
        [
            "complete",
            "--shell",
            "zsh",
            "--line",
            "git ",
            "--cursor",
            "4",
            "--cwd",
            &cwd,
            "--format",
            "shell-tsv-v2",
        ],
    );

    let mut insert_texts: Vec<String> = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.first().copied() == Some("__shac_request_id") {
            continue;
        }
        if fields.len() < 2 {
            continue;
        }
        insert_texts.push(fields[1].to_string());
    }

    // Any of the seeded git subcommands should surface — we don't pin to
    // ranking order, just presence.
    let expected = ["status", "push", "commit", "pull", "log", "checkout"];
    let hit = expected
        .iter()
        .any(|sub| insert_texts.iter().any(|t| t == sub));
    assert!(
        hit,
        "expected at least one of {expected:?} in candidates; got {insert_texts:?}\nraw output:\n{output}"
    );
}
