use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tetra::{
    agent::{AgentCommand, modules},
    catalog::{self, RenderOptions},
    podlet as podlet_generator, recipe,
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

    /// Legacy Podlet-backed generator for the original container schema.
    Podlet(PodletCli),
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
struct PodletCli {
    /// Tetra legacy container recipe YAML.
    recipe: PathBuf,

    /// User override YAML. Values are merged over the recipe.
    user_config: PathBuf,

    /// Podlet executable to run.
    #[arg(long, default_value = "podlet")]
    podlet: OsString,

    /// Directory where Podlet should write generated Quadlet files.
    #[arg(short, long, value_name = "DIR")]
    output_dir: Option<OsString>,

    /// Overwrite generated files if they already exist.
    #[arg(long)]
    overwrite: bool,

    /// Add an [Install] section via Podlet.
    #[arg(long)]
    install: bool,

    /// Override generated Quadlet file name, without extension.
    #[arg(short, long)]
    name: Option<OsString>,

    /// Podman/Quadlet compatibility version to pass to Podlet.
    #[arg(short = 'p', long)]
    podman_version: Option<OsString>,

    /// Skip Podlet's existing-service conflict check.
    #[arg(long)]
    skip_services_check: bool,

    /// Ask Podlet to resolve relative host paths to absolute paths.
    #[arg(short = 'a', long)]
    absolute_host_paths: bool,

    /// Print the Podlet command Tetra would run instead of executing it.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Render(cli) => render(cli),
        Commands::AgentDispatch(cli) => agent_dispatch(cli),
        Commands::Podlet(cli) => podlet(cli),
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

fn agent_dispatch(cli: AgentDispatchCli) -> Result<()> {
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
    let response = modules::default_dispatcher().dispatch(command);
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn podlet(cli: PodletCli) -> Result<()> {
    if !cli.dry_run {
        which::which(&cli.podlet).with_context(|| {
            format!("could not find `{}` in PATH", cli.podlet.to_string_lossy())
        })?;
    }

    let recipe = recipe::load_and_merge(&cli.recipe, &cli.user_config)?;
    let options = podlet_generator::PodletOptions {
        podlet_bin: cli.podlet,
        output_dir: cli.output_dir,
        overwrite: cli.overwrite,
        install: cli.install,
        name: cli.name,
        podman_version: cli.podman_version,
        skip_services_check: cli.skip_services_check,
        absolute_host_paths: cli.absolute_host_paths,
        dry_run: cli.dry_run,
    };

    podlet_generator::run(&recipe, &options)
}
