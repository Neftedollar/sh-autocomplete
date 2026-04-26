use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fish_available() -> bool {
    Command::new("fish")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn shac_fish_path() -> PathBuf {
    std::env::current_dir()
        .expect("current dir")
        .join("shell")
        .join("fish")
        .join("shac.fish")
}

#[test]
fn fish_adapter_passes_syntax_check() {
    let script = shac_fish_path();
    assert!(
        script.exists(),
        "expected fish adapter at {}",
        script.display()
    );

    // Regression guard: the bugs that just got fixed must not return.
    let contents = fs::read_to_string(&script).expect("read shac.fish");
    assert!(
        !contents.contains("[[ "),
        "shac.fish must not contain bash-style `[[ ` test syntax"
    );
    assert!(
        !contents.contains("${SHAC_DISABLE:-}"),
        "shac.fish must not use bash-style `${{SHAC_DISABLE:-}}` parameter expansion"
    );
    assert!(
        !contents.contains("complete --command '*'"),
        "shac.fish must not register the fictional `complete --command '*'` match-all rule"
    );

    if !fish_available() {
        eprintln!("fish not installed; skipping fish -n syntax check");
        return;
    }

    let output = Command::new("fish")
        .arg("-n")
        .arg(&script)
        .output()
        .expect("run fish -n");
    assert!(
        output.status.success(),
        "fish -n failed for {}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        script.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
