//! NFS export configuration module.
//!
//! Reads and replaces `/etc/exports`, lists configured exports by parsing the
//! file, and manages the `nfs-server.service` systemd unit plus `exportfs -ra`
//! to re-export. The `set_config` action accepts the shared `selinux` payload
//! so an export directory can be labeled (typically `public_content_t` /
//! `public_content_rw_t`) in the same request — otherwise SELinux denies nfsd
//! access to the path on enforcing hosts.

use crate::prelude::*;

use crate::{
    agent::module_support::{apply_selinux, parse_payload},
    types::{DryRunRequest, NfsConfigRequest, NfsWriteConfigRequest},
};

/// NFS module entry point registered under feature `nfs`.
pub struct NfsModule;

/// Static capability metadata published via `capabilities`/`plan`.
const INFO: ModuleInfo = ModuleInfo {
    name: "nfs",
    feature: "nfs",
    description: "Manage NFS exports, generated configuration, and service state.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list_exports",
        "get_config",
        "set_config",
        "reload",
        "enable",
        "disable",
    ],
    privileged_actions: &["set_config", "reload", "enable", "disable"],
};

/// Dispatches `nfs` actions. Config reads/writes target `/etc/exports`;
/// `reload` runs `exportfs -ra` so the kernel re-reads the file without a
/// full service restart; `enable`/`disable` drive `nfs-server.service`.
impl AgentModule for NfsModule {
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
            "list_exports" => {
                let payload: NfsConfigRequest = parse_payload(payload)?;
                let contents = payload.read()?;
                Ok(jsonf! {
                    payload.path,
                    "exports": parse_exports(&contents),
                })
            }
            "get_config" => {
                let payload: NfsConfigRequest = parse_payload(payload)?;
                let contents = payload.read()?;
                Ok(jsonf! { payload.path, contents })
            }
            "set_config" => {
                let payload: NfsWriteConfigRequest = parse_payload(payload)?;
                // The whole file is replaced; callers build the complete
                // desired `/etc/exports` rather than patching one export.
                payload.write()?;
                // Default relabel target is the exports file path; callers
                // wanting to label the exported directory pass an explicit
                // `path` inside the selinux object (e.g. `/srv/export`).
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
                // `exportfs -ra` re-exports everything in /etc/exports in
                // place, without bouncing nfs-server — that avoids dropping
                // existing clients mid-reload.
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "exportfs" ["-ra"] ; json)
            }
            "enable" => {
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["enable", "--now", "nfs-server.service"] ; json)
            }
            "disable" => {
                let payload: DryRunRequest = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "systemctl" ["disable", "--now", "nfs-server.service"] ; json)
            }
            _ => INFO.unsupported_action(action),
        }
    }
}

/// Parses `/etc/exports` into one record per non-comment, non-blank line.
///
/// The first whitespace-delimited field is the exported path; everything
/// after it is collected verbatim as the list of `client(spec)` tokens (e.g.
/// `192.168.1.0/24(rw)`, `*(ro)`). Lines without a leading path are dropped.
fn parse_exports(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let path = fields.next()?;
            Some(jsonf! {
                path,
                "clients": fields.collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exports_file() {
        let exports = parse_exports("# comment\n/srv/media 192.168.1.0/24(rw) *(ro)\n");
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0]["path"], "/srv/media");
    }

    #[test]
    fn dry_run_set_config_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports");
        fs::write(&path, "/srv/media *(ro)\n").unwrap();

        let response = NfsModule
            .handle(
                "set_config",
                jsonf! { path, "contents": "/srv/media *(rw)\n", "dry_run": true },
                None,
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("exports")).unwrap(),
            "/srv/media *(ro)\n"
        );
    }

    #[test]
    fn set_config_can_apply_nfs_export_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports");
        let response = NfsModule
            .handle(
                "set_config",
                jsonf! {
                    path,
                    "contents": "/srv/export *(rw)\n",
                    "dry_run": true,
                    "selinux": {
                        "path": "/srv/export",
                        "context_type": "public_content_rw_t",
                        "recursive": true
                    }
                },
                None,
            )
            .unwrap();

        assert_eq!(
            response["selinux"][0]["command"],
            "semanage fcontext -a -t public_content_rw_t /srv/export(/.*)?"
        );
        assert_eq!(
            response["selinux"][1]["command"],
            "restorecon -R -v /srv/export"
        );
    }
}
