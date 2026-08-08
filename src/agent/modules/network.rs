//! Network interface inspection and NetworkManager keyfile management.
//!
//! This module splits host networking into two concerns:
//!
//! - *Live state* (`interfaces`, `status`) is read from `/sys/class/net` sysfs
//!   and `ip -json addr show`, so callers can see what is currently up.
//! - *Persistent configuration* (`get_config`/`set_config`) is managed as
//!   NetworkManager *keyfiles*: INI-style `.nmconnection` profiles under
//!   `/etc/NetworkManager/system-connections/`. The agent reads and writes
//!   those files directly rather than driving `nmcli`, which keeps the wire
//!   format transparent and lets the control plane template profiles. After a
//!   write, callers invoke `reload` to tell NetworkManager to pick up the new
//!   profile via `systemctl reload-or-restart NetworkManager.service`.
//!
//! `set_config` supports the shared `selinux` options so written keyfiles can
//! be relabeled (e.g. `NetworkManager_etc_t`) on SELinux-enabled hosts.

use crate::prelude::*;

use crate::agent::module_support::apply_selinux;
use crate::types::{
    DryRunRequest, NetworkConfigRequest, NetworkInterfaceRequest, NetworkWriteConfigRequest,
};

/// Marker type for the network module. Stateless; all behavior lives in the
/// [`Mod`] impl and the static [`INFO`] descriptor.
#[derive(Clone, Copy, Debug)]
pub struct NetworkModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "network",
    feature: "network",
    description: "Inspect and configure host network interfaces, addresses, DNS, and routes.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "interfaces",
        "status",
        "dns",
        "routes",
        "get_config",
        "set_config",
        "reload",
    ],
    privileged_actions: &["set_config", "reload"],
};

impl Mod for NetworkModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = INFO.metadata_response(action, &payload) {
            return Ok(response);
        }
        Action::from_payload(action, payload)?.handle(user)
    }
}
actions!(Action [payload user] => {
    Interfaces => {
        Ok(jsonf! { "interfaces": read_interfaces().unwrap_or_default() })
    },
    Status: NetworkInterfaceRequest => {
        // `ip -json addr show` returns structured JSON we can pass
        // through verbatim; an optional `dev <name>` narrows the scope.
        let args = payload.ip_args();
        crate::cmd!({ &INFO, "status", user } "ip" => &args ; JSON)
    },
    Dns => Ok(jsonf! { "resolv_conf": read_optional("/etc/resolv.conf") }),
    Routes => crate::cmd!({ &INFO, "routes", user } "ip" ["-json", "route", "show"] JSON),
    GetConfig: NetworkConfigRequest => {
        Ok(jsonf! { payload.path, "contents": payload.read()? })
    },
    SetConfig: NetworkWriteConfigRequest => {
        // Only touch the filesystem outside dry-run; SELinux planning
        // below still runs so the response previews the relabel.
        payload.write()?;
        let selinux = apply_selinux(
            payload.selinux.as_ref(),
            Some(&payload.path),
            payload.dry_run,
        )?;
        Ok(jsonf! {
            payload.path,
            "written": !payload.dry_run,
            payload.dry_run,
            selinux,
        })
    },
    Reload: DryRunRequest => {
        crate::cmd!((payload.dry_run) { &INFO, "reload", user } "systemctl" ["reload-or-restart", "NetworkManager.service"] json)
    },
});

/// Reads the live interface list from `/sys/class/net` sysfs.
///
/// Each entry exposes `operstate` (up/down) and `address` (MAC) as plain text
/// files. Missing attributes are tolerated via `unwrap_or_default` (e.g.
/// virtual or bond devices may not report an operstate), so a single unreadable
/// file does not fail the whole snapshot. Results are sorted by name for stable
/// output to the control plane.
fn read_optional(path: &str) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_owned())
}

fn read_interfaces() -> Result<Vec<Value>> {
    let mut interfaces = Vec::new();
    for entry in fs::read_dir("/sys/class/net").context("failed to read /sys/class/net")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let operstate = fs::read_to_string(entry.path().join("operstate"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        let address = fs::read_to_string(entry.path().join("address"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        interfaces.push(jsonf! { name, operstate, "mac": address });
    }
    interfaces.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(interfaces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::module_support::SelinuxOptions;

    #[test]
    fn dns_read_returns_a_stable_shape() {
        let response = Dns.handle(None).unwrap();
        assert!(response.get("resolv_conf").is_some());
    }

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");
        fs::write(&path, "[connection]\nid=old\n").unwrap();

        let response = SetConfig(NetworkWriteConfigRequest {
            path: path.clone(),
            contents: "[connection]\nid=new\n".into(),
            dry_run: true,
            selinux: None,
        })
        .handle(None)
        .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(fs::read_to_string(path).unwrap(), "[connection]\nid=old\n");
    }

    #[test]
    fn set_config_can_restore_selinux_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");

        let response = SetConfig(NetworkWriteConfigRequest {
            path,
            contents: "[connection]\nid=new\n".into(),
            dry_run: true,
            selinux: Some(SelinuxOptions {
                context_type: Some("NetworkManager_etc_t".into()),
                ..SelinuxOptions::default()
            }),
        })
        .handle(None)
        .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert!(
            response["selinux"][0]["command"]
                .as_str()
                .unwrap()
                .contains("semanage fcontext -a -t NetworkManager_etc_t")
        );
    }

    #[test]
    fn reload_dry_run_does_not_restart_service() {
        let response = Reload(DryRunRequest { dry_run: true })
            .handle(None)
            .unwrap();
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
