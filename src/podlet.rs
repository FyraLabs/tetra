use std::{ffi::OsString, process::Command as ProcessCommand};

use anyhow::{Context, Result, bail};

use crate::recipe::{ContainerRecipe, Recipe};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PodletOptions {
    pub podlet_bin: OsString,
    pub output_dir: Option<OsString>,
    pub overwrite: bool,
    pub install: bool,
    pub name: Option<OsString>,
    pub podman_version: Option<OsString>,
    pub skip_services_check: bool,
    pub absolute_host_paths: bool,
    pub dry_run: bool,
}

pub fn command_args(recipe: &Recipe, options: &PodletOptions) -> Result<Vec<OsString>> {
    recipe.validate()?;

    let mut args = Vec::new();

    if let Some(output_dir) = &options.output_dir {
        args.push("--file".into());
        args.push(output_dir.clone());
    }

    if options.overwrite {
        args.push("--overwrite".into());
    }

    if options.install {
        args.push("--install".into());
    }

    if let Some(name) = &options.name {
        args.push("--name".into());
        args.push(name.clone());
    }

    if let Some(version) = &options.podman_version {
        args.push("--podman-version".into());
        args.push(version.clone());
    }

    if options.skip_services_check {
        args.push("--skip-services-check".into());
    }

    if options.absolute_host_paths {
        args.push("--absolute-host-paths".into());
    }

    if recipe.container.start_with_pod == Some(false) {
        args.push("--no-start-with-pod".into());
    }

    args.push("podman".into());
    args.push("run".into());
    push_container_args(&mut args, &recipe.container)?;

    Ok(args)
}

pub fn run(recipe: &Recipe, options: &PodletOptions) -> Result<()> {
    let args = command_args(recipe, options)?;

    if options.dry_run {
        println!("{}", shell_command(&options.podlet_bin, &args));
        return Ok(());
    }

    let status = ProcessCommand::new(&options.podlet_bin)
        .args(&args)
        .status()
        .with_context(|| {
            format!(
                "failed to execute `{}`",
                options.podlet_bin.to_string_lossy()
            )
        })?;

    if !status.success() {
        bail!("podlet exited with status {status}");
    }

    Ok(())
}

fn push_container_args(args: &mut Vec<OsString>, container: &ContainerRecipe) -> Result<()> {
    push_opt(args, "--name", &container.container_name);
    push_opt(
        args,
        "--entrypoint",
        &container
            .entrypoint
            .clone()
            .map(|c| c.into_args().join(" ")),
    );
    push_opt(args, "--workdir", &container.working_dir);
    push_opt(args, "--hostname", &container.hostname);
    push_user(args, container)?;
    push_opt(args, "--pull", &container.pull_policy);
    push_opt(args, "--restart", &container.restart);
    push_opt(args, "--shm-size", &container.shm_size);
    push_opt(args, "--stop-signal", &container.stop_signal);
    push_opt(args, "--stop-timeout", &container.stop_grace_period);
    push_opt(args, "--pod", &container.pod);
    push_opt(args, "--tz", &container.timezone);

    push_bool(args, "--privileged", container.privileged);
    push_bool(args, "--read-only", container.read_only);
    push_optional_bool_value(args, "--http-proxy", container.http_proxy);
    if container.notify == Some(true) {
        push_arg(args, "--sdnotify", "container");
    }

    for (key, value) in &container.environment {
        push_pair(args, "--env", key, value);
    }
    for port in &container.ports {
        push_arg(args, "--publish", port);
    }
    for volume in &container.volumes {
        push_arg(args, "--volume", volume);
    }
    for device in &container.devices {
        push_arg(args, "--device", device);
    }
    for dns in &container.dns {
        push_arg(args, "--dns", dns);
    }
    for dns_search in &container.dns_search {
        push_arg(args, "--dns-search", dns_search);
    }
    for group in &container.group_add {
        push_arg(args, "--group-add", group);
    }
    for network in &container.networks {
        push_arg(args, "--network", network);
    }
    if let Some(network_mode) = &container.network_mode {
        push_arg(args, "--network", network_mode);
    }
    for secret in &container.secrets {
        push_arg(args, "--secret", secret);
    }
    for (key, value) in &container.ulimits {
        push_pair(args, "--ulimit", key, value);
    }
    for cap in &container.cap_add {
        push_arg(args, "--cap-add", cap);
    }
    for cap in &container.cap_drop {
        push_arg(args, "--cap-drop", cap);
    }
    for (key, value) in &container.labels {
        push_pair(args, "--label", key, value);
    }
    for (key, value) in &container.annotations {
        push_pair(args, "--annotation", key, value);
    }
    for (key, value) in &container.sysctls {
        push_pair(args, "--sysctl", key, value);
    }
    for tmpfs in &container.tmpfs {
        push_arg(args, "--tmpfs", tmpfs);
    }
    for uid_map in &container.uid_map {
        push_arg(args, "--uidmap", uid_map);
    }
    for podman_arg in &container.podman_args {
        args.push(OsString::from(podman_arg));
    }

    push_quadlet_only(args, "--module", &container.module);
    push_quadlet_only(args, "--pids-limit", &container.pids_limit);
    reject_unsupported("container.reload_cmd", &container.reload_cmd)?;
    reject_unsupported("container.reload_signal", &container.reload_signal)?;
    push_quadlet_only(args, "--subgidname", &container.sub_gid_map);
    push_quadlet_only(args, "--subuidname", &container.sub_uid_map);

    if let Some(retry) = container.retry {
        push_arg(args, "--retry", retry.to_string());
    }
    push_quadlet_only(args, "--retry-delay", &container.retry_delay);

    if let Some(autoupdate) = &container.autoupdate {
        push_arg(
            args,
            "--label",
            format!("io.containers.autoupdate={autoupdate}"),
        );
    }

    let image = container
        .image
        .as_ref()
        .filter(|image| !image.is_empty())
        .context("container.image is required")?;
    args.push(image.into());

    if let Some(command) = container.command.clone() {
        args.extend(command.into_args().into_iter().map(OsString::from));
    }

    Ok(())
}

fn push_arg(args: &mut Vec<OsString>, flag: &str, value: impl Into<OsString>) {
    args.push(flag.into());
    args.push(value.into());
}

fn push_opt(args: &mut Vec<OsString>, flag: &str, value: &Option<String>) {
    if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
        push_arg(args, flag, value);
    }
}

fn push_pair(args: &mut Vec<OsString>, flag: &str, key: &str, value: &str) {
    push_arg(args, flag, format!("{key}={value}"));
}

fn push_bool(args: &mut Vec<OsString>, flag: &str, value: Option<bool>) {
    if value == Some(true) {
        args.push(flag.into());
    }
}

fn push_optional_bool_value(args: &mut Vec<OsString>, flag: &str, value: Option<bool>) {
    if let Some(value) = value {
        push_arg(args, flag, value.to_string());
    }
}

fn push_user(args: &mut Vec<OsString>, container: &ContainerRecipe) -> Result<()> {
    match (&container.user, &container.group) {
        (Some(user), Some(group)) if !user.contains(':') => {
            push_arg(args, "--user", format!("{user}:{group}"))
        }
        (Some(user), None) => push_arg(args, "--user", user),
        (Some(_), Some(_)) => bail!(
            "container.group cannot be combined with a container.user that already includes a group"
        ),
        (None, Some(_)) => bail!(
            "container.group requires container.user so Podlet can express it as --user UID:GID"
        ),
        (None, None) => {}
    }

    Ok(())
}

fn push_quadlet_only(args: &mut Vec<OsString>, flag: &str, value: &Option<String>) {
    push_opt(args, flag, value);
}

fn reject_unsupported(field: &str, value: &Option<String>) -> Result<()> {
    if value.is_some() {
        bail!(
            "{field} is not currently exposed by Podlet's CLI; remove it or add direct Podlet library support"
        )
    }

    Ok(())
}

pub fn shell_command(program: &OsString, args: &[OsString]) -> String {
    std::iter::once(program)
        .chain(args.iter())
        .map(|arg| {
            let arg = arg.to_string_lossy();
            shlex::try_quote(&arg)
                .map(|quoted| quoted.into_owned())
                .unwrap_or_else(|_| arg.into_owned())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::recipe::{ContainerRecipe, Recipe};

    #[test]
    fn builds_podlet_run_command() {
        let recipe = Recipe {
            container: ContainerRecipe {
                container_name: Some("web".into()),
                image: Some("docker.io/library/caddy:latest".into()),
                ports: vec!["8080:80".into()],
                volumes: vec!["./Caddyfile:/etc/caddy/Caddyfile:Z".into()],
                command: Some(crate::recipe::Command::List(vec![
                    "caddy".into(),
                    "run".into(),
                ])),
                ..Default::default()
            },
        };
        let options = PodletOptions {
            podlet_bin: OsString::from("podlet"),
            install: true,
            ..Default::default()
        };

        let args = command_args(&recipe, &options).unwrap();

        assert_eq!(
            args,
            vec![
                OsString::from("--install"),
                OsString::from("podman"),
                OsString::from("run"),
                OsString::from("--name"),
                OsString::from("web"),
                OsString::from("--publish"),
                OsString::from("8080:80"),
                OsString::from("--volume"),
                OsString::from("./Caddyfile:/etc/caddy/Caddyfile:Z"),
                OsString::from("docker.io/library/caddy:latest"),
                OsString::from("caddy"),
                OsString::from("run"),
            ]
        );
    }
}
