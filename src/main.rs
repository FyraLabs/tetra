use std::{
    fs,
    io::{self, Read},
    net::SocketAddr,
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
#[cfg(feature = "polkit")]
use tetra::agent::polkit;

use tetra::{
    agent::{
        AgentCommand, backend,
        http::{self, HttpAgentConfig},
        transport::TransportConfig,
        vsock::{self, VsockAgentConfig},
        websocket::{self, WebSocketAgentConfig},
        websocket_server::{self, WebSocketServerConfig},
    },
    catalog::{self, RenderOptions},
};

/// Tetra CLI entry point.
///
/// The binary has two broad modes, exposed as subcommands:
///
/// - **Recipe rendering** (`render`) — pure local tooling that turns a YAML
///   recipe + Tera templates into Quadlet and companion files. No agent
///   backend, no transport.
/// - **Agent** (`agent-dispatch`, `agent-serve`, `agent-vsock-serve`,
///   `agent-connect`) — the same Kameo-backed dispatcher exposed through four
///   different surfaces: a one-shot CLI, a dev HTTP API, a vsock smoke-test
///   listener, and the production outbound WSS control-plane connection.
#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Tetra agent and recipe tooling for generating Podman Quadlets"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Render a Tetra app recipe into Quadlet files with Tera templates.
    Render(RenderCli),

    /// Dispatch one signed agent command envelope locally.
    AgentDispatch(AgentDispatchCli),

    /// Serve the local agent backend over a small HTTP API.
    AgentServe(AgentServeCli),

    /// Serve the local agent backend over a Linux virtio-vsock listener.
    AgentVsockServe(AgentVsockServeCli),

    /// Connect the local agent backend to an outbound WSS control plane.
    AgentConnect(AgentConnectCli),

    /// Serve an authenticated development WebSocket for a dashboard client.
    AgentWsServe(AgentWsServeCli),

    /// Report the current user-session polkit integration status.
    #[cfg(feature = "polkit")]
    PolkitStatus,
}

#[derive(Debug, Parser)]
struct RenderCli {
    /// Tetra recipe YAML.
    recipe: PathBuf,

    /// User values YAML for recipe parameters.
    #[arg(short, long)]
    values: Option<PathBuf>,

    /// Directory containing Tera templates referenced by the recipe.
    #[arg(short, long, value_name = "DIR")]
    templates_dir: PathBuf,

    /// Directory where rendered Quadlet files should be written.
    #[arg(short, long, value_name = "DIR")]
    output_dir: Option<PathBuf>,

    /// Print rendered resources instead of writing them.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Parser)]
struct AgentDispatchCli {
    /// JSON command envelope to dispatch. Reads stdin when omitted.
    command: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AgentServeCli {
    /// Address for the test HTTP agent API to listen on.
    #[arg(long, default_value = "127.0.0.1:7777")]
    listen: SocketAddr,

    /// Optional bearer token required by browser clients.
    #[arg(long, env = "TETRA_AGENT_TOKEN")]
    bearer_token: Option<String>,
}

#[derive(Debug, Parser)]
struct AgentVsockServeCli {
    /// Vsock port to listen on inside the VM guest.
    #[arg(long, default_value_t = 2048)]
    port: u32,

    /// Maximum accepted command JSON body size in bytes.
    #[arg(long, default_value_t = 1024 * 1024)]
    max_command_bytes: usize,
}

#[derive(Debug, Parser)]
struct AgentWsServeCli {
    /// Loopback address for the development WebSocket listener.
    #[arg(long, default_value = "127.0.0.1:7780")]
    listen: SocketAddr,

    /// URL-safe base64 Ed25519 public key enrolled for the dashboard controller.
    #[arg(long, env = "TETRA_CONTROLLER_PUBLIC_KEY")]
    controller_public_key: Option<String>,

    /// One-time token accepted to enroll a controller while no key is stored.
    #[arg(long, env = "TETRA_ENROLLMENT_TOKEN")]
    enrollment_token: Option<String>,

    /// Mutable identity directory. Defaults to /var/lib/tetra/identity.
    #[arg(long, default_value = "/var/lib/tetra/identity")]
    identity_dir: PathBuf,

    /// PEM certificate for WSS. Required with --tls-key for non-loopback binds.
    #[arg(long)]
    tls_cert: Option<PathBuf>,

    /// PEM private key for WSS. Required with --tls-cert for non-loopback binds.
    #[arg(long)]
    tls_key: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct AgentConnectCli {
    /// JSON transport config containing control_plane_url and optional TLS paths.
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Stable dashboard host id for this agent.
    #[arg(long, env = "TETRA_HOST_ID")]
    host_id: String,

    /// Reconnect with exponential backoff when the control-plane session closes.
    #[arg(long, default_value_t = true)]
    reconnect: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render(cli) => render(cli),
        Commands::AgentDispatch(cli) => agent_dispatch(cli).await,
        Commands::AgentServe(cli) => agent_serve(cli).await,
        Commands::AgentVsockServe(cli) => agent_vsock_serve(cli),
        Commands::AgentConnect(cli) => agent_connect(cli).await,
        Commands::AgentWsServe(cli) => agent_ws_serve(cli).await,
        #[cfg(feature = "polkit")]
        Commands::PolkitStatus => polkit_status(),
    }
}

fn render(cli: RenderCli) -> Result<()> {
    let resources = catalog::render_from_files(&RenderOptions {
        recipe_path: cli.recipe,
        values_path: cli.values,
        templates_dir: cli.templates_dir,
        output_dir: cli.output_dir,
        dry_run: cli.dry_run,
    })?;

    // In dry-run mode the renderer already skipped writes; print each resource
    // to stdout with a `--- filename` separator so the caller can preview what
    // would land on disk.
    if cli.dry_run {
        for resource in resources {
            println!("--- {}", resource.filename);
            print!("{}", resource.contents);
            if !resource.contents.ends_with('\n') {
                println!();
            }
        }
    }

    Ok(())
}

async fn agent_dispatch(cli: AgentDispatchCli) -> Result<()> {
    // Read the command envelope from either a path arg or stdin. Stdin lets
    // the agent be driven from a shell pipeline without a temp file.
    let text = match cli.command {
        Some(path) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read command `{}`", path.display()))?,
        None => {
            let mut text = String::new();
            io::stdin()
                .read_to_string(&mut text)
                .context("failed to read command from stdin")?;
            text
        }
    };

    let command: AgentCommand =
        serde_json::from_str(&text).context("failed to parse agent command JSON")?;
    // Spawn a one-shot backend, dispatch, and print the response. The actor is
    // dropped when this future completes — no long-lived state survives the
    // CLI invocation, which is the right behavior for a one-shot dispatch.
    let response = backend::dispatch_with_default_backend(command).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

#[cfg(feature = "polkit")]
fn polkit_status() -> Result<()> {
    let status = polkit::discover_status();
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

async fn agent_ws_serve(cli: AgentWsServeCli) -> Result<()> {
    websocket_server::serve(WebSocketServerConfig {
        listen: cli.listen,
        controller_public_key: cli.controller_public_key,
        enrollment_token: cli.enrollment_token,
        identity_dir: cli.identity_dir,
        tls_cert_path: cli.tls_cert,
        tls_key_path: cli.tls_key,
    })
    .await
}

async fn agent_serve(cli: AgentServeCli) -> Result<()> {
    eprintln!("serving Tetra agent API on http://{}", cli.listen);
    if cli.bearer_token.is_some() {
        eprintln!("bearer token authentication is enabled");
    } else {
        eprintln!("bearer token authentication is disabled");
    }

    http::serve(HttpAgentConfig {
        listen: cli.listen,
        bearer_token: cli.bearer_token,
    })
    .await
}

fn agent_vsock_serve(cli: AgentVsockServeCli) -> Result<()> {
    vsock::serve(VsockAgentConfig {
        port: cli.port,
        max_command_bytes: cli.max_command_bytes,
    })
}

async fn agent_connect(cli: AgentConnectCli) -> Result<()> {
    let text = fs::read_to_string(&cli.config)
        .with_context(|| format!("failed to read config `{}`", cli.config.display()))?;
    let transport: TransportConfig =
        serde_json::from_str(&text).context("failed to parse transport config JSON")?;

    websocket::run(WebSocketAgentConfig {
        transport,
        host_id: cli.host_id,
        reconnect: cli.reconnect,
    })
    .await
}
