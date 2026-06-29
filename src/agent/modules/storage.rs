use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, SelinuxOptions, apply_selinux, handle_metadata, parse_payload,
        run_command, run_command_or_dry_run, unsupported_action,
    },
};

pub struct StorageModule;

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
};

#[derive(Debug, Deserialize)]
struct PathPayload {
    path: PathBuf,
    #[serde(default)]
    dry_run: bool,
}

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

impl AgentModule for StorageModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => Ok(json!({
                "mounts": read_mounts("/proc/mounts").unwrap_or_default(),
                "partitions": read_partitions("/proc/partitions").unwrap_or_default(),
            })),
            "status" => {
                let payload: PathPayload = parse_payload(payload)?;
                run_command("df", ["-h", payload.path.to_string_lossy().as_ref()])
            }
            "mount" => {
                let payload: MountPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if let Some(fstype) = payload.fstype {
                    args.extend(["-t".to_string(), fstype]);
                }
                if let Some(options) = payload.options {
                    args.extend(["-o".to_string(), options]);
                }
                let target = PathBuf::from(&payload.target);
                args.extend([payload.source, payload.target]);
                let mount = run_command_or_dry_run("mount", args, payload.dry_run)?;
                let selinux =
                    apply_selinux(payload.selinux.as_ref(), Some(&target), payload.dry_run)?;
                Ok(json!({ "mount": mount, "selinux": selinux }))
            }
            "unmount" => {
                let payload: PathPayload = parse_payload(payload)?;
                run_command_or_dry_run(
                    "umount",
                    [payload.path.to_string_lossy().as_ref()],
                    payload.dry_run,
                )
            }
            "configure" => {
                let payload: ConfigurePayload = parse_payload(payload)?;
                if !payload.dry_run {
                    append_fstab_entry(&payload.fstab_path, &payload.entry)?;
                }
                let selinux = apply_selinux(
                    payload.selinux.as_ref(),
                    Some(&payload.fstab_path),
                    payload.dry_run,
                )?;
                Ok(json!({
                    "fstab_path": payload.fstab_path,
                    "configured": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "selinux": selinux,
                }))
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_fstab_path() -> PathBuf {
    PathBuf::from("/etc/fstab")
}

fn read_mounts(path: impl Into<PathBuf>) -> Result<Vec<Value>> {
    let path = path.into();
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read mounts `{}`", path.display()))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() >= 4).then(|| {
                json!({
                    "source": fields[0],
                    "target": fields[1],
                    "filesystem": fields[2],
                    "options": fields[3],
                })
            })
        })
        .collect())
}

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
                json!({
                    "major": fields[0],
                    "minor": fields[1],
                    "blocks": fields[2],
                    "name": fields[3],
                })
            })
        })
        .collect())
}

fn append_fstab_entry(path: &PathBuf, entry: &str) -> Result<()> {
    let mut text = fs::read_to_string(path).unwrap_or_default();
    if !text.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    text.push_str(entry.trim_end());
    text.push('\n');
    fs::write(path, text).with_context(|| format!("failed to write `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
