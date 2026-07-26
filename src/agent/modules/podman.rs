//! Podman container, image, volume, and network inspection and lifecycle.
//!
//! A thin wrapper around the `podman` CLI. The listing actions (`containers`,
//! `images`, `volumes`, `networks`, `inspect`) all rely on podman's native
//! `--format json`, so [`run_command_json`] parses stdout straight into the
//! response `data` field with no module-specific parsing of our own. The
//! mutating lifecycle actions (`start`/`stop`/`restart`/`remove`) honor the
//! shared `dry_run` flag. Note the action name `remove` maps to the `podman rm`
//! subcommand, keeping the protocol verb consistent across modules while the
//! underlying CLI uses its native spelling.

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, NamedPayload, handle_metadata, parse_payload,
        run_command_for_module, run_command_json_for_module, run_command_or_dry_run_for_module,
        unsupported_action,
    },
};

/// Marker type for the podman module. Stateless; all behavior lives in the
/// [`AgentModule`] impl and the static [`INFO`] descriptor.
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
    privileged_actions: &["start", "stop", "restart", "remove"],
};

/// Payload for `logs`: which container to tail and how many trailing lines to
/// return (defaults to 100).
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

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            // Listing actions lean on podman's own `--format json`, which emits
            // a JSON array directly. `run_command_json` parses that stdout and
            // exposes it as `data`, alongside the raw command/result fields.
            "containers" => run_command_json_for_module(
                &INFO,
                action,
                "podman",
                ["ps", "--all", "--format", "json"],
                user,
            ),
            // `inspect` returns a rich JSON document for a single container (or
            // image/volume/network when qualified). It is passed through as-is.
            "inspect" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_json_for_module(
                    &INFO,
                    action,
                    "podman",
                    ["inspect", &payload.name],
                    user,
                )
            }
            "images" => run_command_json_for_module(
                &INFO,
                action,
                "podman",
                ["images", "--format", "json"],
                user,
            ),
            "volumes" => run_command_json_for_module(
                &INFO,
                action,
                "podman",
                ["volume", "ls", "--format", "json"],
                user,
            ),
            "networks" => run_command_json_for_module(
                &INFO,
                action,
                "podman",
                ["network", "ls", "--format", "json"],
                user,
            ),
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                // `--tail` bounds the response; `logs` is a read and therefore
                // never dry-run.
                run_command_for_module(
                    &INFO,
                    action,
                    "podman",
                    ["logs", "--tail", &payload.lines.to_string(), &payload.name],
                    user,
                )
            }
            // Lifecycle verbs share the trivial `podman <verb> <name>` shape, so
            // the action string is forwarded directly as the subcommand.
            "start" | "stop" | "restart" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run_for_module(
                    &INFO,
                    action,
                    "podman",
                    [action, &payload.name],
                    payload.dry_run,
                    user,
                )
            }
            // Protocol verb is `remove`; the podman subcommand is `rm`.
            "remove" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run_for_module(
                    &INFO,
                    action,
                    "podman",
                    ["rm", &payload.name],
                    payload.dry_run,
                    user,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

const fn default_log_lines() -> u16 {
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
            .handle("remove", json!({ "name": "app", "dry_run": true }), None)
            .unwrap();

        assert_eq!(response["command"], "podman rm app");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn inspect_requires_name_payload() {
        let response = PodmanModule.handle("inspect", json!({}), None).unwrap_err();
        assert!(response.to_string().contains("invalid command payload"));
    }
}
