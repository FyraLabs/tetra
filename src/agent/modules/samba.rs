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

pub struct SambaModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "samba",
    feature: "samba",
    description: "Manage Samba shares, users, generated configuration, and service state.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list_shares",
        "get_config",
        "set_config",
        "reload",
        "enable",
        "disable",
    ],
};

#[derive(Debug, Deserialize)]
struct ConfigPayload {
    #[serde(default = "default_smb_conf")]
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SetConfigPayload {
    #[serde(default = "default_smb_conf")]
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

impl AgentModule for SambaModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list_shares" => {
                let payload: ConfigPayload = parse_payload(payload)?;
                let contents = read_config(&payload.path)?;
                Ok(json!({ "path": payload.path, "shares": parse_samba_shares(&contents) }))
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
                run_command_or_dry_run(
                    "systemctl",
                    ["reload-or-restart", "smb.service"],
                    payload.dry_run,
                )
            }
            "enable" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    ["enable", "--now", "smb.service"],
                    payload.dry_run,
                )
            }
            "disable" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "systemctl",
                    ["disable", "--now", "smb.service"],
                    payload.dry_run,
                )
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_smb_conf() -> PathBuf {
    PathBuf::from("/etc/samba/smb.conf")
}

fn read_config(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
}

fn parse_samba_shares(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| line.trim_matches(&['[', ']'][..]).to_string())
        .filter(|name| !name.eq_ignore_ascii_case("global"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_samba_share_names() {
        let shares = parse_samba_shares("[global]\n[media]\n path = /srv/media\n[homes]\n");
        assert_eq!(shares, vec!["media", "homes"]);
    }

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smb.conf");
        fs::write(&path, "[global]\n").unwrap();

        let response = SambaModule
            .handle(
                "set_config",
                json!({ "path": path, "contents": "[media]\n", "dry_run": true }),
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("smb.conf")).unwrap(),
            "[global]\n"
        );
    }

    #[test]
    fn set_config_can_apply_samba_share_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smb.conf");
        let response = SambaModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "[media]\npath = /srv/media\n",
                    "dry_run": true,
                    "selinux": {
                        "path": "/srv/media",
                        "context_type": "samba_share_t",
                        "recursive": true
                    }
                }),
            )
            .unwrap();

        assert_eq!(
            response["selinux"][0]["command"],
            "semanage fcontext -a -t samba_share_t /srv/media(/.*)?"
        );
        assert_eq!(
            response["selinux"][1]["command"],
            "restorecon -R -v /srv/media"
        );
    }
}
