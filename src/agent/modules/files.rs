//! File read/write module for managed host configuration.
//!
//! `files` is the lowest-level persistence module: it lets the control plane
//! read an arbitrary path and write contents to it, optionally relabeling the
//! result with SELinux via the shared `apply_selinux` helper. Other modules
//! (network, quadlets, samba, nfs, storage) reuse the same write + SELinux
//! pattern but scope writes to their own managed paths; this module is the
//! general-purpose escape hatch.

use crate::prelude::*;

use crate::agent::module_support::{
    ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata, unsupported_action,
};

/// Marker type for the files module. Stateless; all behavior lives in the
/// [`AgentModule`] impl and the static [`INFO`] descriptor.
pub struct FileModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "files",
    feature: "files",
    description: "Read and write host files for managed configuration.",
    status: ModuleStatus::Available,
    actions: &["capabilities", "read", "write"],
    privileged_actions: &["write"],
};

/// Payload for the `read` action: just the path to read from the host.
#[derive(Debug, Deserialize)]
struct ReadPayload {
    path: PathBuf,
}

/// Payload for the `write` action. `dry_run` skips the filesystem mutation but
/// still echoes the planned result; `selinux` optionally relabels the written
/// file via the shared `apply_selinux` helper.
#[derive(Debug, Deserialize)]
struct WritePayload {
    path: PathBuf,
    contents: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

impl AgentModule for FileModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, _user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = handle_metadata(INFO, action, &payload) {
            return Ok(response);
        }

        match action {
            "read" => {
                let payload: ReadPayload = serde_json::from_value(payload)?;
                let contents = fs::read_to_string(&payload.path)
                    .with_context(|| format!("failed to read `{}`", payload.path.display()))?;
                Ok(jsonf! { payload.path, contents })
            }
            "write" => {
                let payload: WritePayload = serde_json::from_value(payload)?;
                // Skip the actual write in dry-run; the SELinux plan below is
                // still computed and echoed back so callers can preview it.
                if !payload.dry_run {
                    fs::write(&payload.path, payload.contents)
                        .with_context(|| format!("failed to write `{}`", payload.path.display()))?;
                }
                // `apply_selinux` is a no-op when no options are supplied, so
                // calling it unconditionally is safe. It returns a list of
                // command-result objects (empty when nothing was requested).
                let selinux = apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&payload.path),
                    payload.dry_run,
                )?;
                Ok(jsonf! { payload.path, "written": !payload.dry_run, payload.dry_run, selinux })
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_write_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.conf");
        let response = FileModule
            .handle(
                "write",
                jsonf! { path, "contents": "enabled=true\n", "dry_run": true },
                None,
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(response["dry_run"], true);
        assert!(!dir.path().join("managed.conf").exists());
    }

    #[test]
    fn write_can_apply_selinux_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.conf");
        let response = FileModule
            .handle(
                "write",
                jsonf! {
                    path,
                    "contents": "enabled=true\n",
                    "dry_run": true,
                    "selinux": {
                        "context_type": "container_file_t"
                    }
                },
                None,
            )
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
