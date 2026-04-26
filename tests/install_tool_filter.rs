//! E2E test: `shac install` only seeds priors for CLIs that are actually
//! installed on the test machine.
//!
//! The test runs install with a minimal PATH (`/usr/bin:/bin` only) so that
//! development tools like `kubectl`, `docker`, `pnpm`, `yarn`, `gh`, and
//! `pytest` are provably absent. It then queries `command_docs` for priors
//! rows from those commands and asserts zero rows.
//!
//! We cannot assert which commands ARE present (e.g. `git` is at `/usr/bin/git`
//! on macOS, but may not be elsewhere), so we only assert the negative:
//! "noisy" cloud/JS/infra tools that are never in the system prefix should
//! produce zero priors rows.

mod support;

use support::TestEnv;

/// Commands that are very unlikely to live in `/usr/bin` or `/bin` on any
/// standard macOS or Linux machine. If `shac install` filters correctly, none
/// of these should have priors rows when we use a minimal PATH.
const DEFINITELY_NOT_IN_USR_BIN: &[&str] =
    &["kubectl", "docker", "pnpm", "yarn", "gh", "pytest", "cargo", "rustc"];

#[test]
fn install_with_minimal_path_excludes_noisy_cli_priors() {
    let env = TestEnv::new("tool-filter");

    // Run install with a stripped-down PATH so the tool detector only sees
    // /usr/bin and /bin. We must still include bin_dir so the shac binary
    // itself (invoked by the install flow) is findable.
    let minimal_path = format!(
        "{}:/usr/bin:/bin",
        env.bin_dir.display()
    );

    let mut cmd = env.shac_cmd();
    cmd.env("PATH", &minimal_path);
    cmd.args(["install", "--shell", "zsh", "--edit-rc", "--yes", "--no-import"]);

    let output = cmd.output().expect("run install");
    assert!(
        output.status.success(),
        "install failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Loaded command priors"),
        "expected 'Loaded command priors' in install output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Detected") && stdout.contains("installed CLIs"),
        "expected detection summary in output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("seeded") && stdout.contains("command priors"),
        "expected seeded count in output, got:\n{stdout}"
    );

    // Open DB directly and check that noisy CLIs have zero priors rows.
    let paths = env.app_paths();
    let db = shac::db::AppDb::open(&paths.db_file).expect("open db");

    for &cmd_name in DEFINITELY_NOT_IN_USR_BIN {
        let docs = db.docs_for_command(cmd_name).expect("docs_for_command");
        let priors_rows: Vec<_> = docs
            .iter()
            .filter(|d| d.source == shac::priors::PRIORS_SOURCE)
            .collect();
        assert!(
            priors_rows.is_empty(),
            "expected zero priors rows for '{}' (not in /usr/bin), got {} rows: {:?}",
            cmd_name,
            priors_rows.len(),
            priors_rows
        );
    }
}

#[test]
fn install_output_shows_detected_n_installed_clis() {
    // With the real PATH, at least some CLIs should be detected and the output
    // must include the detection summary line with non-zero counts.
    let env = TestEnv::new("tool-filter-counts");

    let output = support::run_ok(
        &env,
        ["install", "--shell", "zsh", "--edit-rc", "--yes", "--no-import"],
    );

    assert!(
        output.contains("Detected") && output.contains("installed CLIs"),
        "expected 'Detected N installed CLIs' in install output, got:\n{output}"
    );
    assert!(
        output.contains("seeded") && output.contains("command priors"),
        "expected 'seeded M command priors' in install output, got:\n{output}"
    );
}

#[test]
fn seed_priors_filtered_unit_only_git() {
    // Direct unit-level check via the library: seeding with only "git"
    // in the ToolDetection must produce only git prior rows.
    use std::collections::HashSet;
    use shac::tools::ToolDetection;

    let mut installed = HashSet::new();
    installed.insert("git".to_string());
    let detection = ToolDetection { installed };

    let db = shac::db::AppDb::open(std::path::Path::new(":memory:")).expect("open db");
    let seeded = shac::priors::seed_priors_into_docs_filtered(&db, &detection)
        .expect("seed filtered priors");

    // Count expected git priors.
    let git_count = shac::priors::PRIORS
        .iter()
        .filter(|p| p.command == "git")
        .count();
    assert!(git_count > 0, "test data: no git priors in corpus");
    assert_eq!(seeded, git_count, "seeded count should match git priors only");

    // Assert no rows for a non-git command.
    let docker_docs = db.docs_for_command("docker").expect("docs_for_command");
    let docker_priors: Vec<_> = docker_docs
        .iter()
        .filter(|d| d.source == shac::priors::PRIORS_SOURCE)
        .collect();
    assert!(
        docker_priors.is_empty(),
        "expected zero docker priors when docker not in detection; got: {:?}",
        docker_priors
    );
}
