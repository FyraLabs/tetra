//! Persistent Tetra host identity for authenticated transport enrollment.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ed25519_dalek::{SigningKey, VerifyingKey};

const PRIVATE_KEY_FILE: &str = "ed25519-private.key";
const CONTROLLER_PUBLIC_KEY_FILE: &str = "controller-ed25519-public.key";
const PRIVATE_KEY_BYTES: usize = 32;

#[derive(Debug, Clone)]
pub struct HostIdentity {
    signing_key: SigningKey,
    path: PathBuf,
}

impl HostIdentity {
    pub fn load_or_generate<P: AsRef<Path>>(directory: P) -> Result<Self> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "failed to create identity directory `{}`",
                directory.display()
            )
        })?;
        let path = directory.join(PRIVATE_KEY_FILE);
        let bytes = if path.exists() {
            let bytes = fs::read(&path)
                .with_context(|| format!("failed to read host identity `{}`", path.display()))?;
            ensure!(
                bytes.len() == PRIVATE_KEY_BYTES,
                "host identity has invalid length"
            );
            bytes
        } else {
            let mut bytes = [0_u8; PRIVATE_KEY_BYTES];
            fill_random(&mut bytes)?;
            let temporary =
                directory.join(format!(".{PRIVATE_KEY_FILE}.tmp-{}", std::process::id()));
            fs::write(&temporary, bytes).with_context(|| {
                format!(
                    "failed to write temporary host identity `{}`",
                    temporary.display()
                )
            })?;
            fs::set_permissions(&temporary, permissions_private()).with_context(|| {
                format!("failed to restrict host identity `{}`", temporary.display())
            })?;
            fs::rename(&temporary, &path)
                .with_context(|| format!("failed to install host identity `{}`", path.display()))?;
            bytes.to_vec()
        };

        let bytes: [u8; PRIVATE_KEY_BYTES] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity length mismatch"))?;
        Ok(Self {
            signing_key: SigningKey::from_bytes(&bytes),
            path,
        })
    }

    #[must_use]
    pub const fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the directory that holds the identity files.
    ///
    /// # Panics
    ///
    /// Panics if the identity path somehow has no parent directory. This
    /// should never happen because the path is built from a directory plus
    /// a filename.
    #[must_use]
    pub fn directory(&self) -> &Path {
        self.path
            .parent()
            .expect("identity path always has a parent")
    }

    pub fn load_controller_key(&self) -> Result<Option<String>> {
        let path = self.directory().join(CONTROLLER_PUBLIC_KEY_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let value = fs::read_to_string(&path)
            .with_context(|| format!("failed to read controller key `{}`", path.display()))?;
        let value = value.trim().to_owned();
        ensure!(!value.is_empty(), "controller public key is empty");
        Ok(Some(value))
    }

    pub fn enroll_controller_key(&self, public_key: &str) -> Result<()> {
        ensure!(
            !public_key.trim().is_empty(),
            "controller public key cannot be empty"
        );
        let path = self.directory().join(CONTROLLER_PUBLIC_KEY_FILE);
        let temporary = self.directory().join(format!(
            ".{CONTROLLER_PUBLIC_KEY_FILE}.tmp-{}",
            std::process::id()
        ));
        fs::write(&temporary, format!("{}\n", public_key.trim())).with_context(|| {
            format!(
                "failed to write temporary controller key `{}`",
                temporary.display()
            )
        })?;
        fs::set_permissions(&temporary, permissions_private()).with_context(|| {
            format!(
                "failed to restrict controller key `{}`",
                temporary.display()
            )
        })?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("failed to install controller key `{}`", path.display()))?;
        Ok(())
    }
}

fn fill_random(bytes: &mut [u8]) -> Result<()> {
    use std::io::Read;
    let mut file = fs::File::open("/dev/urandom").context("failed to open /dev/urandom")?;
    file.read_exact(bytes)
        .context("failed to read random host identity bytes")?;
    Ok(())
}

#[cfg(unix)]
fn permissions_private() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn permissions_private() -> std::fs::Permissions {
    std::fs::Permissions::readonly()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persists_identity_across_loads() {
        let directory = tempdir().unwrap();
        let first = HostIdentity::load_or_generate(directory.path()).unwrap();
        let first_public = first.verifying_key();
        let second = HostIdentity::load_or_generate(directory.path()).unwrap();
        assert_eq!(first_public, second.verifying_key());
        assert_eq!(second.path(), directory.path().join(PRIVATE_KEY_FILE));
    }
}
