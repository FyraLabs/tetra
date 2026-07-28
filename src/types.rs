//! Shared domain types used by Tetra's agent modules

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::agent::module_support::SelinuxOptions;

#[derive(Debug, Deserialize)]
pub struct FileReadRequest {
    pub path: PathBuf,
}

impl FileReadRequest {
    pub fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read `{}`", self.path.display()))
    }
}

#[derive(Debug, Deserialize)]
pub struct FileWriteRequest {
    pub path: PathBuf,
    pub contents: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
}

impl FileWriteRequest {
    pub fn write(&self) -> Result<()> {
        if !self.dry_run {
            fs::write(&self.path, &self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct NetworkInterfaceRequest {
    pub interface: Option<String>,
}

impl NetworkInterfaceRequest {
    #[must_use]
    pub fn ip_args(&self) -> Vec<String> {
        let mut args = vec!["-json".into(), "addr".into(), "show".into()];
        if let Some(interface) = &self.interface {
            args.extend(["dev".into(), interface.clone()]);
        }
        args
    }
}

#[derive(Debug, Deserialize)]
pub struct NetworkConfigRequest {
    pub path: PathBuf,
}

impl NetworkConfigRequest {
    pub fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read `{}`", self.path.display()))
    }
}

#[derive(Debug, Deserialize)]
pub struct NetworkWriteConfigRequest {
    pub path: PathBuf,
    pub contents: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
}

impl NetworkWriteConfigRequest {
    pub fn write(&self) -> Result<()> {
        if !self.dry_run {
            fs::write(&self.path, &self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct DryRunRequest {
    #[serde(default)]
    pub dry_run: bool,
}

fn default_exports_path() -> PathBuf {
    PathBuf::from("/etc/exports")
}

#[derive(Debug, Deserialize)]
pub struct NfsConfigRequest {
    #[serde(default = "default_exports_path")]
    pub path: PathBuf,
}

impl NfsConfigRequest {
    pub fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read `{}`", self.path.display()))
    }
}

#[derive(Debug, Deserialize)]
pub struct NfsWriteConfigRequest {
    #[serde(default = "default_exports_path")]
    pub path: PathBuf,
    pub contents: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
}

impl NfsWriteConfigRequest {
    pub fn write(&self) -> Result<()> {
        if !self.dry_run {
            fs::write(&self.path, &self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        Ok(())
    }
}

fn default_samba_config_path() -> PathBuf {
    PathBuf::from("/etc/samba/smb.conf")
}

#[derive(Debug, Deserialize)]
pub struct SambaConfigRequest {
    #[serde(default = "default_samba_config_path")]
    pub path: PathBuf,
}

impl SambaConfigRequest {
    pub fn read(&self) -> Result<String> {
        fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read `{}`", self.path.display()))
    }
}

#[derive(Debug, Deserialize)]
pub struct SambaWriteConfigRequest {
    #[serde(default = "default_samba_config_path")]
    pub path: PathBuf,
    pub contents: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
}

impl SambaWriteConfigRequest {
    pub fn write(&self) -> Result<()> {
        if !self.dry_run {
            fs::write(&self.path, &self.contents)
                .with_context(|| format!("failed to write `{}`", self.path.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceScope {
    #[default]
    System,
    User,
}

impl ServiceScope {
    #[must_use]
    pub fn command_args<const N: usize>(self, args: [&str; N]) -> Vec<&str> {
        match self {
            Self::System => args.to_vec(),
            Self::User => {
                let mut scoped = Vec::with_capacity(args.len().saturating_add(1));
                scoped.push("--user");
                scoped.extend(args);
                scoped
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ServiceRequest {
    pub service: String,
    #[serde(default)]
    pub scope: ServiceScope,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct ServiceLogsRequest {
    pub service: String,
    #[serde(default)]
    pub scope: ServiceScope,
    #[serde(default = "default_log_lines")]
    pub lines: u16,
}

#[derive(Debug, Deserialize)]
pub struct DaemonReloadRequest {
    #[serde(default)]
    pub scope: ServiceScope,
    #[serde(default)]
    pub dry_run: bool,
}

const fn default_log_lines() -> u16 {
    100
}

#[derive(Debug, serde::Serialize)]
pub struct ServiceStatus {
    pub unit: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub description: String,
}

impl ServiceStatus {
    #[must_use]
    pub fn parse_all(stdout: &str) -> Vec<Self> {
        stdout.lines().filter_map(Self::parse).collect()
    }

    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        let unit = fields.next()?;
        let load = fields.next()?;
        let active = fields.next()?;
        let sub = fields.next()?;
        Some(Self {
            unit: unit.to_owned(),
            load: load.to_owned(),
            active: active.to_owned(),
            sub: sub.to_owned(),
            description: fields.collect::<Vec<_>>().join(" "),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct VirtualMachineCreateRequest {
    pub xml_path: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct VirtualMachineLogsRequest {
    #[serde(default = "default_log_lines")]
    pub lines: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_write_request_honors_dry_run_and_writes_when_requested() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("managed.conf");
        let dry_run = FileWriteRequest {
            path: path.clone(),
            contents: "dry run".into(),
            dry_run: true,
            selinux: None,
        };
        dry_run.write().unwrap();
        assert!(!path.exists());

        let write = FileWriteRequest {
            path: path.clone(),
            contents: "written".into(),
            dry_run: false,
            selinux: None,
        };
        write.write().unwrap();
        assert_eq!(FileReadRequest { path }.read().unwrap(), "written");
    }

    #[test]
    fn network_interface_request_builds_optional_device_arguments() {
        assert_eq!(
            NetworkInterfaceRequest { interface: None }.ip_args(),
            ["-json", "addr", "show"]
        );
        assert_eq!(
            NetworkInterfaceRequest {
                interface: Some("eno1".into())
            }
            .ip_args(),
            ["-json", "addr", "show", "dev", "eno1"]
        );
    }

    #[test]
    fn configuration_requests_keep_their_module_defaults() {
        let nfs: NfsConfigRequest = serde_json::from_value(json!({})).unwrap();
        let samba: SambaConfigRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(nfs.path, PathBuf::from("/etc/exports"));
        assert_eq!(samba.path, PathBuf::from("/etc/samba/smb.conf"));
    }

    #[test]
    fn service_requests_default_and_apply_scope_consistently() {
        let logs: ServiceLogsRequest =
            serde_json::from_value(json!({ "service": "sshd" })).unwrap();
        assert_eq!(logs.lines, 100);
        assert_eq!(
            logs.scope.command_args(["status", "sshd"]),
            ["status", "sshd"]
        );
        assert_eq!(
            ServiceScope::User.command_args(["status", "sshd"]),
            ["--user", "status", "sshd"]
        );
    }

    #[test]
    fn service_status_parser_skips_malformed_rows_and_preserves_description() {
        let statuses = ServiceStatus::parse_all(
            "invalid\nsshd.service loaded active running OpenSSH server daemon\n",
        );
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].unit, "sshd.service");
        assert_eq!(statuses[0].description, "OpenSSH server daemon");
    }

    #[test]
    fn virtual_machine_log_request_defaults_to_one_hundred_lines() {
        let logs: VirtualMachineLogsRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(logs.lines, 100);
    }
}
