use shac::i18n::{resolve_locale, LocaleSource};

#[test]
fn shac_locale_env_wins() {
    let resolved = resolve_locale(
        Some("ru".to_string()),
        None,
        Some("de_DE.UTF-8".to_string()),
        Some("fr_FR".to_string()),
    );
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::Env);
}

#[test]
fn config_overrides_lc_and_lang() {
    let resolved = resolve_locale(None, Some("de".to_string()), Some("ru_RU".to_string()), None);
    assert_eq!(resolved.lang, "de");
    assert_eq!(resolved.source, LocaleSource::Config);
}

#[test]
fn lc_messages_used_when_no_env_or_config() {
    let resolved = resolve_locale(None, None, Some("ru_RU.UTF-8".to_string()), Some("en_US".to_string()));
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::AutoLcMessages);
}

#[test]
fn lang_used_when_no_env_config_or_lc() {
    let resolved = resolve_locale(None, None, None, Some("fr_CA.UTF-8".to_string()));
    assert_eq!(resolved.lang, "fr");
    assert_eq!(resolved.source, LocaleSource::AutoLang);
}

#[test]
fn falls_back_to_en_when_nothing_set() {
    let resolved = resolve_locale(None, None, None, None);
    assert_eq!(resolved.lang, "en");
    assert_eq!(resolved.source, LocaleSource::Default);
}

#[test]
fn empty_config_string_is_treated_as_unset() {
    let resolved = resolve_locale(None, Some("".to_string()), None, Some("ru_RU".to_string()));
    assert_eq!(resolved.lang, "ru");
    assert_eq!(resolved.source, LocaleSource::AutoLang);
}

#[test]
fn normalizes_locale_strings() {
    assert_eq!(resolve_locale(Some("ru_RU.UTF-8".to_string()), None, None, None).lang, "ru");
    assert_eq!(resolve_locale(Some("de.UTF-8".to_string()), None, None, None).lang, "de");
    assert_eq!(resolve_locale(Some("EN".to_string()), None, None, None).lang, "en");
}

#[test]
fn lang_c_normalizes_to_en() {
    assert_eq!(resolve_locale(Some("C".to_string()), None, None, None).lang, "en");
    assert_eq!(resolve_locale(Some("c".to_string()), None, None, None).lang, "en");
}

#[test]
fn lang_posix_normalizes_to_en() {
    assert_eq!(resolve_locale(Some("POSIX".to_string()), None, None, None).lang, "en");
    assert_eq!(resolve_locale(Some("posix".to_string()), None, None, None).lang, "en");
}
