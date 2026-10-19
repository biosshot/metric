//! The pinned `sentry-cli` packages in `sdk-tests/sentry-cli`, shared by the contract tests.

use std::{
    error::Error,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use wait_timeout::ChildExt;

/// The selected current 3.x release and the retained 2.x compatibility release.
pub const PACKAGES: [&str; 2] = ["@sentry/cli", "sentry-cli-v2"];

pub fn cli_executable(root: &Path, package: &str) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
    // Let the pinned npm package select its native binary for the current OS/arch.
    let mut child = Command::new("node")
        .current_dir(root)
        .args([
            "-e",
            "const cli = require(process.argv[1]); process.stdout.write((cli.SentryCli ?? cli).getPath())",
            package,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if child.wait_timeout(Duration::from_secs(20))?.is_none() {
        child.kill()?;
        child.wait()?;
        return Err(format!("timed out resolving the native binary for {package}").into());
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "could not resolve {package}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let executable = PathBuf::from(String::from_utf8(output.stdout)?);
    if !executable.is_file() {
        return Err(format!(
            "resolved {package} binary does not exist: {}",
            executable.display()
        )
        .into());
    }
    Ok(executable)
}

pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("server crate lives under repository/crates")
        .to_path_buf()
}
