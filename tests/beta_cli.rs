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
    assert!(checks.iter().any(|check| {
        check["name"].as_str() == Some("active_shac") && check["ok"].as_bool() == Some(true)
    }));

    let zsh_doctor: Value = serde_json::from_str(&support::run_ok(
        &env,
        ["doctor", "--json", "--shell", "zsh"],
    ))
    .expect("zsh doctor json");
    let zsh_checks = zsh_doctor.as_array().expect("zsh doctor checks");
    assert!(zsh_checks.iter().any(|check| {
        check["name"].as_str() == Some("zsh_adapter_version") && check["ok"].as_bool() == Some(true)
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

#[test]
fn doctor_surfaces_cold_start_telemetry_checks() {
    let env = support::TestEnv::new("doctor-cold-start");

    // No install, no imports — but `doctor` should still produce the three
    // cold-start telemetry checks (PLAN §7.12) so the output is uniform.
    support::run_ok(&env, ["install", "--shell", "zsh", "--edit-rc", "--no-import"]);

    let doctor: Value =
        serde_json::from_str(&support::run_ok(&env, ["doctor", "--json"])).expect("doctor json");
    let checks = doctor.as_array().expect("doctor checks array");
    let names: Vec<&str> = checks
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();

    assert!(names.contains(&"cold_start_paths"), "missing cold_start_paths check: {names:?}");
    assert!(names.contains(&"cold_start_history"), "missing cold_start_history check: {names:?}");
    assert!(names.contains(&"time_to_first_accept"), "missing time_to_first_accept check: {names:?}");

    // With --no-import + no imports recorded, all three should be `fail`
    // (zero rows / zero history / no accept yet) but the detail strings
    // must be informative, not panicky.
    let by_name = |name: &str| -> &Value {
        checks.iter().find(|c| c["name"].as_str() == Some(name)).expect(name)
    };

    let paths_check = by_name("cold_start_paths");
    assert_eq!(paths_check["ok"].as_bool(), Some(false));
    assert!(paths_check["detail"].as_str().unwrap_or("").contains("0 entries"));

    let history_check = by_name("cold_start_history");
    assert_eq!(history_check["ok"].as_bool(), Some(false));
    let history_detail = history_check["detail"].as_str().unwrap_or("");
    assert!(history_detail.contains("0 imported events"));
    assert!(history_detail.contains("%"), "history detail should include coverage percent: {history_detail}");

    let ttfa_check = by_name("time_to_first_accept");
    assert_eq!(ttfa_check["ok"].as_bool(), Some(false));
    assert!(ttfa_check["detail"].as_str().unwrap_or("").contains("press Tab"));
}
