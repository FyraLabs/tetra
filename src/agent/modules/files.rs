//! File read/write module for managed host configuration.
//!
//! `files` is the lowest-level persistence module: it lets the control plane
//! read an arbitrary path and write contents to it, optionally relabeling the
//! result with SELinux via the shared `apply_selinux` helper. Other modules
//! (network, quadlets, samba, nfs, storage) reuse the same write + SELinux
//! pattern but scope writes to their own managed paths; this module is the
//! general-purpose escape hatch.

use crate::prelude::*;

use crate::agent::module_support::{SelinuxOptions, apply_selinux};

/// Marker type for the files module. Stateless; all behavior lives in the
/// [`Mod`] impl and the static [`INFO`] descriptor.
#[derive(Clone, Copy, Debug)]
pub struct FilesModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "files",
    feature: "files",
    description: "Read and write host files for managed configuration.",
    status: ModuleStatus::Available,
    actions: &["capabilities", "read", "write"],
    privileged_actions: &["write"],
};

impl Mod for FilesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [self user] => {
    Read { path: PathBuf } => {
        let contents = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read `{}`", self.path.display()))?;
        Ok(jsonf! { self.path, contents })
    },
    Write {
        path: PathBuf,
        contents: String,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        selinux: Option<SelinuxOptions>,
    } => {
        // Skip the actual write in dry-run; the SELinux plan below is
        // still computed and echoed back so callers can preview it.
        if !self.dry_run {
            fs::write(&self.path, self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        // `apply_selinux` is a no-op when no options are supplied, so
        // calling it unconditionally is safe. It returns a list of
        // command-result objects (empty when nothing was requested).
        let selinux = apply_selinux(
            self.selinux.as_ref(),
            Some(&self.path),
            self.dry_run,
        )?;
        Ok(jsonf! { self.path, "written": !self.dry_run, self.dry_run, selinux })
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_write_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.conf");
        let response = Write {
            path,
            contents: "enabled=true\n".into(),
            dry_run: true,
            selinux: None,
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(response["dry_run"], true);
        assert!(!dir.path().join("managed.conf").exists());
    }

    #[test]
    fn write_can_apply_selinux_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.conf");
        let response = Write {
            path,
            contents: "enabled=true\n".to_owned(),
            dry_run: true,
            selinux: Some(SelinuxOptions {
                context_type: Some("container_file_t".to_owned()),
                ..SelinuxOptions::default()
            }),
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert!(
            response["selinux"][0]["command"]
                .as_str()
                .unwrap()
                .contains("semanage fcontext -a -t container_file_t")
        );
        assert!(
            response["selinux"][1]["command"]
                .as_str()
                .unwrap()
                .contains("restorecon -v")
        );
    }
}
