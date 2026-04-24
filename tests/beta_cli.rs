mod support;

use serde_json::Value;

#[test]
fn beta_cli_supports_disable_doctor_and_managed_rc_blocks() {
    let env = support::TestEnv::new("beta-cli");

    support::run_ok(&env, ["install", "--shell", "zsh", "--edit-rc"]);
    let zshrc = std::fs::read_to_string(env.home.join(".zshrc")).expect("read zshrc");
    assert!(zshrc.contains("# >>> shac initialize >>>"));
    assert!(zshrc.contains("shac.zsh"));

    let doctor: Value =
        serde_json::from_str(&support::run_ok(&env, ["doctor", "--json"])).expect("doctor json");
    let checks = doctor.as_array().expect("doctor checks");
    assert!(checks.iter().any(|check| {
        check["name"].as_str() == Some("zsh_adapter") && check["ok"].as_bool() == Some(true)
    }));

    support::run_ok(&env, ["config", "set", "enabled", "off"]);
    support::run_ok(&env, ["config", "set", "ui.zsh.menu_detail", "minimal"]);
    support::run_ok(&env, ["config", "set", "ui.zsh.show_source", "on"]);
    support::run_ok(&env, ["config", "set", "ui.zsh.max_items", "4"]);
    assert_eq!(
        support::run_ok(&env, ["config", "get", "ui.zsh.menu_detail"]).trim(),
        "minimal"
    );
    let shell_env = support::run_ok(&env, ["shell-env", "--shell", "zsh"]);
    assert!(
        shell_env.contains("_shac_ui_menu_detail='minimal'"),
        "shell-env should expose zsh ui detail: {shell_env}"
    );
    assert!(
        shell_env.contains("_shac_ui_show_source=1"),
        "shell-env should expose zsh source flag: {shell_env}"
    );
    assert!(
        shell_env.contains("_shac_ui_max_items=4"),
        "shell-env should expose zsh max items: {shell_env}"
    );

    let completion: Value = serde_json::from_str(&support::run_ok(
        &env,
        [
            "complete", "--shell", "zsh", "--line", "pyt", "--cursor", "3", "--format", "json",
        ],
    ))
    .expect("disabled completion json");
    assert_eq!(completion["fallback"].as_bool(), Some(true));
    assert_eq!(completion["items"].as_array().map(Vec::len), Some(0));

    let debug: Value = serde_json::from_str(&support::run_ok(
        &env,
        [
            "debug",
            "completion",
            "--shell",
            "zsh",
            "--line",
            "pyt",
            "--cursor",
            "3",
        ],
    ))
    .expect("debug completion json");
    assert_eq!(debug["disabled"].as_bool(), Some(true));
    assert_eq!(debug["response"]["fallback"].as_bool(), Some(true));

    support::run_ok(&env, ["uninstall", "--shell", "zsh", "--edit-rc"]);
    let zshrc = std::fs::read_to_string(env.home.join(".zshrc")).unwrap_or_default();
    assert!(!zshrc.contains("# >>> shac initialize >>>"));
    assert!(!env.zsh_script_path().exists());
}
