use anyhow::Result;
use kameo::{
    actor::{Actor, ActorRef, Spawn},
    error::Infallible,
    message::{Context, Message},
};

use super::{AgentCommand, AgentResponse, Dispatcher, messages::DispatchCommand, modules};

pub struct AgentBackend {
    dispatcher: Dispatcher,
}

impl AgentBackend {
    pub fn new(dispatcher: Dispatcher) -> Self {
        Self { dispatcher }
    }

    pub fn with_default_modules() -> Self {
        Self::new(modules::default_dispatcher())
    }

    pub fn spawn_default() -> ActorRef<Self> {
        Self::spawn(modules::default_dispatcher())
    }
}

impl Actor for AgentBackend {
    type Args = Dispatcher;
    type Error = Infallible;

    async fn on_start(
        dispatcher: Self::Args,
        _actor_ref: ActorRef<Self>,
    ) -> Result<Self, Self::Error> {
        Ok(Self::new(dispatcher))
    }
}

impl Message<DispatchCommand> for AgentBackend {
    type Reply = Result<AgentResponse>;

    async fn handle(
        &mut self,
        DispatchCommand(command): DispatchCommand,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.dispatcher.dispatch(command))
    }
}

pub async fn dispatch_with_default_backend(command: AgentCommand) -> Result<AgentResponse> {
    let backend = AgentBackend::spawn_default();
    backend
        .ask(DispatchCommand(command))
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))
}
