//! Actual composed server, native Docker DB, HTTP auth/ingest/finalization/restart.
use std::process::Command;

#[test]
#[ignore = "requires Docker PostgreSQL fixture and Python; no silent skip"]
fn postgres_runtime_without_mongo_auth_ingest_scope_and_restart() {
    let report = run_suite("server");
    assert!(report["passed"].as_u64().unwrap() > 0);
    assert_eq!(report["schema_generation"], 21);
}

#[test]
#[ignore = "requires Docker PostgreSQL TCP/TLS fixtures and local certificates"]
fn postgres_protected_startup_exits_on_permanent_tls_failures() {
    let report = run_suite("startup");
    assert_eq!(report["passed"], 5);
}

fn run_suite(suite: &str) -> serde_json::Value {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new("python")
        .current_dir(workspace)
        .args([
            "scripts/test-postgres.py",
            "--suite",
            suite,
            "--binary",
            env!("CARGO_BIN_EXE_metric-server"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
