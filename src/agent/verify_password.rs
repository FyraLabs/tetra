//! Password verification for headless elevation.
//!
//! On a headless server, the dashboard must provide the administrator password
//! directly. Tetra verifies it against the host's shadow database without
//! retaining the plaintext.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Verify a password for `username` using the host's shadow database.
///
/// This uses `/usr/sbin/unix_chkpwd` when available (part of Linux-PAM and
/// present on Fedora/RHEL systems). Since Tetra runs as root it can invoke
/// the helper directly.
///
/// Returns `true` when the password is correct, `false` otherwise.
///
/// # Errors
/// Errors only on spawn failure or missing helper.
pub fn verify_password(username: &str, password: &str) -> Result<bool> {
    if username.is_empty() || password.is_empty() {
        bail!("username and password must be non-empty");
    }

    let spawn_cmd = |program: &str| {
        Command::new(program)
            .arg(username)
            .arg("invoke")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };

    let mut child = spawn_cmd("/usr/sbin/unix_chkpwd").or_else(|e| {
        spawn_cmd("/sbin/unix_chkpwd")
            .context("failed to spawn unix_chkpwd")
            .context(e)
    })?;

    (child.stdin.take().context("no stdin")?).write_all(password.as_bytes())?;
    let status = child.wait().context("unix_chkpwd exited abnormally")?;
    Ok(status.success())
}
