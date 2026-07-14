use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload, run_command,
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
        "daemon_reload",
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
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default = "default_log_lines")]
    lines: u16,
}

#[derive(Debug, Deserialize)]
struct DaemonReloadPayload {
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct ServicePayload {
    service: String,
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum ServiceScope {
    #[default]
    System,
    User,
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
                    systemctl_args(
                        payload.scope,
                        ["--no-pager", "--plain", "status", &payload.service],
                    ),
                )
            }
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                run_command(
                    "journalctl",
                    journalctl_args(
                        payload.scope,
                        [
                            "--no-pager",
                            "-u",
                            &payload.service,
                            "-n",
                            &payload.lines.to_string(),
                        ],
                    ),
                )
            }
            "daemon_reload" => {
                let payload: DaemonReloadPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    systemctl_args(payload.scope, ["daemon-reload"]),
                    payload.dry_run,
                )
            }
            "start" | "stop" | "restart" | "enable" | "disable" => {
                let payload: ServicePayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    systemctl_args(payload.scope, [action, &payload.service]),
                    payload.dry_run,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn systemctl_args<const N: usize>(scope: ServiceScope, args: [&str; N]) -> Vec<&str> {
    match scope {
        ServiceScope::System => args.to_vec(),
        ServiceScope::User => {
            let mut scoped = Vec::with_capacity(args.len() + 1);
            scoped.push("--user");
            scoped.extend(args);
            scoped
        }
    }
}

fn journalctl_args<const N: usize>(scope: ServiceScope, args: [&str; N]) -> Vec<&str> {
    match scope {
        ServiceScope::System => args.to_vec(),
        ServiceScope::User => {
            let mut scoped = Vec::with_capacity(args.len() + 1);
            scoped.push("--user");
            scoped.extend(args);
            scoped
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

    #[test]
    fn daemon_reload_supports_dry_run() {
        let response = ServicesModule
            .handle("daemon_reload", json!({ "dry_run": true }))
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
            )
            .unwrap();

        assert_eq!(
            response["command"],
            "systemctl --user start tetra-demo.service"
        );
    }
}
