use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata, parse_payload,
        safe_join, unsupported_action,
    },
};

pub struct QuadletsModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "quadlets",
    feature: "quadlets",
    description: "Manage Quadlet files separately from systemd unit service control.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "read",
        "write",
        "delete",
        "validate",
        "install",
    ],
};

const QUADLET_EXTENSIONS: &[&str] = &["container", "kube", "network", "pod", "volume"];

#[derive(Debug, Deserialize)]
struct BasePayload {
    base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
}

#[derive(Debug, Deserialize)]
struct FilePayload {
    base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
    filename: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct WritePayload {
    base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
    filename: String,
    contents: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

#[derive(Debug, Deserialize)]
struct InstallPayload {
    base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
    resources: Vec<InstallResource>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

#[derive(Debug, Deserialize)]
struct InstallResource {
    filename: String,
    contents: String,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

#[derive(Debug, Serialize)]
struct QuadletFile {
    filename: String,
    path: PathBuf,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
enum QuadletScope {
    #[default]
    User,
    System,
}


impl AgentModule for QuadletsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => {
                let payload: BasePayload = parse_payload(payload)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                Ok(json!({ "base_dir": base_dir, "files": list_quadlets(&base_dir)? }))
            }
            "read" => {
                let payload: FilePayload = parse_payload(payload)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                let path = safe_join(&base_dir, &payload.filename)?;
                let contents = fs::read_to_string(&path)
                    .with_context(|| format!("failed to read `{}`", path.display()))?;
                Ok(
                    json!({ "base_dir": base_dir, "filename": payload.filename, "contents": contents }),
                )
            }
            "write" => {
                let payload: WritePayload = parse_payload(payload)?;
                validate_quadlet(&payload.filename, &payload.contents)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                let path = safe_join(&base_dir, &payload.filename)?;
                if !payload.dry_run {
                    fs::create_dir_all(&base_dir)
                        .with_context(|| format!("failed to create `{}`", base_dir.display()))?;
                    fs::write(&path, &payload.contents)
                        .with_context(|| format!("failed to write `{}`", path.display()))?;
                }
                let selinux =
                    apply_selinux(payload.selinux.as_ref(), Some(&path), payload.dry_run)?;
                Ok(json!({
                    "base_dir": base_dir,
                    "filename": payload.filename,
                    "path": path,
                    "written": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "selinux": selinux,
                }))
            }
            "delete" => {
                let payload: FilePayload = parse_payload(payload)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                let path = safe_join(&base_dir, &payload.filename)?;
                if !payload.dry_run {
                    fs::remove_file(&path)
                        .with_context(|| format!("failed to delete `{}`", path.display()))?;
                }
                Ok(json!({
                    "base_dir": base_dir,
                    "filename": payload.filename,
                    "path": path,
                    "deleted": !payload.dry_run,
                    "dry_run": payload.dry_run,
                }))
            }
            "validate" => {
                let payload: WritePayload = parse_payload(payload)?;
                validate_quadlet(&payload.filename, &payload.contents)?;
                Ok(json!({ "filename": payload.filename, "valid": true }))
            }
            "install" => {
                let payload: InstallPayload = parse_payload(payload)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                if !payload.dry_run {
                    fs::create_dir_all(&base_dir)
                        .with_context(|| format!("failed to create `{}`", base_dir.display()))?;
                }

                let mut installed = Vec::new();
                let mut selinux = Vec::new();
                for resource in payload.resources {
                    validate_quadlet(&resource.filename, &resource.contents)?;
                    let path = safe_join(&base_dir, &resource.filename)?;
                    if !payload.dry_run {
                        fs::write(&path, &resource.contents)
                            .with_context(|| format!("failed to write `{}`", path.display()))?;
                    }
                    selinux.extend(apply_selinux(
                        resource.selinux.as_ref(),
                        Some(&path),
                        payload.dry_run,
                    )?);
                    installed.push(QuadletFile {
                        filename: resource.filename,
                        path,
                    });
                }
                selinux.extend(apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&base_dir),
                    payload.dry_run,
                )?);

                Ok(json!({
                    "base_dir": base_dir,
                    "installed": installed,
                    "written": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "selinux": selinux,
                }))
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn quadlet_base_dir(base_dir: Option<PathBuf>, scope: QuadletScope) -> Result<PathBuf> {
    if let Some(base_dir) = base_dir {
        return Ok(base_dir);
    }

    match scope {
        QuadletScope::User => {
            let home =
                std::env::var_os("HOME").context("HOME is not set and no base_dir was provided")?;
            Ok(PathBuf::from(home).join(".config/containers/systemd"))
        }
        QuadletScope::System => Ok(PathBuf::from("/etc/containers/systemd")),
    }
}

fn list_quadlets(base_dir: &Path) -> Result<Vec<QuadletFile>> {
    if !base_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(base_dir)
        .with_context(|| format!("failed to read `{}`", base_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if is_quadlet_filename(filename) {
            files.push(QuadletFile {
                filename: filename.to_string(),
                path,
            });
        }
    }
    files.sort_by(|left, right| left.filename.cmp(&right.filename));
    Ok(files)
}

fn validate_quadlet(filename: &str, contents: &str) -> Result<()> {
    if !is_quadlet_filename(filename) {
        bail!("`{filename}` is not a supported Quadlet filename");
    }
    if contents.trim().is_empty() {
        bail!("Quadlet contents cannot be empty");
    }
    if !contents.lines().any(|line| {
        matches!(
            line.trim(),
            "[Container]" | "[Kube]" | "[Network]" | "[Pod]" | "[Volume]"
        )
    }) {
        bail!("Quadlet contents must include a Quadlet section");
    }
    Ok(())
}

fn is_quadlet_filename(filename: &str) -> bool {
    QUADLET_EXTENSIONS
        .iter()
        .any(|extension| filename.ends_with(&format!(".{extension}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_outside_base_dir() {
        let base = Path::new("/tmp/quadlets");
        assert!(safe_join(base, "../unit.container").is_err());
        assert!(safe_join(base, "/tmp/unit.container").is_err());
        assert_eq!(
            safe_join(base, "unit.container").unwrap(),
            base.join("unit.container")
        );
    }

    #[test]
    fn validates_quadlet_sections_and_extensions() {
        assert!(validate_quadlet("app.container", "[Container]\nImage=example\n").is_ok());
        assert!(validate_quadlet("app.service", "[Container]\nImage=example\n").is_err());
        assert!(validate_quadlet("app.container", "[Service]\nExecStart=true\n").is_err());
    }

    #[test]
    fn system_scope_uses_system_quadlet_directory() {
        assert_eq!(
            quadlet_base_dir(None, QuadletScope::System).unwrap(),
            PathBuf::from("/etc/containers/systemd")
        );
    }

    #[test]
    fn dry_run_write_does_not_create_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.container");
        let response = QuadletsModule
            .handle(
                "write",
                json!({
                    "base_dir": dir.path(),
                    "filename": "app.container",
                    "contents": "[Container]\nImage=example\n",
                    "dry_run": true
                }),
            )
            .unwrap();

        assert_eq!(response["dry_run"], true);
        assert_eq!(response["written"], false);
        assert!(!path.exists());
    }

    #[test]
    fn install_can_restore_quadlet_contexts() {
        let dir = tempfile::tempdir().unwrap();
        let response = QuadletsModule
            .handle(
                "install",
                json!({
                    "base_dir": dir.path(),
                    "dry_run": true,
                    "resources": [
                        {
                            "filename": "app.container",
                            "contents": "[Container]\nImage=example\n"
                        }
                    ],
                    "selinux": {
                        "context_type": "container_unit_file_t",
                        "recursive": true
                    }
                }),
            )
            .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert!(
            response["selinux"][0]["command"]
                .as_str()
                .unwrap()
                .contains("semanage fcontext -a -t container_unit_file_t")
        );
        assert!(
            response["selinux"][1]["command"]
                .as_str()
                .unwrap()
                .contains("restorecon -R -v")
        );
    }
}
