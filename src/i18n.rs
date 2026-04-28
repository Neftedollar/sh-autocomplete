//! Localization: locale resolution and string lookup.
//!
//! Resolution priority (high → low): SHAC_LOCALE env, ui.locale config,
//! LC_MESSAGES env, LANG env, "en" default. All inputs normalized to a
//! 2-letter language code.

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

fn normalize(raw: &str) -> String {
    let lowercase = raw.to_ascii_lowercase();
    let cut = lowercase
        .split(|c: char| c == '_' || c == '.' || c == '-')
        .next()
        .unwrap_or("en");
    if cut.is_empty() { "en".into() } else { cut.into() }
}
