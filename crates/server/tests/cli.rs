use std::{fs, path::PathBuf, process::Command};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_metric-server")
}

fn temporary_config(label: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "metric-cli-{label}-{}-{}.toml",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn check_config_succeeds_with_clean_defaults() {
    let output = Command::new(binary())
        .arg("--check-config")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "configuration is valid\n"
    );
}

#[test]
fn container_config_passes_the_production_config_gate() {
    let config =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/metric.container.toml");
    let output = Command::new(binary())
        .args(["--config", config.to_str().unwrap(), "--check-config"])
        .env("MONGODB_URI", "mongodb://mongo:27017/?retryWrites=false")
        .env(
            "SCRUB_HMAC_KEY",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn effective_config_redacts_literal_secret() {
    let path = temporary_config(
        "redaction",
        r#"
[mongodb]
uri = { literal = "must-not-escape" }

[projects]
scrub_hmac_key = { literal = "0707070707070707070707070707070707070707070707070707070707070707" }

[development]
allow_literal_secrets = true
"#,
    );
    let output = Command::new(binary())
        .args([
            "--config",
            path.to_str().unwrap(),
            "--print-effective-config",
        ])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stderr}");
    assert!(!stdout.contains("must-not-escape"));
    assert!(!stderr.contains("must-not-escape"));
    assert!(!stdout.contains("0707070707070707"));
    assert!(stdout.contains("<redacted:literal>"));
}

#[test]
fn unknown_toml_field_fails_closed() {
    let path = temporary_config("unknown", "typo_field = true\n");
    let output = Command::new(binary())
        .args(["--config", path.to_str().unwrap(), "--check-config"])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert!(!output.status.success());
}

#[test]
fn unknown_app_environment_path_fails_closed() {
    let output = Command::new(binary())
        .arg("--check-config")
        .env("APP__TYPO__FIELD", "true")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn app_environment_has_precedence_over_toml() {
    let path = temporary_config(
        "precedence",
        "[server]\nhttp_address = \"127.0.0.1:3001\"\n",
    );
    let output = Command::new(binary())
        .args([
            "--config",
            path.to_str().unwrap(),
            "--print-effective-config",
        ])
        .env("APP__SERVER__HTTP_ADDRESS", "127.0.0.1:3002")
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("127.0.0.1:3002"));
    assert!(!stdout.contains("127.0.0.1:3001"));
}

#[test]
fn malformed_config_diagnostic_does_not_echo_secret_text() {
    let path = temporary_config(
        "malformed-secret",
        "[mongodb]\nuri = { literal = \"must-stay-secret\", typo = true }\n",
    );
    let output = Command::new(binary())
        .args(["--config", path.to_str().unwrap(), "--check-config"])
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success());
    assert!(!stderr.contains("must-stay-secret"));
}
