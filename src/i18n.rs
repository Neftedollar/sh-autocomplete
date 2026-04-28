//! Localization: locale resolution and string lookup.
//!
//! Resolution priority (high → low): SHAC_LOCALE env, ui.locale config,
//! LC_MESSAGES env, LANG env, "en" default. All inputs normalized to a
//! 2-letter language code.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

const EN_TOML: &str = include_str!("../locales/en.toml");

type LangMap = HashMap<String, String>;

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    locales: HashMap<String, LangMap>,
}

impl Catalog {
    pub fn bundled_en() -> Self {
        let mut catalog = Self::default();
        catalog
            .merge_locale("en", EN_TOML)
            .expect("bundled en.toml must parse");
        catalog
    }

    pub fn merge_locale(&mut self, lang: &str, toml_str: &str) -> Result<()> {
        let parsed: toml::Value = toml::from_str(toml_str).context("parse locale toml")?;
        let mut flat = self.locales.remove(lang).unwrap_or_default();
        flatten(&mut flat, "", &parsed);
        self.locales.insert(lang.to_string(), flat);
        Ok(())
    }

    /// Build catalog: bundled `en` + bundled `<lang>` (none yet) + user override at
    /// `<config_dir>/locales/<lang>.toml` and/or `<config_dir>/locales/en.toml`.
    pub fn build(config_dir: &Path, lang: &str) -> Self {
        let mut catalog = Self::bundled_en();
        let user_lang = config_dir.join("locales").join(format!("{lang}.toml"));
        if lang != "en" && user_lang.exists() {
            match fs::read_to_string(&user_lang) {
                Ok(raw) => {
                    if let Err(e) = catalog.merge_locale(lang, &raw) {
                        eprintln!(
                            "shac: failed to parse user locale {}: {e:#}",
                            user_lang.display()
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "shac: failed to read user locale {}: {e:#}",
                        user_lang.display()
                    );
                }
            }
        }
        let user_en = config_dir.join("locales").join("en.toml");
        if user_en.exists() {
            match fs::read_to_string(&user_en) {
                Ok(raw) => {
                    if let Err(e) = catalog.merge_locale("en", &raw) {
                        eprintln!(
                            "shac: failed to parse user locale {}: {e:#}",
                            user_en.display()
                        );
                    }
                }
                Err(e) => {
                    eprintln!(
                        "shac: failed to read user locale {}: {e:#}",
                        user_en.display()
                    );
                }
            }
        }
        catalog
    }

    /// Return all keys present in the bundled (and merged) `en` locale, sorted.
    pub fn known_keys(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .locales
            .get("en")
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        out.sort();
        out
    }

    /// Keys present in `en` but missing in `lang`. Sorted.
    pub fn missing_keys(&self, lang: &str) -> Vec<String> {
        let en = match self.locales.get("en") {
            Some(m) => m,
            None => return vec![],
        };
        let other = self.locales.get(lang);
        let mut out: Vec<String> = en
            .keys()
            .filter(|k| other.map(|m| !m.contains_key(*k)).unwrap_or(true))
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// Locale files found at `<config_dir>/locales/*.toml` (just the language stems).
    pub fn user_locale_files(config_dir: &std::path::Path) -> Vec<String> {
        let dir = config_dir.join("locales");
        let mut out = vec![];
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                if let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) {
                    if let Some(stem) = name.strip_suffix(".toml") {
                        out.push(stem.to_string());
                    }
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn flatten(out: &mut LangMap, prefix: &str, value: &toml::Value) {
    match value {
        toml::Value::Table(table) => {
            for (k, v) in table {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(out, &next, v);
            }
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        _ => {}
    }
}

#[derive(Debug, Clone)]
pub struct Translator {
    lang: String,
    catalog: Catalog,
}

impl Translator {
    pub fn new(lang: String, catalog: Catalog) -> Self {
        Self { lang, catalog }
    }

    pub fn new_for_test(lang: &str, catalog: &Catalog) -> Self {
        Self {
            lang: lang.into(),
            catalog: catalog.clone(),
        }
    }

    pub fn lookup(&self, key: &str) -> String {
        self.lookup_with(key, &[])
    }

    pub fn lookup_with(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let raw = self.lookup_raw(key).unwrap_or_else(|| key.to_string());
        interpolate(&raw, vars)
    }

    fn lookup_raw(&self, key: &str) -> Option<String> {
        if let Some(map) = self.catalog.locales.get(&self.lang) {
            if let Some(value) = map.get(key) {
                return Some(value.clone());
            }
        }
        self.catalog
            .locales
            .get("en")
            .and_then(|m| m.get(key))
            .cloned()
    }
}

// Inputs assumed to not contain `{...}` substrings; tip texts are bundled and trusted.
fn interpolate(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        let placeholder = format!("{{{name}}}");
        out = out.replace(&placeholder, value);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocaleSource {
    Env,
    Config,
    AutoLcMessages,
    AutoLang,
    Default,
}

#[derive(Debug, Clone)]
pub struct ResolvedLocale {
    pub lang: String,
    pub source: LocaleSource,
}

pub fn resolve_locale(
    shac_locale_env: Option<String>,
    ui_locale_config: Option<String>,
    lc_messages_env: Option<String>,
    lang_env: Option<String>,
) -> ResolvedLocale {
    if let Some(value) = non_empty(shac_locale_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::Env };
    }
    if let Some(value) = non_empty(ui_locale_config) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::Config };
    }
    if let Some(value) = non_empty(lc_messages_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::AutoLcMessages };
    }
    if let Some(value) = non_empty(lang_env) {
        return ResolvedLocale { lang: normalize(&value), source: LocaleSource::AutoLang };
    }
    ResolvedLocale { lang: "en".into(), source: LocaleSource::Default }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.trim().is_empty())
}

// TODO(v0.6+): Handle IETF BCP 47 tags like `zh-Hans-CN` more richly. Today we
// strip everything after the first `-`/`_`/`.` separator, which discards script
// and region subtags (e.g. `Hans` vs `Hant`). Acceptable for v0.5 since we only
// ship language-level translations, but revisit when script-aware locales land.
fn normalize(raw: &str) -> String {
    let lowercase = raw.to_ascii_lowercase();
    let cut = lowercase
        .split(|c: char| c == '_' || c == '.' || c == '-')
        .next()
        .unwrap_or("");
    if cut.is_empty() {
        return "en".into();
    }
    // POSIX convention: `C` and `POSIX` mean the C/English fallback locale.
    if cut == "c" || cut == "posix" {
        return "en".into();
    }
    cut.into()
}
