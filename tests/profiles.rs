//! Integration tests for the static command-profile registry.
//!
//! These exercise [`shac::profiles::arg_type_for`] against parsed contexts
//! produced by [`shac::context::parse`]. Pure-Rust unit tests for `lookup`
//! live inline in `src/profiles.rs`.

use std::path::Path;

use shac::context;
use shac::profiles::{self, ArgType};

fn parsed(line: &str) -> context::ParsedContext {
    let cwd = Path::new("/tmp");
    context::parse(line, line.len(), cwd)
}

#[test]
fn lookup_returns_known_command() {
    assert!(profiles::lookup("git").is_some());
    assert!(profiles::lookup("ssh").is_some());
    assert!(profiles::lookup("xyzzy").is_none());
}

#[test]
fn arg_type_for_cd_is_directory() {
    assert_eq!(
        profiles::arg_type_for(&parsed("cd ")),
        Some(ArgType::Directory)
    );
}

#[test]
fn arg_type_for_cd_with_partial_path_is_directory() {
    // Active token shape doesn't change the type.
    assert_eq!(
        profiles::arg_type_for(&parsed("cd somepath")),
        Some(ArgType::Directory)
    );
}

#[test]
fn arg_type_for_git_default_is_subcommand() {
    assert_eq!(
        profiles::arg_type_for(&parsed("git ")),
        Some(ArgType::Subcommand)
    );
}

#[test]
fn arg_type_for_git_while_typing_subcommand_is_subcommand() {
    // User is partway through typing "checkout"; tokens[1]="che" — not yet a
    // recognized subcommand, so we stay on the command's default.
    assert_eq!(
        profiles::arg_type_for(&parsed("git che")),
        Some(ArgType::Subcommand)
    );
}

#[test]
fn arg_type_for_git_checkout_is_branch() {
    assert_eq!(
        profiles::arg_type_for(&parsed("git checkout ")),
        Some(ArgType::Branch)
    );
}

#[test]
fn arg_type_for_git_add_is_path() {
    assert_eq!(
        profiles::arg_type_for(&parsed("git add ")),
        Some(ArgType::Path)
    );
}

#[test]
fn arg_type_for_ssh_is_host() {
    assert_eq!(profiles::arg_type_for(&parsed("ssh ")), Some(ArgType::Host));
}

#[test]
fn arg_type_for_npm_run_is_script() {
    assert_eq!(
        profiles::arg_type_for(&parsed("npm run ")),
        Some(ArgType::Script)
    );
}

#[test]
fn arg_type_for_npm_install_is_none() {
    assert_eq!(
        profiles::arg_type_for(&parsed("npm install ")),
        Some(ArgType::None)
    );
}

#[test]
fn arg_type_for_kubectl_get_is_resource() {
    assert_eq!(
        profiles::arg_type_for(&parsed("kubectl get ")),
        Some(ArgType::Resource)
    );
}

#[test]
fn arg_type_for_docker_run_is_image() {
    assert_eq!(
        profiles::arg_type_for(&parsed("docker run ")),
        Some(ArgType::Image)
    );
}

#[test]
fn arg_type_for_make_is_target() {
    assert_eq!(
        profiles::arg_type_for(&parsed("make ")),
        Some(ArgType::Target)
    );
}

#[test]
fn arg_type_for_unknown_command_falls_back_to_path() {
    assert_eq!(
        profiles::arg_type_for(&parsed("xyzzy ")),
        Some(ArgType::Path)
    );
}

#[test]
fn arg_type_for_empty_input_is_none() {
    assert_eq!(profiles::arg_type_for(&parsed("")), None);
}

#[test]
fn arg_type_for_git_checkout_with_extra_positional_remains_branch() {
    // v1 limitation documented in profiles.rs: positional depth past the
    // subcommand slot is not tracked. Pin the current behaviour so future
    // refinements show up as deliberate test changes.
    assert_eq!(
        profiles::arg_type_for(&parsed("git checkout main ")),
        Some(ArgType::Branch)
    );
}
