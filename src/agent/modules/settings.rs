//! Always-available settings module.
//!
//! Unlike the feature-gated modules in this crate, `settings` is compiled
//! unconditionally so the control plane can always discover basic host facts
//! (OS, architecture, family) via `get_system`. It is also the simplest
//! reference implementation of the `AgentModule` trait for new contributors.

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{ModuleInfo, ModuleStatus, handle_metadata, parse_payload, run_command_or_dry_run_for_module, unsupported_action},
};

/// Marker type for the always-on settings module. It carries no state: all
/// behavior is expressed through the `AgentModule` impl and the static
/// [`INFO`] descriptor below.
pub struct SettingsModule;

#[derive(Debug, Deserialize)]
struct SetHostnamePayload {
    hostname: String,
    #[serde(default)]
    dry_run: bool,
}

const INFO: ModuleInfo = ModuleInfo {
    name: "settings",
    // "core" is a pseudo-feature: this module has no Cargo feature flag and is
    // always compiled in. The field exists only so the descriptor shape matches
    // the other modules.
    feature: "core",
    description: "Agent and host settings that are always available.",
    status: ModuleStatus::Available,
    actions: &["capabilities", "get_system", "set_hostname"],
    privileged_actions: &["set_hostname"],
};

impl AgentModule for SettingsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        // Delegate the cross-module metadata actions (`capabilities`, `plan`)
        // first. When matched, the early return skips the action match below;
        // otherwise the payload is forwarded to the module-specific handlers.
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            // `std::env::consts` are compile-time constants derived from the
            // target triple, so `get_system` performs no host probe and is safe
            // to call in any context.
            "get_system" => Ok(json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "family": std::env::consts::FAMILY,
            })),
            "set_hostname" => {
                let payload: SetHostnamePayload = parse_payload(payload)?;
                run_command_or_dry_run_for_module(&INFO, action, "hostnamectl", ["set-hostname", &payload.hostname], payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}
