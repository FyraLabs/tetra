use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, NamedPayload, handle_metadata, parse_payload, run_command,
        run_command_json, run_command_or_dry_run, unsupported_action,
    },
};

pub struct PodmanModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "podman",
    feature: "podman",
    description: "Inspect and manage Podman containers, images, volumes, networks, and logs.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "containers",
        "inspect",
        "images",
        "volumes",
        "networks",
        "logs",
        "start",
        "stop",
        "restart",
        "remove",
    ],
};

#[derive(Debug, Deserialize)]
struct LogsPayload {
    name: String,
    #[serde(default = "default_log_lines")]
    lines: u16,
}

impl AgentModule for PodmanModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "containers" => run_command_json("podman", ["ps", "--all", "--format", "json"]),
            "inspect" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_json("podman", ["inspect", &payload.name])
            }
            "images" => run_command_json("podman", ["images", "--format", "json"]),
            "volumes" => run_command_json("podman", ["volume", "ls", "--format", "json"]),
            "networks" => run_command_json("podman", ["network", "ls", "--format", "json"]),
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                run_command(
                    "podman",
                    ["logs", "--tail", &payload.lines.to_string(), &payload.name],
                )
            }
            "start" | "stop" | "restart" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("podman", [action, &payload.name], payload.dry_run)
            }
            "remove" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("podman", ["rm", &payload.name], payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_log_lines() -> u16 {
    100
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn dry_run_remove_does_not_call_podman() {
        let response = PodmanModule
            .handle("remove", json!({ "name": "app", "dry_run": true }))
            .unwrap();

        assert_eq!(response["command"], "podman rm app");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn inspect_requires_name_payload() {
        let response = PodmanModule.handle("inspect", json!({})).unwrap_err();
        assert!(response.to_string().contains("invalid command payload"));
    }
}
