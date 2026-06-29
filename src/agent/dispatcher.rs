use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{AgentCommand, AgentResponse, module_support::ModuleInfo};

pub trait AgentModule: Send + Sync {
    fn info(&self) -> ModuleInfo;

    fn name(&self) -> &'static str {
        self.info().name
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value>;
}

#[derive(Default)]
pub struct Dispatcher {
    modules: BTreeMap<String, Box<dyn AgentModule>>,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_module(mut self, module: impl AgentModule + 'static) -> Self {
        self.register(module);
        self
    }

    pub fn register(&mut self, module: impl AgentModule + 'static) {
        self.modules
            .insert(module.name().to_string(), Box::new(module));
    }

    pub fn dispatch(&self, command: AgentCommand) -> AgentResponse {
        match self.try_dispatch(&command) {
            Ok(payload) => AgentResponse::ok(command.id, payload),
            Err(error) => AgentResponse::error(command.id, error.to_string()),
        }
    }

    pub fn capabilities(&self) -> Vec<ModuleInfo> {
        self.modules.values().map(|module| module.info()).collect()
    }

    fn try_dispatch(&self, command: &AgentCommand) -> Result<Value> {
        verify_signature(command)?;
        if command.module == "agent" && command.action == "capabilities" {
            return Ok(json!({ "modules": self.capabilities() }));
        }

        let Some(module) = self.modules.get(&command.module) else {
            bail!("unknown module `{}`", command.module);
        };

        module.handle(&command.action, command.payload.clone())
    }
}

fn verify_signature(command: &AgentCommand) -> Result<()> {
    if command.signature.as_deref() == Some("") {
        bail!("command signature cannot be empty");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Echo;

    impl AgentModule for Echo {
        fn info(&self) -> ModuleInfo {
            ModuleInfo {
                name: "echo",
                feature: "test",
                description: "Test echo module",
                status: super::super::module_support::ModuleStatus::Available,
                actions: &["ping"],
            }
        }

        fn handle(&self, action: &str, payload: Value) -> Result<Value> {
            Ok(json!({ "action": action, "payload": payload }))
        }
    }

    #[test]
    fn routes_commands_to_named_modules() {
        let dispatcher = Dispatcher::new().with_module(Echo);
        let response = dispatcher.dispatch(AgentCommand {
            id: "cmd-1".into(),
            module: "echo".into(),
            action: "ping".into(),
            payload: json!({ "value": 42 }),
            signature: None,
        });

        assert!(response.ok);
        assert_eq!(
            response.payload,
            Some(json!({ "action": "ping", "payload": { "value": 42 } }))
        );
    }
}
