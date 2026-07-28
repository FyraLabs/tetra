use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

use super::{AgentCommand, AgentResponse, module_support::ModuleInfo};

/// A single host-management surface exposed to the dashboard.
///
/// Each module owns one slice of host state (settings, files, services,
/// quadlets, …). The [`Dispatcher`] looks up a module by name and hands the
/// command's `action` and `payload` to its `handle` method.
///
/// Modules are stateless: `handle` takes `&self`, so the same module can be
/// invoked concurrently from multiple transport tasks. State lives in the
/// host (systemd, the filesystem, etc.), not in the module.
#[allow(clippy::missing_errors_doc)]
pub trait AgentModule: Send + Sync {
    /// Static metadata describing this module to the dashboard: name, feature
    /// flag, description, status, and the actions it supports.
    fn info(&self) -> ModuleInfo;

    /// Convenience defaulting `name` to the name in [`info`](Self::info).
    /// Overridable in case a module wants to register under an alias without
    /// changing its reported metadata.
    fn name(&self) -> &'static str {
        self.info().name
    }

    /// Handle one action.
    ///
    /// - `action` is the command's `action` field
    /// - `payload` is the command's `payload` (already parsed from JSON by the transport)
    ///
    /// Implementations conventionally start with [`super::module_support::handle_metadata`] to
    /// serve the shared `capabilities`/`plan` meta-actions, then match on `action`.
    fn handle(&self, action: &str, payload: Value, user: Option<&str>) -> Result<Value>;
}

/// Registry of modules that the dispatcher routes commands to.
///
/// Built via [`Dispatcher::new`] + [`Dispatcher::with_module`] (or
/// [`modules::default_dispatcher`](super::modules::default_dispatcher) for the
/// feature-gated default set). The `BTreeMap` keeps module iteration
/// deterministic — `agent.capabilities` lists modules in name order, which
/// makes dashboard diffs stable.
#[derive(Default)]
pub struct Dispatcher {
    modules: BTreeMap<String, Box<dyn AgentModule>>,
}

impl Dispatcher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style registration: `Dispatcher::new().with_module(Foo).with_module(Bar)`.
    #[must_use]
    pub fn with_module<M: AgentModule + 'static>(mut self, module: M) -> Self {
        self.register(module);
        self
    }

    /// Register a module under its `name()`. A later registration with the
    /// same name replaces the earlier one.
    pub fn register<M: AgentModule + 'static>(&mut self, module: M) {
        self.modules
            .insert(module.name().to_owned(), Box::new(module));
    }

    /// Dispatch one command: route it to the matching module, or to the
    /// built-in `agent.capabilities` action. Any error becomes an
    /// `AgentResponse::error` with the same command `id` — the caller always
    /// gets a well-formed response, never a panic.
    #[must_use]
    pub fn dispatch(&self, command: AgentCommand) -> AgentResponse {
        match self.try_dispatch(&command) {
            Ok(payload) => AgentResponse::ok(command.id, payload),
            Err(error) => AgentResponse::error(command.id, error.to_string()),
        }
    }

    /// Snapshot of every module's [`ModuleInfo`]. Used to answer
    /// `agent.capabilities`; also useful for diagnostics and tests.
    #[must_use]
    pub fn capabilities(&self) -> Vec<ModuleInfo> {
        self.modules.values().map(|module| module.info()).collect()
    }

    fn try_dispatch(&self, command: &AgentCommand) -> Result<Value> {
        command.validate()?;
        // `agent.capabilities` is reserved at the dispatcher level rather than
        // living in a fake `agent` module, so it always reports the *actually
        // registered* module set even if a custom dispatcher was built.
        if command.requests_capabilities() {
            return Ok(json!({ "modules": self.capabilities() }));
        }

        let Some(module) = self.modules.get(&command.module) else {
            bail!("unknown module `{}`", command.module);
        };

        module.handle(&command.action, command.payload.clone(), command.user())
    }
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
                privileged_actions: &[],
            }
        }

        fn handle(&self, action: &str, payload: Value, _user: Option<&str>) -> Result<Value> {
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
            user: None,
        });

        assert!(response.ok);
        assert_eq!(
            response.payload,
            Some(json!({ "action": "ping", "payload": { "value": 42 } }))
        );
    }
}
