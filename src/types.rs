//! Shared domain types used by Tetra's agent modules

use serde_yaml::Value as YamlValue;

use crate::agent::module_support::SelinuxOptions;
use crate::prelude::*;

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

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
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

const fn default_true() -> bool {
    true
}

/// Target of an `apps.remove` action.
///
/// Identifies which installed app to remove, in which scope, and whether to
/// converge systemd (stop/disable + daemon-reload) while removing it. The
/// directory overrides exist so tests and custom deployments can redirect the
/// Quadlet scan dir and the companion data root.
#[derive(Debug, Deserialize)]
pub struct AppRequest {
    pub name: String,
    #[serde(default)]
    pub scope: ServiceScope,
    pub base_dir: Option<PathBuf>,
    pub files_base_dir: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub converge: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// Where an app's recipe comes from.
///
/// A recipe is either shipped inline (`recipe` + `templates`) or referenced
/// on disk (`recipe_path` + `templates_dir`); exactly one form must be given.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AppRecipeSource {
    Inline {
        recipe: String,
        #[serde(default)]
        templates: BTreeMap<String, String>,
    },
    File {
        recipe_path: PathBuf,
        templates_dir: PathBuf,
    },
}

/// Payload for `apps.create`: cook a recipe into an installed app bundle.
///
/// `values` are merged over `values_path` (inline wins), so a dashboard can
/// ship a defaults file plus per-instance overrides. `converge` controls the
/// systemd phase; when `false` only files are written and systemd is left
/// untouched (no daemon-reload, no enable/start), letting a controller run
/// that phase itself through the `services` module.
#[derive(Debug, Deserialize)]
pub struct AppCreateRequest {
    pub name: String,
    #[serde(default)]
    pub scope: ServiceScope,
    pub base_dir: Option<PathBuf>,
    pub files_base_dir: Option<PathBuf>,
    pub recipe: Option<String>,
    #[serde(default)]
    pub templates: BTreeMap<String, String>,
    pub recipe_path: Option<PathBuf>,
    pub templates_dir: Option<PathBuf>,
    pub values_path: Option<PathBuf>,
    #[serde(default)]
    pub values: BTreeMap<String, YamlValue>,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
    #[serde(default = "default_true")]
    pub converge: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// Payload for `apps.update`: re-cook an installed app.
///
/// `values` are merged per-key over the stored values from `create` (or the
/// last `update`), so secrets do not have to be re-sent on every edit.
/// Passing a new `recipe` / `recipe_path` swaps the recipe source (e.g. a
/// recipe upgrade).
#[derive(Debug, Deserialize)]
pub struct AppUpdateRequest {
    pub name: String,
    #[serde(default)]
    pub scope: ServiceScope,
    pub base_dir: Option<PathBuf>,
    pub files_base_dir: Option<PathBuf>,
    pub recipe: Option<String>,
    pub templates: Option<BTreeMap<String, String>>,
    pub recipe_path: Option<PathBuf>,
    pub templates_dir: Option<PathBuf>,
    #[serde(default)]
    pub values: BTreeMap<String, YamlValue>,
    #[serde(default)]
    pub selinux: Option<SelinuxOptions>,
    #[serde(default = "default_true")]
    pub converge: bool,
    #[serde(default)]
    pub dry_run: bool,
}

/// Payload for `apps.get`: read one installed app's manifest and on-disk
/// state. Read-only, so there is no `dry_run`.
#[derive(Debug, Deserialize)]
pub struct AppGetRequest {
    pub name: String,
    #[serde(default)]
    pub scope: ServiceScope,
    pub base_dir: Option<PathBuf>,
    pub files_base_dir: Option<PathBuf>,
}

/// Payload for `apps.list`: enumerate installed app bundles under the
/// companion data root. Read-only.
#[derive(Debug, Deserialize)]
pub struct AppListRequest {
    #[serde(default)]
    pub scope: ServiceScope,
    pub base_dir: Option<PathBuf>,
    pub files_base_dir: Option<PathBuf>,
}

/// The on-disk record of one installed app.
///
/// Serialized as `<bundle>/app.json` under the companion data root (e.g.
/// `/var/lib/tetra/quadlets/<name>/app.json` in system scope), tying the app
/// to the recipe and values it was cooked from.
///
/// The manifest is what makes the bundle self-contained: `update` re-renders
/// from the stored recipe source and merged values, `remove` derives the
/// units/services to tear down, and config backups can snapshot one directory
/// per app. `values` may contain secrets, so the manifest is root-owned data
/// and is excluded from secret-free backups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    /// Manifest schema version; currently 1.
    pub version: u32,
    pub name: String,
    pub scope: ServiceScope,
    pub recipe_id: String,
    pub recipe_version: String,
    pub recipe: AppRecipeSource,
    #[serde(default)]
    pub values: BTreeMap<String, YamlValue>,
    /// Quadlet unit filenames installed into the scan directory.
    #[serde(default)]
    pub units: Vec<String>,
    /// Companion file paths, relative to the bundle directory.
    #[serde(default)]
    pub files: Vec<String>,
    /// Seconds since the Unix epoch.
    pub created_at: u64,
    pub updated_at: u64,
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
