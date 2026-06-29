use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleStatus {
    Available,
    Planned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModuleInfo {
    pub name: &'static str,
    pub feature: &'static str,
    pub description: &'static str,
    pub status: ModuleStatus,
    pub actions: &'static [&'static str],
}

pub fn handle_metadata(info: ModuleInfo, action: &str, payload: Value) -> Result<Option<Value>> {
    match action {
        "capabilities" => Ok(Some(json!(info))),
        "plan" => Ok(Some(json!({
            "module": info.name,
            "feature": info.feature,
            "status": info.status,
            "requested": payload,
        }))),
        _ => Ok(None),
    }
}

pub fn unsupported_action(module: &str, action: &str) -> Result<Value> {
    bail!("unsupported {module} action `{action}`")
}

#[derive(Debug, Deserialize)]
pub struct NamedPayload {
    pub name: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct ServicePayload {
    pub service: String,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SelinuxOptions {
    #[serde(default)]
    pub enabled: bool,
    pub path: Option<String>,
    pub path_pattern: Option<String>,
    pub context_type: Option<String>,
    #[serde(default)]
    pub recursive: bool,
}

pub fn parse_payload<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T> {
    serde_json::from_value(payload).context("invalid command payload")
}

pub fn run_command<I, S>(program: &str, args: I) -> Result<Value>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(json!(run_command_output(program, args, false)?))
}

pub fn run_command_or_dry_run<I, S>(program: &str, args: I, dry_run: bool) -> Result<Value>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Ok(json!(run_command_output(program, args, dry_run)?))
}

pub fn run_command_json<I, S>(program: &str, args: I) -> Result<Value>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let result = run_command_output(program, args, false)?;
    let data: Value =
        serde_json::from_str(&result.stdout).context("failed to parse command stdout as JSON")?;
    Ok(json!({
        "command": result.command,
        "status": result.status,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "dry_run": result.dry_run,
        "data": data,
    }))
}

pub fn run_command_output<I, S>(program: &str, args: I, dry_run: bool) -> Result<CommandResult>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect();
    let command = command_display(program, &args);

    if dry_run {
        return Ok(CommandResult {
            command,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            dry_run: true,
        });
    }

    let output = Command::new(program)
        .args(&args)
        .output()
        .with_context(|| format!("failed to run `{program}`"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let code = output.status.code();

    if !output.status.success() {
        bail!(
            "`{}` failed with status {:?}: {}",
            command,
            code,
            stderr.trim()
        );
    }

    Ok(CommandResult {
        command,
        status: code,
        stdout,
        stderr,
        dry_run: false,
    })
}

pub fn apply_selinux(
    options: Option<&SelinuxOptions>,
    default_path: Option<&Path>,
    dry_run: bool,
) -> Result<Vec<Value>> {
    let Some(options) = options else {
        return Ok(Vec::new());
    };

    if !options.enabled
        && options.path.is_none()
        && options.path_pattern.is_none()
        && options.context_type.is_none()
    {
        return Ok(Vec::new());
    }

    let path = options
        .path
        .clone()
        .or_else(|| default_path.map(|path| path.to_string_lossy().into_owned()));
    let mut operations = Vec::new();

    if let Some(context_type) = &options.context_type {
        let pattern = options
            .path_pattern
            .clone()
            .or_else(|| {
                path.as_ref()
                    .map(|path| default_fcontext_pattern(path, options.recursive))
            })
            .context("SELinux context_type requires path, path_pattern, or default path")?;
        operations.push(run_command_or_dry_run(
            "semanage",
            ["fcontext", "-a", "-t", context_type, &pattern],
            dry_run,
        )?);
    }

    if let Some(path) = path {
        let mut args = Vec::new();
        if options.recursive {
            args.push("-R".to_string());
        }
        args.push("-v".to_string());
        args.push(path);
        operations.push(run_command_or_dry_run("restorecon", args, dry_run)?);
    }

    Ok(operations)
}

pub fn safe_join(base: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("path `{name}` must be relative and stay within the base directory");
    }

    Ok(base.join(path))
}

pub fn command_display(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_string())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_fcontext_pattern(path: &str, recursive: bool) -> String {
    if recursive {
        format!("{path}(/.*)?")
    } else {
        path.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandResult {
    pub command: String,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub dry_run: bool,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Payload {
        name: String,
    }

    #[test]
    fn parses_typed_payloads() {
        let payload: Payload = parse_payload(json!({ "name": "example" })).unwrap();
        assert_eq!(
            payload,
            Payload {
                name: "example".into()
            }
        );
    }

    #[test]
    fn rejects_invalid_typed_payloads() {
        let error = parse_payload::<Payload>(json!({ "other": true })).unwrap_err();
        assert!(error.to_string().contains("invalid command payload"));
    }

    #[test]
    fn safe_join_rejects_paths_that_escape_base() {
        let base = Path::new("/tmp/base");
        assert_eq!(
            safe_join(base, "unit.container").unwrap(),
            base.join("unit.container")
        );
        assert!(safe_join(base, "../unit.container").is_err());
        assert!(safe_join(base, "/tmp/unit.container").is_err());
    }

    #[test]
    fn formats_command_display() {
        assert_eq!(
            command_display("systemctl", &["status".into(), "nginx.service".into()]),
            "systemctl status nginx.service"
        );
    }

    #[test]
    fn dry_run_command_does_not_execute() {
        let result = run_command_output("definitely-not-a-real-command", ["arg"], true).unwrap();
        assert_eq!(result.command, "definitely-not-a-real-command arg");
        assert!(result.dry_run);
        assert_eq!(result.status, None);
    }

    #[test]
    fn selinux_helper_builds_fcontext_and_restorecon_commands() {
        let options = SelinuxOptions {
            enabled: true,
            path: None,
            path_pattern: None,
            context_type: Some("samba_share_t".into()),
            recursive: true,
        };
        let operations =
            apply_selinux(Some(&options), Some(Path::new("/srv/share")), true).unwrap();

        assert_eq!(operations.len(), 2);
        assert_eq!(
            operations[0]["command"],
            "semanage fcontext -a -t samba_share_t /srv/share(/.*)?"
        );
        assert_eq!(operations[1]["command"], "restorecon -R -v /srv/share");
    }

    #[test]
    fn selinux_helper_is_noop_when_not_requested() {
        let operations = apply_selinux(
            Some(&SelinuxOptions::default()),
            Some(Path::new("/srv/share")),
            true,
        )
        .unwrap();
        assert!(operations.is_empty());
    }
}
