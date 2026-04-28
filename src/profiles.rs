//! Static command-profile registry.
//!
//! Declares the [`ArgType`] of the first positional argument for ~30 common
//! shell commands, with subcommand-level overrides (e.g. `git checkout` →
//! [`ArgType::Branch`]). The completion engine consults this registry to pick
//! a candidate source generically rather than hard-coding behaviour for `cd`.
//!
//! This module is **registry-only**: the engine integration that consumes
//! [`arg_type_for`] in `dispatch_path_like` is implemented in a follow-up step.

use crate::context::ParsedContext;

/// The kind of value that fills the active argument slot for a command.
///
/// Coarse-grained on purpose — the engine maps each variant to a candidate
/// source (filesystem walk, git ref enumeration, ssh known_hosts, kube
/// resources, etc.). Variants that don't have a meaningful completion source
/// today (e.g. `npm install <package>`) use [`ArgType::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Directory,
    Branch,
    Host,
    Resource,
    Script,
    Image,
    Subcommand,
    Flag,
    Path,
    Workspace,
    Target,
    None,
}

/// A static profile describing how the engine should complete arguments for a
/// single command.
#[derive(Debug, Clone, Copy)]
pub struct CommandProfile {
    /// The bare command name as it appears as `argv[0]` (no path, no shell
    /// quoting). Matched case-sensitively.
    pub command: &'static str,
    /// The [`ArgType`] used when no recognized subcommand is present.
    pub default_arg: ArgType,
    /// Subcommand-level overrides keyed by `argv[1]`. Empty slice means the
    /// command has no subcommand-specific completions.
    pub subcommands: &'static [(&'static str, ArgType)],
}

/// The static command registry. ~30 common commands; perf is irrelevant so we
/// scan linearly. Keep this list in sync with the table in
/// `PLAN-cold-start-and-hybrid-cd.md` §3.
static PROFILES: &[CommandProfile] = &[
    // Directory-changing builtins.
    CommandProfile {
        command: "cd",
        default_arg: ArgType::Directory,
        subcommands: &[],
    },
    CommandProfile {
        command: "pushd",
        default_arg: ArgType::Directory,
        subcommands: &[],
    },
    CommandProfile {
        command: "popd",
        default_arg: ArgType::Directory,
        subcommands: &[],
    },
    // git: subcommand-driven, with branch- and path-level overrides.
    CommandProfile {
        command: "git",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("checkout", ArgType::Branch),
            ("switch", ArgType::Branch),
            ("branch", ArgType::Branch),
            ("merge", ArgType::Branch),
            ("rebase", ArgType::Branch),
            ("worktree", ArgType::Subcommand),
            ("clone", ArgType::Path),
            ("add", ArgType::Path),
            ("rm", ArgType::Path),
            ("mv", ArgType::Path),
        ],
    },
    // Remote-host commands.
    CommandProfile {
        command: "ssh",
        default_arg: ArgType::Host,
        subcommands: &[],
    },
    CommandProfile {
        command: "scp",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    CommandProfile {
        command: "mosh",
        default_arg: ArgType::Host,
        subcommands: &[],
    },
    CommandProfile {
        command: "rsync",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    // Node package managers.
    CommandProfile {
        command: "npm",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("run", ArgType::Script),
            ("install", ArgType::None),
            ("i", ArgType::None),
            ("uninstall", ArgType::None),
        ],
    },
    CommandProfile {
        command: "pnpm",
        default_arg: ArgType::Subcommand,
        subcommands: &[("run", ArgType::Script), ("install", ArgType::None)],
    },
    CommandProfile {
        command: "yarn",
        default_arg: ArgType::Subcommand,
        subcommands: &[("run", ArgType::Script)],
    },
    // Cluster / container tooling.
    CommandProfile {
        command: "kubectl",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("get", ArgType::Resource),
            ("describe", ArgType::Resource),
            ("delete", ArgType::Resource),
            ("apply", ArgType::Path),
            ("logs", ArgType::Resource),
            ("exec", ArgType::Resource),
        ],
    },
    CommandProfile {
        command: "docker",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("run", ArgType::Image),
            ("pull", ArgType::Image),
            ("push", ArgType::Image),
            ("rmi", ArgType::Image),
            ("exec", ArgType::Resource),
        ],
    },
    // Rust.
    CommandProfile {
        command: "cargo",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("run", ArgType::None),
            ("test", ArgType::None),
            ("build", ArgType::None),
        ],
    },
    // Task runners — treat first positional arg as a target.
    CommandProfile {
        command: "make",
        default_arg: ArgType::Target,
        subcommands: &[],
    },
    CommandProfile {
        command: "just",
        default_arg: ArgType::Target,
        subcommands: &[],
    },
    CommandProfile {
        command: "task",
        default_arg: ArgType::Target,
        subcommands: &[],
    },
    // Editors (workspace = directory or .code-workspace, etc.).
    CommandProfile {
        command: "code",
        default_arg: ArgType::Workspace,
        subcommands: &[],
    },
    CommandProfile {
        command: "subl",
        default_arg: ArgType::Workspace,
        subcommands: &[],
    },
    CommandProfile {
        command: "idea",
        default_arg: ArgType::Workspace,
        subcommands: &[],
    },
    // Terminal editors and interpreters take file paths.
    CommandProfile {
        command: "nvim",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    CommandProfile {
        command: "vim",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    CommandProfile {
        command: "vi",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    CommandProfile {
        command: "python",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    CommandProfile {
        command: "python3",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    // Package / repo CLIs that route through a subcommand layer.
    CommandProfile {
        command: "brew",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("install", ArgType::Subcommand),
            ("uninstall", ArgType::Subcommand),
            ("info", ArgType::Subcommand),
            ("cask", ArgType::Subcommand),
        ],
    },
    CommandProfile {
        command: "gh",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("pr", ArgType::Subcommand),
            ("issue", ArgType::Subcommand),
            ("repo", ArgType::Subcommand),
        ],
    },
    CommandProfile {
        command: "dotnet",
        default_arg: ArgType::Subcommand,
        subcommands: &[
            ("run", ArgType::None),
            ("build", ArgType::None),
            ("test", ArgType::None),
            ("add", ArgType::Path),
            ("new", ArgType::Subcommand),
        ],
    },
    // Pytest takes a file/directory path.
    CommandProfile {
        command: "pytest",
        default_arg: ArgType::Path,
        subcommands: &[],
    },
    // Shells invoked positionally take a script path.
    CommandProfile {
        command: "bash",
        default_arg: ArgType::Script,
        subcommands: &[],
    },
    CommandProfile {
        command: "sh",
        default_arg: ArgType::Script,
        subcommands: &[],
    },
    CommandProfile {
        command: "zsh",
        default_arg: ArgType::Script,
        subcommands: &[],
    },
    // Introspection commands take a command name (treated as Subcommand here —
    // engine will surface known commands).
    CommandProfile {
        command: "which",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
    CommandProfile {
        command: "type",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
    CommandProfile {
        command: "man",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
    CommandProfile {
        command: "help",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
    // Multiplexers / cloud CLIs — subcommand-first; leaving overrides empty
    // for v1 and letting the default Subcommand label catch it.
    CommandProfile {
        command: "tmux",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
    CommandProfile {
        command: "aws",
        default_arg: ArgType::Subcommand,
        subcommands: &[],
    },
];

/// Look up a static profile for the bare command name. Returns `None` if no
/// profile is registered.
pub fn lookup(command: &str) -> Option<&'static CommandProfile> {
    PROFILES.iter().find(|p| p.command == command)
}

/// Determine the [`ArgType`] for the active token in `parsed`.
///
/// Semantics:
/// 1. No command typed yet → `None`.
/// 2. Command not in registry → `Some(ArgType::Path)` (permissive default —
///    most unknown CLIs accept paths).
/// 3. Registered command, recognized subcommand at `tokens[1]` → that
///    subcommand's [`ArgType`].
/// 4. Otherwise → the profile's `default_arg`.
///
/// **Known v1 limitation:** we do not track positional depth beyond the
/// subcommand slot. So `git checkout main <here>` still reports `Branch`
/// because we matched `checkout`. Refining to per-position completion is
/// future work for the integration step.
pub fn arg_type_for(parsed: &ParsedContext) -> Option<ArgType> {
    let command = parsed.command.as_deref().filter(|c| !c.is_empty())?;

    let profile = match lookup(command) {
        Some(p) => p,
        None => return Some(ArgType::Path),
    };

    // The command itself is at tokens[0]; a subcommand (if any) at tokens[1].
    // Only treat tokens[1] as a subcommand when the user has moved beyond it
    // (i.e. there is at least one more token slot after it, which our parser
    // models by pushing an empty string when the line ends with whitespace).
    // Otherwise the user is still typing the subcommand and we want the
    // command's default (typically `Subcommand`).
    if parsed.tokens.len() >= 3 {
        if let Some(sub) = parsed.tokens.get(1) {
            if let Some((_, arg_type)) = profile.subcommands.iter().find(|(name, _)| name == sub) {
                return Some(*arg_type);
            }
        }
    }

    Some(profile.default_arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_registered_commands() {
        assert!(lookup("git").is_some());
        assert!(lookup("ssh").is_some());
        assert!(lookup("cd").is_some());
        assert!(lookup("kubectl").is_some());
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("xyzzy").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn registry_has_no_duplicate_commands() {
        let mut names: Vec<&str> = PROFILES.iter().map(|p| p.command).collect();
        names.sort_unstable();
        let len_before = names.len();
        names.dedup();
        assert_eq!(names.len(), len_before, "duplicate command in PROFILES");
    }

    #[test]
    fn registry_size_is_reasonable() {
        // Sanity check — keep the registry within ~30-50 entries.
        assert!(PROFILES.len() >= 25, "registry shrunk unexpectedly");
        assert!(PROFILES.len() <= 60, "registry grew without bound");
    }
}
