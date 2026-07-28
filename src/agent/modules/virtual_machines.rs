//! libvirt virtual machine inspection and lifecycle via `virsh`.
//!
//! This module wraps the `virsh` CLI for domain (VM) management plus
//! `journalctl` for the `libvirtd` service log. Read actions (`list`,
//! `status`, `logs`) run unconditionally; lifecycle actions (`start`, `stop`,
//! `restart`, `create`, `delete`) honor the shared `dry_run` flag.
//!
//! A couple of protocol-to-CLI mappings are non-obvious:
//! - `stop` maps to `virsh shutdown` (graceful guest shutdown), and `restart`
//!   maps to `virsh reboot`, rather than hard power operations.
//! - `create` maps to `virsh define`, which registers a *persistent* domain
//!   from an XML file. We avoid `virsh create` because that builds a transient
//!   domain that disappears on shutdown.
//!
//! `list` and `status` additionally parse virsh's text output into stable
//! `domains` / `domain` JSON fields so callers do not have to.

use crate::prelude::*;

/// Marker type for the `virtual_machines` module. Stateless; all behavior lives
/// in the [`Mod`] impl and the static [`INFO`] descriptor.
#[derive(Clone, Copy, Debug)]
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
    privileged_actions: &["start", "stop", "restart", "create", "delete"],
};

impl Mod for VirtualMachinesModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [self user] => {
    List => {
        let result = crate::cmd!({ &INFO, "list", user } "virsh" ["list", "--all"])?;
        let domains = parse_virsh_list(&result.stdout);
        Ok(jsonf! {
            "command": result.command,
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "dry_run": result.dry_run,
            domains,
        })
    },
    Status { name: String } => {
        let result = crate::cmd!({ &INFO, "status", user } "virsh" ["dominfo", &self.name])?;
        let info = parse_virsh_dominfo(&result.stdout);
        Ok(jsonf! {
            "command": result.command,
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "dry_run": result.dry_run,
            "domain": info,
        })
    },
    Logs {
        #[serde(default = "default_log_lines")]
        lines: u16,
    } => crate::cmd!({ &INFO, "logs", user } "journalctl" [
        "--no-pager",
        "-u",
        "libvirtd.service",
        "-n",
        &self.lines.to_string()
    ] ; json),
    Start {
        name: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "start", user } "virsh" ["start", &self.name] ; json),
    Stop {
        name: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "stop", user } "virsh" ["shutdown", &self.name] ; json),
    Restart {
        name: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "restart", user } "virsh" ["reboot", &self.name] ; json),
    Create {
        xml_path: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "create", user } "virsh" ["define", &self.xml_path] ; json),
    Delete {
        name: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "delete", user } "virsh" ["undefine", &self.name] ; json),
});

const fn default_log_lines() -> u16 {
    100
}

/// Parses `virsh list --all` output into domain objects.
///
/// The table has a header row, a `---` separator, then one row per domain in
/// the form `Id Name State`. `skip_while` + `skip(1)` jumps past the separator;
/// from there each row's first two whitespace fields are id and name, and the
/// remaining fields (state can be multiple words, e.g. "shut off") are rejoined
/// as the state. An id of `-` means the domain is not running, so we emit `null`
/// for `id` in that case to keep the field typed as a number-or-null for callers.
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
            Some(jsonf! {
                "id": (id != "-").then_some(id),
                "name": name,
                "state": fields.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// Parses `virsh dominfo` output into a flat JSON object.
///
/// `dominfo` emits `Key: Value` lines (e.g. `Name: vm1`, `CPU(s): 2`). We
/// lower-case keys and replace spaces with underscores so `CPU(s)` becomes
/// `cpu(s)`, giving callers stable, case-insensitive field names without having
/// to know virsh's exact capitalization. Lines without a colon (blank lines,
/// section headers) are skipped.
fn parse_virsh_dominfo(stdout: &str) -> Value {
    let mut object = serde_json::Map::new();
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        object.insert(
            key.trim().to_lowercase().replace(' ', "_"),
            Value::String(value.trim().to_owned()),
        );
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_start_does_not_call_virsh() {
        let response = Start {
            name: "vm1".into(),
            dry_run: true,
        }
        .handle(None)
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

    #[test]
    fn create_payload_requires_xml_path() {
        let result = Create::from_payload("create", json!({ "dry_run": true }));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("missing field `xml_path`"));
    }

    #[test]
    fn status_requires_name() {
        let result = Status::from_payload("status", json!({}));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("missing field `name`"));
    }
}
