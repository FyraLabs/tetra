use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::AgentModule;

pub struct FileModule;

#[derive(Debug, Deserialize)]
struct ReadPayload {
    path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct WritePayload {
    path: PathBuf,
    contents: String,
}

impl AgentModule for FileModule {
    fn name(&self) -> &'static str {
        "files"
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        match action {
            "read" => {
                let payload: ReadPayload = serde_json::from_value(payload)?;
                let contents = fs::read_to_string(&payload.path)
                    .with_context(|| format!("failed to read `{}`", payload.path.display()))?;
                Ok(json!({ "path": payload.path, "contents": contents }))
            }
            "write" => {
                let payload: WritePayload = serde_json::from_value(payload)?;
                fs::write(&payload.path, payload.contents)
                    .with_context(|| format!("failed to write `{}`", payload.path.display()))?;
                Ok(json!({ "path": payload.path, "written": true }))
            }
            _ => bail!("unsupported files action `{action}`"),
        }
    }
}
