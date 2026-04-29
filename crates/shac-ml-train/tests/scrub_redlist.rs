use shac_ml_train::scrub::scrub_text;

#[test]
fn macos_home_path_replaced() {
    assert_eq!(scrub_text("cd /Users/roman/dev/shac"), "cd <HOME>/dev/shac");
}

#[test]
fn linux_home_path_replaced() {
    assert_eq!(scrub_text("ls /home/alice/projects"), "ls <HOME>/projects");
}

#[test]
fn macos_var_folders_replaced() {
    let out = scrub_text("cat /var/folders/aa/bb/T/build.log");
    assert_eq!(out, "cat <TMPDIR>/build.log");
}

#[test]
fn tmp_random_id_replaced() {
    let out = scrub_text("rm /tmp/tmpA1B2c3D4e5_x");
    assert!(out.contains("<TMPDIR>"));
    assert!(!out.contains("tmpA1B2c3D4e5_x"));
}

#[test]
fn email_replaced() {
    assert_eq!(
        scrub_text("git config user.email roman@example.com"),
        "git config user.email <EMAIL>"
    );
}

#[test]
fn ipv4_replaced() {
    assert_eq!(scrub_text("ssh 10.0.1.42"), "ssh <IP>");
}

#[test]
fn long_hex_token_replaced() {
    let out = scrub_text("export TOKEN=abcdef0123456789abcdef0123456789");
    assert!(out.contains("<TOKEN>"));
    assert!(!out.contains("abcdef0123456789abcdef0123456789"));
}

#[test]
fn aws_access_key_replaced() {
    assert_eq!(
        scrub_text("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"),
        "export AWS_ACCESS_KEY_ID=<AWS_KEY>"
    );
}

#[test]
fn benign_text_unchanged() {
    assert_eq!(scrub_text("cargo test --release"), "cargo test --release");
    assert_eq!(scrub_text("git status"), "git status");
}
