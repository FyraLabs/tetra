use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, ServicePayload, handle_metadata, parse_payload, run_command,
        run_command_or_dry_run, run_command_output, unsupported_action,
    },
};

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
        "start",
        "stop",
        "restart",
        "enable",
        "disable",
    ],
};

#[derive(Debug, Deserialize)]
struct LogsPayload {
    service: String,
    #[serde(default = "default_log_lines")]
    lines: u16,
}

impl AgentModule for ServicesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => {
                let result = run_command_output(
                    "systemctl",
                    [
                        "--no-pager",
                        "--plain",
                        "--legend=false",
                        "list-units",
                        "--type=service",
                        "--all",
                    ],
                    false,
                )?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "services": parse_systemctl_services(&result.stdout),
                }))
            }
            "status" => {
                let payload: ServicePayload = parse_payload(payload)?;
                run_command(
                    "systemctl",
                    ["--no-pager", "--plain", "status", &payload.service],
                )
            }
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                run_command(
                    "journalctl",
                    [
                        "--no-pager",
                        "-u",
                        &payload.service,
                        "-n",
                        &payload.lines.to_string(),
                    ],
                )
            }
            "start" | "stop" | "restart" | "enable" | "disable" => {
                let payload: ServicePayload = parse_payload(payload)?;
                run_command_or_dry_run("systemctl", [action, &payload.service], payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_log_lines() -> u16 {
    100
}

fn parse_systemctl_services(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 4).then(|| {
                json!({
                    "unit": fields[0],
                    "load": fields[1],
                    "active": fields[2],
                    "sub": fields[3],
                    "description": fields.get(4..).unwrap_or(&[]).join(" "),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn parses_systemctl_service_rows() {
        let services =
            parse_systemctl_services("sshd.service loaded active running OpenSSH server daemon\n");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["unit"], "sshd.service");
        assert_eq!(services[0]["description"], "OpenSSH server daemon");
    }

    #[test]
    fn dry_run_start_does_not_call_systemctl() {
        let response = ServicesModule
            .handle(
                "start",
                json!({ "service": "sshd.service", "dry_run": true }),
            )
            .unwrap();

        assert_eq!(response["command"], "systemctl start sshd.service");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
