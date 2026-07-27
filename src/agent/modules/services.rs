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
use itertools::Itertools;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload, unsupported_action,
    },
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

/// Payload for `logs`: which service journal to read, in which scope, and how
/// many trailing lines to return (defaults to 100).
#[derive(Debug, Deserialize)]
struct LogsPayload {
    service: String,
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default = "default_log_lines")]
    lines: u16,
}

/// Payload for `daemon_reload`. Unlike other mutations it carries no service
/// name, since `daemon-reload` operates on the whole unit file tree.
#[derive(Debug, Deserialize)]
struct DaemonReloadPayload {
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for the single-service actions (`status`, `start`, `stop`,
/// `restart`, `enable`, `disable`).
#[derive(Debug, Deserialize)]
struct ServicePayload {
    service: String,
    #[serde(default)]
    scope: ServiceScope,
    #[serde(default)]
    dry_run: bool,
}

/// Selects the systemd instance to talk to. `System` (the default) targets the
/// PID 1 system manager; `User` targets the requesting user's systemd, which we
/// request by prepending `--user` to every `systemctl`/`journalctl` call.
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

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = handle_metadata(INFO, action, payload.clone()) {
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
                    "services": parse_systemctl_services(&result.stdout),
                }))
            }
            "status" => {
                let payload: ServicePayload = parse_payload(payload)?;
                let args = systemctl_args(
                    payload.scope,
                    ["--no-pager", "--plain", "status", &payload.service],
                );
                crate::cmd!({ &INFO, action, user } "systemctl" => &args ; json)
            }
            "logs" => {
                let payload: LogsPayload = parse_payload(payload)?;
                // `journalctl -u <unit>` follows the unit's journal across
                // whatever files it spans; `-n` caps the tail to keep payloads
                // bounded. `logs` is a read and therefore never dry-run.
                crate::cmd!({ &INFO, action, user } "journalctl" => &journalctl_args(
                    payload.scope,
                    [
                        "--no-pager",
                        "-u",
                        &payload.service,
                        "-n",
                        &payload.lines.to_string(),
                    ],
                ); json)
            }
            "daemon_reload" => {
                let payload: DaemonReloadPayload = parse_payload(payload)?;
                let args = systemctl_args(payload.scope, ["daemon-reload"]);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" => &args ; json)
            }
            // The five single-service mutations share one arm because their
            // `systemctl` invocation has identical shape: `systemctl <action>
            // <service>`. The `action` string is already a valid systemctl
            // subcommand, which is why it can be forwarded directly.
            "start" | "stop" | "restart" | "enable" | "disable" => {
                let payload: ServicePayload = parse_payload(payload)?;
                let args = systemctl_args(payload.scope, [action, &payload.service]);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" => &args ; json)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

/// Prepends `--user` to a `systemctl` argument list when talking to the
/// per-user systemd instance; system scope is the default and needs no flag.
/// The const generic `N` lets callers pass a fixed-size array of `&str`
/// without having to allocate a `Vec` at the call site.
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

/// Same scope-prefixing as [`systemctl_args`], but for `journalctl` invocations
/// so `logs` reads from the correct journal.
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

const fn default_log_lines() -> u16 {
    100
}

/// Parses the whitespace-separated rows emitted by `systemctl list-units`.
///
/// Each row has the fixed prefix `UNIT LOAD ACTIVE SUB` followed by a
/// free-form `DESCRIPTION` that may contain spaces. We split on runs of
/// whitespace and treat the first four fields as the fixed columns, rejoining
/// everything from index 4 onward as the description. Rows with fewer than
/// four fields (e.g. stray blank lines or the suppressed legend) are dropped
/// via the `>= 4` guard.
fn parse_systemctl_services(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let (unit, load, active, sub) = it.next_tuple()?;
            Some(json!({
                "unit": unit,
                "load": load,
                "active": active,
                "sub": sub,
                "description": it.join(" "),
            }))
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
