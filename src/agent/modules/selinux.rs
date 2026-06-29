use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload, run_command_or_dry_run,
        run_command_output, unsupported_action,
    },
};

pub struct SelinuxModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "selinux",
    feature: "selinux",
    description: "Inspect and manage SELinux mode, booleans, file contexts, and relabeling.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "status",
        "enforce",
        "booleans",
        "set_boolean",
        "file_contexts",
        "add_file_context",
        "delete_file_context",
        "restore_context",
    ],
};

#[derive(Debug, Deserialize)]
struct SetBooleanPayload {
    name: String,
    value: bool,
    #[serde(default = "default_persistent")]
    persistent: bool,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct FileContextPayload {
    path_pattern: String,
    context_type: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct DeleteFileContextPayload {
    path_pattern: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct RestoreContextPayload {
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    dry_run: bool,
}

impl AgentModule for SelinuxModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "status" => {
                let result = run_command_output("sestatus", std::iter::empty::<&str>(), false)?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "selinux": parse_sestatus(&result.stdout),
                }))
            }
            "enforce" => {
                let result = run_command_output("getenforce", std::iter::empty::<&str>(), false)?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "mode": result.stdout.trim(),
                }))
            }
            "booleans" => {
                let result = run_command_output("getsebool", ["-a"], false)?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "booleans": parse_getsebool(&result.stdout),
                }))
            }
            "set_boolean" => {
                let payload: SetBooleanPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if payload.persistent {
                    args.push("-P".to_string());
                }
                args.push(payload.name);
                args.push(boolean_value(payload.value).to_string());
                run_command_or_dry_run("setsebool", args, payload.dry_run)
            }
            "file_contexts" => {
                let result = run_command_output("semanage", ["fcontext", "-l"], false)?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "file_contexts": parse_semanage_fcontext(&result.stdout),
                }))
            }
            "add_file_context" => {
                let payload: FileContextPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "semanage",
                    [
                        "fcontext",
                        "-a",
                        "-t",
                        &payload.context_type,
                        &payload.path_pattern,
                    ],
                    payload.dry_run,
                )
            }
            "delete_file_context" => {
                let payload: DeleteFileContextPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "semanage",
                    ["fcontext", "-d", &payload.path_pattern],
                    payload.dry_run,
                )
            }
            "restore_context" => {
                let payload: RestoreContextPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if payload.recursive {
                    args.push("-R".to_string());
                }
                args.push("-v".to_string());
                args.push(payload.path);
                run_command_or_dry_run("restorecon", args, payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_persistent() -> bool {
    true
}

fn boolean_value(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn parse_sestatus(stdout: &str) -> Value {
    let mut object = serde_json::Map::new();
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        object.insert(normalize_key(key), Value::String(value.trim().to_string()));
    }
    Value::Object(object)
}

fn parse_getsebool(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once("-->")?;
            Some(json!({
                "name": name.trim(),
                "enabled": matches!(value.trim(), "on" | "1" | "true"),
                "value": value.trim(),
            }))
        })
        .collect()
}

fn parse_semanage_fcontext(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("SELinux fcontext") && !line.starts_with('-'))
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 3 {
                return None;
            }
            let context = fields[fields.len() - 1];
            Some(json!({
                "path_pattern": fields[0],
                "file_type": fields[1..fields.len() - 1].join(" "),
                "context": context,
                "context_type": extract_context_type(context),
            }))
        })
        .collect()
}

fn extract_context_type(context: &str) -> Option<&str> {
    context.split(':').nth(2)
}

fn normalize_key(key: &str) -> String {
    key.trim().to_lowercase().replace([' ', '-'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn parses_sestatus_output() {
        let status = parse_sestatus(
            "SELinux status:                 enabled\nCurrent mode:                   enforcing\nPolicy version:                 33\n",
        );
        assert_eq!(status["selinux_status"], "enabled");
        assert_eq!(status["current_mode"], "enforcing");
        assert_eq!(status["policy_version"], "33");
    }

    #[test]
    fn parses_getsebool_output() {
        let booleans = parse_getsebool("virt_use_nfs --> off\nhttpd_can_network_connect --> on\n");
        assert_eq!(booleans.len(), 2);
        assert_eq!(booleans[0]["name"], "virt_use_nfs");
        assert_eq!(booleans[0]["enabled"], false);
        assert_eq!(booleans[1]["enabled"], true);
    }

    #[test]
    fn parses_semanage_fcontext_output() {
        let contexts = parse_semanage_fcontext(
            "SELinux fcontext                                   type               Context\n\
             /srv/tetra(/.*)?                                  all files          system_u:object_r:container_file_t:s0\n",
        );
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0]["path_pattern"], "/srv/tetra(/.*)?");
        assert_eq!(contexts[0]["file_type"], "all files");
        assert_eq!(contexts[0]["context_type"], "container_file_t");
    }

    #[test]
    fn dry_run_set_boolean_does_not_call_setsebool() {
        let response = SelinuxModule
            .handle(
                "set_boolean",
                json!({ "name": "virt_use_nfs", "value": true, "dry_run": true }),
            )
            .unwrap();

        assert_eq!(response["command"], "setsebool -P virt_use_nfs on");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn dry_run_restore_context_does_not_call_restorecon() {
        let response = SelinuxModule
            .handle(
                "restore_context",
                json!({ "path": "/srv/tetra", "recursive": true, "dry_run": true }),
            )
            .unwrap();

        assert_eq!(response["command"], "restorecon -R -v /srv/tetra");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
