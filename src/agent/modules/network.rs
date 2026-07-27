//! Network interface inspection and `NetworkManager` keyfile management.
//!
//! This module splits host networking into two concerns:
//!
//! - *Live state* (`interfaces`, `status`) is read from `/sys/class/net` sysfs
//!   and `ip -json addr show`, so callers can see what is currently up.
//! - *Persistent configuration* (`get_config`/`set_config`) is managed as
//!   `NetworkManager` *keyfiles*: INI-style `.nmconnection` profiles under
//!   `/etc/NetworkManager/system-connections/`. The agent reads and writes
//!   those files directly rather than driving `nmcli`, which keeps the wire
//!   format transparent and lets the control plane template profiles. After a
//!   write, callers invoke `reload` to tell `NetworkManager` to pick up the new
//!   profile via `systemctl reload-or-restart NetworkManager.service`.
//!
//! `set_config` supports the shared `selinux` options so written keyfiles can
//! be relabeled (e.g. `NetworkManager_etc_t`) on SELinux-enabled hosts.

use crate::prelude::*;

use crate::agent::module_support::{
    SelinuxOptions, apply_selinux, handle_metadata, parse_payload, unsupported_action,
};

/// Marker type for the network module. Stateless; all behavior lives in the
/// [`AgentModule`] impl and the static [`INFO`] descriptor.
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
        "get_config",
        "set_config",
        "reload",
    ],
    privileged_actions: &["set_config", "reload"],
};

/// Payload for `status`; the optional interface name narrows `ip addr show`
/// to a single device via `dev <name>`. Omitting it lists all interfaces.
#[derive(Debug, Deserialize)]
struct InterfacePayload {
    interface: Option<String>,
}

/// Payload for `get_config`: the keyfile path to read (typically under
/// `/etc/NetworkManager/system-connections/`).
#[derive(Debug, Deserialize)]
struct ConfigPayload {
    path: PathBuf,
}

/// Payload for `set_config`: writes `contents` to the keyfile at `path`. The
/// `selinux` option relabels the file after the write; `dry_run` skips both.
#[derive(Debug, Deserialize)]
struct SetConfigPayload {
    path: PathBuf,
    contents: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

/// Payload for `reload`: only carries the standard `dry_run` flag, since the
/// reload target (NetworkManager.service) is fixed.
#[derive(Debug, Deserialize)]
struct DryRunPayload {
    #[serde(default)]
    dry_run: bool,
}

impl AgentModule for NetworkModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = handle_metadata(INFO, action, payload.clone()) {
            return Ok(response);
        }

        match action {
            // Sysfs-based snapshot of present interfaces. `unwrap_or_default`
            // keeps the action resilient: a transient read failure yields an
            // empty list rather than a 500 to the control plane.
            "interfaces" => Ok(jsonf! { "interfaces": read_interfaces().unwrap_or_default() }),
            "status" => {
                let payload: InterfacePayload = parse_payload(payload)?;
                // `ip -json addr show` returns structured JSON we can pass
                // through verbatim; an optional `dev <name>` narrows the scope.
                let mut args = vec!["-json".to_owned(), "addr".to_owned(), "show".to_owned()];
                if let Some(interface) = payload.interface {
                    args.push("dev".into());
                    args.push(interface);
                }
                crate::cmd!({ &INFO, action, user } "ip" => &args ; JSON)
            }
            "get_config" => {
                let payload: ConfigPayload = parse_payload(payload)?;
                let contents = fs::read_to_string(&payload.path)
                    .with_context(|| format!("failed to read `{}`", payload.path.display()))?;
                Ok(jsonf! { payload.path, contents })
            }
            "set_config" => {
                let payload: SetConfigPayload = parse_payload(payload)?;
                // Only touch the filesystem outside dry-run; SELinux planning
                // below still runs so the response previews the relabel.
                if !payload.dry_run {
                    fs::write(&payload.path, payload.contents)
                        .with_context(|| format!("failed to write `{}`", payload.path.display()))?;
                }
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
            }
            "reload" => {
                let payload: DryRunPayload = parse_payload(payload)?;
                // `reload-or-restart` applies new keyfiles without dropping
                // active connections when possible, falling back to a restart.
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["reload-or-restart", "NetworkManager.service"] ; json)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

/// Reads the live interface list from `/sys/class/net` sysfs.
///
/// Each entry exposes `operstate` (up/down) and `address` (MAC) as plain text
/// files. Missing attributes are tolerated via `unwrap_or_default` (e.g.
/// virtual or bond devices may not report an operstate), so a single unreadable
/// file does not fail the whole snapshot. Results are sorted by name for stable
/// output to the control plane.
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
    use crate::agent::AgentModule;

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");
        fs::write(&path, "[connection]\nid=old\n").unwrap();

        let response = NetworkModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "[connection]\nid=new\n",
                    "dry_run": true
                }),
                None,
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("connection.nmconnection")).unwrap(),
            "[connection]\nid=old\n"
        );
    }

    #[test]
    fn set_config_can_restore_selinux_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.nmconnection");

        let response = NetworkModule
            .handle(
                "set_config",
                json!({
                    "path": path,
                    "contents": "[connection]\nid=new\n",
                    "dry_run": true,
                    "selinux": {
                        "context_type": "NetworkManager_etc_t"
                    }
                }),
                None,
            )
            .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert!(
            response["selinux"][0]["command"]
                .as_str()
                .unwrap()
                .contains("semanage fcontext -a -t NetworkManager_etc_t")
        );
    }
}
