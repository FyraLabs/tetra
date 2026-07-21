//! Development inbound WebSocket server for dashboard-to-Tetra connections.
//!
//! This is intentionally separate from `websocket.rs`, which is the outbound
//! production control-plane transport. The server requires an explicitly
//! configured controller public key and authenticates before dispatching frames.

use std::{
    fs::File,
    io::BufReader,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use rand::RngExt;
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[cfg(feature = "polkit")]
use super::polkit::{DEFAULT_ELEVATION_TTL, ElevationGrant};

use super::{
    AgentBackend,
    crypto::{parse_verifying_key, public_key_fingerprint, verify_challenge_signature},
    identity::HostIdentity,
    protocol::{AuthFrame, AuthenticatedSession, PROTOCOL_VERSION, SessionPolicy, unix_timestamp},
    queue::{DEFAULT_QUEUE_CAPACITY, DispatchQueue, QueueError},
};
use ed25519_dalek::VerifyingKey;

#[derive(Debug, Clone)]
pub struct WebSocketServerConfig {
    pub listen: SocketAddr,
    /// Explicit controller key for development. Once enrolled, the persisted
    /// key under `identity_dir` is used on subsequent starts.
    pub controller_public_key: Option<String>,
    /// One-time token accepted only while no controller key is enrolled.
    pub enrollment_token: Option<String>,
    pub identity_dir: PathBuf,
    pub tls_cert_path: Option<PathBuf>,
    pub tls_key_path: Option<PathBuf>,
}

pub async fn serve(config: WebSocketServerConfig) -> Result<()> {
    let tls_acceptor = tls_acceptor(&config)?;
    validate_listener_security(&config.listen, tls_acceptor.is_some())?;

    let identity = HostIdentity::load_or_generate(&config.identity_dir)?;
    let persisted_controller_key = identity.load_controller_key()?;
    let controller_public_key = match config.controller_public_key {
        Some(configured) => {
            if let Some(persisted) = &persisted_controller_key {
                ensure!(
                    persisted == &configured,
                    "configured controller key differs from the enrolled key; rotate it explicitly"
                );
            } else {
                // `--controller-public-key` is a bootstrap convenience. Keep
                // it in the mutable identity store so the agent does not become
                // unpaired on its next restart.
                identity.enroll_controller_key(&configured)?;
            }
            Some(configured)
        }
        None => persisted_controller_key,
    }
    .map(|key| parse_verifying_key(&key).context("invalid controller public key"))
    .transpose()?;
    let listener = TcpListener::bind(config.listen).await.with_context(|| {
        format!(
            "failed to bind development WebSocket listener on {}",
            config.listen
        )
    })?;
    let queue = DispatchQueue::spawn(AgentBackend::spawn_default(), DEFAULT_QUEUE_CAPACITY);

    eprintln!(
        "serving authenticated Tetra WebSocket on {}://{}",
        if tls_acceptor.is_some() { "wss" } else { "ws" },
        config.listen
    );
    eprintln!("host identity: {}", identity.path().display());
    eprintln!(
        "host fingerprint: {}",
        public_key_fingerprint(&identity.verifying_key())
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        let queue = queue.clone();
        let identity = identity.clone();
        let enrollment_token = config.enrollment_token.clone();
        let tls_acceptor = tls_acceptor.clone();
        tokio::spawn(async move {
            let stream = match tls_acceptor {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(stream) => Box::new(stream) as Box<dyn AsyncReadWrite>,
                    Err(error) => {
                        eprintln!("TLS peer {peer} failed handshake: {error}");
                        return;
                    }
                },
                None => Box::new(stream) as Box<dyn AsyncReadWrite>,
            };
            if let Err(error) = handle_connection(
                stream,
                peer,
                queue,
                identity,
                controller_public_key,
                enrollment_token,
            )
            .await
            {
                eprintln!("WebSocket peer {peer} closed with error: {error:?}");
            }
        });
    }
}

async fn handle_connection(
    stream: Box<dyn AsyncReadWrite>,
    peer: SocketAddr,
    queue: DispatchQueue,
    identity: HostIdentity,
    controller_key: Option<VerifyingKey>,
    enrollment_token: Option<String>,
) -> Result<()> {
    let mut socket = accept_async(stream)
        .await
        .context("WebSocket handshake failed")?;
    let session_id = random_token(24)?;
    let challenge = random_token(32)?;
    let controller_key = match controller_key {
        Some(key) => key,
        None => {
            send(
                &mut socket,
                &AuthFrame::EnrollmentRequired {
                    host_fingerprint: public_key_fingerprint(&identity.verifying_key()),
                },
            )
            .await?;
            let Some(message) = socket.next().await else {
                bail!("peer disconnected before enrollment")
            };
            let AuthFrame::Enroll { token, public_key } = parse_message(message?)? else {
                send_error(&mut socket, "controller enrollment is required").await?;
                bail!("peer sent non-enrollment frame to unpaired host")
            };
            let expected_token = enrollment_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("no enrollment token configured"))?;
            if token != expected_token {
                send_error(&mut socket, "invalid enrollment token").await?;
                bail!("invalid enrollment token")
            }
            let key = parse_verifying_key(&public_key).context("invalid enrollment public key")?;
            identity.enroll_controller_key(&public_key)?;
            send(
                &mut socket,
                &AuthFrame::Response {
                    response: super::AgentResponse::ok("enrolled", json!({"host_fingerprint": public_key_fingerprint(&identity.verifying_key())})),
                },
            )
            .await?;
            key
        }
    };

    send(
        &mut socket,
        &AuthFrame::Challenge {
            protocol_version: PROTOCOL_VERSION.into(),
            session_id: session_id.clone(),
            challenge: challenge.clone(),
            host_fingerprint: public_key_fingerprint(&identity.verifying_key()),
        },
    )
    .await?;

    let Some(message) = socket.next().await else {
        bail!("peer disconnected before authentication")
    };
    let frame = parse_message(message?)?;
    let AuthFrame::Authenticate {
        protocol_version,
        session_id: received_session,
        public_key,
        signature,
    } = frame
    else {
        send_error(&mut socket, "authentication is required before commands").await?;
        bail!("peer sent non-authentication frame")
    };
    if protocol_version != PROTOCOL_VERSION || received_session != session_id {
        send_error(&mut socket, "protocol version or session id mismatch").await?;
        bail!("authentication context mismatch")
    }
    let supplied_key = parse_verifying_key(&public_key)?;
    if supplied_key != controller_key {
        send_error(&mut socket, "controller public key is not enrolled").await?;
        bail!("controller public key is not enrolled")
    }
    verify_challenge_signature(
        &controller_key,
        &signature,
        &protocol_version,
        &session_id,
        &challenge,
    )?;
    let mut session =
        AuthenticatedSession::new(session_id.clone(), controller_key, SessionPolicy::default())?;
    send(
        &mut socket,
        &AuthFrame::Response {
            response: super::AgentResponse::ok("authenticated", json!({"session_id": session_id})),
        },
    )
    .await?;

    // A grant only controls elevation UI state. Typed privileged helpers must
    // still perform their own polkit authorization checks before an operation.
    #[cfg(feature = "polkit")]
    let mut elevation: Option<ElevationGrant> = None;

    while let Some(message) = socket.next().await {
        let frame = parse_message(message?)?;
        match frame {
            #[cfg(feature = "polkit")]
            AuthFrame::ElevationRequest {
                session_id: requested_session,
            } => {
                if requested_session != session.session_id() {
                    send_error(&mut socket, "elevation request session does not match").await?;
                    continue;
                }
                match ElevationGrant::request(requested_session, DEFAULT_ELEVATION_TTL) {
                    Ok(grant) => {
                        let expires_at = grant
                            .expires_in_seconds()
                            .map(|seconds| unix_timestamp().unwrap_or(0) + seconds);
                        elevation = Some(grant);
                        send(&mut socket, &AuthFrame::ElevationStatus {
                            state: super::protocol::ElevationState::Active,
                            expires_at,
                            message: Some("Administrator mode is active. Privileged operations still require typed helper authorization.".into()),
                        }).await?;
                    }
                    Err(error) => {
                        elevation = None;
                        send(
                            &mut socket,
                            &AuthFrame::ElevationStatus {
                                state: super::protocol::ElevationState::Inactive,
                                expires_at: None,
                                message: Some(error.to_string()),
                            },
                        )
                        .await?;
                    }
                }
            }
            #[cfg(feature = "polkit")]
            AuthFrame::ElevationRevoke {
                session_id: requested_session,
            } => {
                if requested_session != session.session_id() {
                    send_error(&mut socket, "elevation revoke session does not match").await?;
                    continue;
                }
                elevation = None;
                send(
                    &mut socket,
                    &AuthFrame::ElevationStatus {
                        state: super::protocol::ElevationState::Inactive,
                        expires_at: None,
                        message: Some("Administrator mode was revoked.".into()),
                    },
                )
                .await?;
            }
            AuthFrame::Command { .. } => {
                #[cfg(feature = "polkit")]
                if let Some(grant) = &elevation
                    && !grant.is_active_for(session.session_id())
                {
                    elevation = None;
                    send(&mut socket, &AuthFrame::ElevationStatus {
                        state: super::protocol::ElevationState::Inactive,
                        expires_at: None,
                        message: Some("Administrator mode expired; request elevation again before a privileged operation.".into()),
                    }).await?;
                }
                let now = unix_timestamp()?;
                let command = session.accept_command(&frame, now)?.clone();
                match queue.dispatch(command).await {
                    Ok(response) => send(&mut socket, &AuthFrame::Response { response }).await?,
                    Err(QueueError::Full) => {
                        send_error(
                            &mut socket,
                            "Tetra command queue is full; retry after backoff",
                        )
                        .await?;
                    }
                    Err(QueueError::Closed) => {
                        send_error(&mut socket, "Tetra command queue is unavailable").await?;
                        bail!("dispatch queue closed")
                    }
                }
            }
            AuthFrame::Error { error } => bail!("peer reported error: {error}"),
            _ => {
                send_error(&mut socket, "unsupported frame after authentication").await?;
                bail!("unsupported authenticated frame from {peer}")
            }
        }
    }
    Ok(())
}

fn parse_message(message: Message) -> Result<AuthFrame> {
    match message {
        Message::Text(text) => {
            serde_json::from_str(&text).context("invalid authenticated WebSocket JSON")
        }
        Message::Binary(bytes) => {
            serde_json::from_slice(&bytes).context("invalid authenticated WebSocket JSON")
        }
        Message::Close(_) => bail!("peer closed WebSocket"),
        Message::Ping(_) | Message::Pong(_) => bail!("unexpected WebSocket control frame"),
        Message::Frame(_) => bail!("unexpected raw WebSocket frame"),
    }
}

async fn send<S>(socket: &mut S, frame: &AuthFrame) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let text = serde_json::to_string(frame)?;
    socket
        .send(Message::Text(text.into()))
        .await
        .context("failed to send authenticated frame")
}

async fn send_error<S>(socket: &mut S, error: &str) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    <S as futures_util::Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    send(
        socket,
        &AuthFrame::Error {
            error: error.into(),
        },
    )
    .await
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

fn validate_listener_security(listen: &SocketAddr, tls_enabled: bool) -> Result<()> {
    if !listen.ip().is_loopback() && !tls_enabled {
        bail!("non-loopback WebSocket listeners require --tls-cert and --tls-key");
    }
    Ok(())
}

fn tls_acceptor(config: &WebSocketServerConfig) -> Result<Option<TlsAcceptor>> {
    match (&config.tls_cert_path, &config.tls_key_path) {
        (None, None) => Ok(None),
        (Some(cert), Some(key)) => Ok(Some(TlsAcceptor::from(Arc::new(load_tls_config(
            cert, key,
        )?)))),
        _ => bail!("--tls-cert and --tls-key must be supplied together"),
    }
}

fn load_tls_config(cert_path: &Path, key_path: &Path) -> Result<rustls::ServerConfig> {
    let mut cert_reader =
        BufReader::new(File::open(cert_path).with_context(|| {
            format!("failed to open TLS certificate `{}`", cert_path.display())
        })?);
    let certificates = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to parse TLS certificate PEM")?;
    let mut key_reader = BufReader::new(
        File::open(key_path)
            .with_context(|| format!("failed to open TLS key `{}`", key_path.display()))?,
    );
    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("failed to parse TLS private key PEM")?
        .ok_or_else(|| anyhow::anyhow!("TLS private key PEM contains no private key"))?;
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .context("invalid TLS certificate/private-key pair")
}

fn random_token(bytes: usize) -> Result<String> {
    let mut value = vec![0_u8; bytes];
    rand::rng().fill(&mut value[..]);
    Ok(URL_SAFE_NO_PAD.encode(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCommand,
        crypto::{public_key_fingerprint, sign_challenge, sign_command},
    };
    use ed25519_dalek::SigningKey;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::net::TcpStream;
    use tokio_tungstenite::connect_async;

    async fn receive(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<TcpStream>,
        >,
    ) -> AuthFrame {
        let message = socket.next().await.unwrap().unwrap();
        parse_message(message).unwrap()
    }

    #[test]
    fn random_tokens_are_non_empty_and_different() {
        let first = random_token(16).unwrap();
        let second = random_token(16).unwrap();
        assert!(!first.is_empty());
        assert_ne!(first, second);
    }

    #[test]
    fn rejects_non_loopback_plaintext_listener() {
        let address: SocketAddr = "192.0.2.1:7780".parse().unwrap();
        assert!(validate_listener_security(&address, false).is_err());
    }

    #[tokio::test]
    async fn enrollment_persists_controller_key_and_continues_to_challenge() {
        let controller = SigningKey::from_bytes(&[13_u8; 32]);
        let identity_dir = tempdir().unwrap();
        let identity = HostIdentity::load_or_generate(identity_dir.path()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let queue = DispatchQueue::spawn(AgentBackend::spawn_default(), DEFAULT_QUEUE_CAPACITY);
        let expected_host_fingerprint = public_key_fingerprint(&identity.verifying_key());
        let controller_public_key = public_key_fingerprint(&controller.verifying_key());

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(
                Box::new(stream),
                peer,
                queue,
                identity,
                None,
                Some("enroll-once".into()),
            )
            .await
            .unwrap();
        });

        let (mut socket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let required = receive(&mut socket).await;
        assert!(
            matches!(required, AuthFrame::EnrollmentRequired { host_fingerprint } if host_fingerprint == expected_host_fingerprint)
        );
        let enroll = AuthFrame::Enroll {
            token: "enroll-once".into(),
            public_key: controller_public_key.clone(),
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&enroll).unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(receive(&mut socket).await, AuthFrame::Response { response } if response.ok)
        );
        let AuthFrame::Challenge {
            protocol_version,
            session_id,
            challenge,
            ..
        } = receive(&mut socket).await
        else {
            panic!("expected post-enrollment challenge")
        };
        let authenticate = AuthFrame::Authenticate {
            protocol_version: protocol_version.clone(),
            session_id: session_id.clone(),
            public_key: controller_public_key,
            signature: sign_challenge(&controller, &protocol_version, &session_id, &challenge),
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&authenticate).unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(
            matches!(receive(&mut socket).await, AuthFrame::Response { response } if response.ok)
        );
        socket.close(None).await.unwrap();

        assert_eq!(
            HostIdentity::load_or_generate(identity_dir.path())
                .unwrap()
                .load_controller_key()
                .unwrap(),
            Some(public_key_fingerprint(&controller.verifying_key()))
        );
    }

    #[tokio::test]
    async fn authenticated_client_can_dispatch_a_command() {
        let controller = SigningKey::from_bytes(&[11_u8; 32]);
        let identity_dir = tempdir().unwrap();
        let identity = HostIdentity::load_or_generate(identity_dir.path()).unwrap();
        let expected_host_fingerprint = public_key_fingerprint(&identity.verifying_key());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let queue = DispatchQueue::spawn(AgentBackend::spawn_default(), DEFAULT_QUEUE_CAPACITY);
        let controller_key = controller.verifying_key();

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle_connection(
                Box::new(stream),
                peer,
                queue,
                identity,
                Some(controller_key),
                None,
            )
            .await
            .unwrap();
        });

        let (mut socket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let AuthFrame::Challenge {
            protocol_version,
            session_id,
            challenge,
            host_fingerprint,
        } = receive(&mut socket).await
        else {
            panic!("expected challenge")
        };
        assert_eq!(host_fingerprint, expected_host_fingerprint);

        let public_key = public_key_fingerprint(&controller.verifying_key());
        let authenticate = AuthFrame::Authenticate {
            protocol_version: protocol_version.clone(),
            session_id: session_id.clone(),
            public_key,
            signature: sign_challenge(&controller, &protocol_version, &session_id, &challenge),
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&authenticate).unwrap().into(),
            ))
            .await
            .unwrap();
        let authenticated = receive(&mut socket).await;
        assert!(matches!(authenticated, AuthFrame::Response { response } if response.ok));

        let timestamp = unix_timestamp().unwrap();
        let nonce = "nonce-000000000001".to_string();
        let mut command = AgentCommand {
            id: "cmd-settings".into(),
            module: "settings".into(),
            action: "get_system".into(),
            payload: json!({}),
            signature: None,
        };
        command.signature =
            Some(sign_command(&controller, &command, &session_id, 0, timestamp, &nonce).unwrap());
        let command_frame = AuthFrame::Command {
            session_id,
            sequence: 0,
            timestamp,
            nonce,
            command,
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&command_frame).unwrap().into(),
            ))
            .await
            .unwrap();
        let response = receive(&mut socket).await;
        assert!(matches!(response, AuthFrame::Response { response } if response.ok));
        socket.close(None).await.unwrap();
    }
}
