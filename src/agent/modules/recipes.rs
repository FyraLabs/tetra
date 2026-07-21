//! Recipes agent module.
//!
//! Exposes Tetra's recipe catalog over the agent protocol so the
//! Ultramarine Server dashboard can render recipes on a remote host without
//! baking the recipe files into the agent. Recipes are YAML documents that
//! declare metadata, parameters, requirements, and Tera-templated resources
//! (Quadlet units plus companion files); see `crate::catalog` for the schema
//! and rendering engine.
//!
//! Actions come in two flavors:
//! - File-based (`render`, `context`): the recipe and templates already live
//!   on the host filesystem, addressed by path.
//! - Inline (`render_inline`, `context_inline`): the dashboard ships the
//!   recipe body, templates, and values in the command itself. This lets a
//!   remote catalog publish new recipes without bumping the installed Tetra
//!   version, as long as the recipe schema is one the agent understands.

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

/// Agent module that wraps the recipe catalog for remote rendering.
///
/// Registered behind the `recipes` cargo feature. The module holds no state;
/// all parsing and rendering is delegated to `crate::catalog`.
pub struct RecipeModule;

/// Static descriptor published via the shared `capabilities` and `plan`
/// actions. Kept as a `const` so the dispatcher and callers share one source
/// of truth for the module's name, feature gate, and supported actions.
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

/// Payload for `render`: render a recipe already on the host's filesystem.
///
/// `values_path` and `output_dir` are optional. Omitting `output_dir` (or
/// setting `dry_run`) returns rendered resources without writing to disk,
/// which is useful for previewing.
#[derive(Debug, Deserialize)]
struct RenderPayload {
    recipe_path: PathBuf,
    templates_dir: PathBuf,
    values_path: Option<PathBuf>,
    output_dir: Option<PathBuf>,
    #[serde(default)]
    dry_run: bool,
}

/// Payload for `render_inline`: the dashboard ships an entire recipe bundle
/// (recipe body + named templates + values) in one command. Intended for
/// recipes fetched from a remote catalog, so Tetra need not be updated when
/// new recipes are published.
#[derive(Debug, Deserialize)]
struct InlineRenderPayload {
    recipe: String,
    #[serde(default)]
    templates: BTreeMap<String, String>,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

/// Payload for `context`: resolve a recipe's parameter context (defaults,
/// generated values, validation) without rendering templates, so the
/// dashboard can build a parameter form before committing to a render.
#[derive(Debug, Deserialize)]
struct ContextPayload {
    recipe_path: PathBuf,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

/// Inline variant of [`ContextPayload`]: same purpose, but the recipe body
/// is supplied directly rather than read from disk.
#[derive(Debug, Deserialize)]
struct InlineContextPayload {
    recipe: String,
    #[serde(default)]
    values: BTreeMap<String, YamlValue>,
}

/// Dispatches recipe actions to the catalog. See [`crate::catalog`] for the
/// rendering engine shared with the CLI.
impl AgentModule for RecipeModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        // Every module first answers the shared `capabilities`/`plan`
        // metadata actions before dispatching its own action set.
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
                // Echo the parsed recipe alongside the rendered resources so
                // the dashboard can display normalized metadata (id, version,
                // parameters) without re-parsing the YAML it just sent.
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
