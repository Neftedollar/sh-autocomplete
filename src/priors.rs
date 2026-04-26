//! Bundled command priors — static corpus seeded into `command_docs` on first
//! install. Each entry is a `(command, item_type, item_value, description)`
//! tuple matching the existing [`StoredDoc`] shape.
//!
//! The corpus is hand-curated, product-neutral, and covers the ~12 commands
//! that drive the cold-start experience for `shac` users. With no
//! `~/.zsh_history` present, completion would otherwise collapse to
//! alphabetical command names — these priors give the user real grammar to
//! cycle through on first install.
//!
//! Item-type semantics (matching `should_include_doc` in `src/engine.rs`):
//! - `"subcommand"` — surfaces in [`TokenRole::Command`] and
//!   [`TokenRole::SubcommandOrArg`]. Used both for single tokens (`status`)
//!   and for short multi-token grammars (`run dev`, `get pods`) — the engine
//!   matches on `item_value.starts_with(active)`, so multi-token values still
//!   surface as one menu entry.
//! - `"option"` — surfaces in [`TokenRole::Option`] / [`TokenRole::SubcommandOrArg`].
//!   Used sparingly for flag-only entries.
//!
//! The seed function uses `"priors"` as the `source` field so `--explain`
//! output and stats can distinguish bundled corpus from imported docs.

use anyhow::Result;
use std::collections::HashMap;

use crate::db::{AppDb, StoredDoc};
use crate::tools::ToolFilter;

/// One bundled prior. Matches the column layout of `command_docs`.
#[derive(Debug, Clone, Copy)]
pub struct Prior {
    /// Bare command name, e.g. `"git"`.
    pub command: &'static str,
    /// Either `"subcommand"` or `"option"`. See module docs for semantics.
    pub item_type: &'static str,
    /// The grammar value, e.g. `"status"` or `"run dev"`.
    pub item_value: &'static str,
    /// One-line human description.
    pub description: &'static str,
}

/// Source label written to `command_docs.source` for bundled priors.
pub const PRIORS_SOURCE: &str = "priors";

/// Static corpus of common command-grammar pairs. Hand-curated; ~60 entries.
///
/// Coverage is intentionally narrow but deep — the 12 commands `shac`'s
/// existing `src/profiles.rs` registry already singles out as
/// completion-driven (`git`, `npm`, `pnpm`, `yarn`, `cargo`, `docker`,
/// `kubectl`, `gh`, `brew`, `make`, `python`, `pip`).
pub static PRIORS: &[Prior] = &[
    // ------------------------------------------------------------------
    // git
    // ------------------------------------------------------------------
    Prior { command: "git", item_type: "subcommand", item_value: "status",        description: "Show working tree status" },
    Prior { command: "git", item_type: "subcommand", item_value: "push",          description: "Update remote refs along with associated objects" },
    Prior { command: "git", item_type: "subcommand", item_value: "pull",          description: "Fetch from and integrate with another repository" },
    Prior { command: "git", item_type: "subcommand", item_value: "commit",        description: "Record changes to the repository" },
    Prior { command: "git", item_type: "subcommand", item_value: "commit -m",     description: "Commit with an inline message" },
    Prior { command: "git", item_type: "subcommand", item_value: "log",           description: "Show commit logs" },
    Prior { command: "git", item_type: "subcommand", item_value: "log --oneline", description: "Compact one-line commit log" },
    Prior { command: "git", item_type: "subcommand", item_value: "checkout",      description: "Switch branches or restore working tree files" },
    Prior { command: "git", item_type: "subcommand", item_value: "switch",        description: "Switch branches" },
    Prior { command: "git", item_type: "subcommand", item_value: "rebase",        description: "Reapply commits on top of another base tip" },
    Prior { command: "git", item_type: "subcommand", item_value: "rebase -i",     description: "Interactive rebase" },
    Prior { command: "git", item_type: "subcommand", item_value: "stash",         description: "Stash the changes in a dirty working directory" },
    Prior { command: "git", item_type: "subcommand", item_value: "merge",         description: "Join two or more development histories together" },
    Prior { command: "git", item_type: "subcommand", item_value: "diff",          description: "Show changes between commits, commit and working tree, etc." },
    Prior { command: "git", item_type: "subcommand", item_value: "diff --stat",   description: "Diffstat summary" },
    Prior { command: "git", item_type: "subcommand", item_value: "branch -a",     description: "List all (local + remote) branches" },
    Prior { command: "git", item_type: "subcommand", item_value: "fetch",         description: "Download objects and refs from another repository" },
    Prior { command: "git", item_type: "subcommand", item_value: "reset",         description: "Reset current HEAD to the specified state" },

    // ------------------------------------------------------------------
    // npm
    // ------------------------------------------------------------------
    Prior { command: "npm", item_type: "subcommand", item_value: "install",   description: "Install a package and its dependencies" },
    Prior { command: "npm", item_type: "subcommand", item_value: "run dev",   description: "Run the project's dev script" },
    Prior { command: "npm", item_type: "subcommand", item_value: "run build", description: "Run the project's build script" },
    Prior { command: "npm", item_type: "subcommand", item_value: "run test",  description: "Run the project's test script" },
    Prior { command: "npm", item_type: "subcommand", item_value: "run lint",  description: "Run the project's lint script" },
    Prior { command: "npm", item_type: "subcommand", item_value: "audit",     description: "Run a security audit" },
    Prior { command: "npm", item_type: "subcommand", item_value: "ci",        description: "Clean install from package-lock.json" },

    // ------------------------------------------------------------------
    // pnpm
    // ------------------------------------------------------------------
    Prior { command: "pnpm", item_type: "subcommand", item_value: "install",   description: "Install dependencies" },
    Prior { command: "pnpm", item_type: "subcommand", item_value: "run dev",   description: "Run the project's dev script" },
    Prior { command: "pnpm", item_type: "subcommand", item_value: "run build", description: "Run the project's build script" },
    Prior { command: "pnpm", item_type: "subcommand", item_value: "run test",  description: "Run the project's test script" },

    // ------------------------------------------------------------------
    // yarn
    // ------------------------------------------------------------------
    Prior { command: "yarn", item_type: "subcommand", item_value: "install",   description: "Install dependencies" },
    Prior { command: "yarn", item_type: "subcommand", item_value: "dev",       description: "Run the dev script" },
    Prior { command: "yarn", item_type: "subcommand", item_value: "build",     description: "Run the build script" },
    Prior { command: "yarn", item_type: "subcommand", item_value: "test",      description: "Run the test script" },

    // ------------------------------------------------------------------
    // cargo
    // ------------------------------------------------------------------
    Prior { command: "cargo", item_type: "subcommand", item_value: "build",           description: "Compile the current package" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "build --release", description: "Compile with release optimizations" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "test",            description: "Run the tests" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "run",             description: "Run a binary or example" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "check",           description: "Analyze without producing artifacts" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "clippy",          description: "Run the clippy linter" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "fmt",             description: "Format the source code" },
    Prior { command: "cargo", item_type: "subcommand", item_value: "add",             description: "Add a dependency to Cargo.toml" },

    // ------------------------------------------------------------------
    // docker
    // ------------------------------------------------------------------
    Prior { command: "docker", item_type: "subcommand", item_value: "ps",            description: "List running containers" },
    Prior { command: "docker", item_type: "subcommand", item_value: "ps -a",         description: "List all containers (running and stopped)" },
    Prior { command: "docker", item_type: "subcommand", item_value: "images",        description: "List images" },
    Prior { command: "docker", item_type: "subcommand", item_value: "run -it",       description: "Run a container interactively" },
    Prior { command: "docker", item_type: "subcommand", item_value: "build -t",      description: "Build an image with a tag" },
    Prior { command: "docker", item_type: "subcommand", item_value: "exec -it",      description: "Exec into a running container" },
    Prior { command: "docker", item_type: "subcommand", item_value: "logs -f",       description: "Follow container logs" },
    Prior { command: "docker", item_type: "subcommand", item_value: "compose up",    description: "Create and start compose services" },
    Prior { command: "docker", item_type: "subcommand", item_value: "compose down",  description: "Stop and remove compose services" },

    // ------------------------------------------------------------------
    // kubectl
    // ------------------------------------------------------------------
    Prior { command: "kubectl", item_type: "subcommand", item_value: "get pods",            description: "List pods in the current namespace" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "get svc",             description: "List services" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "apply -f",            description: "Apply a configuration from a file" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "describe pod",        description: "Show details of a pod" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "logs -f",             description: "Stream logs from a pod" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "exec -it",            description: "Exec into a running pod" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "delete pod",          description: "Delete a pod" },
    Prior { command: "kubectl", item_type: "subcommand", item_value: "config use-context",  description: "Switch the active kubeconfig context" },

    // ------------------------------------------------------------------
    // gh
    // ------------------------------------------------------------------
    Prior { command: "gh", item_type: "subcommand", item_value: "pr list",     description: "List pull requests in this repo" },
    Prior { command: "gh", item_type: "subcommand", item_value: "pr view",     description: "View a pull request" },
    Prior { command: "gh", item_type: "subcommand", item_value: "pr create",   description: "Create a pull request" },
    Prior { command: "gh", item_type: "subcommand", item_value: "pr checkout", description: "Checkout a pull request locally" },
    Prior { command: "gh", item_type: "subcommand", item_value: "issue list",  description: "List issues" },
    Prior { command: "gh", item_type: "subcommand", item_value: "repo clone",  description: "Clone a repository" },
    Prior { command: "gh", item_type: "subcommand", item_value: "repo view",   description: "View a repository" },

    // ------------------------------------------------------------------
    // brew
    // ------------------------------------------------------------------
    Prior { command: "brew", item_type: "subcommand", item_value: "install",           description: "Install a formula or cask" },
    Prior { command: "brew", item_type: "subcommand", item_value: "uninstall",         description: "Uninstall a formula or cask" },
    Prior { command: "brew", item_type: "subcommand", item_value: "update",            description: "Update Homebrew itself" },
    Prior { command: "brew", item_type: "subcommand", item_value: "upgrade",           description: "Upgrade installed formulae and casks" },
    Prior { command: "brew", item_type: "subcommand", item_value: "list",              description: "List installed formulae" },
    Prior { command: "brew", item_type: "subcommand", item_value: "info",              description: "Show information about a formula" },
    Prior { command: "brew", item_type: "subcommand", item_value: "services list",     description: "List launchd-managed brew services" },
    Prior { command: "brew", item_type: "subcommand", item_value: "services restart",  description: "Restart a brew-managed service" },

    // ------------------------------------------------------------------
    // make
    // ------------------------------------------------------------------
    Prior { command: "make", item_type: "subcommand", item_value: "build",   description: "Run the build target" },
    Prior { command: "make", item_type: "subcommand", item_value: "test",    description: "Run the test target" },
    Prior { command: "make", item_type: "subcommand", item_value: "install", description: "Run the install target" },
    Prior { command: "make", item_type: "subcommand", item_value: "clean",   description: "Run the clean target" },

    // ------------------------------------------------------------------
    // python / python3
    // ------------------------------------------------------------------
    Prior { command: "python",  item_type: "subcommand", item_value: "-m venv",         description: "Create a virtual environment" },
    Prior { command: "python",  item_type: "subcommand", item_value: "-m pip install",  description: "Install a package via pip" },
    Prior { command: "python",  item_type: "subcommand", item_value: "-m http.server",  description: "Serve the current directory over HTTP" },
    Prior { command: "python3", item_type: "subcommand", item_value: "-m venv",         description: "Create a virtual environment" },
    Prior { command: "python3", item_type: "subcommand", item_value: "-m pip install",  description: "Install a package via pip" },

    // ------------------------------------------------------------------
    // pip
    // ------------------------------------------------------------------
    Prior { command: "pip", item_type: "subcommand", item_value: "install",                  description: "Install a package" },
    Prior { command: "pip", item_type: "subcommand", item_value: "install -e .",             description: "Install the current project in editable mode" },
    Prior { command: "pip", item_type: "subcommand", item_value: "install -r requirements.txt", description: "Install packages from a requirements file" },
    Prior { command: "pip", item_type: "subcommand", item_value: "freeze",                   description: "Output installed packages in requirements format" },
    Prior { command: "pip", item_type: "subcommand", item_value: "list",                     description: "List installed packages" },
    Prior { command: "pip", item_type: "subcommand", item_value: "uninstall",                description: "Uninstall a package" },
];

/// Number of bundled priors in the static corpus. Useful for printer output
/// without exposing the slice itself.
pub fn count_priors() -> usize {
    PRIORS.len()
}

/// Convert one [`Prior`] to the [`StoredDoc`] row shape.
fn prior_to_stored_doc(p: &Prior) -> StoredDoc {
    StoredDoc {
        command: p.command.to_string(),
        item_type: p.item_type.to_string(),
        item_value: p.item_value.to_string(),
        description: p.description.to_string(),
        source: PRIORS_SOURCE.to_string(),
    }
}

/// Seed the bundled prior corpus into `command_docs`.
///
/// Idempotent: each per-command write goes through
/// [`AppDb::replace_docs_for_command`], which `DELETE`s and re-`INSERT`s the
/// rows for that command. Re-running does not duplicate.
///
/// **Note:** `replace_docs_for_command` deletes *all* rows for a command, so
/// if a command has both priors and imported help docs, the priors will
/// overwrite. For the cold-start path this is the intended behaviour — priors
/// run first on fresh install before any help-text indexer fills the table.
/// Subsequent `shac install` runs do not change `command_docs` net of priors.
///
/// Returns the total number of prior rows seeded.
///
/// For the filtered variant that only seeds priors for installed CLIs, use
/// [`seed_priors_into_docs_filtered`].
pub fn seed_priors_into_docs(db: &AppDb) -> Result<usize> {
    seed_priors_into_docs_filtered(db, &crate::tools::AdmitAll)
}

/// Seed only those prior entries whose command is considered installed by
/// `filter`.
///
/// Callers at install time should pass a [`crate::tools::ToolDetection`]
/// obtained from [`crate::tools::detect_tools()`]. To seed everything
/// (e.g. for tests or explicit user requests), pass
/// [`crate::tools::AdmitAll`].
///
/// Returns the number of prior rows actually written to the DB.
pub fn seed_priors_into_docs_filtered<F: ToolFilter>(db: &AppDb, filter: &F) -> Result<usize> {
    // Group priors by command — `replace_docs_for_command` is per-command.
    let mut grouped: HashMap<&'static str, Vec<StoredDoc>> = HashMap::new();
    for p in PRIORS {
        if filter.has(p.command) {
            grouped.entry(p.command).or_default().push(prior_to_stored_doc(p));
        }
    }
    let mut total = 0usize;
    for (cmd, docs) in &grouped {
        db.replace_docs_for_command(cmd, docs)?;
        total += docs.len();
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn priors_corpus_is_nonempty_and_well_formed() {
        assert!(
            PRIORS.len() >= 50,
            "priors corpus shrunk unexpectedly: {} entries",
            PRIORS.len()
        );
        for p in PRIORS {
            assert!(!p.command.is_empty(), "empty command in prior: {:?}", p);
            assert!(!p.item_type.is_empty(), "empty item_type in prior: {:?}", p);
            assert!(!p.item_value.is_empty(), "empty item_value in prior: {:?}", p);
            assert!(!p.description.is_empty(), "empty description in prior: {:?}", p);
            assert!(
                p.item_type == "subcommand" || p.item_type == "option",
                "unsupported item_type {:?} in prior: {:?}",
                p.item_type,
                p
            );
        }
    }

    #[test]
    fn priors_have_no_duplicate_keys() {
        let mut seen: HashSet<(&str, &str, &str)> = HashSet::new();
        for p in PRIORS {
            let key = (p.command, p.item_type, p.item_value);
            assert!(
                seen.insert(key),
                "duplicate prior key: {:?}",
                key
            );
        }
    }

    #[test]
    fn count_priors_matches_slice_len() {
        assert_eq!(count_priors(), PRIORS.len());
    }

    /// Count rows in `command_docs` whose `source` is the priors label,
    /// summed across every distinct command in [`PRIORS`]. We do not have a
    /// public connection accessor on [`AppDb`], so we walk the prior commands
    /// and use [`AppDb::docs_for_command`] which reads `source`.
    fn count_seeded_priors(db: &AppDb) -> usize {
        let mut commands: Vec<&str> = PRIORS.iter().map(|p| p.command).collect();
        commands.sort_unstable();
        commands.dedup();
        commands
            .iter()
            .map(|c| {
                db.docs_for_command(c)
                    .expect("docs_for_command")
                    .into_iter()
                    .filter(|d| d.source == PRIORS_SOURCE)
                    .count()
            })
            .sum()
    }

    #[test]
    fn seed_priors_into_docs_inserts_rows() {
        let db = AppDb::open(std::path::Path::new(":memory:")).expect("open in-memory db");
        let seeded = seed_priors_into_docs(&db).expect("seed priors");
        assert_eq!(seeded, PRIORS.len());
        assert_eq!(count_seeded_priors(&db), PRIORS.len());
    }

    #[test]
    fn seed_priors_idempotent() {
        let db = AppDb::open(std::path::Path::new(":memory:")).expect("open in-memory db");
        seed_priors_into_docs(&db).expect("seed priors first time");
        seed_priors_into_docs(&db).expect("seed priors second time");
        assert_eq!(count_seeded_priors(&db), PRIORS.len());
    }

    #[test]
    fn seed_priors_filtered_excludes_uninstalled() {
        use std::collections::HashSet;
        use crate::tools::ToolDetection;

        // Detection with only "git" installed.
        let mut installed = HashSet::new();
        installed.insert("git".to_string());
        let detection = ToolDetection { installed };

        let db = AppDb::open(std::path::Path::new(":memory:")).expect("open in-memory db");
        let seeded = seed_priors_into_docs_filtered(&db, &detection)
            .expect("seed filtered priors");

        // Only git priors should be seeded.
        let git_count = PRIORS.iter().filter(|p| p.command == "git").count();
        assert_eq!(seeded, git_count, "expected only git priors, got {seeded}");

        // No other command should have rows.
        for p in PRIORS {
            if p.command == "git" {
                continue;
            }
            let docs = db.docs_for_command(p.command).expect("docs_for_command");
            let priors_rows: Vec<_> = docs
                .iter()
                .filter(|d| d.source == PRIORS_SOURCE)
                .collect();
            assert!(
                priors_rows.is_empty(),
                "expected no priors rows for '{}', got {:?}",
                p.command,
                priors_rows
            );
        }
    }

    #[test]
    fn seed_priors_filtered_admits_all_with_admit_all() {
        use crate::tools::AdmitAll;

        let db = AppDb::open(std::path::Path::new(":memory:")).expect("open in-memory db");
        let seeded = seed_priors_into_docs_filtered(&db, &AdmitAll)
            .expect("seed with AdmitAll");
        assert_eq!(
            seeded,
            PRIORS.len(),
            "AdmitAll should seed every prior; got {seeded}"
        );
    }
}
