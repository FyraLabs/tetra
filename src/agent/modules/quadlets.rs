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
        "list_files",
    ],
};

const QUADLET_EXTENSIONS: &[&str] = &["container", "kube", "network", "pod", "volume"];

#[derive(Debug, Deserialize)]
struct BasePayload {
    base_dir: Option<PathBuf>,
    files_base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
}

#[derive(Debug, Deserialize)]
struct FilePayload {
    base_dir: Option<PathBuf>,
    files_base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
    filename: String,
    #[serde(default)]
    companion: bool,
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
    files_base_dir: Option<PathBuf>,
    #[serde(default)]
    scope: QuadletScope,
    resources: Vec<InstallResource>,
    #[serde(default)]
    files: Vec<InstallResource>,
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

#[derive(Debug, Serialize)]
struct ManagedFile {
    filename: String,
    path: PathBuf,
    quadlet: bool,
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
            "list_files" => {
                let payload: BasePayload = parse_payload(payload)?;
                let base_dir = quadlet_base_dir(payload.base_dir, payload.scope)?;
                let files_base_dir =
                    quadlet_files_base_dir(payload.files_base_dir, payload.scope, None)?;
                let mut files = list_quadlet_files(&base_dir)?;
                files.extend(list_companion_files(&files_base_dir)?);
                files.sort_by(|left, right| {
                    left.quadlet
                        .cmp(&right.quadlet)
                        .reverse()
                        .then_with(|| left.filename.cmp(&right.filename))
                });
                Ok(json!({
                    "base_dir": base_dir,
                    "files_base_dir": files_base_dir,
                    "files": files
                }))
            }
            "read" => {
                let payload: FilePayload = parse_payload(payload)?;
                let bundle_name = if payload.companion {
                    None
                } else {
                    Some(quadlet_bundle_name(&payload.filename)?)
                };
                let base_dir = if payload.companion {
                    quadlet_files_base_dir(
                        payload.files_base_dir,
                        payload.scope,
                        bundle_name.as_deref(),
                    )?
                } else {
                    quadlet_base_dir(payload.base_dir, payload.scope)?
                };
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
                let bundle_name = payload
                    .resources
                    .first()
                    .map(|resource| quadlet_bundle_name(&resource.filename))
                    .transpose()?;
                let files_base_dir = quadlet_files_base_dir(
                    payload.files_base_dir,
                    payload.scope,
                    bundle_name.as_deref(),
                )?;
                if !payload.dry_run {
                    fs::create_dir_all(&base_dir)
                        .with_context(|| format!("failed to create `{}`", base_dir.display()))?;
                    fs::create_dir_all(&files_base_dir).with_context(|| {
                        format!("failed to create `{}`", files_base_dir.display())
                    })?;
                }

                let mut installed = Vec::new();
                let mut selinux = Vec::new();
                for resource in payload.resources {
                    validate_quadlet(&resource.filename, &resource.contents)?;
                    let path = write_resource(&base_dir, &resource, payload.dry_run)?;
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
                let mut files = Vec::new();
                for resource in payload.files {
                    let path = write_resource(&files_base_dir, &resource, payload.dry_run)?;
                    selinux.extend(apply_selinux(
                        resource.selinux.as_ref(),
                        Some(&path),
                        payload.dry_run,
                    )?);
                    files.push(ManagedFile {
                        filename: resource.filename,
                        path,
                        quadlet: false,
                    });
                }
                selinux.extend(apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&base_dir),
                    payload.dry_run,
                )?);

                Ok(json!({
                    "base_dir": base_dir,
                    "files_base_dir": files_base_dir,
                    "installed": installed,
                    "files": files,
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

fn quadlet_files_base_dir(
    files_base_dir: Option<PathBuf>,
    scope: QuadletScope,
    bundle_name: Option<&str>,
) -> Result<PathBuf> {
    let base = if let Some(files_base_dir) = files_base_dir {
        files_base_dir
    } else {
        match scope {
            QuadletScope::User => {
                let xdg_data_home = std::env::var_os("XDG_DATA_HOME");
                let base = if let Some(xdg_data_home) = xdg_data_home {
                    PathBuf::from(xdg_data_home)
                } else {
                    let home = std::env::var_os("HOME")
                        .context("HOME is not set and no files_base_dir was provided")?;
                    PathBuf::from(home).join(".local/share")
                };
                base.join("tetra/quadlets")
            }
            QuadletScope::System => PathBuf::from("/var/lib/tetra/quadlets"),
        }
    };

    if let Some(bundle_name) = bundle_name {
        safe_join(&base, bundle_name)
    } else {
        Ok(base)
    }
}

fn quadlet_bundle_name(filename: &str) -> Result<String> {
    let path = Path::new(filename);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("path `{filename}` must be relative and stay within the base directory");
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Quadlet filename must include a file name")?;
    let Some((stem, extension)) = file_name.rsplit_once('.') else {
        bail!("`{filename}` is not a supported Quadlet filename");
    };
    if !QUADLET_EXTENSIONS.contains(&extension) || stem.is_empty() {
        bail!("`{filename}` is not a supported Quadlet filename");
    }
    Ok(stem.to_string())
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

fn list_quadlet_files(base_dir: &Path) -> Result<Vec<ManagedFile>> {
    Ok(list_quadlets(base_dir)?
        .into_iter()
        .map(|file| ManagedFile {
            filename: file.filename,
            path: file.path,
            quadlet: true,
        })
        .collect())
}

fn list_companion_files(base_dir: &Path) -> Result<Vec<ManagedFile>> {
    if !base_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    collect_files(base_dir, base_dir, &mut files)?;
    files.sort_by(|left, right| left.filename.cmp(&right.filename));
    Ok(files)
}

fn collect_files(base_dir: &Path, dir: &Path, files: &mut Vec<ManagedFile>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read `{}`", dir.display()))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(base_dir, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let filename = path
            .strip_prefix(base_dir)
            .with_context(|| format!("failed to make `{}` relative", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        files.push(ManagedFile {
            quadlet: is_quadlet_filename(&filename),
            filename,
            path,
        });
    }

    Ok(())
}

fn write_resource(base_dir: &Path, resource: &InstallResource, dry_run: bool) -> Result<PathBuf> {
    let path = safe_join(base_dir, &resource.filename)?;
    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
        fs::write(&path, &resource.contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
    }
    Ok(path)
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
    fn system_scope_uses_var_lib_for_companion_files() {
        assert_eq!(
            quadlet_files_base_dir(None, QuadletScope::System, Some("app")).unwrap(),
            PathBuf::from("/var/lib/tetra/quadlets/app")
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

    #[test]
    fn install_writes_companion_files_under_mutable_files_base_dir() {
        let quadlet_dir = tempfile::tempdir().unwrap();
        let files_dir = tempfile::tempdir().unwrap();
        let response = QuadletsModule
            .handle(
                "install",
                json!({
                    "base_dir": quadlet_dir.path(),
                    "files_base_dir": files_dir.path(),
                    "resources": [
                        {
                            "filename": "site.container",
                            "contents": "[Container]\nImage=nginx\n"
                        }
                    ],
                    "files": [
                        {
                            "filename": "index.html",
                            "contents": "<h1>Hello</h1>\n"
                        },
                        {
                            "filename": "nginx/default.conf",
                            "contents": "server {}\n"
                        }
                    ]
                }),
            )
            .unwrap();

        assert_eq!(response["written"], true);
        assert_eq!(
            response["base_dir"],
            quadlet_dir.path().to_string_lossy().as_ref()
        );
        assert_eq!(
            response["files_base_dir"],
            files_dir.path().join("site").to_string_lossy().as_ref()
        );
        assert_eq!(response["files"].as_array().unwrap().len(), 2);
        assert_eq!(
            fs::read_to_string(files_dir.path().join("site/index.html")).unwrap(),
            "<h1>Hello</h1>\n"
        );
        assert_eq!(
            fs::read_to_string(files_dir.path().join("site/nginx/default.conf")).unwrap(),
            "server {}\n"
        );
        assert!(!quadlet_dir.path().join("site/index.html").exists());
    }

    #[test]
    fn list_files_includes_quadlets_and_companion_files() {
        let quadlet_dir = tempfile::tempdir().unwrap();
        let files_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(files_dir.path().join("site")).unwrap();
        fs::write(
            quadlet_dir.path().join("site.container"),
            "[Container]\nImage=nginx\n",
        )
        .unwrap();
        fs::write(files_dir.path().join("site/index.html"), "<h1>Hello</h1>\n").unwrap();

        let response = QuadletsModule
            .handle(
                "list_files",
                json!({ "base_dir": quadlet_dir.path(), "files_base_dir": files_dir.path() }),
            )
            .unwrap();
        let files = response["files"].as_array().unwrap();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["filename"], "site.container");
        assert_eq!(files[0]["quadlet"], true);
        assert_eq!(files[1]["filename"], "site/index.html");
        assert_eq!(files[1]["quadlet"], false);
    }

    #[test]
    fn read_can_load_companion_files_from_files_base_dir() {
        let files_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(files_dir.path().join("site")).unwrap();
        fs::write(files_dir.path().join("site/index.html"), "<h1>Hello</h1>\n").unwrap();

        let response = QuadletsModule
            .handle(
                "read",
                json!({
                    "files_base_dir": files_dir.path(),
                    "filename": "site/index.html",
                    "companion": true
                }),
            )
            .unwrap();

        assert_eq!(response["contents"], "<h1>Hello</h1>\n");
    }
}
