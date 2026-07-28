//! Samba share configuration module.
//!
//! Reads and replaces `/etc/samba/smb.conf`, lists configured shares by
//! scanning for `[name]` section headers, and manages the `smb.service`
//! systemd unit (reload/enable/disable). The `set_config` action also accepts
//! the shared `selinux` payload so a share directory can be labeled
//! `samba_share_t` in the same request that defines the share — otherwise
//! SELinux will deny smbd access to the path on enforcing hosts.

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    agent::{
        AgentModule,
        module_support::{ModuleInfo, ModuleStatus, apply_selinux, parse_payload},
    },
    types::{DryRunRequest, SambaConfigRequest, SambaWriteConfigRequest},
};

/// Samba module entry point registered under feature `samba`.
pub struct SambaModule;

/// Static capability metadata published via `capabilities`/`plan`.
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
    privileged_actions: &["set_config", "reload", "enable", "disable"],
};

/// Dispatches `samba` actions. Config reads/writes target `smb.conf`;
/// service actions shell out to `systemctl` against `smb.service`.
impl AgentModule for SambaModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Standard metadata fast-path: `capabilities` and `plan` are answered
        // from `INFO` without touching the system.
        if let Some(response) = INFO.metadata_response(action, &payload) {
            return Ok(response);
        }

        match action {
            "list_shares" => {
                let payload: SambaConfigRequest = parse_payload(payload)?;
                let contents = payload.read()?;
                Ok(json!({ "path": payload.path, "shares": parse_samba_shares(&contents) }))
            }
            "get_config" => {
                let payload: SambaConfigRequest = parse_payload(payload)?;
                let contents = payload.read()?;
                Ok(json!({ "path": payload.path, "contents": contents }))
            }
            "set_config" => {
                let payload: SambaWriteConfigRequest = parse_payload(payload)?;
                // The whole file is replaced; callers are expected to build the
                // full desired smb.conf (including `[global]`) rather than
                // patch a single share in place.
                payload.write()?;
                // The default relabel target is the config file path; callers
                // wanting to label the share directory pass an explicit `path`
                // inside the selinux object pointing at e.g. `/srv/media`.
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
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["reload-or-restart", "smb.service"] json)
            }
            "enable" => {
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["enable", "--now", "smb.service"] json)
            }
            "disable" => {
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["disable", "--now", "smb.service"] json)
            }
            _ => INFO.unsupported_action(action),
        }
    }
}

/// Extracts share section names from smb.conf contents.
///
/// Scans for `[name]` lines and returns the inner names, skipping `[global]`
/// (Samba's settings section, not a share). The rest of the file is not
/// validated — malformed lines are simply ignored, mirroring smbd's own
/// tolerant parsing posture.
fn parse_samba_shares(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .map(|line| line.trim_matches(&['[', ']'][..]).to_owned())
        .filter(|name| !name.eq_ignore_ascii_case("global"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

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
                None,
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
                None,
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
