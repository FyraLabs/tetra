use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::agent::AgentModule;

pub struct ServicesModule;

impl AgentModule for ServicesModule {
    fn name(&self) -> &'static str {
        "services"
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        match action {
            "plan" => Ok(json!({
                "manager": "systemd",
                "requested": payload,
                "status": "planned",
            })),
            _ => bail!("unsupported services action `{action}`"),
        }
    }
}
