//! Samba share configuration module.
//!
//! Reads and replaces `/etc/samba/smb.conf`, lists configured shares by
//! scanning for `[name]` section headers, and manages the `smb.service`
//! systemd unit (reload/enable/disable). The `set_config` action also accepts
//! the shared `selinux` payload so a share directory can be labeled
//! `samba_share_t` in the same request that defines the share — otherwise
//! SELinux will deny smbd access to the path on enforcing hosts.

use crate::{
    agent::module_support::{SelinuxOptions, apply_selinux},
    prelude::*,
};

/// Samba module entry point registered under feature `samba`.
#[derive(Clone, Copy, Debug)]
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

impl Mod for SambaModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [self user] => {
    ListShares {
        #[serde(default = "default_smb_conf")]
        path: PathBuf,
    } => {
        let contents = read_config(&self.path)?;
        Ok(jsonf! { self.path, "shares": parse_samba_shares(&contents) })
    },
    GetConfig {
        #[serde(default = "default_smb_conf")]
        path: PathBuf,
    } => {
        let contents = read_config(&self.path)?;
        Ok(jsonf! { self.path, contents })
    },
    SetConfig {
        #[serde(default = "default_smb_conf")]
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
        Ok(jsonf! {
            self.path,
            "written": !self.dry_run,
            self.dry_run,
            selinux,
        })
    },
    Reload {
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "reload", user } "systemctl" ["reload-or-restart", "smb.service"] json),
    Enable {
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "enable", user } "systemctl" ["enable", "--now", "smb.service"] json),
    Disable {
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((self.dry_run) { &INFO, "disable", user } "systemctl" ["disable", "--now", "smb.service"] json),

});

/// Default smb.conf location used when a read/write action omits `path`.
fn default_smb_conf() -> PathBuf {
    PathBuf::from("/etc/samba/smb.conf")
}

/// Reads the smb.conf file as UTF-8 text with a context-rich error.
fn read_config(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
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

        let response = SetConfig {
            path: path.clone(),
            contents: "[media]\n".into(),
            dry_run: true,
            selinux: None,
        }
        .handle(None)
        .unwrap();

        assert_eq!(response["written"], false);
        assert_eq!(fs::read_to_string(path).unwrap(), "[global]\n");
    }

    #[test]
    fn set_config_can_apply_samba_share_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smb.conf");

        let response = SetConfig {
            path: path.clone(),
            contents: "[media]\npath = /srv/media\n".into(),
            dry_run: true,
            selinux: Some(SelinuxOptions {
                path: Some("/srv/media".into()),
                context_type: Some("samba_share_t".into()),
                recursive: true,
                ..SelinuxOptions::default()
            }),
        }
        .handle(None)
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

    #[test]
    fn list_shares_parses_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smb.conf");
        fs::write(&path, "[global]\n[media]\npath = /srv/media\n").unwrap();

        let response = ListShares { path: path.clone() }.handle(None).unwrap();

        assert_eq!(response["path"].as_str().unwrap(), &path);
        assert_eq!(response["shares"].as_array().unwrap().len(), 1);
        assert_eq!(response["shares"][0], "media");
    }

    #[test]
    fn reload_dry_run_does_not_restart_service() {
        let response = Reload { dry_run: true }.handle(None).unwrap();
        assert_eq!(
            response["command"],
            "systemctl reload-or-restart smb.service"
        );
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
