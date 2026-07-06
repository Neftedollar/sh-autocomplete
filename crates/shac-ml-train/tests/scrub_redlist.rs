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
fn benign_text_unchanged() {
    assert_eq!(scrub_text("cargo test --release"), "cargo test --release");
    assert_eq!(scrub_text("git status"), "git status");
}

// ---------------------------------------------------------------------------
// Must-survive: benign shell content the scrubber must NOT mangle
// (over-scrubbing pollutes the training corpus and vocab.json).
// ---------------------------------------------------------------------------

#[test]
fn host_port_survives() {
    assert_eq!(
        scrub_text("curl localhost:8080/health"),
        "curl localhost:8080/health"
    );
}

#[test]
fn docker_port_mapping_survives() {
    assert_eq!(
        scrub_text("docker run -p 8080:80 nginx"),
        "docker run -p 8080:80 nginx"
    );
}

#[test]
fn clock_times_survive() {
    assert_eq!(scrub_text("git log --since 12:30"), "git log --since 12:30");
    assert_eq!(
        scrub_text("journalctl --since 12:30:45"),
        "journalctl --since 12:30:45"
    );
}

#[test]
fn long_home_path_survives_beyond_home_replacement() {
    assert_eq!(
        scrub_text("cd /Users/roman/dev/very-long-project-name/src"),
        "cd <HOME>/dev/very-long-project-name/src"
    );
}

#[test]
fn env_var_assignment_survives() {
    assert_eq!(
        scrub_text("export CARGO_TARGET_DIR=target/debug-build"),
        "export CARGO_TARGET_DIR=target/debug-build"
    );
}

#[test]
fn long_flag_with_path_value_survives() {
    assert_eq!(
        scrub_text("npm run build --workspace=packages/web-frontend"),
        "npm run build --workspace=packages/web-frontend"
    );
}

#[test]
fn long_relative_path_survives() {
    assert_eq!(
        scrub_text("wc -l crates/shac-ml-train/src/bin/gen_synthetic.rs"),
        "wc -l crates/shac-ml-train/src/bin/gen_synthetic.rs"
    );
}

#[test]
fn flag_equals_long_value_survives() {
    assert_eq!(
        scrub_text("cargo build --target-dir=custom/build-output"),
        "cargo build --target-dir=custom/build-output"
    );
}

// ---------------------------------------------------------------------------
// Must-scrub: secret shapes shared with shac::import::SECRET_PATTERNS
// plus regressions for the structural rules.
// ---------------------------------------------------------------------------

#[test]
fn aws_access_key_replaced() {
    let out = scrub_text("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE");
    assert_eq!(out, "export AWS_ACCESS_KEY_ID=<TOKEN>");
}

#[test]
fn aws_temporary_key_replaced() {
    let out = scrub_text("export AWS_ACCESS_KEY_ID=ASIAIOSFODNN7EXAMPLE");
    assert_eq!(out, "export AWS_ACCESS_KEY_ID=<TOKEN>");
}

#[test]
fn db_connection_string_password_scrubbed() {
    let out = scrub_text("psql postgres://roman:hunter2@localhost:5432/db");
    assert!(!out.contains("hunter2"), "password survived: {out}");
    assert!(!out.contains("roman"), "db user survived: {out}");
    // Host/port/db structure stays useful for training.
    assert!(out.contains("localhost:5432/db"), "structure lost: {out}");
}

#[test]
fn slack_token_replaced() {
    let out = scrub_text(&format!("slack chat send --token xoxb{}", "-2444333222111-AbCdEfGhIjKlMnOp"));
    assert!(!out.contains("xoxb"), "slack token survived: {out}");
    assert!(out.contains("<TOKEN>"));
}

#[test]
fn sk_key_replaced() {
    let out = scrub_text("export OPENAI_API_KEY=sk-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
    assert_eq!(out, "export OPENAI_API_KEY=<TOKEN>");
}

#[test]
fn jwt_fully_replaced() {
    let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9\
               .eyJzdWIiOiIxMjM0NTY3ODkwIn0\
               .SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    let out = scrub_text(&format!("curl -H 'Authorization: Bearer {jwt}'"));
    assert!(!out.contains("eyJ"), "jwt header/payload survived: {out}");
    assert!(!out.contains("SflKxw"), "jwt signature survived: {out}");
    assert!(out.contains("<TOKEN>"));
}

#[test]
fn home_and_email_regression() {
    assert_eq!(
        scrub_text("cd /Users/roman && git config user.email roman@example.com"),
        "cd <HOME> && git config user.email <EMAIL>"
    );
}

#[test]
fn ipv6_shapes_replaced() {
    assert_eq!(scrub_text("ping6 fe80::1"), "ping6 <IP>");
    assert_eq!(scrub_text("ping6 ::1"), "ping6 <IP>");
    assert_eq!(
        scrub_text("curl http://[::1]:8080/health"),
        "curl http://[<IP>]:8080/health"
    );
    assert_eq!(
        scrub_text("ssh 2001:0db8:85a3:0000:0000:8a2e:0370:7334"),
        "ssh <IP>"
    );
}

#[test]
fn rust_module_path_with_hex_segment_survives() {
    // `add` is all-hex, so without the left-context requirement the `::b`
    // IPv6 branch used to turn this into `cargo test scrub<IP>`.
    assert_eq!(scrub_text("cargo test scrub::add"), "cargo test scrub::add");
    assert_eq!(
        scrub_text("rg shac::import::SECRET_PATTERNS src"),
        "rg shac::import::SECRET_PATTERNS src"
    );
}

// ---------------------------------------------------------------------------
// Key-context rule: `<secret-ish key>=<value>` / `<secret-ish key> <value>`
// scrub regardless of the value's alphabet.
// ---------------------------------------------------------------------------

#[test]
fn aws_secret_access_key_assignment_replaced() {
    // 40-char base64-ish value with slashes: escapes the generic token rule,
    // must be caught by key context.
    let out = scrub_text("export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    assert_eq!(out, "export AWS_SECRET_ACCESS_KEY=<TOKEN>");
}

#[test]
fn aws_secret_access_key_space_form_replaced() {
    let out = scrub_text(
        "aws configure set aws_secret_access_key wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
    );
    assert_eq!(out, "aws configure set aws_secret_access_key <TOKEN>");
}

#[test]
fn password_assignments_replaced() {
    assert_eq!(
        scrub_text("export DB_PASSWORD=hunter2"),
        "export DB_PASSWORD=<TOKEN>"
    );
    assert_eq!(
        scrub_text("mysql -u root --password=hunter2"),
        "mysql -u root --password=<TOKEN>"
    );
}

#[test]
fn password_stdin_flag_survives() {
    // Key-context rules must not treat a value-less flag as a key.
    assert_eq!(
        scrub_text("docker login -u roman --password-stdin"),
        "docker login -u roman --password-stdin"
    );
    assert_eq!(
        scrub_text("docker login --password-stdin registry.example.internal"),
        "docker login --password-stdin registry.example.internal"
    );
}

#[test]
fn secret_wordlike_identifiers_survive() {
    // 'tokenizer' contains 'token' but the word is not `_`/`-` delimited.
    assert_eq!(
        scrub_text("cargo test tokenizer -- --nocapture"),
        "cargo test tokenizer -- --nocapture"
    );
    // Bare 'token' followed by a short non-secret argument.
    assert_eq!(
        scrub_text("rg -n token src/import.rs"),
        "rg -n token src/import.rs"
    );
}

// ---------------------------------------------------------------------------
// Well-known token prefixes added to SECRET_PATTERNS (full-token replacement).
// ---------------------------------------------------------------------------

#[test]
fn gitlab_pat_replaced() {
    let out = scrub_text("curl -H \"PRIVATE-TOKEN: glpat-AbCdEfGhIjKlMnOpQrSt\" https://gitlab.example.internal/api/v4/projects");
    assert_eq!(
        out,
        "curl -H \"PRIVATE-TOKEN: <TOKEN>\" https://gitlab.example.internal/api/v4/projects"
    );
}

#[test]
fn github_oauth_token_replaced() {
    // Exact equality proves the pattern consumes the `gho_` prefix too
    // (the generic rule alone would leave `gho_<TOKEN>`).
    let out = scrub_text("echo gho_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
    assert_eq!(out, "echo <TOKEN>");
}

#[test]
fn npm_token_replaced() {
    let out = scrub_text("echo npm_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
    assert_eq!(out, "echo <TOKEN>");
}

// ---------------------------------------------------------------------------
// '='-padded base64 blobs, with and without '/'.
// ---------------------------------------------------------------------------

#[test]
fn basic_auth_base64_replaced() {
    let out = scrub_text("curl -H \"Authorization: Basic dXNlcjpzM2NyM3RwYXNzd29yZA==\"");
    assert_eq!(out, "curl -H \"Authorization: Basic <TOKEN>\"");
}

#[test]
fn padded_base64_with_slash_replaced() {
    // Slash-containing base64 escapes the generic token rule; the padding
    // rule must consume the whole blob.
    let out = scrub_text("echo AbCd/EfGh+IjKl/MnOpQrStUvWx0123== | base64 -d");
    assert_eq!(out, "echo <TOKEN> | base64 -d");
}
