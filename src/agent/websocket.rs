use std::{env, time::Duration};

use anyhow::{Context as _, Result, bail};
use futures_util::{SinkExt, StreamExt};
use kameo::actor::ActorRef;
use rand::RngExt;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{
    AgentBackend, AgentCommand, AgentResponse, DispatchCommand,
    transport::{TransportConfig, TransportEndpoint},
};

const PROTOCOL_VERSION: &str = "2026-06-29";
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct WebSocketAgentConfig {
    pub transport: TransportConfig,
    pub host_id: String,
    pub reconnect: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TransportFrame {
    Hello {
        host_id: String,
        agent_version: String,
        protocol_version: String,
        hostname: Option<String>,
        os: String,
        arch: String,
    },
    Command {
        command: AgentCommand,
    },
    Response {
        response: AgentResponse,
    },
    Ping {
        id: Option<String>,
        sent_at: Option<String>,
    },
    Pong {
        id: Option<String>,
        sent_at: Option<String>,
    },
    Error {
        error: String,
    },
}

pub async fn run(config: WebSocketAgentConfig) -> Result<()> {
    let url = match config.transport.endpoint()? {
        TransportEndpoint::WebSocket { url } => url,
        TransportEndpoint::Vsock(_) => {
            bail!("agent-connect only supports ws:// and wss:// endpoints")
        }
    };

    validate_tls_config(&url, &config.transport)?;

    let backend = AgentBackend::spawn_default();
    let mut attempt = 0_u32;

    loop {
        match connect_once(&url, &config, backend.clone()).await {
            Ok(()) => {
                if !config.reconnect {
                    return Ok(());
                }
                attempt = 0;
                eprintln!("websocket session closed; reconnecting");
            }
            Err(error) => {
                if !config.reconnect {
                    return Err(error);
                }
                attempt = attempt.saturating_add(1);
                eprintln!("websocket session failed: {error:?}");
            }
        }

        sleep(reconnect_delay(attempt)).await;
    }
}

async fn connect_once(
    url: &str,
    config: &WebSocketAgentConfig,
    backend: ActorRef<AgentBackend>,
) -> Result<()> {
    let (mut socket, response) = connect_async(url)
        .await
        .with_context(|| format!("failed to connect to control plane `{url}`"))?;
    eprintln!(
        "connected to control plane `{url}` with HTTP {}",
        response.status()
    );

    send_frame(
        &mut socket,
        &TransportFrame::Hello {
            host_id: config.host_id.clone(),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            hostname: hostname(),
            os: env::consts::OS.to_string(),
            arch: env::consts::ARCH.to_string(),
        },
    )
    .await?;

    while let Some(message) = socket.next().await {
        match message.context("failed to read websocket message")? {
            Message::Text(text) => {
                let frame: TransportFrame =
                    serde_json::from_str(&text).context("invalid websocket frame JSON")?;
                handle_frame(&mut socket, backend.clone(), frame).await?;
            }
            Message::Binary(bytes) => {
                let frame: TransportFrame =
                    serde_json::from_slice(&bytes).context("invalid websocket frame JSON")?;
                handle_frame(&mut socket, backend.clone(), frame).await?;
            }
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            Message::Close(frame) => {
                eprintln!("control plane closed websocket: {frame:?}");
                return Ok(());
            }
            Message::Frame(_) => {}
        }
    }

    Ok(())
}

async fn handle_frame<S>(
    socket: &mut S,
    backend: ActorRef<AgentBackend>,
    frame: TransportFrame,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    match frame {
        TransportFrame::Command { command } => {
            let response = match backend.ask(DispatchCommand(command)).await {
                Ok(response) => response,
                Err(error) => AgentResponse::error("dispatch-error", error.to_string()),
            };
            send_frame(socket, &TransportFrame::Response { response }).await?;
        }
        TransportFrame::Ping { id, sent_at } => {
            send_frame(socket, &TransportFrame::Pong { id, sent_at }).await?;
        }
        TransportFrame::Hello { .. }
        | TransportFrame::Response { .. }
        | TransportFrame::Pong { .. }
        | TransportFrame::Error { .. } => {
            send_frame(
                socket,
                &TransportFrame::Error {
                    error: "unsupported frame from control plane".into(),
                },
            )
            .await?;
        }
    }

    Ok(())
}

async fn send_frame<S>(socket: &mut S, frame: &TransportFrame) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let text = serde_json::to_string(frame).context("failed to serialize websocket frame")?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("failed to send websocket frame")
}

fn validate_tls_config(url: &str, config: &TransportConfig) -> Result<()> {
    if url.starts_with("wss://")
        && (config.client_cert_path.is_none()
            || config.client_key_path.is_none()
            || config.server_ca_path.is_none())
    {
        eprintln!(
            "warning: wss endpoint configured without full mTLS paths; using platform root validation for this session"
        );
    }

    Ok(())
}

fn reconnect_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(6);
    let base = Duration::from_secs(2_u64.saturating_pow(exponent));
    let capped = base.min(MAX_RECONNECT_DELAY);
    let jitter_ms = rand::rng().random_range(0..=1000);
    capped + Duration::from_millis(jitter_ms)
}

fn hostname() -> Option<String> {
    env::var("HOSTNAME")
        .ok()
        .or_else(|| env::var("COMPUTERNAME").ok())
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_delay_is_capped() {
        assert!(reconnect_delay(10) <= MAX_RECONNECT_DELAY + Duration::from_secs(1));
    }

    #[test]
    fn command_frame_round_trips() {
        let frame = TransportFrame::Command {
            command: AgentCommand {
                id: "cmd-1".into(),
                module: "settings".into(),
                action: "get_system".into(),
                payload: json!({}),
                signature: None,
            },
        };

        let value: Value = serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(value["type"], "command");
        assert_eq!(value["command"]["id"], "cmd-1");
    }
}
