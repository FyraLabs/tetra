use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use serde_yaml::Value as YamlValue;

use crate::{
    agent::{
        AgentModule,
        module_support::{ModuleInfo, ModuleStatus, handle_metadata, unsupported_action},
    },
    catalog::{self, RenderOptions},
};

pub struct RecipeModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "recipes",
    feature: "recipes",
    description: "Render app recipes into Quadlet resources and expose template context.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "render",
        "render_inline",
        "context",
        "context_inline",
    ],
};

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
struct InlineRenderPayload {
    recipe: String,
    #[serde(default)]
    templates: BTreeMap<String, String>,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

#[derive(Debug, Deserialize)]
struct ContextPayload {
    recipe_path: PathBuf,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

#[derive(Debug, Deserialize)]
struct InlineContextPayload {
    recipe: String,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

impl AgentModule for RecipeModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

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
            "render_inline" => {
                let payload: InlineRenderPayload = serde_json::from_value(payload)?;
                let recipe = catalog::load_recipe_from_str(&payload.recipe)?;
                let resources = catalog::render_recipe_with_templates(
                    &recipe,
                    &payload.values,
                    &payload.templates,
                )?;
                Ok(json!({ "recipe": recipe, "resources": resources }))
            }
            "context" => {
                let payload: ContextPayload = serde_json::from_value(payload)?;
                let recipe = catalog::load_recipe(payload.recipe_path)?;
                let context = catalog::context_for_agent(&recipe, &payload.values)?;
                Ok(json!({ "context": context }))
            }
            "context_inline" => {
                let payload: InlineContextPayload = serde_json::from_value(payload)?;
                let recipe = catalog::load_recipe_from_str(&payload.recipe)?;
                let context = catalog::context_for_agent(&recipe, &payload.values)?;
                Ok(json!({ "recipe": recipe, "context": context }))
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}
