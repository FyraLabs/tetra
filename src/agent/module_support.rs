use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};

/// Whether a module is shipping in this build or is a placeholder for a future
/// feature. The dashboard uses this to grey out planned-but-unavailable
/// modules in the capabilities list.
///
/// Serialized as `"available"`/`"planned"` via `snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleStatus {
    Available,
    Planned,
}

/// Static metadata each module exposes about itself.
///
/// Returned by [`AgentModule::info`](super::AgentModule::info) and used to:
/// - answer the dispatcher-level `agent.capabilities` action (lists modules)
/// - answer each module's own `capabilities` action (single-module info)
/// - drive the dashboard's module/action pickers
///
/// `name` and `feature` are `&'static str` because every module holds its
/// `INFO` as a `const` — there's no allocation per dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModuleInfo {
    pub name: &'static str,
    pub feature: &'static str,
    pub description: &'static str,
    pub status: ModuleStatus,
    pub actions: &'static [&'static str],
    /// Actions that require root privileges and an active elevation grant on
    /// headless hosts. The dashboard uses this to request elevation proactively.
    pub privileged_actions: &'static [&'static str],
}

impl ModuleInfo {
    /// Whether `action` is listed in [`privileged_actions`].
    #[must_use]
    pub fn is_privileged(&self, action: &str) -> bool {
        self.privileged_actions.contains(&action)
    }
}

/// Handle the shared `capabilities` and `plan` meta-actions that every
/// module supports.
///
/// Convention: each module's `handle` method calls this first and returns
/// `Ok(Some(...))` if it matched, then matches on its own module-specific
/// actions. `plan` is a lightweight "what would you do with this payload?"
/// endpoint — it returns the module metadata plus the echoed payload, so
/// the dashboard can show a preview without committing to a dry run.
///
/// Returns `Ok(None)` when the action isn't one of these meta-actions, so the
/// caller can fall through to its own match.
pub fn handle_metadata(info: ModuleInfo, action: &str, payload: Value) -> Result<Option<Value>> {
    // NOTE: why do you need Result<_> and Ok()?
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

/// Bail with a consistent "unknown action" error message. Centralizing this
/// keeps the error format uniform across modules.
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

/// The shared `SELinux` labeling options accepted by any module that creates or
/// manages host paths.
///
/// This is the dashboard's single knob for "label this path too" — e.g. a
/// Samba share's `set_config` action can label the share directory in the same
/// call that writes `smb.conf`. See [`apply_selinux`] for the semantics.
///
/// `enabled` exists as an explicit opt-in flag so callers can pass a fully
/// populated object in JSON without triggering labeling — they'd just leave
/// `enabled` at its default `false`.
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

/// Deserialize a typed payload from a [`serde_json::Value`] with a consistent
/// "invalid command payload" error message. Centralizing this keeps the
/// per-module error surface uniform.
pub fn parse_payload<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T> {
    serde_json::from_value(payload).context("invalid command payload")
}

/// Whether the module supports running non-privileged actions as a non-root
/// user via `runuser`. When `true`, safe actions can be executed unprivileged
/// by passing a user to the `_for_module` helpers.
pub const DEFAULT_USER_SUPPORT: bool = cfg!(target_os = "linux");

/// Execute commands. Wrapper around [`run_command_output_as`].
///
/// # Usage
/// - prefix `DRY_RUN` or `DRY_RUN(bool_expr)` to set dry run
/// - prefix `{ &INFO, action, user }` to set default user
/// - then add your program name (any expression)
/// - add `=> args` for your arguments (`args` need to be `Copy`!)
/// - add `; json` to wrap it around [`serde_json::json!`]
/// - add `; JSON` to obtain json in the following format:
/// ```ts
/// {
///     "command": result.command,
///     "status": result.status,
///     "stdout": result.stdout,
///     "stderr": result.stderr,
///     "dry_run": result.dry_run,
///     "data": stdout,
/// }
/// ```
///
/// For aesthetic/formatting purposes, the macro partially accepts usages with elided `;` or `=>`.
#[macro_export]
macro_rules! cmd {
    ($($idk:tt)+) => {
        $crate::__cmd_inner!((false, None) $($idk)+)
    };
}

#[macro_export]
macro_rules! __cmd_inner {
    (($dry_run:expr, $default_user:expr) DRY_RUN($dry:expr) $($idk:tt)+) => {
        $crate::__cmd_inner!(($dry, $default_user) $($idk)+)
    };
    (($dry_run:expr, $default_user:expr) DRY_RUN $($idk:tt)+) => {
        $crate::__cmd_inner!((true, $default_user) $($idk)+)
    };
    (($dry_run:expr, $default_user:expr) { $info:expr, $act:expr, $user:expr } $($idk:tt)+) => {{
        let default_user = $crate::agent::module_support::effective_user($info, $act, $user);
        $crate::__cmd_inner!(($dry_run, default_user) $($idk)+)
    }};
    (($dry_run:expr, $default_user:expr) $program:literal $args:expr $(; $($idk:tt)*)?) => {
        $crate::__cmd_inner!(
            @{$($($idk)*)?}
            $crate::agent::module_support::run_command_output_as($program, $args, $dry_run, $default_user)
        )
    };
    (($dry_run:expr, $default_user:expr) $program:literal [$($args:tt)+] $($idk:tt)*) => {
        $crate::__cmd_inner!(
            @{$($idk)*}
            $crate::agent::module_support::run_command_output_as($program, [$($args)+], $dry_run, $default_user)
        )
    };
    (($dry_run:expr, $default_user:expr) $program:expr $(; $($idk:tt)*)?) => {
        $crate::__cmd_inner!(
            @{$($($idk)*)?}
            $crate::agent::module_support::run_command_output_as($program, &[""; 0], $dry_run, $default_user)
        )
    };
    (($dry_run:expr, $default_user:expr) $program:expr => $args:expr $(; $($idk:tt)*)?) => {
        $crate::__cmd_inner!(
            @{$($($idk)*)?}
            $crate::agent::module_support::run_command_output_as($program, $args, $dry_run, $default_user)
        )
    };
    (($dry_run:expr, $default_user:expr) $program:expr => [$($args:tt)+] $($idk:tt)*) => {
        $crate::__cmd_inner!(
            @{$($idk)*}
            $crate::agent::module_support::run_command_output_as($program, [$($args)+], $dry_run, $default_user)
        )
    };
    (@{}$final:expr) => { $final };
    (@{json}$final:expr) => { $final.map(|obj| ::serde_json::json!(obj)) };
    (@{JSON}$final:expr) => { $final.and_then(|result| {
        let data: ::serde_json::Value =
            ::serde_json::from_str(&result.stdout).context("failed to parse command stdout as JSON")?;
        Ok(::serde_json::json!({
            "command": result.command,
            "status": result.status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "dry_run": result.dry_run,
            "data": data,
        }))
    }) };
}

/// Run a host command and return its [`CommandResult`] as JSON, executing
/// unconditionally (no dry-run support).
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command<I, S>(program: &str, args: I) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    Ok(json!(run_command_output(program, args, false)?))
}

/// Run a host command, or when `dry_run` is true, return what would have been
/// run without executing it. The returned JSON always carries `dry_run` so the
/// caller can distinguish a preview from a real result by inspecting the
/// payload, not just by remembering what it asked for.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_or_dry_run<I, S>(program: &str, args: I, dry_run: bool) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    Ok(json!(run_command_output_as(program, args, dry_run, None)?))
}

/// Run a host command that is expected to print JSON to stdout, and return a
/// composite value that includes the parsed stdout (`data`) alongside the
/// command metadata (status, raw stdout/stderr, `dry_run` flag).
///
/// This is the helper for commands like `podman inspect --format json` where
/// the interesting content is structured — callers get the parsed value
/// directly rather than re-parsing `stdout` themselves.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_json<I, S>(program: &str, args: I) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    let result = run_command_output_as(program, args, false, None)?;
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

/// Run a host command (or fake it for a dry run) and return a structured
/// [`CommandResult`].
///
/// Dry-run mode short-circuits before spawning the process: the returned
/// `CommandResult` carries the exact command string that *would* have been run
/// (so the dashboard can show it), with empty stdout/stderr and `status: None`.
///
/// On a real run, a non-zero exit status is converted to an error with the
/// trimmed stderr in the message — modules don't need to re-check exit codes.
/// Run a host command (or fake it for a dry run) and return a structured
/// [`CommandResult`].  When `default_user` is set and the action is not
/// privileged, the command is executed via `runuser` so it runs under the
/// configured unprivileged account rather than root.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_output<I, S>(program: &str, args: I, dry_run: bool) -> Result<CommandResult>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    run_command_output_as(program, args, dry_run, None)
}

/// Run a host command, optionally impersonating `default_user`.
///
/// When `default_user` is `Some`, the command is wrapped in
/// `runuser -u <user> -- <program> <args>`.  This lets Tetra run as root
/// while executing day-to-day operations (podman inspect, reading configs)
/// with unprivileged credentials.  Privileged actions bypass this wrapper
/// by passing `default_user: None`.
pub fn run_command_output_as<I, S>(
    program: &str,
    args: I,
    dry_run: bool,
    default_user: Option<&str>,
) -> Result<CommandResult>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    let command = command_display(program, args);

    if dry_run {
        return Ok(CommandResult {
            command,
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            dry_run: true,
        });
    }

    let output = match default_user {
        Some(user) if DEFAULT_USER_SUPPORT => Command::new("runuser")
            .args(["-u", user, "--", program])
            .args(args)
            .output()
            .with_context(|| format!("failed to run `{program}` as `{user}`")),
        _ => Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run `{program}`")),
    }?;

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

/// Compute the effective OS user for a module action.
///
/// Privileged actions always run as root (`None`). Unprivileged actions run as
/// the supplied `user` when it is present, otherwise they fall back to root.
#[inline]
pub fn effective_user<'a>(
    info: &'a ModuleInfo,
    action: &'a str,
    user: Option<&'a str>,
) -> Option<&'a str> {
    if info.privileged_actions.contains(&action) {
        None
    } else {
        user
    }
}

/// Like [`run_command`], but respects [`ModuleInfo::privileged_actions`] and
/// runs unprivileged actions as the supplied `user` when available.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_for_module<I, S>(
    info: &ModuleInfo,
    action: &str,
    program: &str,
    args: I,
    user: Option<&str>,
) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    Ok(json!(run_command_output_as(
        program,
        args,
        false,
        effective_user(info, action, user)
    )?))
}

/// Like [`run_command_or_dry_run`], but respects [`ModuleInfo::privileged_actions`]
/// and runs unprivileged actions as the supplied `user` when available.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_or_dry_run_for_module<I, S>(
    info: &ModuleInfo,
    action: &str,
    program: &str,
    args: I,
    dry_run: bool,
    user: Option<&str>,
) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    Ok(json!(run_command_output_as(
        program,
        args,
        dry_run,
        effective_user(info, action, user)
    )?))
}

/// Like [`run_command_json`], but respects [`ModuleInfo::privileged_actions`] and
/// runs unprivileged actions as the supplied `user` when available.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_json_for_module<I, S>(
    info: &ModuleInfo,
    action: &str,
    program: &str,
    args: I,
    user: Option<&str>,
) -> Result<Value>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    let result = run_command_output_as(program, args, false, effective_user(info, action, user))?;
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

/// Like [`run_command_output`], but respects [`ModuleInfo::privileged_actions`] and
/// runs unprivileged actions as the supplied `user` when available.
#[deprecated = "use crate::cmd!() instead"]
pub fn run_command_output_for_module<I, S>(
    info: &ModuleInfo,
    action: &str,
    program: &str,
    args: I,
    dry_run: bool,
    user: Option<&str>,
) -> Result<CommandResult>
where
    I: IntoIterator<Item = S> + Copy,
    S: AsRef<OsStr> + std::fmt::Display,
{
    run_command_output_as(program, args, dry_run, effective_user(info, action, user))
}

/// Apply `SELinux` file-context labeling to a path as part of a module action.
///
/// Two phases:
/// 1. If `context_type` is set, run `semanage fcontext -a -t <type>
///    <pattern>` to register the persistent label rule. The pattern is taken
///    from `path_pattern` if present, otherwise derived from `path` (with
///    `(/.*)?` appended when `recursive` is set, matching the convention
///    `restorecon` expects).
/// 2. If `path` is set, run `restorecon [-R] -v <path>` to apply the label
///    immediately. `restorecon` reads the fcontext database, so it must run
///    *after* the `semanage` call.
///
/// Returns one [`Value`] per operation performed (so the dashboard can show
/// what was done), and is a no-op when the options are absent or `enabled`
/// is `false` and no other field triggers a label.
///
/// `default_path` is the path the calling module is already acting on (e.g.
/// the Samba share directory); it's used only when the caller didn't pass an
/// explicit `path` in the options. This lets the dashboard send a minimal
/// `{ "context_type": "samba_share_t" }` payload and have the module's own
/// context fill in the path.
///
/// Dry-run mode propagates to both `semanage` and `restorecon` calls, so a
/// dry-run preview of the *whole* action (e.g. write smb.conf + label it)
/// shows the exact commands without changing the host.
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
        operations.push(cmd!(DRY_RUN(dry_run) "semanage" ["fcontext", "-a", "-t", context_type, &pattern]; json)?);
    }

    if let Some(path) = path {
        let mut args = Vec::new();
        if options.recursive {
            args.push("-R".to_owned());
        }
        args.push("-v".to_owned());
        args.push(path);
        operations.push(cmd!(DRY_RUN(dry_run) "restorecon" &args ; json)?);
    }

    Ok(operations)
}

/// Join `name` onto `base`, rejecting absolute paths or any component that
/// would escape `base` (`..`, Windows prefixes).
///
/// This is the guard against path-traversal in module payloads — e.g. a
/// Quadlet filename from the dashboard can't reach outside the Quadlet scan
/// directory by sending `../../../etc/passwd`.
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

/// Render a command and its args as a single space-joined display string.
/// Used both for dry-run previews and for error messages (`\`foo bar\` failed`).
#[must_use]
pub fn command_display<
    's,
    I: IntoIterator<Item = S>,
    S: std::fmt::Display,
    P: std::fmt::Display,
>(
    program: P,
    args: I,
) -> String {
    let args = (args.into_iter()).map(|arg| Box::new(arg) as Box<dyn std::fmt::Display>);
    (std::iter::once(Box::new(program) as Box<dyn std::fmt::Display>).chain(args)).join(" ")
}

/// Build the default `semanage fcontext` pattern for `path`. Recursive
/// labeling appends `(/.*)?` so that `restorecon` relabels the whole subtree;
/// non-recursive labeling targets just the path itself.
fn default_fcontext_pattern(path: &str, recursive: bool) -> String {
    if recursive {
        format!("{path}(/.*)?")
    } else {
        path.to_owned()
    }
}

/// The result of running (or dry-running) a host command. Serialized as JSON
/// by the `run_command*` helpers and returned to the dashboard so the UI can
/// show what actually ran, not just the final value.
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
            command_display("systemctl", &["status", "nginx.service"]),
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
