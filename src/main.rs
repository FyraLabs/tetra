use std::{
    fs,
    io::{self, Read},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tetra::{
    agent::{AgentCommand, backend},
    catalog::{self, RenderOptions},
};

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render(cli) => render(cli),
        Commands::AgentDispatch(cli) => agent_dispatch(cli).await,
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
    let response = backend::dispatch_with_default_backend(command).await?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
