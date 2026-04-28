mod support;

use shac::i18n::{resolve_locale, Catalog, LocaleSource, Translator};

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
    let resolved = resolve_locale(
        None,
        Some("de".to_string()),
        Some("ru_RU".to_string()),
        None,
    );
    assert_eq!(resolved.lang, "de");
    assert_eq!(resolved.source, LocaleSource::Config);
}

#[test]
fn lc_messages_used_when_no_env_or_config() {
    let resolved = resolve_locale(
        None,
        None,
        Some("ru_RU.UTF-8".to_string()),
        Some("en_US".to_string()),
    );
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
    assert_eq!(
        resolve_locale(Some("ru_RU.UTF-8".to_string()), None, None, None).lang,
        "ru"
    );
    assert_eq!(
        resolve_locale(Some("de.UTF-8".to_string()), None, None, None).lang,
        "de"
    );
    assert_eq!(
        resolve_locale(Some("EN".to_string()), None, None, None).lang,
        "en"
    );
}

#[test]
fn lang_c_normalizes_to_en() {
    assert_eq!(
        resolve_locale(Some("C".to_string()), None, None, None).lang,
        "en"
    );
    assert_eq!(
        resolve_locale(Some("c".to_string()), None, None, None).lang,
        "en"
    );
}

#[test]
fn lang_posix_normalizes_to_en() {
    assert_eq!(
        resolve_locale(Some("POSIX".to_string()), None, None, None).lang,
        "en"
    );
    assert_eq!(
        resolve_locale(Some("posix".to_string()), None, None, None).lang,
        "en"
    );
}

#[test]
fn english_lookup_returns_key_text() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    assert!(translator
        .lookup("greeter.first_run")
        .contains("shac is ready"));
}

#[test]
fn missing_key_returns_key_string_for_visibility() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    assert_eq!(
        translator.lookup("tips.does_not_exist"),
        "tips.does_not_exist"
    );
}

#[test]
fn russian_partial_translation_falls_back_to_english_per_key() {
    let mut catalog = Catalog::bundled_en();
    catalog
        .merge_locale(
            "ru",
            r#"
        [tips]
        git_branches = "ветки этого репо подтягиваются автоматически"
    "#,
        )
        .expect("parse ru toml");

    let translator = Translator::new_for_test("ru", &catalog);
    assert!(translator.lookup("tips.git_branches").starts_with("ветки"));
    assert!(translator.lookup("tips.ssh_hosts").contains("hosts from"));
}

#[test]
fn interpolation_replaces_placeholders() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    let out = translator.lookup_with("tips.unknown_command", &[("bin", "kubectl")]);
    assert!(out.contains("kubectl"));
    assert!(!out.contains("{bin}"));
}

#[test]
fn interpolation_leaves_literal_when_placeholder_missing() {
    let translator = Translator::new_for_test("en", &Catalog::bundled_en());
    let out = translator.lookup_with("tips.unknown_command", &[]);
    assert!(
        out.contains("{bin}"),
        "missing placeholder leaves literal {{bin}}"
    );
}

#[test]
fn locale_current_reports_default_when_unset() {
    let env = support::TestEnv::new("locale-current");
    let out = support::run_ok(&env, ["locale", "current"]);
    assert!(out.contains("en"), "expected en default, got: {out}");
}

#[test]
fn locale_set_persists_to_config() {
    let env = support::TestEnv::new("locale-set");
    support::run_ok(&env, ["locale", "set", "ru"]);
    let out = support::run_ok(&env, ["config", "get", "ui.locale"]);
    assert!(out.contains("ru"), "expected ru in config, got:\n{out}");
}

#[test]
fn locale_dump_keys_lists_known_keys() {
    let env = support::TestEnv::new("locale-dump");
    let out = support::run_ok(&env, ["locale", "dump-keys"]);
    assert!(out.contains("tips.git_branches"));
    assert!(out.contains("greeter.first_run"));
}
