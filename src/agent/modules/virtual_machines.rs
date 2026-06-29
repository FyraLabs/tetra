use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, NamedPayload, handle_metadata, parse_payload, run_command,
        run_command_or_dry_run, run_command_output, unsupported_action,
    },
};

pub struct VirtualMachinesModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "virtual_machines",
    feature: "virtual-machines",
    description: "Inspect and control virtual machines, images, storage pools, and console/log access.",
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
        "create",
        "delete",
    ],
};

#[derive(Debug, Deserialize)]
struct CreatePayload {
    xml_path: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct LogsPayload {
    #[serde(default = "default_log_lines")]
    lines: u16,
}

impl AgentModule for VirtualMachinesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => {
                let result = run_command_output("virsh", ["list", "--all"], false)?;
                let domains = parse_virsh_list(&result.stdout);
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "domains": domains,
                }))
            }
            "status" => {
                let payload: NamedPayload = parse_payload(payload)?;
                let result = run_command_output("virsh", ["dominfo", &payload.name], false)?;
                let info = parse_virsh_dominfo(&result.stdout);
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "domain": info,
                }))
            }
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                run_command(
                    "journalctl",
                    [
                        "--no-pager",
                        "-u",
                        "libvirtd.service",
                        "-n",
                        &payload.lines.to_string(),
                    ],
                )
            }
            "start" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("virsh", ["start", &payload.name], payload.dry_run)
            }
            "stop" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("virsh", ["shutdown", &payload.name], payload.dry_run)
            }
            "restart" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("virsh", ["reboot", &payload.name], payload.dry_run)
            }
            "create" => {
                let payload: CreatePayload = parse_payload(payload)?;
                run_command_or_dry_run("virsh", ["define", &payload.xml_path], payload.dry_run)
            }
            "delete" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("virsh", ["undefine", &payload.name], payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_log_lines() -> u16 {
    100
}

fn parse_virsh_list(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .skip_while(|line| !line.trim_start().starts_with("---"))
        .skip(1)
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            let id = fields.next()?;
            let name = fields.next()?;
            Some(json!({
                "id": (id != "-").then_some(id),
                "name": name,
                "state": fields.collect::<Vec<_>>().join(" "),
            }))
        })
        .collect()
}

fn parse_virsh_dominfo(stdout: &str) -> Value {
    let mut object = serde_json::Map::new();
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        object.insert(
            key.trim().to_lowercase().replace(' ', "_"),
            Value::String(value.trim().to_string()),
        );
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn dry_run_start_does_not_call_virsh() {
        let response = VirtualMachinesModule
            .handle("start", json!({ "name": "vm1", "dry_run": true }))
            .unwrap();

        assert_eq!(response["command"], "virsh start vm1");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn parses_virsh_list_output() {
        let domains = parse_virsh_list(
            " Id   Name      State\n--------------------------\n 1    vm1       running\n -    vm2       shut off\n",
        );
        assert_eq!(domains.len(), 2);
        assert_eq!(domains[0]["name"], "vm1");
        assert_eq!(domains[1]["state"], "shut off");
    }

    #[test]
    fn parses_virsh_dominfo_output() {
        let domain = parse_virsh_dominfo("Name: vm1\nState: running\nCPU(s): 2\n");
        assert_eq!(domain["name"], "vm1");
        assert_eq!(domain["state"], "running");
        assert_eq!(domain["cpu(s)"], "2");
    }
}
