use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata, parse_payload,
        run_command_json, run_command_or_dry_run, unsupported_action,
    },
};

pub struct NetworkModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "network",
    feature: "network",
    description: "Inspect and configure host network interfaces, addresses, DNS, and routes.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "interfaces",
        "status",
        "get_config",
        "set_config",
        "reload",
    ],
};

#[derive(Debug, Deserialize)]
struct InterfacePayload {
    interface: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfigPayload {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SetConfigPayload {
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

impl AgentModule for NetworkModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "interfaces" => Ok(json!({ "interfaces": read_interfaces().unwrap_or_default() })),
            "status" => {
                let payload: InterfacePayload = parse_payload(payload)?;
                let mut args = vec!["-json".to_string(), "addr".to_string(), "show".to_string()];
                if let Some(interface) = payload.interface {
                    args.push("dev".into());
                    args.push(interface);
                }
                run_command_json("ip", args)
            }
            "get_config" => {
                let payload: ConfigPayload = parse_payload(payload)?;
                let contents = fs::read_to_string(&payload.path)
                    .with_context(|| format!("failed to read `{}`", payload.path.display()))?;
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
                run_command_or_dry_run(
                    "systemctl",
                    ["reload-or-restart", "NetworkManager.service"],
                    payload.dry_run,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn read_interfaces() -> Result<Vec<Value>> {
    let mut interfaces = Vec::new();
    for entry in fs::read_dir("/sys/class/net").context("failed to read /sys/class/net")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let operstate = fs::read_to_string(entry.path().join("operstate"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let address = fs::read_to_string(entry.path().join("address"))
            .unwrap_or_default()
            .trim()
            .to_string();
        interfaces.push(json!({
            "name": name,
            "operstate": operstate,
            "mac": address,
        }));
    }
    interfaces.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(interfaces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");
        fs::write(&path, "[connection]\nid=old\n").unwrap();

        let response = NetworkModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "[connection]\nid=new\n",
                    "dry_run": true
                }),
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("connection.nmconnection")).unwrap(),
            "[connection]\nid=old\n"
        );
    }

    #[test]
    fn set_config_can_restore_selinux_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");

        let response = NetworkModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "[connection]\nid=new\n",
                    "dry_run": true,
                    "selinux": {
                        "context_type": "NetworkManager_etc_t"
                    }
                }),
            )
            .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert!(
            response["selinux"][0]["command"]
                .as_str()
                .unwrap()
                .contains("semanage fcontext -a -t NetworkManager_etc_t")
        );
    }
}
