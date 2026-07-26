//! `SELinux` management module.
//!
//! Wraps the userspace `SELinux` toolchain (`sestatus`, `getenforce`,
//! `getsebool`, `setsebool`, `semanage fcontext`, `restorecon`) so the
//! control plane can inspect and change policy state through the standard
//! module envelope.
//!
//! This module owns *policy-level* `SELinux` operations: querying mode,
//! flipping booleans, and registering or removing file-context rules plus
//! relabeling. Modules that merely *apply* a label to a path they manage
//! (storage, samba, nfs, files, quadlets, network) do not call into this
//! code; they share the `SelinuxOptions` payload via `apply_selinux()` in
//! `module_support.rs`.

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload,
        run_command_or_dry_run_for_module, run_command_output_for_module, unsupported_action,
    },
};

/// `SELinux` module entry point registered under feature `selinux`.
///
/// Stateless: every action is a fresh invocation of an underlying `SELinux`
/// tool, so there is nothing to hold across requests.
pub struct SelinuxModule;

/// Static capability metadata published via the `capabilities`/`plan` actions.
const INFO: ModuleInfo = ModuleInfo {
    name: "selinux",
    feature: "selinux",
    description: "Inspect and manage SELinux mode, booleans, file contexts, and relabeling.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "status",
        "enforce",
        "booleans",
        "set_boolean",
        "file_contexts",
        "add_file_context",
        "delete_file_context",
        "restore_context",
    ],
    privileged_actions: &[
        "enforce",
        "set_boolean",
        "add_file_context",
        "delete_file_context",
        "restore_context",
    ],
};

/// Payload for `set_boolean`.
///
/// `persistent` defaults to `true` because most callers want a change to
/// survive reboot; opt out explicitly with `"persistent": false` for a
/// runtime-only tweak (useful when validating a policy change before
/// committing it).
#[derive(Debug, Deserialize)]
struct SetBooleanPayload {
    name: String,
    value: bool,
    #[serde(default = "default_persistent")]
    persistent: bool,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `add_file_context`.
///
/// `path_pattern` is passed verbatim to `semanage fcontext -a`. Unlike the
/// shared `SelinuxOptions` flow used by other modules, this module does **not**
/// auto-derive the `PATH(/.*)?` recursive form — the caller supplies the exact
/// regex pattern semanage will store. This keeps the rule-adding action honest
/// about what is being registered: callers may want `(/.*)?`, `/.+`, or a bare
/// path with no recursion, and silently rewriting the pattern would hide that
/// choice from the operator reading the audit log.
#[derive(Debug, Deserialize)]
struct FileContextPayload {
    path_pattern: String,
    context_type: String,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `delete_file_context`.
///
/// The pattern must match what was originally added, including any `(/.*)?`
/// suffix — `semanage fcontext -d` deletes by exact pattern match.
#[derive(Debug, Deserialize)]
struct DeleteFileContextPayload {
    path_pattern: String,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `restore_context` (`restorecon`).
///
/// `restorecon` walks the filesystem at `path` and relabels each entry to
/// whatever the active policy — including any fcontext rules added via
/// `add_file_context` — says it should be. It is the second half of the
/// standard add-rule-then-relabel flow: registering an fcontext changes the
/// policy, but existing inodes keep their old labels until `restorecon` runs.
#[derive(Debug, Deserialize)]
struct RestoreContextPayload {
    path: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    dry_run: bool,
}

/// Dispatches `selinux` actions to the underlying tools.
///
/// Read actions (`status`, `enforce`, `booleans`, `file_contexts`) always run
/// the real tool even under `dry_run` — they have no side effects, and the
/// parsed output is what the caller actually wants. Mutating actions
/// (`set_boolean`, `add_file_context`, `delete_file_context`,
/// `restore_context`) honor `dry_run` and short-circuit before exec.
impl AgentModule for SelinuxModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Standard metadata fast-path: `capabilities` and `plan` are answered
        // from `INFO` without touching the system.
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "status" => {
                let result = crate::cmd!({&INFO, action, user} "sestatus")?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "selinux": parse_sestatus(&result.stdout),
                }))
            }
            "enforce" => {
                let result = crate::cmd!({&INFO, action, user} "getenforce")?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "mode": result.stdout.trim(),
                }))
            }
            "booleans" => {
                let result = crate::cmd!({&INFO, action, user} "getsebool" ["-a"])?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "booleans": parse_getsebool(&result.stdout),
                }))
            }
            "set_boolean" => {
                let payload: SetBooleanPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                // -P persists the boolean to /etc/selinux/targeted/... so it
                // survives reboot; without it the change lives only in the
                // running policy and is lost on the next load.
                if payload.persistent {
                    args.push("-P".to_owned());
                }
                args.push(payload.name);
                args.push(boolean_value(payload.value).to_owned());
                crate::cmd!({&INFO, action, user} DRY_RUN(payload.dry_run) "setsebool" &args ; json)
            }
            "file_contexts" => {
                // `semanage fcontext -l` dumps every fcontext rule the policy
                // knows about; `parse_semanage_fcontext` below turns the
                // tabular output into structured JSON.
                let result = crate::cmd!({&INFO, action, user} "semanage" ["fcontext", "-l"])?;
                Ok(json!({
                    "command": result.command,
                    "status": result.status,
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "dry_run": result.dry_run,
                    "file_contexts": parse_semanage_fcontext(&result.stdout),
                }))
            }
            "add_file_context" => {
                let payload: FileContextPayload = parse_payload(payload)?;
                // The pattern is forwarded unchanged; the caller is
                // responsible for the regex shape (e.g. `/srv(/.*)?`).
                crate::cmd!(DRY_RUN(payload.dry_run){&INFO, action, user} "semanage" [
                    "fcontext",
                    "-a",
                    "-t",
                    &payload.context_type,
                    &payload.path_pattern,
                ] json)
            }
            "delete_file_context" => {
                let payload: DeleteFileContextPayload = parse_payload(payload)?;
                crate::cmd!(DRY_RUN(payload.dry_run){&INFO, action, user} "semange" ["fcontext", "-d", &payload.path_pattern] json)
            }
            "restore_context" => {
                let payload: RestoreContextPayload = parse_payload(payload)?;
                let mut args = Vec::new();
                if payload.recursive {
                    args.push("-R".to_owned());
                }
                // -v is always on so the response stdout lists every relabeled
                // path — that listing is what operators expect to audit a
                // relabeling run against.
                args.push("-v".to_owned());
                args.push(payload.path);
                crate::cmd!(DRY_RUN(payload.dry_run){&INFO, action, user} "restorecon" &args ; json)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

const fn default_persistent() -> bool {
    true
}

const fn boolean_value(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// Parses `sestatus` output into a flat object keyed by lowercased,
/// underscore-separated field names (e.g. `SELinux status:` becomes
/// `selinux_status`). Lines without a `Key: value` pair are dropped.
fn parse_sestatus(stdout: &str) -> Value {
    let mut object = serde_json::Map::new();
    for line in stdout.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        object.insert(normalize_key(key), Value::String(value.trim().to_owned()));
    }
    Value::Object(object)
}

/// Parses `getsebool -a` output, one boolean per line as `name --> value`.
///
/// `enabled` normalizes the raw string (`on`/`1`/`true`) to a boolean so
/// callers do not have to re-parse it; the original `value` is preserved too.
fn parse_getsebool(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once("-->")?;
            Some(json!({
                "name": name.trim(),
                "enabled": matches!(value.trim(), "on" | "1" | "true"),
                "value": value.trim(),
            }))
        })
        .collect()
}

/// Parses `semanage fcontext -l` output.
///
/// Output shape is roughly `PATTERN  FILE_TYPE  CONTEXT`, where `file_type`
/// may be a multi-word phrase like `all files` and `CONTEXT` is a
/// `user:role:type:level` quadruple. The first printed line is a column
/// header and the second is a `----` separator; both are filtered out below.
fn parse_semanage_fcontext(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // Drop the column header and the underline separator that semanage
        // prints at the top of its output.
        .filter(|line| !line.starts_with("SELinux fcontext") && !line.starts_with('-'))
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 3 {
                return None;
            }
            // The context is always the last whitespace-delimited field; the
            // middle fields collapse back into the `file_type` string.
            let context = fields[fields.len() - 1];
            Some(json!({
                "path_pattern": fields[0],
                "file_type": fields[1..fields.len() - 1].join(" "),
                "context": context,
                "context_type": extract_context_type(context),
            }))
        })
        .collect()
}

/// Pulls the `SELinux` *type* out of a `user:role:type:level` context string.
///
/// The type is the third colon-separated field — the part callers actually
/// compare against labels like `container_file_t` or `samba_share_t`.
fn extract_context_type(context: &str) -> Option<&str> {
    context.split(':').nth(2)
}

/// Normalizes a `sestatus` field name into a stable JSON key: trimmed,
/// lowercased, with spaces and dashes turned into underscores.
fn normalize_key(key: &str) -> String {
    key.trim().to_lowercase().replace([' ', '-'], "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentModule;

    #[test]
    fn parses_sestatus_output() {
        let status = parse_sestatus(
            "SELinux status:                 enabled\nCurrent mode:                   enforcing\nPolicy version:                 33\n",
        );
        assert_eq!(status["selinux_status"], "enabled");
        assert_eq!(status["current_mode"], "enforcing");
        assert_eq!(status["policy_version"], "33");
    }

    #[test]
    fn parses_getsebool_output() {
        let booleans = parse_getsebool("virt_use_nfs --> off\nhttpd_can_network_connect --> on\n");
        assert_eq!(booleans.len(), 2);
        assert_eq!(booleans[0]["name"], "virt_use_nfs");
        assert_eq!(booleans[0]["enabled"], false);
        assert_eq!(booleans[1]["enabled"], true);
    }

    #[test]
    fn parses_semanage_fcontext_output() {
        let contexts = parse_semanage_fcontext(
            "SELinux fcontext                                   type               Context\n\
             /srv/tetra(/.*)?                                  all files          system_u:object_r:container_file_t:s0\n",
        );
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0]["path_pattern"], "/srv/tetra(/.*)?");
        assert_eq!(contexts[0]["file_type"], "all files");
        assert_eq!(contexts[0]["context_type"], "container_file_t");
    }

    #[test]
    fn dry_run_set_boolean_does_not_call_setsebool() {
        let response = SelinuxModule
            .handle(
                "set_boolean",
                json!({ "name": "virt_use_nfs", "value": true, "dry_run": true }),
                None,
            )
            .unwrap();

        assert_eq!(response["command"], "setsebool -P virt_use_nfs on");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }

    #[test]
    fn dry_run_restore_context_does_not_call_restorecon() {
        let response = SelinuxModule
            .handle(
                "restore_context",
                json!({ "path": "/srv/tetra", "recursive": true, "dry_run": true }),
                None,
            )
            .unwrap();

        assert_eq!(response["command"], "restorecon -R -v /srv/tetra");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
