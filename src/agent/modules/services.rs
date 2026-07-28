//! systemd service inspection and control via `systemctl` / `journalctl`.
//!
//! The services module is the control plane's window into the host's service
//! manager. Read actions (`list`, `status`, `logs`) run unconditionally;//! mutating actions (`start`/`stop`/`restart`/`enable`/`disable` and
//! `daemon_reload`) honor the shared `dry_run` flag so a controller can preview
//! the exact `systemctl` invocation before applying it.
//!
//! Both system and per-user systemd scopes are supported: a `scope` field on
//! the payload selects which, and is translated into the `--user` flag (or its
//! absence) on every `systemctl`/`journalctl` invocation. `list` additionally
//! parses the raw `list-units` table into a stable `services` array.

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    agent::{
        AgentModule,
        module_support::{ModuleInfo, ModuleStatus, parse_payload},
    },
    types::{DaemonReloadRequest, ServiceLogsRequest, ServiceRequest, ServiceStatus},
};

/// Marker type for the services module. Stateless; all behavior lives in the
/// [`AgentModule`] impl and the static [`INFO`] descriptor.
pub struct ServicesModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "services",
    feature: "services",
    description: "Inspect and control systemd services, including logs and enablement state.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "status",
        "logs",
        "daemon_reload",
        "start",
        "stop",
        "restart",
        "enable",
        "disable",
    ],
    privileged_actions: &[
        "daemon_reload",
        "start",
        "stop",
        "restart",
        "enable",
        "disable",
    ],
};

impl AgentModule for ServicesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the module descriptor first.
        if let Some(response) = INFO.metadata_response(action, &payload) {
            return Ok(response);
        }

        match action {
            // `list-units` is a read, so it is never dry-run. The flags below
            // ask systemctl for machine-friendly output: `--plain` disables
            // column alignment/headers, `--legend=false` drops the summary
            // footer, and `--no-pager` avoids interactive pagers. We still
            // return the raw stdout alongside the parsed `services` array so a
            // controller can fall back to the verbatim table if needed.
            "list" => {
                let result = crate::cmd!({ &INFO, action, user } "systemctl" [
                    "--no-pager",
                    "--plain",
                    "--legend=false",
                    "list-units",
                    "--type=service",
                    "--all",
                ])?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "services": ServiceStatus::parse_all(&result.stdout),
                }))
            }
            "status" => {
                let payload: ServiceRequest = parse_payload(payload)?;
                let args = payload.scope.command_args([
                    "--no-pager",
                    "--plain",
                    "status",
                    &payload.service,
                ]);
                crate::cmd!({ &INFO, action, user } "systemctl" => &args ; json)
            }
            "logs" => {
                let payload: ServiceLogsRequest = parse_payload(payload)?;
                // `journalctl -u <unit>` follows the unit's journal across
                // whatever files it spans; `-n` caps the tail to keep payloads
                // bounded. `logs` is a read and therefore never dry-run.
                let lines = payload.lines.to_string();
                let args = payload.scope.command_args([
                    "--no-pager",
                    "-u",
                    &payload.service,
                    "-n",
                    &lines,
                ]);
                crate::cmd!({ &INFO, action, user } "journalctl" => &args ; json)
            }
            "daemon_reload" => {
                let payload: DaemonReloadRequest = parse_payload(payload)?;
                let args = payload.scope.command_args(["daemon-reload"]);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" => &args ; json)
            }
            // The five single-service mutations share one arm because their
            // `systemctl` invocation has identical shape: `systemctl <action>
            // <service>`. The `action` string is already a valid systemctl
            // subcommand, which is why it can be forwarded directly.
            "start" | "stop" | "restart" | "enable" | "disable" => {
                let payload: ServiceRequest = parse_payload(payload)?;
                let args = payload.scope.command_args([action, &payload.service]);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" => &args ; json)
            }
            _ => INFO.unsupported_action(action),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn parses_systemctl_service_rows() {
        let services =
            ServiceStatus::parse_all("sshd.service loaded active running OpenSSH server daemon\n");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].unit, "sshd.service");
        assert_eq!(services[0].description, "OpenSSH server daemon");
    }

    #[test]
    fn dry_run_start_does_not_call_systemctl() {
        let response = ServicesModule
            .handle(
                "start",
                json!({ "service": "sshd.service", "dry_run": true }),
                None,
            )
            .unwrap();

        assert_eq!(response["command"], "systemctl start sshd.service");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn daemon_reload_supports_dry_run() {
        let response = ServicesModule
            .handle("daemon_reload", json!({ "dry_run": true }), None)
            .unwrap();

        assert_eq!(response["command"], "systemctl daemon-reload");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn user_scope_adds_systemctl_user_flag() {
        let response = ServicesModule
            .handle(
                "start",
                json!({ "service": "tetra-demo.service", "scope": "user", "dry_run": true }),
                None,
            )
            .unwrap();

        assert_eq!(
            response["command"],
            "systemctl --user start tetra-demo.service"
        );
    }
}
