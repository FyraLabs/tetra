use std::fs;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, NamedPayload, handle_metadata, parse_payload, run_command,
        run_command_or_dry_run, unsupported_action,
    },
};

pub struct UsersModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "users",
    feature: "users",
    description: "Inspect and manage local users, groups, and account-related configuration.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "status",
        "create",
        "update",
        "delete",
        "set_password",
        "groups",
    ],
};

#[derive(Debug, Deserialize)]
struct CreatePayload {
    name: String,
    shell: Option<String>,
    home: Option<String>,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct UpdatePayload {
    name: String,
    shell: Option<String>,
    home: Option<String>,
    groups: Option<Vec<String>>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct SetPasswordPayload {
    name: String,
    password_hash: String,
    #[serde(default)]
    dry_run: bool,
}

impl AgentModule for UsersModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => Ok(json!({ "users": parse_passwd(&read_file("/etc/passwd")?) })),
            "groups" => Ok(json!({ "groups": parse_group(&read_file("/etc/group")?) })),
            "status" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command("id", [&payload.name])
            }
            "create" => {
                let payload: CreatePayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if payload.system {
                    args.push("--system".to_string());
                }
                if let Some(shell) = payload.shell {
                    args.extend(["--shell".into(), shell]);
                }
                if let Some(home) = payload.home {
                    args.extend(["--home-dir".into(), home]);
                }
                args.push(payload.name);
                run_command_or_dry_run("useradd", args, payload.dry_run)
            }
            "update" => {
                let payload: UpdatePayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if let Some(shell) = payload.shell {
                    args.extend(["--shell".into(), shell]);
                }
                if let Some(home) = payload.home {
                    args.extend(["--home".into(), home]);
                }
                if let Some(groups) = payload.groups {
                    args.extend(["--groups".into(), groups.join(",")]);
                }
                args.push(payload.name);
                run_command_or_dry_run("usermod", args, payload.dry_run)
            }
            "delete" => {
                let payload: NamedPayload = parse_payload(payload)?;
                run_command_or_dry_run("userdel", [&payload.name], payload.dry_run)
            }
            "set_password" => {
                let payload: SetPasswordPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "usermod",
                    ["--password", &payload.password_hash, &payload.name],
                    payload.dry_run,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn read_file(path: &str) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{path}`"))
}

fn parse_passwd(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 7).then(|| {
                json!({
                    "name": fields[0],
                    "uid": fields[2],
                    "gid": fields[3],
                    "gecos": fields[4],
                    "home": fields[5],
                    "shell": fields[6],
                })
            })
        })
        .collect()
}

fn parse_group(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 4).then(|| {
                json!({
                    "name": fields[0],
                    "gid": fields[2],
                    "members": fields[3].split(',').filter(|member| !member.is_empty()).collect::<Vec<_>>(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd_entries() {
        let users = parse_passwd("root:x:0:0:root:/root:/bin/bash\n");
        assert_eq!(users[0]["name"], "root");
        assert_eq!(users[0]["home"], "/root");
    }

    #[test]
    fn parses_group_entries() {
        let groups = parse_group("wheel:x:10:root,admin\n");
        assert_eq!(groups[0]["name"], "wheel");
        assert_eq!(groups[0]["members"][0], "root");
    }

    #[test]
    fn dry_run_create_does_not_call_useradd() {
        let response = UsersModule
            .handle("create", json!({ "name": "testuser", "dry_run": true }))
            .unwrap();

        assert_eq!(response["command"], "useradd testuser");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
