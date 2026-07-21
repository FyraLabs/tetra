//! Feature-gated polkit session discovery and elevation status.
//!
//! This module deliberately does not elevate generic commands. It establishes
//! the state needed by the authenticated WSS protocol and defers to an existing
//! desktop agent (such as Noctalia) when one is available.

use std::{
    env,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::Serialize;

use super::{polkit_native, protocol::ElevationState};

/// Default duration for a dashboard-visible elevation grant. The actual typed
/// helper must still ask polkit for every privileged operation; this state is
/// never an authorization substitute.
pub const DEFAULT_ELEVATION_TTL: Duration = Duration::from_secs(30 * 60);
pub const ELEVATE_ACTION_ID: &str = "io.tetra.agent.elevate";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PolkitStatus {
    pub state: ElevationState,
    pub session_id: Option<String>,
    pub message: String,
}

/// In-memory grant tied to one authenticated Tetra WebSocket session. It is
/// intentionally not serialized or persisted, so restart/logout removes it.
#[derive(Debug, Clone)]
pub struct ElevationGrant {
    session_id: String,
    expires_at: Instant,
}

impl ElevationGrant {
    pub fn request(session_id: impl Into<String>, ttl: Duration) -> Result<Self> {
        let session_id = session_id.into();
        if session_id.is_empty() {
            anyhow::bail!("elevation session id cannot be empty");
        }
        if !check_authorization_interactive(ELEVATE_ACTION_ID)? {
            anyhow::bail!("polkit denied administrator mode");
        }
        Ok(Self {
            session_id,
            expires_at: Instant::now() + ttl,
        })
    }

    pub fn is_active_for(&self, session_id: &str) -> bool {
        self.session_id == session_id && Instant::now() < self.expires_at
    }

    pub fn expires_in_seconds(&self) -> Option<i64> {
        self.expires_at
            .checked_duration_since(Instant::now())
            .map(|duration| duration.as_secs() as i64)
    }
}

/// Inspect the current process environment. A session D-Bus plus logind session
/// means a desktop agent may own authentication; Tetra must defer rather than
/// registering a competing listener.
pub fn discover_status() -> PolkitStatus {
    if polkit_native::native_types().is_none() {
        return PolkitStatus {
            state: ElevationState::Unavailable,
            session_id: None,
            message: "libpolkit-agent-1 is unavailable; install polkit-devel before enabling Tetra polkit support.".into(),
        };
    }

    let session_id = env::var("XDG_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let session_bus = env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|value| !value.is_empty());

    match (session_id, session_bus) {
        (Some(session_id), Some(_)) if user_bus_has_known_agent() => PolkitStatus {
            state: ElevationState::ExistingAgent,
            session_id: Some(session_id),
            message: "An existing polkit-capable desktop agent is active; Tetra will defer authentication prompts to it.".into(),
        },
        (Some(session_id), Some(_)) => PolkitStatus {
            state: ElevationState::Inactive,
            session_id: Some(session_id),
            message: "A user session is available but no known desktop polkit agent was detected; Tetra agent registration is not implemented yet.".into(),
        },
        (Some(session_id), None) => PolkitStatus {
            state: ElevationState::Unavailable,
            session_id: Some(session_id),
            message: "A logind session exists but no user D-Bus is available for polkit agent integration.".into(),
        },
        (None, _) => PolkitStatus {
            state: ElevationState::Unavailable,
            session_id: None,
            message: "No logind user session is available for polkit authentication.".into(),
        },
    }
}

fn user_bus_has_known_agent() -> bool {
    let Ok(output) = Command::new("busctl")
        .args(["--user", "list", "--no-pager"])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    text.contains("noctalia")
        || text.contains("polkit-kde")
        || text.contains("polkit-gnome")
        || text.contains("lxpolkit")
}

/// Ask polkit whether the current process may perform a named action without
/// interactive authorization. This is a status check only; interaction belongs
/// to the desktop/Tetra authentication agent and narrow privileged helper.
/// Request interactive authorization through the existing desktop agent. On
/// this workstation that is Noctalia; a future Tetra-owned agent will use the
/// same polkit action but relay the conversation over authenticated WSS.
pub fn check_authorization_interactive(action_id: &str) -> Result<bool> {
    check_authorization(action_id, true)
}

pub fn check_authorization_noninteractive(action_id: &str) -> Result<bool> {
    check_authorization(action_id, false)
}

fn pkcheck_args(action_id: &str, process_id: &str, allow_user_interaction: bool) -> Vec<String> {
    let mut args = vec![
        "--action-id".into(),
        action_id.into(),
        "--process".into(),
        process_id.into(),
    ];
    if allow_user_interaction {
        args.push("--allow-user-interaction".into());
    }
    args
}

fn check_authorization(action_id: &str, allow_user_interaction: bool) -> Result<bool> {
    let process_id = std::process::id().to_string();
    let mut command = Command::new("pkcheck");
    command.args(pkcheck_args(action_id, &process_id, allow_user_interaction));
    let output = command.output().context("failed to execute pkcheck")?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkcheck_args_only_request_interaction_when_explicit() {
        assert!(
            !pkcheck_args(ELEVATE_ACTION_ID, "42", false)
                .iter()
                .any(|arg| arg == "--allow-user-interaction")
        );
        assert!(
            pkcheck_args(ELEVATE_ACTION_ID, "42", true)
                .iter()
                .any(|arg| arg == "--allow-user-interaction")
        );
    }

    #[test]
    fn status_is_serializable() {
        let status = PolkitStatus {
            state: ElevationState::Inactive,
            session_id: Some("2".into()),
            message: "test".into(),
        };
        assert_eq!(serde_json::to_value(status).unwrap()["state"], "inactive");
    }
}
