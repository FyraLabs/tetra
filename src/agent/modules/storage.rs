//! Storage inspection and configuration module.
//!
//! Surfaces host storage state (`/proc/mounts`, `/proc/partitions`, `df`) and
//! performs mount/unmount and `/etc/fstab` edits. The `mount` and `configure`
//! actions also accept the shared `selinux` payload so the control plane can
//! label a freshly mounted or configured path in the same request — important
//! on SELinux-enabled hosts where an unlabeled mount point would be denied
//! access to its intended service.

use crate::prelude::*;

use crate::agent::module_support::{
    SelinuxOptions, apply_selinux, handle_metadata, parse_payload, unsupported_action,
};

/// Storage module entry point registered under feature `storage`.
pub struct StorageModule;

/// Static capability metadata published via `capabilities`/`plan`.
const INFO: ModuleInfo = ModuleInfo {
    name: "storage",
    feature: "storage",
    description: "Inspect disks, filesystems, mounts, and storage-related configuration.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "status",
        "mount",
        "unmount",
        "configure",
    ],
    privileged_actions: &["mount", "unmount", "configure"],
};

/// Payload carrying a single path, used by `status` (the `df` target) and
/// `unmount` (the mount point to detach).
#[derive(Debug, Deserialize)]
struct PathPayload {
    path: PathBuf,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for the `mount` action.
///
/// `fstype` and `options` are optional and only forwarded to `mount` when
/// present, so callers can let `mount` autodetect the filesystem when they do
/// not know it ahead of time.
#[derive(Debug, Deserialize)]
struct MountPayload {
    source: String,
    target: String,
    fstype: Option<String>,
    options: Option<String>,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

/// Payload for the `configure` action, which appends a line to `/etc/fstab`.
///
/// `entry` is a raw fstab line as it should appear in the file; the module
/// does not parse or validate it, only appends. `fstab_path` defaults to
/// `/etc/fstab` but can be overridden — used by tests, and useful for staging
/// an edit against a copy before swapping it into place.
#[derive(Debug, Deserialize)]
struct ConfigurePayload {
    #[serde(default = "default_fstab_path")]
    fstab_path: PathBuf,
    entry: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    selinux: Option<SelinuxOptions>,
}

/// Dispatches `storage` actions.
///
/// Read actions (`list`, `status`) never take `dry_run` since they only
/// inspect state; `mount`, `unmount`, and `configure` honor `dry_run` and
/// short-circuit before exec/write.
impl AgentModule for StorageModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Standard metadata fast-path: `capabilities` and `plan` are answered
        // from `INFO` without touching the system.
        if let Some(response) = handle_metadata(INFO, action, &payload) {
            return Ok(response);
        }

        match action {
            "list" => Ok(jsonf! {
                // unwrap_or_default swallows read/parse errors and yields an
                // empty list — `list` is best-effort inventory, not a health
                // probe, so a missing `/proc/*` file should not fail the call.
                "mounts": read_mounts("/proc/mounts").unwrap_or_default(),
                "partitions": read_partitions("/proc/partitions").unwrap_or_default(),
            }),
            "status" => {
                let payload: PathPayload = parse_payload(payload)?;
                crate::cmd!({ &INFO, action, user } "df" ["-h", payload.path.to_string_lossy().as_ref()] ; json)
            }
            "mount" => {
                let payload: MountPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if let Some(fstype) = payload.fstype {
                    args.extend(["-t".to_owned(), fstype]);
                }
                if let Some(options) = payload.options {
                    args.extend(["-o".to_owned(), options]);
                }
                // The target is captured separately so it can be used as the
                // default relabel path for the shared SELinux options below;
                // it is consumed by `args.extend` right after, which is why we
                // clone it into a PathBuf first.
                let target = PathBuf::from(&payload.target);
                args.extend([payload.source, payload.target]);
                let mount =
                    crate::cmd!((payload.dry_run) { &INFO, action, user } "mount" => &args ; json)?;
                // Label the freshly mounted target as part of the same action;
                // see `apply_selinux` in module_support.rs for the option
                // resolution rules.
                let selinux =
                    apply_selinux(payload.selinux.as_ref(), Some(&target), payload.dry_run)?;
                Ok(jsonf! { mount, selinux })
            }
            "unmount" => {
                let payload: PathPayload = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "umount" [payload.path.to_string_lossy().as_ref()] ; json)
            }
            "configure" => {
                let payload: ConfigurePayload = parse_payload(payload)?;
                if !payload.dry_run {
                    append_fstab_entry(&payload.fstab_path, &payload.entry)?;
                }
                // The default relabel target here is `fstab_path`, which is
                // rarely what callers want; pass an explicit `path` inside the
                // selinux object to label the mount target instead.
                let selinux = apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&payload.fstab_path),
                    payload.dry_run,
                )?;
                Ok(jsonf! {
                    payload.fstab_path,
                    "configured": !payload.dry_run,
                    payload.dry_run,
                    selinux,
                })
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

/// Default fstab location used when `configure` is invoked without an
/// explicit `fstab_path`.
fn default_fstab_path() -> PathBuf {
    PathBuf::from("/etc/fstab")
}

/// Parses `/proc/mounts`, whose rows are whitespace-delimited as
/// `source target fstype opts dump pass`. Only the first four fields are
/// surfaced; rows with fewer than four fields are skipped rather than
/// erroring, so a malformed kernel-provided line never breaks the call.
fn read_mounts(path: impl Into<PathBuf>) -> Result<Vec<Value>> {
    let path = path.into();
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read mounts `{}`", path.display()))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 4).then(|| {
                jsonf! {
                    "source": fields[0],
                    "target": fields[1],
                    "filesystem": fields[2],
                    "options": fields[3],
                }
            })
        })
        .collect())
}

/// Parses `/proc/partitions`, which has a two-line header (a column line
/// followed by a blank line) and then `major minor blocks name` rows.
/// `skip(2)` drops the header; the strict four-field filter then keeps only
/// real partition rows.
fn read_partitions(path: impl Into<PathBuf>) -> Result<Vec<Value>> {
    let path = path.into();
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read partitions `{}`", path.display()))?;
    Ok(text
        .lines()
        .skip(2)
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() == 4).then(|| {
                jsonf! {
                    "major": fields[0],
                    "minor": fields[1],
                    "blocks": fields[2],
                    "name": fields[3],
                }
            })
        })
        .collect())
}

/// Appends a single entry to an fstab file safely.
///
/// Reads the existing file (treating a missing file as empty), guarantees a
/// newline separates any existing content from the new entry, trims trailing
/// whitespace from the entry, and ensures the file ends with exactly one
/// newline. This sidesteps the two common fstab foot-guns: joining two lines
/// into one (no separator), and leaving the file without a terminating
/// newline (which some fstab parsers reject).
fn append_fstab_entry(path: &PathBuf, entry: &str) -> Result<()> {
    let mut text = fs::read_to_string(path).unwrap_or_default();
    // Only add a separator when there is existing content that lacks a
    // trailing newline; an empty file or one already ending in `\n` needs
    // none, and adding one would create a stray blank line.
    if !text.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    text.push_str(entry.trim_end());
    // Always terminate the file with a newline so subsequent appends and most
    // fstab parsers stay happy.
    text.push('\n');
    fs::write(path, text).with_context(|| format!("failed to write `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_mounts_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mounts");
        fs::write(&path, "/dev/sda1 / ext4 rw,relatime 0 0\n").unwrap();

        let mounts = read_mounts(path).unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["source"], "/dev/sda1");
        assert_eq!(mounts[0]["target"], "/");
        assert_eq!(mounts[0]["filesystem"], "ext4");
    }

    #[test]
    fn parses_partitions_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partitions");
        fs::write(
            &path,
            "major minor  #blocks  name\n\n   8        0  976762584 sda\n",
        )
        .unwrap();

        let partitions = read_partitions(path).unwrap();
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0]["name"], "sda");
        assert_eq!(partitions[0]["blocks"], "976762584");
    }

    #[test]
    fn appends_fstab_entries_with_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fstab");
        fs::write(&path, "/dev/sda1 / ext4 defaults 0 1").unwrap();

        append_fstab_entry(&path, "/dev/sdb1 /data ext4 defaults 0 2").unwrap();

        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "/dev/sda1 / ext4 defaults 0 1\n/dev/sdb1 /data ext4 defaults 0 2\n"
        );
    }

    #[test]
    fn dry_run_configure_does_not_write_fstab() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fstab");
        fs::write(&path, "/dev/sda1 / ext4 defaults 0 1\n").unwrap();

        let response = StorageModule
            .handle(
                "configure",
                json!({
                    "fstab_path": path,
                    "entry": "/dev/sdb1 /data ext4 defaults 0 2",
                    "dry_run": true
                }),
                None,
            )
            .unwrap();

        assert_eq!(response["configured"], false);
        assert_eq!(
            fs::read_to_string(dir.path().join("fstab")).unwrap(),
            "/dev/sda1 / ext4 defaults 0 1\n"
        );
    }

    #[test]
    fn dry_run_mount_does_not_call_mount() {
        let response = StorageModule
            .handle(
                "mount",
                json!({
                    "source": "/dev/example",
                    "target": "/mnt/example",
                    "dry_run": true
                }),
                None,
            )
            .unwrap();

        assert_eq!(
            response["mount"]["command"],
            "mount /dev/example /mnt/example"
        );
        assert_eq!(response["mount"]["dry_run"], true);
        assert!(response["mount"]["status"].is_null());
    }

    #[test]
    fn mount_can_apply_selinux_context_to_target() {
        let response = StorageModule
            .handle(
                "mount",
                json!({
                    "source": "/dev/example",
                    "target": "/srv/data",
                    "dry_run": true,
                    "selinux": {
                        "context_type": "container_file_t",
                        "recursive": true
                    }
                }),
                None,
            )
            .unwrap();

        assert_eq!(response["selinux"].as_array().unwrap().len(), 2);
        assert_eq!(
            response["selinux"][0]["command"],
            "semanage fcontext -a -t container_file_t /srv/data(/.*)?"
        );
        assert_eq!(
            response["selinux"][1]["command"],
            "restorecon -R -v /srv/data"
        );
    }
}
