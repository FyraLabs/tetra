use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    pub container: ContainerRecipe,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContainerRecipe {
    pub container_name: Option<String>,
    pub image: Option<String>,
    pub command: Option<Command>,
    pub entrypoint: Option<Command>,
    pub environment: BTreeMap<String, String>,
    pub ports: Vec<String>,
    pub volumes: Vec<String>,
    pub devices: Vec<String>,
    pub dns: Vec<String>,
    pub dns_search: Vec<String>,
    pub group_add: Vec<String>,
    pub networks: Vec<String>,
    pub network_mode: Option<String>,
    pub secrets: Vec<String>,
    pub ulimits: BTreeMap<String, String>,
    pub working_dir: Option<String>,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub privileged: Option<bool>,
    pub read_only: Option<bool>,
    pub pull_policy: Option<String>,
    pub restart: Option<String>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub sysctls: BTreeMap<String, String>,
    pub tmpfs: Vec<String>,
    pub shm_size: Option<String>,
    pub stop_signal: Option<String>,
    pub stop_grace_period: Option<String>,

    // Tetra/Quadlet-specific fields not represented by Compose.
    pub autoupdate: Option<String>,
    pub module: Option<String>,
    pub group: Option<String>,
    pub http_proxy: Option<bool>,
    pub notify: Option<bool>,
    pub pids_limit: Option<String>,
    pub pod: Option<String>,
    pub reload_cmd: Option<String>,
    pub reload_signal: Option<String>,
    pub retry: Option<u32>,
    pub retry_delay: Option<String>,
    pub start_with_pod: Option<bool>,
    pub sub_gid_map: Option<String>,
    pub sub_uid_map: Option<String>,
    pub timezone: Option<String>,
    pub uid_map: Vec<String>,
    pub podman_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    String(String),
    List(Vec<String>),
}

impl Command {
    pub fn into_args(self) -> Vec<String> {
        match self {
            Self::String(command) => shlex::split(&command).unwrap_or_else(|| vec![command]),
            Self::List(args) => args,
        }
    }
}

pub fn load_recipe(path: impl AsRef<Path>) -> Result<Recipe> {
    let path = path.as_ref();
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read recipe `{}`", path.display()))?;
    serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse recipe YAML `{}`", path.display()))
}

pub fn load_and_merge(
    recipe_path: impl AsRef<Path>,
    user_config_path: impl AsRef<Path>,
) -> Result<Recipe> {
    let recipe_path = recipe_path.as_ref();
    let user_config_path = user_config_path.as_ref();

    let recipe_text = fs::read_to_string(recipe_path)
        .with_context(|| format!("failed to read recipe `{}`", recipe_path.display()))?;
    let user_text = fs::read_to_string(user_config_path).with_context(|| {
        format!(
            "failed to read user config `{}`",
            user_config_path.display()
        )
    })?;

    let mut recipe_value: Value = serde_yaml::from_str(&recipe_text)
        .with_context(|| format!("failed to parse recipe YAML `{}`", recipe_path.display()))?;
    let user_value: Value = serde_yaml::from_str(&user_text).with_context(|| {
        format!(
            "failed to parse user config YAML `{}`",
            user_config_path.display()
        )
    })?;

    merge_yaml(&mut recipe_value, user_value);

    serde_yaml::from_value(recipe_value).context("merged config is not a valid Tetra recipe")
}

fn merge_yaml(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(base), Value::Mapping(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(base_value) => merge_yaml(base_value, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

impl Recipe {
    pub fn validate(&self) -> Result<()> {
        if self
            .container
            .image
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            bail!("container.image is required");
        }

        if self.container.reload_cmd.is_some() && self.container.reload_signal.is_some() {
            bail!("container.reload_cmd and container.reload_signal are mutually exclusive");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_nested_yaml() {
        let mut base: Value = serde_yaml::from_str(
            r#"
container:
  image: example:old
  environment:
    ONE: one
    TWO: two
"#,
        )
        .unwrap();
        let overlay: Value = serde_yaml::from_str(
            r#"
container:
  image: example:new
  environment:
    TWO: overridden
"#,
        )
        .unwrap();

        merge_yaml(&mut base, overlay);
        let recipe: Recipe = serde_yaml::from_value(base).unwrap();

        assert_eq!(recipe.container.image.as_deref(), Some("example:new"));
        assert_eq!(
            recipe.container.environment.get("ONE").map(String::as_str),
            Some("one")
        );
        assert_eq!(
            recipe.container.environment.get("TWO").map(String::as_str),
            Some("overridden")
        );
    }

    #[test]
    fn command_string_uses_shell_like_splitting() {
        assert_eq!(
            Command::String("echo 'hello world'".into()).into_args(),
            vec!["echo", "hello world"]
        );
    }
}
