use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use serde_yaml::Value as YamlValue;

use crate::{
    agent::AgentModule,
    catalog::{self, RenderOptions},
};

pub struct RecipeModule;

#[derive(Debug, Deserialize)]
struct RenderPayload {
    recipe_path: PathBuf,
    templates_dir: PathBuf,
    values_path: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
struct ContextPayload {
    recipe_path: PathBuf,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

impl AgentModule for RecipeModule {
    fn name(&self) -> &'static str {
        "recipes"
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        match action {
            "render" => {
                let payload: RenderPayload = serde_json::from_value(payload)?;
                let resources = catalog::render_from_files(&RenderOptions {
                    recipe_path: payload.recipe_path,
                    values_path: payload.values_path,
                    templates_dir: payload.templates_dir,
                    output_dir: payload.output_dir,
                    dry_run: payload.dry_run,
                })?;
                Ok(json!({ "resources": resources }))
            }
            "context" => {
                let payload: ContextPayload = serde_json::from_value(payload)?;
                let recipe = catalog::load_recipe(payload.recipe_path)?;
                let context = catalog::context_for_agent(&recipe, &payload.values)?;
                Ok(json!({ "context": context }))
            }
            _ => bail!("unsupported recipes action `{action}`"),
        }
    }
}
