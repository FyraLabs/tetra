use anyhow::Result;
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{ModuleInfo, ModuleStatus, handle_metadata, unsupported_action},
};

pub struct SettingsModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "settings",
    feature: "core",
    description: "Agent and host settings that are always available.",
    status: ModuleStatus::Available,
    actions: &["capabilities", "get_system"],
};

impl AgentModule for SettingsModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload)? {
            return Ok(response);
        }

        match action {
            "get_system" => Ok(json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "family": std::env::consts::FAMILY,
            })),
            _ => unsupported_action(INFO.name, action),
        }
    }
}
