//! Local user and group management.
//!
//! Wraps the classic shadow-utils CLIs (`useradd`, `usermod`, `userdel`) for
//! account lifecycle, and reads `/etc/passwd` and `/etc/group` directly for
//! listing. Mutating actions honor the shared `dry_run` flag.
//!
//! A few conventions worth knowing:
//! - `useradd` and `usermod` use *different* flag names for the home directory
//!   (`--home-dir` vs `--home`); the arg-building code below mirrors that
//!   difference on purpose.
//! - `set_password` never sees a plaintext password. The caller supplies a
//!   pre-hashed value (`$y$…`, `$6$…`, …) which is passed straight to
//!   `usermod --password`, matching how `/etc/shadow` stores it.
//! - `create`/`update` deliberately omit flags the caller did not set, so
//!   shadow-utils defaults apply for anything left unspecified.

use std::fs;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, NamedPayload, handle_metadata, parse_payload, unsupported_action,
    },
};

/// Marker type for the users module. Stateless; all behavior lives in the
/// [`AgentModule`] impl and the static [`INFO`] descriptor.
pub struct UsersModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "users",
    feature: "users",
    description: "Inspect and manage local users, groups, and account-related configuration.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "status",
        "create",
        "update",
        "delete",
        "set_password",
        "groups",
    ],
    privileged_actions: &["create", "update", "delete", "set_password", "groups"],
};

/// Payload for `create`. All optional fields are forwarded to `useradd` only
/// when present, so unspecified attributes fall back to `useradd` defaults.
#[derive(Debug, Deserialize)]
struct CreatePayload {
    name: String,
    shell: Option<String>,
    home: Option<String>,
    /// Creates a system account (UID pulled from the system range) via
    /// `useradd --system`.
    #[serde(default)]
    system: bool,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `update`. `groups`, when present, is joined with commas and
/// passed to `usermod --groups`, which *replaces* the user's supplementary
/// group list rather than appending.
#[derive(Debug, Deserialize)]
struct UpdatePayload {
    name: String,
    shell: Option<String>,
    home: Option<String>,
    groups: Option<Vec<String>>,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `set_password`. `password_hash` is a pre-computed crypt hash as
/// stored in `/etc/shadow`; the agent does not hash plaintext.
#[derive(Debug, Deserialize)]
struct SetPasswordPayload {
    name: String,
    password_hash: String,
    #[serde(default)]
    dry_run: bool,
}

macro_rules! flag {
    ($args:ident $payload:ident:$($idk:tt)+) => {
        $($args.extend(flag!(@$payload $idk));)+
    };
    (@$payload:ident [$field:ident]) => {
        $payload.$field.into_iter().flat_map(|$field| [stringify!(--$field).to_owned(), $field])
    };
    (@$payload:ident [$s:literal $field:ident]) => {
        $payload.$field.into_iter().flat_map(|$field| [$s.to_owned(), $field])
    };
    (@$payload:ident $field:ident) => {
        $payload.$field.then(|| stringify!(--$field).to_owned())
    };
    (@$payload:ident $field:ident($e:expr)) => {
        $payload.$field.then(|| $e)
    };
}

impl AgentModule for UsersModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        // Delegate `capabilities`/`plan` to the shared metadata handler first.
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            // `/etc/passwd` is the source of truth for local accounts; the
            // password field is deliberately not surfaced (it is `x` under
            // shadow passwords, with the real hash in `/etc/shadow`).
            "list" => Ok(json!({ "users": parse_passwd(&read_file("/etc/passwd")?) })),
            "groups" => Ok(json!({ "groups": parse_group(&read_file("/etc/group")?) })),
            // `id` exits non-zero for an unknown account, so `status` doubles as
            // an existence check via the wrapped command's exit status.
            "status" => {
                let payload: NamedPayload = parse_payload(payload)?;
                crate::cmd!({ &INFO, action, user } "id" [&payload.name] json)
            }
            "create" => {
                let payload: CreatePayload = parse_payload(payload)?;
                let mut args = Vec::new();
                // `useradd` spells the home flag `--home-dir` (unlike
                // `usermod`'s `--home` below).
                flag!(args payload: system [shell] ["--home-dir" home]);
                args.push(payload.name);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "useradd" => &args ; json)
            }
            "update" => {
                let payload: UpdatePayload = parse_payload(payload)?;
                let mut args = Vec::new();
                flag!(args payload: [shell] [home]);
                args.extend(
                    (payload.groups.into_iter()).flat_map(|g| ["--groups".to_owned(), g.join(",")]),
                );
                args.push(payload.name);
                crate::cmd!((payload.dry_run) { &INFO, action, user } "usermod" => &args ; json)
            }
            "delete" => {
                let payload: NamedPayload = parse_payload(payload)?;
                crate::cmd!((payload.dry_run) { &INFO, action, user } "userdel" [&payload.name] json)
            }
            "set_password" => {
                let payload: SetPasswordPayload = parse_payload(payload)?;
                // The hash is passed through verbatim; `usermod --password`
                // expects the same string `/etc/shadow` would store.
                crate::cmd!((payload.dry_run) { &INFO, action, user } "usermod" ["--password", &payload.password_hash, &payload.name] json)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

/// Small helper to read a host file with a context-bearing error. Used for
/// `/etc/passwd` and `/etc/group`.
fn read_file(path: &str) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("failed to read `{path}`"))
}

/// Parses `/etc/passwd` into user objects.
///
/// The passwd format is `name:passwd:uid:gid:gecos:home:shell` (7 fields). We
/// intentionally skip field 1 (`passwd`), which is `x` under shadow passwords
/// and never useful to the control plane. The `>= 7` guard drops malformed or
/// comment lines rather than panicking.
fn parse_passwd(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 7).then(|| {
                json!({
                    "name": fields[0],
                    "uid": fields[2],
                    "gid": fields[3],
                    "gecos": fields[4],
                    "home": fields[5],
                    "shell": fields[6],
                })
            })
        })
        .collect()
}

/// Parses `/etc/group` into group objects.
///
/// The group format is `name:passwd:gid:members` (4 fields), where `members`
/// is a comma-separated list. The `passwd` field (index 1) is skipped. Empty
/// member strings are filtered out so a trailing comma does not yield a bogus
/// empty member.
fn parse_group(contents: &str) -> Vec<Value> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(':').collect();
            (fields.len() >= 4).then(|| {
                json!({
                    "name": fields[0],
                    "gid": fields[2],
                    "members": fields[3].split(',').filter(|member| !member.is_empty()).collect::<Vec<_>>(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_passwd_entries() {
        let users = parse_passwd("root:x:0:0:root:/root:/bin/bash\n");
        assert_eq!(users[0]["name"], "root");
        assert_eq!(users[0]["home"], "/root");
    }

    #[test]
    fn parses_group_entries() {
        let groups = parse_group("wheel:x:10:root,admin\n");
        assert_eq!(groups[0]["name"], "wheel");
        assert_eq!(groups[0]["members"][0], "root");
    }

    #[test]
    fn dry_run_create_does_not_call_useradd() {
        let response = UsersModule
            .handle(
                "create",
                json!({ "name": "testuser", "dry_run": true }),
                None,
            )
            .unwrap();

        assert_eq!(response["command"], "useradd testuser");
        assert_eq!(response["dry_run"], true);
        assert!(response["status"].is_null());
    }
}
