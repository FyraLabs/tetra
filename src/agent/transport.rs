use anyhow::Result;

use super::{AgentCommand, AgentResponse};

pub trait Transport {
    fn connect(&mut self) -> Result<()>;
    fn receive(&mut self) -> Result<Option<AgentCommand>>;
    fn send(&mut self, response: AgentResponse) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportConfig {
    pub control_plane_url: String,
    pub client_cert_path: String,
    pub client_key_path: String,
    pub server_ca_path: String,
}
