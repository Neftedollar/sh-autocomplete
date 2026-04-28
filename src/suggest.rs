//! `shac suggest` — context-aware list of applicable shac features.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::config::AppConfig;
use crate::i18n::{resolve_locale, Catalog, Translator};
use crate::tips::{self, Context, Tip};

#[derive(Debug, Serialize)]
pub struct SuggestItem {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct SuggestGroup {
    pub title: String,
    pub items: Vec<SuggestItem>,
}

#[derive(Debug, Serialize, Default)]
pub struct SuggestOutput {
    pub groups: Vec<SuggestGroup>,
}

pub struct SuggestInput<'a> {
    pub cwd: &'a Path,
    pub home: &'a Path,
    pub config_dir: &'a Path,
    pub config: &'a AppConfig,
    pub all: bool,
    pub accepted_sources_recent: HashSet<String>,
}

pub fn run(input: &SuggestInput<'_>) -> Result<SuggestOutput> {
    let resolved = resolve_locale(
        std::env::var("SHAC_LOCALE").ok(),
        Some(input.config.ui.locale.clone()),
        std::env::var("LC_MESSAGES").ok(),
        std::env::var("LANG").ok(),
    );
    let catalog = Catalog::build(input.config_dir, &resolved.lang);
    let translator = Translator::new(resolved.lang, catalog);

    if input.all {
        let mut group = SuggestGroup {
            title: "all features".into(),
            items: vec![],
        };
        for tip in tips::catalog() {
            group.items.push(SuggestItem {
                id: tip.id.into(),
                text: translator.lookup(tip.text_key),
            });
        }
        return Ok(SuggestOutput {
            groups: vec![group],
        });
    }

    // Probe lines: each capability has a synthetic command-line that activates its trigger.
    let probe_lines: &[(&str, &str)] = &[
        ("git_branches", "git checkout "),
        ("ssh_hosts", "ssh "),
        ("npm_scripts", "npm run "),
        ("kubectl_resources", "kubectl get "),
        ("docker_images", "docker run "),
        ("make_targets", "make "),
        ("hybrid_cd", "cd "),
    ];

    let no_sources: Vec<String> = vec![];
    let base_ctx = Context {
        line: "",
        cursor: 0,
        cwd: input.cwd,
        tty: "",
        home: input.home,
        response_sources: &no_sources,
        has_path_jump: false,
        n_candidates: 0,
        unknown_bin: None,
    };

    let mut available: Vec<&'static Tip> = vec![];
    for tip in tips::catalog() {
        let probe = probe_lines
            .iter()
            .find(|(id, _)| *id == tip.id)
            .map(|(_, l)| *l);
        let line = probe.unwrap_or("");
        let has_path_jump = tip.id == "hybrid_cd";
        let probe_ctx = Context {
            line,
            has_path_jump,
            ..clone_context(&base_ctx)
        };
        if (tip.trigger)(&probe_ctx) {
            available.push(tip);
        }
    }

    let mut group_used = SuggestGroup {
        title: translator.lookup("suggest.header_available"),
        items: vec![],
    };
    let mut group_unused = SuggestGroup {
        title: translator.lookup("suggest.header_unused"),
        items: vec![],
    };
    for tip in available {
        let used = tip
            .source_hint
            .map(|s| input.accepted_sources_recent.contains(s))
            .unwrap_or(false);
        let item = SuggestItem {
            id: tip.id.into(),
            text: translator.lookup(tip.text_key),
        };
        if used {
            group_used.items.push(item);
        } else {
            group_unused.items.push(item);
        }
    }

    let mut groups = vec![];
    if !group_used.items.is_empty() {
        groups.push(group_used);
    }
    if !group_unused.items.is_empty() {
        groups.push(group_unused);
    }
    if groups.is_empty() {
        groups.push(SuggestGroup {
            title: translator.lookup("suggest.no_matches"),
            items: vec![],
        });
    }

    Ok(SuggestOutput { groups })
}

pub fn render_text(out: &SuggestOutput) -> String {
    let mut buf = String::new();
    for (i, group) in out.groups.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        buf.push_str(&group.title);
        buf.push('\n');
        for item in &group.items {
            buf.push_str(&format!("  {}    {}\n", item.id, item.text));
        }
    }
    buf
}

fn clone_context<'a>(c: &Context<'a>) -> Context<'a> {
    Context {
        line: c.line,
        cursor: c.cursor,
        cwd: c.cwd,
        tty: c.tty,
        home: c.home,
        response_sources: c.response_sources,
        has_path_jump: c.has_path_jump,
        n_candidates: c.n_candidates,
        unknown_bin: c.unknown_bin,
    }
}
