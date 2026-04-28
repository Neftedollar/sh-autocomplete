//! Discoverability hints — catalog, selection, persistence, runtime.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipCategory {
    Capability = 0,
    Explanation = 1,
    Config = 2,
}

/// Read-only context passed to trigger predicates.
pub struct Context<'a> {
    pub line: &'a str,
    pub cursor: usize,
    pub cwd: &'a Path,
    pub tty: &'a str,
    pub home: &'a Path,
    pub response_sources: &'a [String],
    pub has_path_jump: bool,
    pub n_candidates: usize,
    pub unknown_bin: Option<&'a str>,
}

pub struct Tip {
    pub id: &'static str,
    pub category: TipCategory,
    pub text_key: &'static str,
    pub max_shows: u32,
    pub source_hint: Option<&'static str>,
    pub trigger: fn(&Context) -> bool,
}

pub fn catalog() -> &'static [Tip] {
    &CATALOG
}

const CATALOG: &[Tip] = &[
    Tip { id: "hybrid_cd",          category: TipCategory::Capability,  text_key: "tips.hybrid_cd",          max_shows: 3, source_hint: Some("path_jump"),     trigger: triggers::hybrid_cd },
    Tip { id: "git_branches",       category: TipCategory::Capability,  text_key: "tips.git_branches",       max_shows: 3, source_hint: Some("git_branches"),  trigger: triggers::git_branches },
    Tip { id: "ssh_hosts",          category: TipCategory::Capability,  text_key: "tips.ssh_hosts",          max_shows: 3, source_hint: Some("ssh_hosts"),     trigger: triggers::ssh_hosts },
    Tip { id: "npm_scripts",        category: TipCategory::Capability,  text_key: "tips.npm_scripts",        max_shows: 3, source_hint: Some("npm_scripts"),   trigger: triggers::npm_scripts },
    Tip { id: "kubectl_resources",  category: TipCategory::Capability,  text_key: "tips.kubectl_resources",  max_shows: 3, source_hint: Some("kubectl_resources"), trigger: triggers::kubectl_resources },
    Tip { id: "docker_images",      category: TipCategory::Capability,  text_key: "tips.docker_images",      max_shows: 3, source_hint: Some("docker_images"), trigger: triggers::docker_images },
    Tip { id: "make_targets",       category: TipCategory::Capability,  text_key: "tips.make_targets",       max_shows: 3, source_hint: Some("make_targets"),  trigger: triggers::make_targets },
    Tip { id: "transitions",        category: TipCategory::Explanation, text_key: "tips.transitions",        max_shows: 5, source_hint: Some("transitions"),   trigger: triggers::transitions },
    Tip { id: "path_jump_cyan",     category: TipCategory::Explanation, text_key: "tips.path_jump_cyan",     max_shows: 5, source_hint: Some("path_jump"),     trigger: triggers::path_jump_cyan },
    Tip { id: "unknown_command",    category: TipCategory::Capability,  text_key: "tips.unknown_command",    max_shows: 3, source_hint: None,                  trigger: triggers::unknown_command },
    Tip { id: "menu_detail_verbose",category: TipCategory::Config,      text_key: "tips.menu_detail_verbose",max_shows: 2, source_hint: None,                  trigger: triggers::menu_detail_verbose },
    Tip { id: "tips_off",           category: TipCategory::Config,      text_key: "tips.tips_off",           max_shows: 2, source_hint: None,                  trigger: triggers::tips_off },
];

mod triggers {
    use super::Context;

    // All return false until Task 6 fills them in.
    pub fn hybrid_cd(_: &Context) -> bool { false }
    pub fn git_branches(_: &Context) -> bool { false }
    pub fn ssh_hosts(_: &Context) -> bool { false }
    pub fn npm_scripts(_: &Context) -> bool { false }
    pub fn kubectl_resources(_: &Context) -> bool { false }
    pub fn docker_images(_: &Context) -> bool { false }
    pub fn make_targets(_: &Context) -> bool { false }
    pub fn transitions(_: &Context) -> bool { false }
    pub fn path_jump_cyan(_: &Context) -> bool { false }
    pub fn unknown_command(_: &Context) -> bool { false }
    pub fn menu_detail_verbose(_: &Context) -> bool { false }
    pub fn tips_off(_: &Context) -> bool { false }
}
