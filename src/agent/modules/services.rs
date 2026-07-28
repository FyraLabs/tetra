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
use serde_json::Value;

use crate::prelude::*;

/// Marker type for the services module. Stateless; all behavior lives in the
/// [`Mod`] impl and the static [`INFO`] descriptor.
#[derive(Clone, Copy, Debug)]
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

impl Mod for ServicesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [self user] => {
    List => {
        let result = crate::cmd!({ &INFO, "list", user } "systemctl" [
            "--no-pager",
            "--plain",
            "--legend=false",
            "list-units",
            "--type=service",
            "--all",
        ])?;
        Ok(jsonf! {
            "command": result.command,
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "dry_run": result.dry_run,
            "services": parse_systemctl_services(&result.stdout),
        })
    },
    Status {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(
            self.scope,
            ["--no-pager", "--plain", "status", &self.service],
        );
        crate::cmd!({ &INFO, "status", user } "systemctl" => &args ; json)
    },
    Logs {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default = "default_log_lines")]
        lines: u16,
    } => {
        crate::cmd!({ &INFO, "logs", user } "journalctl" => &journalctl_args(
            self.scope,
            [
                "--no-pager",
                "-u",
                &self.service,
                "-n",
                &self.lines.to_string(),
            ],
        ); json)
    },
    DaemonReload {
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["daemon-reload"]);
        crate::cmd!((self.dry_run) { &INFO, "daemon_reload", user } "systemctl" => &args ; json)
    },
    Start {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["start", &self.service]);
        crate::cmd!((self.dry_run) { &INFO, "start", user } "systemctl" => &args ; json)
    },
    Stop {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["stop", &self.service]);
        crate::cmd!((self.dry_run) { &INFO, "stop", user } "systemctl" => &args ; json)
    },
    Restart {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["restart", &self.service]);
        crate::cmd!((self.dry_run) { &INFO, "restart", user } "systemctl" => &args ; json)
    },
    Enable {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["enable", &self.service]);
        crate::cmd!((self.dry_run) { &INFO, "enable", user } "systemctl" => &args ; json)
    },
    Disable {
        service: String,
        #[serde(default)]
        scope: ServiceScope,
        #[serde(default)]
        dry_run: bool,
    } => {
        let args = systemctl_args(self.scope, ["disable", &self.service]);
        crate::cmd!((self.dry_run) { &INFO, "disable", user } "systemctl" => &args ; json)
    },
});

/// Prepends `--user` to a `systemctl` argument list when talking to the
/// per-user systemd instance; system scope is the default and needs no flag.
/// The const generic `N` lets callers pass a fixed-size array of `&str`
/// without having to allocate a `Vec` at the call site.
fn systemctl_args<const N: usize>(scope: ServiceScope, args: [&str; N]) -> Vec<&str> {
    match scope {
        ServiceScope::System => args.to_vec(),
        ServiceScope::User => {
            let mut scoped = Vec::with_capacity(args.len().saturating_add(1));
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
            let mut scoped = Vec::with_capacity(args.len().saturating_add(1));
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
            Some(jsonf! {
                unit,
                load,
                active,
                sub,
                "description": it.join(" "),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let response = Start {
            service: "sshd.service".into(),
            scope: ServiceScope::System,
            dry_run: true,
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["command"], "systemctl start sshd.service");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn daemon_reload_supports_dry_run() {
        let response = DaemonReload {
            scope: ServiceScope::System,
            dry_run: true,
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["command"], "systemctl daemon-reload");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn user_scope_adds_systemctl_user_flag() {
        let response = Start {
            service: "tetra-demo.service".into(),
            scope: ServiceScope::User,
            dry_run: true,
        }
        .handle(None)
        .unwrap();

        assert_eq!(
            response["command"],
            "systemctl --user start tetra-demo.service"
        );
    }
}
