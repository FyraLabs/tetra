//! NFS export configuration module.
//!
//! Reads and replaces `/etc/exports`, lists configured exports by parsing the
//! file, and manages the `nfs-server.service` systemd unit plus `exportfs -ra`
//! to re-export. The `set_config` action accepts the shared `selinux` payload
//! so an export directory can be labeled (typically `public_content_t` /
//! `public_content_rw_t`) in the same request — otherwise SELinux denies nfsd
//! access to the path on enforcing hosts.

use crate::prelude::*;

use crate::agent::module_support::{SelinuxOptions, apply_selinux};

/// NFS module entry point registered under feature `nfs`.
#[derive(Clone, Copy, Debug)]
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

impl Mod for NfsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [self user] => {
    ListExports {
        #[serde(default = "default_exports_path")]
        path: PathBuf,
    } => {
        let contents = read_config(&self.path)?;
        Ok(jsonf! { self.path, "exports": parse_exports(&contents) })
    },
    GetConfig {
        #[serde(default = "default_exports_path")]
        path: PathBuf,
    } => {
        let contents = read_config(&self.path)?;
        Ok(jsonf! { self.path, contents })
    },
    SetConfig {
        #[serde(default = "default_exports_path")]
        path: PathBuf,
        contents: String,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        selinux: Option<SelinuxOptions>,
    } => {
        if !self.dry_run {
            fs::write(&self.path, self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        let selinux = apply_selinux(
            self.selinux.as_ref(),
            Some(&self.path),
            self.dry_run,
        )?;
        Ok(jsonf! { self.path, "written": !self.dry_run, self.dry_run, selinux })
    },
    Reload {
        #[serde(default)]
        dry_run: bool,
    } => {
        cmd!((self.dry_run) { &INFO, "reload", user } "exportfs" ["-ra"] json)
    },
    Enable {
        #[serde(default)]
        dry_run: bool,
    } => {
        cmd!((self.dry_run) { &INFO, "enable", user } "systemctl" ["enable", "--now", "nfs-server.service"] json)
    },
    Disable {
        #[serde(default)]
        dry_run: bool,
    } => {
        crate::cmd!((self.dry_run) { &INFO, "disable", user } "systemctl" ["disable", "--now", "nfs-server.service"] json)
    }
});

/// Default `/etc/exports` location used when a read/write action omits
/// `path`.
fn default_exports_path() -> PathBuf {
    PathBuf::from("/etc/exports")
}

/// Reads the exports file as UTF-8 text with a context-rich error.
fn read_config(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
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

        let response = SetConfig {
            path: path.clone(),
            contents: "/srv/media *(rw)\n".into(),
            dry_run: true,
            selinux: None,
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(fs::read_to_string(path).unwrap(), "/srv/media *(ro)\n");
    }

    #[test]
    fn set_config_can_apply_nfs_export_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exports");

        let response = SetConfig {
            path: path.clone(),
            contents: "/srv/export *(rw)\n".into(),
            dry_run: true,
            selinux: Some(SelinuxOptions {
                path: Some("/srv/export".into()),
                context_type: Some("public_content_rw_t".into()),
                recursive: true,
                ..SelinuxOptions::default()
            }),
        }
        .handle(None)
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

    #[test]
    fn reload_dry_run_does_not_execute_exportfs() {
        let response = Reload { dry_run: true }.handle(None).unwrap();
        assert_eq!(response["command"], "exportfs -ra");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
