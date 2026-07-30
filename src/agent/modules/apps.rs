//! App lifecycle: cook recipes into installed, Quadlet-backed apps.
//!
//! Where the `quadlets` module manages individual files and the `recipes`
//! module renders recipe bodies, this module ties a cooked recipe to its
//! installed units and companion files as one lifecycle: `create` an app,
//! `get`/`list` what is installed, `update` it, and `remove` it.
//!
//! ## App bundle layout
//!
//! An installed app is two trees plus a manifest:
//!
//! - **Quadlet unit files** go to the directory Podman scans
//!   (`/etc/containers/systemd` system-wide, `~/.config/containers/systemd`
//!   per-user), so a `daemon-reload` turns them into generated services.
//! - **Companion files** (reverse-proxy snippets, app config, content) go to
//!   a per-app bundle directory under the mutable data root:
//!   `/var/lib/tetra/quadlets/<name>/` system-wide or
//!   `$XDG_DATA_HOME/tetra/quadlets/<name>/` per-user. Keeping them out of
//!   the image-managed scan dir is what makes the layout bootc-friendly.
//! - **`<bundle>/app.json`** is the [`AppManifest`]: the recipe source and
//!   merged values the app was cooked from, plus the list of installed units
//!   and companions. It is what lets `update` re-render and `remove` tear
//!   down without guessing.
//!
//! ## The `create` / `update` pipeline
//!
//! 1. Render the recipe (inline bundle or on-disk paths) with the merged
//!    parameter values.
//! 2. Write companion files into the bundle directory and Quadlet units into
//!    the scan directory (`update` first deletes units/companions that the
//!    new render no longer produces).
//! 3. Write `<bundle>/app.json`.
//! 4. Optionally apply the shared `selinux` payload to the bundle directory.
//! 5. Unless `converge: false`, converge systemd: `daemon-reload`, then
//!    `enable` + `start` (create) or `restart` (update) each service derived
//!    from the app's `.container`/`.kube`/`.pod` units. `.network`/`.volume`
//!    units are not managed directly — Podman pulls them in as dependencies
//!    of the containers.
//!
//! All systemd interaction is delegated to the `services` module and all path
//! handling reuses the `quadlets` module's scope/traversal-safe helpers, so
//! every step stays representable as plain module actions. `remove` stops and
//! disables the app's services, deletes its units, reloads systemd, and
//! removes the bundle directory; stop/disable failures are tolerated (and
//! reported) so removal still cleans up after partial drift. Note that
//! Podman volumes created by `.volume` units are *not* deleted — app data
//! survives removal by design.
//!
//! Every mutating action honors `dry_run`, and the shared `plan` meta-action
//! echoes the request, so the dashboard can preview the exact pipeline —
//! rendered resources, target paths, and `systemctl` commands — before
//! committing.

use serde_yaml::Value as YamlValue;

use crate::agent::module_support::{apply_selinux, safe_join};
use crate::agent::modules::quadlets::{
    QuadletScope, list_companion_files, quadlet_base_dir, quadlet_files_base_dir, validate_quadlet,
};
use crate::catalog::{AppRecipe, RenderedResource, ResourceKind, load_values};
use crate::prelude::*;
use crate::types::{
    AppCreateRequest, AppGetRequest, AppListRequest, AppManifest, AppRecipeSource, AppRequest,
    AppUpdateRequest, ServiceScope,
};

use super::ServicesModule;

/// Agent module backing the `apps` feature. Stateless; the lifecycle state
/// lives on the host in the bundle layout described in the module docs.
#[derive(Clone, Copy, Debug)]
pub struct AppsModule;

/// Static module descriptor advertised via `capabilities`/`plan`. `create`,
/// `update`, and `remove` are privileged: they write system directories and
/// drive `systemctl`.
const INFO: ModuleInfo = ModuleInfo {
    name: "apps",
    feature: "apps",
    description: "Cook recipes into installed Quadlet-backed apps and manage their lifecycle.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "get",
        "create",
        "update",
        "remove",
    ],
    privileged_actions: &["create", "update", "remove"],
};

/// Manifest filename inside every bundle directory. Chosen over `manifest.json`
/// so it sorts first and reads as the bundle's entry point.
const APP_MANIFEST: &str = "app.json";

/// Current manifest schema version written by `create`/`update`.
const MANIFEST_VERSION: u32 = 1;

impl Mod for AppsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the module descriptor first.
        if let Some(response) = INFO.metadata_response(action, &payload) {
            return Ok(response);
        }
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [payload user] => {
    List: AppListRequest => {
        let (_base_dir, files_root) = app_dirs(payload.base_dir, payload.files_base_dir, payload.scope)?;
        let mut apps = Vec::new();
        // Directories under the data root whose `app.json` cannot be read are
        // reported by name rather than failing the whole listing — they are
        // either corrupted bundles or stray unmanaged directories, both of
        // which the dashboard should surface.
        let mut invalid = Vec::new();
        if files_root.exists() {
            for entry in fs::read_dir(&files_root)
                .with_context(|| format!("failed to read `{}`", files_root.display()))?
            {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let bundle_dir = entry.path();
                match load_manifest(&bundle_dir) {
                    Ok(manifest) => apps.push(app_summary(&bundle_dir, &manifest)),
                    Err(_) => invalid.push(entry.file_name().to_string_lossy().into_owned()),
                }
            }
        }
        apps.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        invalid.sort();
        Ok(jsonf! { "files_base_dir": files_root, apps, invalid })
    },
    Get: AppGetRequest => {
        validate_app_name(&payload.name)?;
        let (base_dir, files_root) = app_dirs(payload.base_dir, payload.files_base_dir, payload.scope)?;
        let bundle_dir = safe_join(&files_root, &payload.name)?;
        let manifest = load_manifest(&bundle_dir)?;
        // Report the on-disk state alongside the manifest so drift (a unit
        // deleted behind the agent's back) is visible to the dashboard.
        let units = manifest
            .units
            .iter()
            .map(|filename| {
                let path = base_dir.join(filename);
                jsonf! { filename, path, "exists": path.exists() }
            })
            .collect::<Vec<_>>();
        let files = list_companion_files(&bundle_dir)?
            .into_iter()
            .filter(|file| file.filename() != APP_MANIFEST)
            .collect::<Vec<_>>();
        let services = manifest
            .units
            .iter()
            .filter_map(|unit| unit_service_name(unit))
            .collect::<Vec<_>>();
        Ok(jsonf! { "app": manifest, base_dir, bundle_dir, units, files, services })
    },
    Create: AppCreateRequest => {
        validate_app_name(&payload.name)?;
        let (base_dir, files_root) = app_dirs(payload.base_dir.clone(), payload.files_base_dir.clone(), payload.scope)?;
        let bundle_dir = safe_join(&files_root, &payload.name)?;
        ensure!(
            !bundle_dir.exists(),
            "app `{}` already exists; use `update` to modify it",
            payload.name
        );

        let source = create_source(&payload)?;
        // Inline values win over the optional values file, so a dashboard can
        // ship recipe defaults plus per-instance overrides in one call.
        let mut values = load_values(payload.values_path.as_ref())?;
        values.extend(payload.values);
        inject_bundle_dir(&mut values, &bundle_dir);
        let (recipe, resources) = render_recipe(&source, &values)?;

        let outcome = install_bundle(&base_dir, &bundle_dir, &resources, None, payload.dry_run)?;
        let now = epoch_secs();
        let manifest = AppManifest {
            version: MANIFEST_VERSION,
            name: payload.name,
            scope: payload.scope,
            recipe_id: recipe.recipe_id,
            recipe_version: recipe.version,
            recipe: source,
            values,
            units: outcome.units.clone(),
            files: outcome.files.clone(),
            created_at: now,
            updated_at: now,
        };
        let manifest_path = write_manifest(&bundle_dir, &manifest, payload.dry_run)?;
        let selinux = apply_selinux(payload.selinux.as_ref(), Some(&bundle_dir), payload.dry_run)?;
        let services = outcome
            .units
            .iter()
            .filter_map(|unit| unit_service_name(unit))
            .collect::<Vec<_>>();
        let systemd = if payload.converge {
            systemd_converge(&services, payload.scope, "start", payload.dry_run, user)?
        } else {
            Vec::new()
        };
        Ok(jsonf! {
            "app": manifest,
            base_dir, bundle_dir, manifest_path,
            outcome.units, outcome.files,
            services, systemd, selinux,
            "written": !payload.dry_run,
            payload.dry_run,
        })
    },
    Update: AppUpdateRequest => {
        validate_app_name(&payload.name)?;
        let (base_dir, files_root) = app_dirs(payload.base_dir.clone(), payload.files_base_dir.clone(), payload.scope)?;
        let bundle_dir = safe_join(&files_root, &payload.name)?;
        let mut manifest = load_manifest(&bundle_dir)?;

        // A new recipe source replaces the stored one (recipe upgrade);
        // values merge per-key so secrets collected earlier are not resent.
        if let Some(source) = update_source(&payload)? {
            manifest.recipe = source;
        }
        manifest.values.extend(payload.values);
        inject_bundle_dir(&mut manifest.values, &bundle_dir);
        let (recipe, resources) = render_recipe(&manifest.recipe, &manifest.values)?;
        manifest.recipe_id = recipe.recipe_id;
        manifest.recipe_version = recipe.version;

        let outcome = install_bundle(&base_dir, &bundle_dir, &resources, Some(&manifest), payload.dry_run)?;
        manifest.units.clone_from(&outcome.units);
        manifest.files.clone_from(&outcome.files);
        manifest.updated_at = epoch_secs();
        let manifest_path = write_manifest(&bundle_dir, &manifest, payload.dry_run)?;
        let selinux = apply_selinux(payload.selinux.as_ref(), Some(&bundle_dir), payload.dry_run)?;
        let services = outcome
            .units
            .iter()
            .filter_map(|unit| unit_service_name(unit))
            .collect::<Vec<_>>();
        let systemd = if payload.converge {
            systemd_converge(&services, payload.scope, "restart", payload.dry_run, user)?
        } else {
            Vec::new()
        };
        Ok(jsonf! {
            "app": manifest,
            base_dir, bundle_dir, manifest_path,
            outcome.units, outcome.files,
            outcome.removed_units, outcome.removed_files,
            services, systemd, selinux,
            "written": !payload.dry_run,
            payload.dry_run,
        })
    },
    Remove: AppRequest => {
        validate_app_name(&payload.name)?;
        let (base_dir, files_root) = app_dirs(payload.base_dir, payload.files_base_dir, payload.scope)?;
        let bundle_dir = safe_join(&files_root, &payload.name)?;
        let manifest = load_manifest(&bundle_dir)?;
        let services = manifest
            .units
            .iter()
            .filter_map(|unit| unit_service_name(unit))
            .collect::<Vec<_>>();

        // Stop/disable before deleting anything so systemd can still resolve
        // the units. Failures are reported but tolerated: the unit may
        // already be gone, and removal should still clean up the bundle.
        let mut systemd = Vec::new();
        if payload.converge {
            for service in &services {
                for action in ["stop", "disable"] {
                    match services_action(action, service, payload.scope, payload.dry_run, user) {
                        Ok(op) => systemd.push(op),
                        Err(error) => systemd.push(jsonf! {
                            action, service, "error": error.to_string(),
                        }),
                    }
                }
            }
        }
        let mut deleted_units = Vec::new();
        for unit in &manifest.units {
            let path = safe_join(&base_dir, unit)?;
            if !payload.dry_run && path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to delete `{}`", path.display()))?;
            }
            deleted_units.push(path);
        }
        if payload.converge {
            systemd.push(daemon_reload(payload.scope, payload.dry_run, user)?);
        }
        if !payload.dry_run {
            fs::remove_dir_all(&bundle_dir)
                .with_context(|| format!("failed to remove `{}`", bundle_dir.display()))?;
        }
        Ok(jsonf! {
            payload.name, bundle_dir, deleted_units, services, systemd,
            "bundle_removed": !payload.dry_run,
            payload.dry_run,
        })
    },
});

/// Result of the file-install phase shared by `create` and `update`. Kept
/// separate from the systemd phase so hosts (and tests) can run a files-only
/// pass with `converge: false`.
#[derive(Debug, Default)]
struct InstallOutcome {
    /// Filenames of Quadlet units installed into the scan directory.
    units: Vec<String>,
    /// Companion file paths installed into the bundle, relative to it.
    files: Vec<String>,
    /// Units deleted because the new render no longer produces them.
    removed_units: Vec<String>,
    /// Companion files deleted for the same reason.
    removed_files: Vec<String>,
}

/// Resolve the Quadlet scan directory and the companion data root for an
/// apps request. Apps default to system scope (the agent normally runs as a
/// root system service); the overrides keep the protocol testable.
fn app_dirs(
    base_dir: Option<PathBuf>,
    files_base_dir: Option<PathBuf>,
    scope: ServiceScope,
) -> Result<(PathBuf, PathBuf)> {
    let scope = match scope {
        ServiceScope::System => QuadletScope::System,
        ServiceScope::User => QuadletScope::User,
    };
    let base_dir = quadlet_base_dir(base_dir, scope)?;
    let files_root = quadlet_files_base_dir(files_base_dir, scope, None)?;
    Ok((base_dir, files_root))
}

/// App names become directory and file names, so keep them to a single safe
/// path component. This is stricter than `safe_join` (which only rejects
/// traversal) because the name also shows up in systemd unit names.
fn validate_app_name(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "app name cannot be empty");
    ensure!(
        !name.starts_with('.')
            && !name.starts_with('-')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
        "app name `{name}` must start with an alphanumeric and contain only alphanumerics, `.`, `_`, `-`"
    );
    Ok(())
}

/// Recipes that declare a `bundle_dir` parameter (typically for bind mounts)
/// receive the real per-app bundle directory unless the caller supplied one.
/// Values for undeclared parameters are dropped by the renderer, so injecting
/// unconditionally is safe for recipes without the parameter.
fn inject_bundle_dir(values: &mut BTreeMap<String, YamlValue>, bundle_dir: &Path) {
    values
        .entry("bundle_dir".to_owned())
        .or_insert_with(|| YamlValue::String(bundle_dir.display().to_string()));
}

/// Derive the systemd service Quadlet generates for a unit filename, or
/// `None` for units that are not directly started (`.network`, `.volume` —
/// Podman pulls those in as dependencies of the containers that use them).
/// `.container`/`.kube` produce `<stem>.service`; `.pod` produces
/// `<stem>-pod.service`.
fn unit_service_name(filename: &str) -> Option<String> {
    let (stem, extension) = filename.rsplit_once('.')?;
    match extension {
        "container" | "kube" => Some(format!("{stem}.service")),
        "pod" => Some(format!("{stem}-pod.service")),
        _ => None,
    }
}

/// Build the recipe source for a `create`: exactly one of the inline bundle
/// or the on-disk paths must be given.
fn create_source(payload: &AppCreateRequest) -> Result<AppRecipeSource> {
    match (&payload.recipe, &payload.recipe_path) {
        (Some(recipe), None) => Ok(AppRecipeSource::Inline {
            recipe: recipe.clone(),
            templates: payload.templates.clone(),
        }),
        (None, Some(recipe_path)) => Ok(AppRecipeSource::File {
            recipe_path: recipe_path.clone(),
            templates_dir: payload
                .templates_dir
                .clone()
                .context("`templates_dir` is required with `recipe_path`")?,
        }),
        (Some(_), Some(_)) => bail!("pass either `recipe` or `recipe_path`, not both"),
        (None, None) => bail!("`create` requires either `recipe` (inline) or `recipe_path` (file)"),
    }
}

/// Build the replacement recipe source for an `update`, or `None` to keep the
/// stored one. An inline recipe with no `templates` replaces the template
/// bundle with an empty one; callers keeping the same templates can omit the
/// field only when the stored source is reused unchanged.
fn update_source(payload: &AppUpdateRequest) -> Result<Option<AppRecipeSource>> {
    match (&payload.recipe, &payload.recipe_path) {
        (Some(recipe), None) => Ok(Some(AppRecipeSource::Inline {
            recipe: recipe.clone(),
            templates: payload.templates.clone().unwrap_or_default(),
        })),
        (None, Some(recipe_path)) => Ok(Some(AppRecipeSource::File {
            recipe_path: recipe_path.clone(),
            templates_dir: payload
                .templates_dir
                .clone()
                .context("`templates_dir` is required with `recipe_path`")?,
        })),
        (Some(_), Some(_)) => bail!("pass either `recipe` or `recipe_path`, not both"),
        (None, None) => Ok(None),
    }
}

/// Render a recipe from either source kind, returning the parsed recipe
/// alongside the resources so callers can record `recipe_id`/`version`.
fn render_recipe(
    source: &AppRecipeSource,
    values: &BTreeMap<String, YamlValue>,
) -> Result<(AppRecipe, Vec<RenderedResource>)> {
    match source {
        AppRecipeSource::Inline { recipe, templates } => {
            let recipe = AppRecipe::load_str(recipe)?;
            let resources = recipe.render_with_templates(values, templates)?;
            Ok((recipe, resources))
        }
        AppRecipeSource::File {
            recipe_path,
            templates_dir,
        } => {
            let recipe = AppRecipe::load(recipe_path)?;
            let resources = recipe.render(values, templates_dir)?;
            Ok((recipe, resources))
        }
    }
}

/// Read and parse `<bundle_dir>/app.json`.
fn load_manifest(bundle_dir: &Path) -> Result<AppManifest> {
    let path = bundle_dir.join(APP_MANIFEST);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read app manifest `{}`", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse app manifest `{}`", path.display()))
}

/// Serialize the manifest into the bundle. Skipped on dry runs; returns the
/// target path either way so previews show where it would land.
fn write_manifest(bundle_dir: &Path, manifest: &AppManifest, dry_run: bool) -> Result<PathBuf> {
    let path = bundle_dir.join(APP_MANIFEST);
    if !dry_run {
        let contents =
            serde_json::to_string_pretty(manifest).context("failed to serialize app manifest")?;
        fs::write(&path, contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
    }
    Ok(path)
}

/// Write one file under `base_dir`, creating parent directories for nested
/// companion paths. `safe_join` keeps rendered filenames inside the base.
fn write_file(base_dir: &Path, filename: &str, contents: &str, dry_run: bool) -> Result<PathBuf> {
    let path = safe_join(base_dir, filename)?;
    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
        fs::write(&path, contents)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
    }
    Ok(path)
}

/// Install a render: Quadlet resources into the scan directory, `file`
/// resources into the bundle directory. When `previous` is given (update),
/// units and companions the new render no longer produces are deleted.
/// Directories are created up front so permission/disk failures surface
/// before any file is written; Quadlet validation likewise happens before
/// the first write.
fn install_bundle(
    base_dir: &Path,
    bundle_dir: &Path,
    resources: &[RenderedResource],
    previous: Option<&AppManifest>,
    dry_run: bool,
) -> Result<InstallOutcome> {
    let (units, companions): (Vec<&RenderedResource>, Vec<&RenderedResource>) = resources
        .iter()
        .partition(|resource| resource.kind != ResourceKind::File);
    for unit in &units {
        validate_quadlet(&unit.filename, &unit.contents)?;
    }

    if !dry_run {
        fs::create_dir_all(base_dir)
            .with_context(|| format!("failed to create `{}`", base_dir.display()))?;
        fs::create_dir_all(bundle_dir)
            .with_context(|| format!("failed to create `{}`", bundle_dir.display()))?;
    }

    let mut outcome = InstallOutcome::default();
    if let Some(previous) = previous {
        let desired_units: HashSet<&str> =
            units.iter().map(|unit| unit.filename.as_str()).collect();
        for unit in &previous.units {
            if desired_units.contains(unit.as_str()) {
                continue;
            }
            let path = safe_join(base_dir, unit)?;
            if !dry_run && path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to delete `{}`", path.display()))?;
            }
            outcome.removed_units.push(unit.clone());
        }
        let desired_files: HashSet<&str> = companions
            .iter()
            .map(|file| file.filename.as_str())
            .collect();
        for file in &previous.files {
            if desired_files.contains(file.as_str()) {
                continue;
            }
            let path = safe_join(bundle_dir, file)?;
            if !dry_run && path.exists() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to delete `{}`", path.display()))?;
            }
            outcome.removed_files.push(file.clone());
        }
    }

    for unit in units {
        write_file(base_dir, &unit.filename, &unit.contents, dry_run)?;
        outcome.units.push(unit.filename.clone());
    }
    for companion in companions {
        write_file(
            bundle_dir,
            &companion.filename,
            &companion.contents,
            dry_run,
        )?;
        outcome.files.push(companion.filename.clone());
    }
    Ok(outcome)
}

/// One-line summary of an installed app for the `list` action. Built from a
/// borrow of the manifest; the manifest itself stays available for `get`.
fn app_summary(bundle_dir: &Path, manifest: &AppManifest) -> Value {
    let AppManifest {
        name,
        recipe_id,
        recipe_version,
        scope,
        units,
        created_at,
        updated_at,
        ..
    } = manifest;
    let services = units
        .iter()
        .filter_map(|unit| unit_service_name(unit))
        .collect::<Vec<_>>();
    jsonf! { name, recipe_id, recipe_version, scope, units, services, created_at, updated_at, bundle_dir }
}

/// Run `systemctl daemon-reload` through the services module so the command
/// shape, scope handling, and dry-run behavior stay consistent with the rest
/// of the agent.
fn daemon_reload(scope: ServiceScope, dry_run: bool, user: Option<&str>) -> Result<Value> {
    ServicesModule.handle("daemon_reload", jsonf! { scope, dry_run }, user)
}

/// Run one single-service `systemctl` action (`start`/`stop`/`restart`/
/// `enable`/`disable`) through the services module.
fn services_action(
    action: &str,
    service: &str,
    scope: ServiceScope,
    dry_run: bool,
    user: Option<&str>,
) -> Result<Value> {
    ServicesModule.handle(action, jsonf! { service, scope, dry_run }, user)
}

/// The create/update systemd phase: reload so Quadlet picks up the new unit
/// set, then enable and start (or restart, on update) each app service. The
/// per-action results are returned so the dashboard can show exactly what
/// ran.
fn systemd_converge(
    services: &[String],
    scope: ServiceScope,
    service_action: &str,
    dry_run: bool,
    user: Option<&str>,
) -> Result<Vec<Value>> {
    let mut ops = vec![daemon_reload(scope, dry_run, user)?];
    for service in services {
        ops.push(services_action("enable", service, scope, dry_run, user)?);
        ops.push(services_action(
            service_action,
            service,
            scope,
            dry_run,
            user,
        )?);
    }
    Ok(ops)
}

/// Seconds since the Unix epoch for manifest timestamps. Falls back to 0 if
/// the system clock is before the epoch rather than failing the action.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEMO_RECIPE: &str = r#"
recipe_id: demo-web
name: Demo Web
version: 0.1.0
parameters:
  - key: app_id
    label: App ID
    type: string
    default: demo-web
resources:
  - type: container
    filename: "{{ app_id }}.container"
    template: containers/demo.container.tera
  - type: file
    filename: site/index.html
    template: files/index.html.tera
"#;

    fn demo_templates() -> serde_json::Value {
        json!({
            "containers/demo.container.tera": "[Container]\nContainerName={{ app_id }}\nImage=example/demo:latest\n",
            "files/index.html.tera": "<h1>{{ app_id }}</h1>\n",
        })
    }

    /// A create payload rooted at two tempdirs with convergence disabled, so
    /// tests exercise the full file pipeline without touching systemctl.
    fn create_payload(base_dir: &Path, files_base_dir: &Path) -> Value {
        json!({
            "name": "demo",
            "base_dir": base_dir,
            "files_base_dir": files_base_dir,
            "recipe": DEMO_RECIPE,
            "templates": demo_templates(),
            "values": { "app_id": "demo-web" },
            "converge": false,
        })
    }

    fn dispatch(action: &str, payload: Value) -> Result<Value> {
        AppsModule.handle(action, payload, None)
    }

    #[test]
    fn rejects_unsafe_app_names() {
        for name in [
            "",
            ".hidden",
            "-bad",
            "../escape",
            "a/b",
            "a b",
            "app.json/",
        ] {
            assert!(validate_app_name(name).is_err(), "accepted `{name}`");
        }
        for name in ["demo", "demo-web", "web_1", "my.app"] {
            assert!(validate_app_name(name).is_ok(), "rejected `{name}`");
        }
    }

    #[test]
    fn derives_service_names_from_unit_filenames() {
        assert_eq!(
            unit_service_name("demo-web.container"),
            Some("demo-web.service".to_owned())
        );
        assert_eq!(
            unit_service_name("site.kube"),
            Some("site.service".to_owned())
        );
        assert_eq!(
            unit_service_name("pair.pod"),
            Some("pair-pod.service".to_owned())
        );
        assert_eq!(unit_service_name("demo-net.network"), None);
        assert_eq!(unit_service_name("demo-data.volume"), None);
    }

    #[test]
    fn create_dry_run_previews_pipeline_without_writing() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let mut payload = create_payload(base.path(), files.path());
        payload["converge"] = json!(true);
        payload["dry_run"] = json!(true);

        let response = dispatch("create", payload).unwrap();

        assert_eq!(response["dry_run"], true);
        assert_eq!(response["written"], false);
        assert_eq!(response["units"], json!(["demo-web.container"]));
        assert_eq!(response["files"], json!(["site/index.html"]));
        assert_eq!(response["services"], json!(["demo-web.service"]));
        let commands = response["systemd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| op["command"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            [
                "systemctl daemon-reload",
                "systemctl enable demo-web.service",
                "systemctl start demo-web.service",
            ]
        );
        assert_eq!(response["app"]["recipe_id"], "demo-web");
        // Nothing touched the filesystem.
        assert!(!base.path().join("demo-web.container").exists());
        assert!(!files.path().join("demo").exists());
    }

    #[test]
    fn create_writes_bundle_then_get_and_list_read_it_back() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();

        let response = dispatch("create", create_payload(base.path(), files.path())).unwrap();
        assert_eq!(response["written"], true);
        assert!(response["systemd"].as_array().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(base.path().join("demo-web.container")).unwrap(),
            "[Container]\nContainerName=demo-web\nImage=example/demo:latest\n"
        );
        let bundle = files.path().join("demo");
        assert_eq!(
            fs::read_to_string(bundle.join("site/index.html")).unwrap(),
            "<h1>demo-web</h1>\n"
        );
        let manifest: AppManifest =
            serde_json::from_str(&fs::read_to_string(bundle.join("app.json")).unwrap()).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.recipe_id, "demo-web");
        assert_eq!(manifest.units, ["demo-web.container"]);
        assert_eq!(manifest.files, ["site/index.html"]);
        assert_eq!(manifest.created_at, manifest.updated_at);

        let get = dispatch(
            "get",
            json!({
                "name": "demo",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
            }),
        )
        .unwrap();
        assert_eq!(get["app"]["name"], "demo");
        assert_eq!(get["units"][0]["exists"], true);
        assert_eq!(get["files"][0]["filename"], "site/index.html");

        let list = dispatch(
            "list",
            json!({
                "base_dir": base.path(),
                "files_base_dir": files.path(),
            }),
        )
        .unwrap();
        assert_eq!(list["apps"].as_array().unwrap().len(), 1);
        assert_eq!(list["apps"][0]["name"], "demo");
        assert_eq!(list["apps"][0]["services"], json!(["demo-web.service"]));
        assert!(list["invalid"].as_array().unwrap().is_empty());
    }

    /// Recipe that declares `bundle_dir`, so the agent can point templates at
    /// the real (agent-managed) bundle directory even when the caller leaves
    /// the value blank.
    const SITE_RECIPE: &str = "
recipe_id: static-site
name: Static Site
version: 1.0.0
parameters:
  - key: http_port
    label: HTTP port
    type: integer
    default: 8080
  - key: bundle_dir
    label: Bundle directory
    type: string
    required: false
resources:
  - type: container
    filename: site.container
    template: site.container.tera
";

    fn site_templates() -> serde_json::Value {
        json!({
            "site.container.tera": "[Container]\nImage=docker.io/library/nginx:stable-alpine\nPublishPort={{ http_port }}:80\nVolume={{ bundle_dir }}/html:/usr/share/nginx/html:ro\n",
        })
    }

    fn site_payload(base_dir: &Path, files_base_dir: &Path, values: &serde_json::Value) -> Value {
        json!({
            "name": "site",
            "base_dir": base_dir,
            "files_base_dir": files_base_dir,
            "recipe": SITE_RECIPE,
            "templates": site_templates(),
            "values": values,
            "converge": false,
        })
    }

    #[test]
    fn create_injects_bundle_dir_for_recipes_that_declare_it() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let bundle_dir = files.path().join("site");
        let expected_volume = format!(
            "Volume={}/html:/usr/share/nginx/html:ro",
            bundle_dir.display()
        );

        let response = dispatch(
            "create",
            site_payload(base.path(), files.path(), &json!({ "http_port": 8080 })),
        )
        .unwrap();

        let rendered = fs::read_to_string(base.path().join("site.container")).unwrap();
        assert!(
            rendered.contains(&expected_volume),
            "rendered unit should bind the agent-managed bundle dir:\n{rendered}"
        );
        assert_eq!(
            response["app"]["values"]["bundle_dir"],
            json!(bundle_dir.display().to_string())
        );

        // An update on a bundle whose manifest predates the injection (no
        // `bundle_dir` in stored values) re-adds it, so old installs keep
        // rendering the right path. No recipe is resent: the stored one is used.
        let manifest_path = bundle_dir.join(APP_MANIFEST);
        let mut manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        manifest["values"]
            .as_object_mut()
            .unwrap()
            .remove("bundle_dir");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        dispatch(
            "update",
            json!({
                "name": "site",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
                "values": { "http_port": 8081 },
                "converge": false,
            }),
        )
        .unwrap();

        let rendered = fs::read_to_string(base.path().join("site.container")).unwrap();
        assert!(rendered.contains(&expected_volume), "{rendered}");
        assert!(rendered.contains("PublishPort=8081:80"), "{rendered}");
    }

    #[test]
    fn caller_supplied_bundle_dir_wins_over_injection() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();

        dispatch(
            "create",
            site_payload(
                base.path(),
                files.path(),
                &json!({ "bundle_dir": "/opt/sites" }),
            ),
        )
        .unwrap();

        let rendered = fs::read_to_string(base.path().join("site.container")).unwrap();
        assert!(
            rendered.contains("Volume=/opt/sites/html:/usr/share/nginx/html:ro"),
            "{rendered}"
        );
    }

    #[test]
    fn create_refuses_an_existing_bundle() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        dispatch("create", create_payload(base.path(), files.path())).unwrap();

        let again = dispatch("create", create_payload(base.path(), files.path()));
        let error = again.unwrap_err();
        assert!(error.to_string().contains("already exists"));
    }

    #[test]
    fn update_merges_values_and_removes_stale_files() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        let mut create = create_payload(base.path(), files.path());
        create["values"] = json!({ "app_id": "demo-web", "flavor": "vanilla" });
        dispatch("create", create).unwrap();

        // New render drops the companion file and renames the app (and thus
        // the unit filename); `flavor` is not resent and must survive.
        let updated_recipe = DEMO_RECIPE.replace(
            "  - type: file\n    filename: site/index.html\n    template: files/index.html.tera\n",
            "",
        );
        let response = dispatch(
            "update",
            json!({
                "name": "demo",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
                "recipe": updated_recipe,
                "templates": demo_templates(),
                "values": { "app_id": "demo-web-2" },
                "converge": false,
            }),
        )
        .unwrap();

        assert_eq!(response["units"], json!(["demo-web-2.container"]));
        assert_eq!(response["removed_units"], json!(["demo-web.container"]));
        assert_eq!(response["removed_files"], json!(["site/index.html"]));
        assert!(!base.path().join("demo-web.container").exists());
        assert!(base.path().join("demo-web-2.container").exists());
        assert!(!files.path().join("demo/site/index.html").exists());
        assert_eq!(response["app"]["values"]["flavor"], "vanilla");
        assert!(
            response["app"]["updated_at"].as_u64().unwrap()
                >= response["app"]["created_at"].as_u64().unwrap()
        );
    }

    #[test]
    fn remove_deletes_units_and_bundle() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        dispatch("create", create_payload(base.path(), files.path())).unwrap();

        let response = dispatch(
            "remove",
            json!({
                "name": "demo",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
                "converge": false,
            }),
        )
        .unwrap();

        assert_eq!(response["bundle_removed"], true);
        assert_eq!(response["deleted_units"].as_array().unwrap().len(), 1);
        assert!(!base.path().join("demo-web.container").exists());
        assert!(!files.path().join("demo").exists());

        // Removing again reports the app as gone instead of succeeding.
        let missing = dispatch(
            "remove",
            json!({
                "name": "demo",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
                "converge": false,
            }),
        );
        missing.unwrap_err();
    }

    #[test]
    fn remove_dry_run_shows_teardown_commands() {
        let base = tempfile::tempdir().unwrap();
        let files = tempfile::tempdir().unwrap();
        dispatch("create", create_payload(base.path(), files.path())).unwrap();

        let response = dispatch(
            "remove",
            json!({
                "name": "demo",
                "base_dir": base.path(),
                "files_base_dir": files.path(),
                "dry_run": true,
            }),
        )
        .unwrap();

        let commands = response["systemd"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| op["command"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            [
                "systemctl stop demo-web.service",
                "systemctl disable demo-web.service",
                "systemctl daemon-reload",
            ]
        );
        assert_eq!(response["bundle_removed"], false);
        // Dry run deleted nothing.
        assert!(base.path().join("demo-web.container").exists());
        assert!(files.path().join("demo/app.json").exists());
    }
}
