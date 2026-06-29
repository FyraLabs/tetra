use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata,
        unsupported_action,
    },
};

pub struct FileModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "files",
    feature: "files",
    description: "Read and write host files for managed configuration.",
    status: ModuleStatus::Available,
    actions: &["capabilities", "read", "write"],
};

#[derive(Debug, Deserialize)]
struct ReadPayload {
    path: PathBuf,
}

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

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "read" => {
                let payload: ReadPayload = serde_json::from_value(payload)?;
                let contents = fs::read_to_string(&payload.path)
                    .with_context(|| format!("failed to read `{}`", payload.path.display()))?;
                Ok(json!({ "path": payload.path, "contents": contents }))
            }
            "write" => {
                let payload: WritePayload = serde_json::from_value(payload)?;
                if !payload.dry_run {
                    fs::write(&payload.path, payload.contents)
                        .with_context(|| format!("failed to write `{}`", payload.path.display()))?;
                }
                let selinux = apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&payload.path),
                    payload.dry_run,
                )?;
                Ok(json!({
                    "path": payload.path,
                    "written": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "selinux": selinux,
                }))
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn dry_run_write_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("managed.conf");
        let response = FileModule
            .handle(
                "write",
                json!({ "path": path, "contents": "enabled=true\n", "dry_run": true }),
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
                json!({
                    "path": path,
                    "contents": "enabled=true\n",
                    "dry_run": true,
                    "selinux": {
                        "context_type": "container_file_t"
                    }
                }),
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
