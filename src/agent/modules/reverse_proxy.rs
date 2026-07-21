use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload, run_command_or_dry_run,
        safe_join, unsupported_action,
    },
};

pub struct ReverseProxyModule;

const INFO: ModuleInfo = ModuleInfo {
    name: "reverse_proxy",
    feature: "reverse-proxy",
    description: "Manage Caddy reverse proxy site snippets for hosted services.",
    status: ModuleStatus::Available,
    actions: &[
        "capabilities",
        "plan",
        "list",
        "render",
        "write",
        "delete",
        "reload",
    ],
};

const DEFAULT_CONFIG_DIR: &str = "/etc/caddy/tetra-sites";
const MANAGED_HEADER: &str = "# Managed by Tetra reverse_proxy";

#[derive(Debug, Deserialize)]
struct BasePayload {
    config_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct SitePayload {
    config_dir: Option<PathBuf>,
    domain: String,
    upstream: String,
    #[serde(default = "default_tls")]
    tls: bool,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    reload: bool,
}

#[derive(Debug, Deserialize)]
struct DeletePayload {
    config_dir: Option<PathBuf>,
    domain: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    reload: bool,
}

#[derive(Debug, Deserialize)]
struct ReloadPayload {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct SiteMetadata {
    domain: String,
    upstream: String,
    tls: bool,
}

impl AgentModule for ReverseProxyModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value) -> Result<Value> {
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => {
                let payload: BasePayload = parse_payload(payload)?;
                let config_dir = config_dir(payload.config_dir);
                Ok(json!({ "config_dir": config_dir, "sites": list_sites(&config_dir)? }))
            }
            "render" => {
                let payload: SitePayload = parse_payload(payload)?;
                let site = validated_site(payload.domain, payload.upstream, payload.tls)?;
                Ok(json!({
                    "filename": site_filename(&site.domain),
                    "contents": render_site(&site)?,
                    "site": site,
                }))
            }
            "write" => {
                let payload: SitePayload = parse_payload(payload)?;
                let config_dir = config_dir(payload.config_dir);
                let site = validated_site(payload.domain, payload.upstream, payload.tls)?;
                let filename = site_filename(&site.domain);
                let path = safe_join(&config_dir, &filename)?;
                let contents = render_site(&site)?;

                if !payload.dry_run {
                    fs::create_dir_all(&config_dir)
                        .with_context(|| format!("failed to create `{}`", config_dir.display()))?;
                    fs::write(&path, &contents)
                        .with_context(|| format!("failed to write `{}`", path.display()))?;
                }

                let reload = if payload.reload {
                    Some(reload_caddy(payload.dry_run)?)
                } else {
                    None
                };

                Ok(json!({
                    "config_dir": config_dir,
                    "filename": filename,
                    "path": path,
                    "contents": contents,
                    "site": site,
                    "written": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "reload": reload,
                }))
            }
            "delete" => {
                let payload: DeletePayload = parse_payload(payload)?;
                let config_dir = config_dir(payload.config_dir);
                let domain = validate_domain(&payload.domain)?;
                let filename = site_filename(&domain);
                let path = safe_join(&config_dir, &filename)?;

                if !payload.dry_run && path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("failed to delete `{}`", path.display()))?;
                }

                let reload = if payload.reload {
                    Some(reload_caddy(payload.dry_run)?)
                } else {
                    None
                };

                Ok(json!({
                    "config_dir": config_dir,
                    "filename": filename,
                    "path": path,
                    "deleted": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "reload": reload,
                }))
            }
            "reload" => {
                let payload: ReloadPayload = parse_payload(payload)?;
                reload_caddy(payload.dry_run)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

fn default_tls() -> bool {
    true
}

fn config_dir(path: Option<PathBuf>) -> PathBuf {
    path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_DIR))
}

fn validated_site(domain: String, upstream: String, tls: bool) -> Result<SiteMetadata> {
    Ok(SiteMetadata {
        domain: validate_domain(&domain)?,
        upstream: validate_upstream(&upstream)?,
        tls,
    })
}

fn validate_domain(domain: &str) -> Result<String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        bail!("domain is required");
    }
    if domain.len() > 253
        || domain.contains('/')
        || domain.contains(':')
        || !domain
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        || domain.split('.').any(|part| {
            part.is_empty() || part.starts_with('-') || part.ends_with('-') || part.len() > 63
        })
    {
        bail!("invalid domain `{domain}`");
    }

    Ok(domain)
}

fn validate_upstream(upstream: &str) -> Result<String> {
    let upstream = upstream.trim();
    if upstream.is_empty() {
        bail!("upstream is required");
    }
    if upstream.contains('\n')
        || upstream.contains('\r')
        || upstream.contains('{')
        || upstream.contains('}')
    {
        bail!("invalid upstream `{upstream}`");
    }
    if !upstream.contains(':') {
        bail!("upstream must include a host and port, such as 127.0.0.1:8080");
    }

    Ok(upstream.to_string())
}

fn site_filename(domain: &str) -> String {
    format!(
        "{}.caddy",
        domain.replace('*', "wildcard").replace('.', "_")
    )
}

fn render_site(site: &SiteMetadata) -> Result<String> {
    let metadata = serde_json::to_string(site)?;
    let tls = if site.tls { "" } else { "\n\tauto_https off" };
    Ok(format!(
        "{MANAGED_HEADER}\n# tetra: {metadata}\n{} {{{tls}\n\treverse_proxy {}\n}}\n",
        site.domain, site.upstream
    ))
}

fn list_sites(config_dir: &PathBuf) -> Result<Vec<Value>> {
    let mut sites = Vec::new();
    if !config_dir.exists() {
        return Ok(sites);
    }

    for entry in fs::read_dir(config_dir)
        .with_context(|| format!("failed to read `{}`", config_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("caddy") {
            continue;
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        if !contents.starts_with(MANAGED_HEADER) {
            continue;
        }

        let metadata = contents
            .lines()
            .find_map(|line| line.strip_prefix("# tetra: "))
            .and_then(|raw| serde_json::from_str::<SiteMetadata>(raw).ok());
        if let Some(site) = metadata {
            sites.push(json!({
                "filename": path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                "path": path,
                "domain": site.domain,
                "upstream": site.upstream,
                "tls": site.tls,
            }));
        }
    }

    sites.sort_by(|left, right| left["domain"].as_str().cmp(&right["domain"].as_str()));
    Ok(sites)
}

fn reload_caddy(dry_run: bool) -> Result<Value> {
    run_command_or_dry_run("systemctl", ["reload", "caddy.service"], dry_run)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn renders_caddy_site() {
        let response = ReverseProxyModule
            .handle(
                "render",
                json!({ "domain": "app.example.com", "upstream": "127.0.0.1:8080" }),
            )
            .unwrap();

        assert!(
            response["contents"]
                .as_str()
                .unwrap()
                .contains("app.example.com")
        );
        assert!(
            response["contents"]
                .as_str()
                .unwrap()
                .contains("reverse_proxy 127.0.0.1:8080")
        );
    }

    #[test]
    fn dry_run_write_does_not_create_file() {
        let dir = tempfile::tempdir().unwrap();
        let response = ReverseProxyModule
            .handle(
                "write",
                json!({
                    "config_dir": dir.path(),
                    "domain": "demo.example.com",
                    "upstream": "127.0.0.1:3000",
                    "dry_run": true
                }),
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert!(!dir.path().join("demo_example_com.caddy").exists());
    }
}
