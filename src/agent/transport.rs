use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::{AgentCommand, AgentResponse};

pub trait Transport {
    fn connect(&mut self) -> Result<()>;
    fn receive(&mut self) -> Result<Option<AgentCommand>>;
    fn send(&mut self, response: AgentResponse) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportConfig {
    pub control_plane_url: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert_path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_ca_path: Option<String>,
}

impl TransportConfig {
    pub fn endpoint(&self) -> Result<TransportEndpoint> {
        self.control_plane_url.parse()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEndpoint {
    WebSocket { url: String },
    Vsock(VsockEndpoint),
}

impl FromStr for TransportEndpoint {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        if value.starts_with("wss://") || value.starts_with("ws://") {
            return Ok(Self::WebSocket {
                url: value.to_string(),
            });
        }

        let Some(address) = value.strip_prefix("vsock://") else {
            bail!("unsupported control plane transport `{value}`");
        };

        Ok(Self::Vsock(address.parse()?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockEndpoint {
    pub cid: u32,
    pub port: u32,
}

impl FromStr for VsockEndpoint {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (cid, port) = value
            .rsplit_once(':')
            .ok_or_else(|| anyhow::anyhow!("vsock endpoint must be `vsock://CID:PORT`"))?;

        if port.is_empty() {
            bail!("vsock endpoint port cannot be empty");
        }

        Ok(Self {
            cid: parse_cid(cid)?,
            port: port.parse()?,
        })
    }
}

fn parse_cid(value: &str) -> Result<u32> {
    match value {
        "hypervisor" => Ok(0),
        "local" => Ok(1),
        "host" => Ok(2),
        "" => bail!("vsock endpoint CID cannot be empty"),
        cid => Ok(cid.parse()?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_websocket_control_plane_url() {
        let config = TransportConfig {
            control_plane_url: "wss://dashboard.example.com/tetra/agent".into(),
            client_cert_path: Some("/etc/tetra/agent.crt".into()),
            client_key_path: Some("/etc/tetra/agent.key".into()),
            server_ca_path: Some("/etc/tetra/dashboard-ca.crt".into()),
        };

        assert_eq!(
            config.endpoint().unwrap(),
            TransportEndpoint::WebSocket {
                url: "wss://dashboard.example.com/tetra/agent".into()
            }
        );
    }

    #[test]
    fn parses_vsock_control_plane_url() {
        let config = TransportConfig {
            control_plane_url: "vsock://host:2048".into(),
            client_cert_path: None,
            client_key_path: None,
            server_ca_path: None,
        };

        assert_eq!(
            config.endpoint().unwrap(),
            TransportEndpoint::Vsock(VsockEndpoint { cid: 2, port: 2048 })
        );
    }

    #[test]
    fn parses_numeric_vsock_cid() {
        assert_eq!(
            "vsock://42:2048".parse::<TransportEndpoint>().unwrap(),
            TransportEndpoint::Vsock(VsockEndpoint {
                cid: 42,
                port: 2048
            })
        );
    }

    #[test]
    fn deserializes_vsock_config_without_tls_paths() {
        let config: TransportConfig =
            serde_json::from_str(r#"{ "control_plane_url": "vsock://host:2048" }"#).unwrap();

        assert_eq!(config.client_cert_path, None);
        assert_eq!(
            config.endpoint().unwrap(),
            TransportEndpoint::Vsock(VsockEndpoint { cid: 2, port: 2048 })
        );
    }
}
