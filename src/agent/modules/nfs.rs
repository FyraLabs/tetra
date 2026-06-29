use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata, parse_payload,
        run_command_or_dry_run, unsupported_action,
    },
};

pub struct NfsModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "nfs",
    feature: "nfs",
    description: "Manage NFS exports, generated configuration, and service state.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list_exports",
        "get_config",
        "set_config",
        "reload",
        "enable",
        "disable",
    ],
};

#[derive(Debug, Deserialize)]
struct ConfigPayload {
    #[serde(default = "default_exports_path")]
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SetConfigPayload {
    #[serde(default = "default_exports_path")]
    path: PathBuf,
    contents: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

#[derive(Debug, Deserialize)]
struct DryRunPayload {
    #[serde(default)]
    dry_run: bool,
}

impl AgentModule for NfsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list_exports" => {
                let payload: ConfigPayload = parse_payload(payload)?;
                let contents = read_config(&payload.path)?;
                Ok(json!({ "path": payload.path, "exports": parse_exports(&contents) }))
            }
            "get_config" => {
                let payload: ConfigPayload = parse_payload(payload)?;
                let contents = read_config(&payload.path)?;
                Ok(json!({ "path": payload.path, "contents": contents }))
            }
            "set_config" => {
                let payload: SetConfigPayload = parse_payload(payload)?;
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
            "reload" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                run_command_or_dry_run("exportfs", ["-ra"], payload.dry_run)
            }
            "enable" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    ["enable", "--now", "nfs-server.service"],
                    payload.dry_run,
                )
            }
            "disable" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    ["disable", "--now", "nfs-server.service"],
                    payload.dry_run,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_exports_path() -> PathBuf {
    PathBuf::from("/etc/exports")
}

fn read_config(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
}

fn parse_exports(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let path = fields.next()?;
            Some(json!({
                "path": path,
                "clients": fields.collect::<Vec<_>>(),
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports_file() {
        let exports = parse_exports("# comment\n/srv/media 192.168.1.0/24(rw) *(ro)\n");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0]["path"], "/srv/media");
    }

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports");
        fs::write(&path, "/srv/media *(ro)\n").unwrap();

        let response = NfsModule
            .handle(
                "set_config",
                json!({ "path": path, "contents": "/srv/media *(rw)\n", "dry_run": true }),
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("exports")).unwrap(),
            "/srv/media *(ro)\n"
        );
    }

    #[test]
    fn set_config_can_apply_nfs_export_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports");
        let response = NfsModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "/srv/export *(rw)\n",
                    "dry_run": true,
                    "selinux": {
                        "path": "/srv/export",
                        "context_type": "public_content_rw_t",
                        "recursive": true
                    }
                }),
            )
            .unwrap();

        assert_eq!(
            response["selinux"][0]["command"],
            "semanage fcontext -a -t public_content_rw_t /srv/export(/.*)?"
        );
        assert_eq!(
            response["selinux"][1]["command"],
            "restorecon -R -v /srv/export"
        );
    }
}
