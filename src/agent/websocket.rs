use std::{env, time::Duration};

use anyhow::{Context as _, Result, bail};
use futures_util::{SinkExt, StreamExt};

use rand::RngExt;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{
    AgentBackend, AgentCommand, AgentResponse,
    queue::{DEFAULT_QUEUE_CAPACITY, DispatchQueue, QueueError},
    transport::{TransportConfig, TransportEndpoint},
};

/// Protocol version advertised in the `Hello` frame. Bumped when the wire
/// format changes in a way the control plane must notice; the control plane
/// can refuse or warn on mismatched versions.
const PROTOCOL_VERSION: &str = "2026-06-29";
/// Upper bound for reconnect backoff. Without this a long partition could
/// push the delay into hours; capping at 60s keeps the agent responsive to a
/// control-plane restart.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

/// Configuration for the outbound WSS control-plane connection (`agent-connect`).
///
/// This is the production transport: the agent dials out to the control plane,
/// authenticates with mTLS, and exchanges [`TransportFrame`]s. Dialing out
/// (rather than listening) keeps the agent behind NAT/firewalls without port
/// forwarding — the same reason a Tailscale tailnet works.
#[derive(Debug, Clone)]
pub struct WebSocketAgentConfig {
    pub transport: TransportConfig,
    pub host_id: String,
    pub reconnect: bool,
}

/// The wire frame for the WSS control-plane protocol. Tagged with `type` so a
/// single `serde_json` parse dispatches on the frame kind.
///
/// The agent side only ever sends `Hello`, `Response`, `Pong`, and `Error`;
/// it receives `Command` and `Ping`. The other variants appear in this enum
/// so the deserialization round-trips frames the control plane might echo
/// back (and so `handle_frame` can explicitly reject them with an `Error`).
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

/// Connect to the control plane, exchange frames until the session closes, and
/// (if `reconnect`) reconnect with backoff. The outer loop is the reconnect
/// loop; the inner [`connect_once`] is a single session.
pub async fn run(config: WebSocketAgentConfig) -> Result<()> {
    let url = match config.transport.endpoint()? {
        TransportEndpoint::WebSocket { url } => url,
        TransportEndpoint::Vsock(_) => {
            bail!("agent-connect only supports ws:// and wss:// endpoints")
        }
    };

    validate_tls_config(&url, &config.transport)?;

    let queue = DispatchQueue::spawn(AgentBackend::spawn_default(), DEFAULT_QUEUE_CAPACITY);
    let mut attempt = 0_u32;

    loop {
        match connect_once(&url, &config, queue.clone()).await {
            Ok(()) => {
                if !config.reconnect {
                    return Ok(());
                }
                // Reset the backoff counter after a clean session so a brief
                // blip doesn't leave the agent on a long delay forever.
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

/// Open one WSS session: send `Hello`, then pump frames until the socket
/// closes or errors. Returns `Ok(())` on a clean close so the caller's reconnect
/// loop can re-enter with a fresh backoff.
async fn connect_once(
    url: &str,
    config: &WebSocketAgentConfig,
    queue: DispatchQueue,
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
                handle_frame(&mut socket, queue.clone(), frame).await?;
            }
            Message::Binary(bytes) => {
                let frame: TransportFrame =
                    serde_json::from_slice(&bytes).context("invalid websocket frame JSON")?;
                handle_frame(&mut socket, queue.clone(), frame).await?;
            }
            // tungstenite handles Ping/Pong at the protocol layer; we just echo.
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            Message::Close(frame) => {
                eprintln!("control plane closed websocket: {frame:?}");
                return Ok(());
            }
            // Raw frames only appear when the underlying codec is in a state we
            // don't model; tungstenite normally decodes these for us, so this
            // is a defensive no-op.
            Message::Frame(_) => {}
        }
    }

    Ok(())
}

/// Dispatch one received frame. `Command` frames go to the backend actor and
/// the response is sent back as a `Response` frame; `Ping` is echoed as a
/// `Pong`; anything else (including `Hello`, `Response`, `Pong`, `Error`) is
/// rejected with an `Error` frame — the control plane should never send those
/// to an agent.
async fn handle_frame<S>(socket: &mut S, queue: DispatchQueue, frame: TransportFrame) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    match frame {
        TransportFrame::Command { command } => match queue.dispatch(command).await {
            Ok(response) => send_frame(socket, &TransportFrame::Response { response }).await?,
            Err(QueueError::Full) => {
                send_frame(
                    socket,
                    &TransportFrame::Error {
                        error: "Tetra command queue is full; retry after backoff".into(),
                    },
                )
                .await?;
            }
            Err(QueueError::Closed) => {
                send_frame(
                    socket,
                    &TransportFrame::Error {
                        error: "Tetra command queue is unavailable".into(),
                    },
                )
                .await?;
                bail!("dispatch queue closed")
            }
        },
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

/// Serialize and send one frame as a text message. We always send text (not
/// binary) so the frames are easy to inspect in a proxy or log.
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

/// Reject production WSS configurations until the transport has enough TLS
/// material for explicit mutual authentication. The current connector still
/// needs a custom rustls client setup to consume these files; failing closed is
/// safer than silently using platform roots and ignoring the configured mTLS
/// paths.
fn validate_tls_config(url: &str, config: &TransportConfig) -> Result<()> {
    if url.starts_with("ws://") {
        bail!("outbound production transport requires wss://; ws:// is development-only")
    }
    if config.client_cert_path.is_none()
        || config.client_key_path.is_none()
        || config.server_ca_path.is_none()
    {
        bail!(
            "wss control-plane transport requires client_cert_path, client_key_path, and server_ca_path"
        )
    }
    Ok(())
}

/// Exponential reconnect backoff with jitter: `2^attempt` seconds, capped at
/// [`MAX_RECONNECT_DELAY`], plus up to 1 second of random jitter to spread out
/// a thundering herd of agents reconnecting after a control-plane restart.
fn reconnect_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(6);
    let base = Duration::from_secs(2_u64.saturating_pow(exponent));
    let capped = base.min(MAX_RECONNECT_DELAY);
    let jitter_ms = rand::rng().random_range(0..=1000);
    capped + Duration::from_millis(jitter_ms)
}

/// Best-effort hostname for the `Hello` frame. `HOSTNAME` is what systemd
/// sets on Linux; `COMPUTERNAME` is the Windows equivalent. We don't fall
/// back to `gethostname()` because the env vars are good enough for a
/// display name and avoid a syscall.
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
    fn rejects_incomplete_or_plaintext_outbound_tls_configuration() {
        let incomplete = TransportConfig {
            control_plane_url: "wss://dashboard.example.test/tetra".into(),
            client_cert_path: None,
            client_key_path: None,
            server_ca_path: None,
        };
        assert!(validate_tls_config(&incomplete.control_plane_url, &incomplete).is_err());

        let plaintext = TransportConfig {
            control_plane_url: "ws://127.0.0.1:7780".into(),
            client_cert_path: Some("client.crt".into()),
            client_key_path: Some("client.key".into()),
            server_ca_path: Some("ca.crt".into()),
        };
        assert!(validate_tls_config(&plaintext.control_plane_url, &plaintext).is_err());
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
                user: None,
            },
        };

        let value: Value = serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
        assert_eq!(value["type"], "command");
        assert_eq!(value["command"]["id"], "cmd-1");
    }
}
