use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::Value;

use super::{AgentCommand, AgentResponse};

pub trait AgentModule: Send + Sync {
    fn name(&self) -> &'static str;
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

    fn try_dispatch(&self, command: &AgentCommand) -> Result<Value> {
        verify_signature(command)?;
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
        fn name(&self) -> &'static str {
            "echo"
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
