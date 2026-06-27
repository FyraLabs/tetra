use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::agent::AgentModule;

pub struct SettingsModule;

impl AgentModule for SettingsModule {
    fn name(&self) -> &'static str {
        "settings"
    }

    fn handle(&self, action: &str, _payload: Value) -> Result<Value> {
        match action {
            "get_system" => Ok(json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "family": std::env::consts::FAMILY,
            })),
            _ => bail!("unsupported settings action `{action}`"),
        }
    }
}
