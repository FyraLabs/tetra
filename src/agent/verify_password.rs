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
/// Errors only on spawn failure or missing helper.
pub fn verify_password(username: &str, password: &str) -> Result<bool> {
    if username.is_empty() || password.is_empty() {
        bail!("username and password must be non-empty");
    }

    let Ok(mut child) = Command::new("/usr/sbin/unix_chkpwd")
        .arg(username)
        .arg("invoke")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        // Fallback: try /sbin/unix_chkpwd on some distributions
        let mut child = Command::new("/sbin/unix_chkpwd")
            .arg(username)
            .arg("invoke")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn unix_chkpwd")?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(password.as_bytes())?;
        drop(stdin);
        let status = child.wait().context("unix_chkpwd exited abnormally")?;
        return Ok(status.success());
    };

    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(password.as_bytes())?;
    drop(stdin);
    let status = child.wait().context("unix_chkpwd exited abnormally")?;
    Ok(status.success())
}
