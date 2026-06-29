use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rand::{RngExt, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml::Value as YamlValue;
use tera::{Context as TeraContext, Tera};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppRecipe {
    pub recipe_id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub icon: Option<String>,
    pub version: String,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default)]
    pub resources: Vec<Resource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Requirement {
    Named(String),
    Detailed {
        key: String,
        version: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: ParameterKind,
    #[serde(default)]
    pub required: bool,
    pub placeholder: Option<String>,
    #[serde(default)]
    pub default: Option<YamlValue>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub generate: Option<Generator>,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKind {
    String,
    Secret,
    Integer,
    Boolean,
    Choice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Generator {
    #[serde(rename = "random_32")]
    Random32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    #[serde(rename = "type")]
    pub kind: ResourceKind,
    pub filename: String,
    pub template: String,
    #[serde(default)]
    pub condition: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Container,
    Network,
    Volume,
    Pod,
    Kube,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenderedResource {
    pub kind: ResourceKind,
    pub filename: String,
    pub contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderOptions {
    pub recipe_path: PathBuf,
    pub values_path: Option<PathBuf>,
    pub templates_dir: PathBuf,
    pub output_dir: Option<PathBuf>,
    pub dry_run: bool,
}

pub fn load_recipe(path: impl AsRef<Path>) -> Result<AppRecipe> {
    let path = path.as_ref();
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read recipe `{}`", path.display()))?;
    let recipe: AppRecipe = serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse recipe YAML `{}`", path.display()))?;
    recipe.validate()?;
    Ok(recipe)
}

pub fn load_values(path: Option<impl AsRef<Path>>) -> Result<BTreeMap<String, YamlValue>> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let path = path.as_ref();
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read values `{}`", path.display()))?;
    serde_yaml::from_str(&text)
        .with_context(|| format!("failed to parse values YAML `{}`", path.display()))
}

pub fn render_from_files(options: &RenderOptions) -> Result<Vec<RenderedResource>> {
    let recipe = load_recipe(&options.recipe_path)?;
    let values = load_values(options.values_path.as_deref())?;
    let rendered = render_recipe(&recipe, &values, &options.templates_dir)?;

    if let Some(output_dir) = &options.output_dir
        && !options.dry_run
    {
        fs::create_dir_all(output_dir).with_context(|| {
            format!(
                "failed to create output directory `{}`",
                output_dir.display()
            )
        })?;
        for resource in &rendered {
            let path = output_dir.join(&resource.filename);
            fs::write(&path, &resource.contents)
                .with_context(|| format!("failed to write `{}`", path.display()))?;
        }
    }

    Ok(rendered)
}

pub fn render_recipe(
    recipe: &AppRecipe,
    values: &BTreeMap<String, YamlValue>,
    templates_dir: impl AsRef<Path>,
) -> Result<Vec<RenderedResource>> {
    recipe.validate()?;
    let templates_dir = templates_dir.as_ref();
    let context = build_context(recipe, values)?;
    let mut rendered = Vec::new();

    for resource in &recipe.resources {
        if !condition_matches(resource.condition.as_deref(), &context)? {
            continue;
        }

        let tera_context =
            TeraContext::from_serialize(&context).context("failed to build context")?;
        let filename = Tera::one_off(&resource.filename, &tera_context, false)
            .with_context(|| format!("failed to render filename `{}`", resource.filename))?;
        let mut resource_context = context.clone();
        resource_context.insert(
            "resource".into(),
            serde_json::to_value(resource).context("failed to serialize resource")?,
        );
        resource_context.insert(
            "resource_filename".into(),
            JsonValue::String(filename.clone()),
        );
        resource_context.insert(
            "resource_name".into(),
            JsonValue::String(
                filename
                    .trim_end_matches(resource_extension(resource))
                    .to_string(),
            ),
        );
        let tera_context =
            TeraContext::from_serialize(&resource_context).context("failed to build context")?;
        let template_path = templates_dir.join(&resource.template);
        let template = fs::read_to_string(&template_path)
            .with_context(|| format!("failed to read template `{}`", template_path.display()))?;
        let contents = Tera::one_off(&template, &tera_context, false)
            .with_context(|| format!("failed to render template `{}`", template_path.display()))?;

        rendered.push(RenderedResource {
            kind: resource.kind.clone(),
            filename,
            contents,
        });
    }

    Ok(rendered)
}

fn resource_extension(resource: &Resource) -> &'static str {
    match resource.kind {
        ResourceKind::Container => ".container",
        ResourceKind::Network => ".network",
        ResourceKind::Volume => ".volume",
        ResourceKind::Pod => ".pod",
        ResourceKind::Kube => ".kube",
    }
}

fn build_context(
    recipe: &AppRecipe,
    values: &BTreeMap<String, YamlValue>,
) -> Result<BTreeMap<String, JsonValue>> {
    let mut context = BTreeMap::new();
    context.insert(
        "recipe_id".into(),
        JsonValue::String(recipe.recipe_id.clone()),
    );
    context.insert("name".into(), JsonValue::String(recipe.name.clone()));
    context.insert("version".into(), JsonValue::String(recipe.version.clone()));
    context.insert(
        "recipe".into(),
        serde_json::to_value(recipe).context("failed to serialize recipe metadata")?,
    );

    for parameter in &recipe.parameters {
        let value = match values.get(&parameter.key) {
            Some(value) => value.clone(),
            None if parameter.default.is_some() => parameter.default.clone().unwrap(),
            None if parameter.generate.is_some() => generate_value(parameter)?,
            None if parameter.required => {
                bail!("missing required parameter `{}`", parameter.key);
            }
            None => YamlValue::Null,
        };

        validate_parameter_value(parameter, &value)?;
        context.insert(parameter.key.clone(), yaml_to_json(value)?);
    }

    Ok(context)
}

fn generate_value(parameter: &Parameter) -> Result<YamlValue> {
    match parameter.generate {
        Some(Generator::Random32) => {
            let value: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(32)
                .map(char::from)
                .collect();
            Ok(YamlValue::String(value))
        }
        None => bail!("parameter `{}` does not define a generator", parameter.key),
    }
}

fn validate_parameter_value(parameter: &Parameter, value: &YamlValue) -> Result<()> {
    if matches!(value, YamlValue::Null) && !parameter.required {
        return Ok(());
    }

    match parameter.kind {
        ParameterKind::String | ParameterKind::Secret => {
            if !matches!(value, YamlValue::String(_)) {
                bail!("parameter `{}` must be a string", parameter.key);
            }
        }
        ParameterKind::Integer => {
            let Some(number) = value.as_i64() else {
                bail!("parameter `{}` must be an integer", parameter.key);
            };
            if parameter.min.is_some_and(|min| number < min) {
                bail!("parameter `{}` is below its minimum", parameter.key);
            }
            if parameter.max.is_some_and(|max| number > max) {
                bail!("parameter `{}` is above its maximum", parameter.key);
            }
        }
        ParameterKind::Boolean => {
            if !matches!(value, YamlValue::Bool(_)) {
                bail!("parameter `{}` must be a boolean", parameter.key);
            }
        }
        ParameterKind::Choice => {
            let Some(choice) = value.as_str() else {
                bail!("parameter `{}` must be a choice string", parameter.key);
            };
            if !parameter.options.iter().any(|option| option == choice) {
                bail!("parameter `{}` has an unsupported choice", parameter.key);
            }
        }
    }

    Ok(())
}

fn yaml_to_json(value: YamlValue) -> Result<JsonValue> {
    serde_json::to_value(value).context("failed to convert YAML value to template value")
}

fn condition_matches(
    condition: Option<&str>,
    context: &BTreeMap<String, JsonValue>,
) -> Result<bool> {
    let Some(condition) = condition
        .map(str::trim)
        .filter(|condition| !condition.is_empty())
    else {
        return Ok(true);
    };

    if let Some((key, expected)) = condition.split_once("==") {
        let key = key.trim();
        let expected = parse_condition_literal(expected.trim())?;
        return Ok(context.get(key) == Some(&expected));
    }

    if let Some((key, expected)) = condition.split_once("!=") {
        let key = key.trim();
        let expected = parse_condition_literal(expected.trim())?;
        return Ok(context.get(key) != Some(&expected));
    }

    match context.get(condition) {
        Some(JsonValue::Bool(value)) => Ok(*value),
        Some(JsonValue::Null) | None => Ok(false),
        Some(_) => bail!("condition `{condition}` does not resolve to a boolean"),
    }
}

fn parse_condition_literal(value: &str) -> Result<JsonValue> {
    match value.trim_matches('"') {
        "true" => Ok(JsonValue::Bool(true)),
        "false" => Ok(JsonValue::Bool(false)),
        other if other.parse::<i64>().is_ok() => {
            Ok(JsonValue::Number(other.parse::<i64>()?.into()))
        }
        other => Ok(JsonValue::String(other.to_string())),
    }
}

impl AppRecipe {
    pub fn validate(&self) -> Result<()> {
        if self.recipe_id.trim().is_empty() {
            bail!("recipe_id is required");
        }
        if self.name.trim().is_empty() {
            bail!("name is required");
        }
        if self.version.trim().is_empty() {
            bail!("version is required");
        }
        if self.resources.is_empty() {
            bail!("at least one resource is required");
        }

        let mut parameters = BTreeMap::new();
        for parameter in &self.parameters {
            if parameter.key.trim().is_empty() {
                bail!("parameter key is required");
            }
            if parameters.insert(&parameter.key, true).is_some() {
                bail!("duplicate parameter `{}`", parameter.key);
            }
            if parameter.kind == ParameterKind::Choice && parameter.options.is_empty() {
                bail!("choice parameter `{}` must define options", parameter.key);
            }
        }

        Ok(())
    }
}

pub fn context_for_agent(
    recipe: &AppRecipe,
    values: &BTreeMap<String, YamlValue>,
) -> Result<JsonValue> {
    let context = build_context(recipe, values)?;
    let object: JsonMap<String, JsonValue> = context.into_iter().collect();
    Ok(JsonValue::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nextcloud_recipe() -> AppRecipe {
        serde_yaml::from_str(
            r#"
recipe_id: nextcloud
name: Nextcloud
description: Self-hosted file sharing and collaboration
category: productivity
icon: nextcloud.svg
version: 1.0.0
parameters:
  - key: domain
    label: Domain name
    type: string
    required: true
  - key: enable_redis
    label: Enable Redis
    type: boolean
    default: true
resources:
  - type: container
    filename: "{{ recipe_id }}-app.container"
    template: app.container.tera
  - type: container
    filename: "{{ recipe_id }}-redis.container"
    template: redis.container.tera
    condition: "enable_redis == true"
"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_new_recipe_schema() {
        let recipe = nextcloud_recipe();

        assert_eq!(recipe.recipe_id, "nextcloud");
        assert_eq!(recipe.parameters.len(), 2);
        assert_eq!(recipe.resources.len(), 2);
    }

    #[test]
    fn filters_conditional_resources() {
        let recipe = nextcloud_recipe();
        let values = BTreeMap::from([
            (
                "domain".into(),
                YamlValue::String("cloud.example.com".into()),
            ),
            ("enable_redis".into(), YamlValue::Bool(false)),
        ]);

        let context = build_context(&recipe, &values).unwrap();

        assert!(!condition_matches(Some("enable_redis == true"), &context).unwrap());
        assert!(condition_matches(Some("enable_redis != true"), &context).unwrap());
    }

    #[test]
    fn renders_resource_templates() {
        let templates = tempfile::tempdir().unwrap();
        fs::write(
            templates.path().join("app.container.tera"),
            "[Container]\nContainerName={{ recipe_id }}\nEnvironment=DOMAIN={{ domain }}\n",
        )
        .unwrap();
        fs::write(
            templates.path().join("redis.container.tera"),
            "[Container]\nContainerName={{ recipe_id }}-redis\n",
        )
        .unwrap();

        let values = BTreeMap::from([(
            "domain".into(),
            YamlValue::String("cloud.example.com".into()),
        )]);

        let rendered = render_recipe(&nextcloud_recipe(), &values, templates.path()).unwrap();

        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].filename, "nextcloud-app.container");
        assert!(rendered[0].contents.contains("DOMAIN=cloud.example.com"));
        assert_eq!(rendered[1].filename, "nextcloud-redis.container");
    }
}
