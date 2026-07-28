//! Always-available settings module.
//!
//! Unlike the feature-gated modules in this crate, `settings` is compiled
//! unconditionally so the control plane can always discover basic host facts
//! (OS, architecture, family) via `get_system`. It is also the simplest
//! reference implementation of the `AgentModule` trait for new contributors.

use crate::prelude::*;

/// Marker type for the always-on settings module. It carries no state: all
/// behavior is expressed through the `Mod` impl and the static
/// [`INFO`] descriptor below.
#[derive(Clone, Copy, Debug)]
pub struct SettingsModule;

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

impl Mod for SettingsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value> {
        Action::from_payload(action, payload)?.handle(user)
    }
}

actions!(Action [payload user] => {
    // `std::env::consts` are compile-time constants derived from the
    // target triple, so `get_system` performs no host probe and is safe
    // to call in any context.
    GetSystem => Ok(jsonf! {
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "family": std::env::consts::FAMILY,
    }),
    SetHostname {
        hostname: String,
        #[serde(default)]
        dry_run: bool,
    } => crate::cmd!((payload.dry_run) { &INFO, "set_hostname", user } "hostnamectl" ["set-hostname", &payload.hostname] json),
});
