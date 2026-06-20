use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tetra::{podlet, recipe};

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about = "Generate Podman Quadlets from Tetra recipes using Podlet"
)]
struct Cli {
    /// Tetra recipe YAML.
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

    if !cli.dry_run {
        which::which(&cli.podlet).with_context(|| {
            format!("could not find `{}` in PATH", cli.podlet.to_string_lossy())
        })?;
    }

    let recipe = recipe::load_and_merge(&cli.recipe, &cli.user_config)?;
    let options = podlet::PodletOptions {
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

    podlet::run(&recipe, &options)
}
