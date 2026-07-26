//! Caddy reverse proxy site management.
//!
//! Manages per-site Caddy snippet files under a managed directory so the
//! Ultramarine Server dashboard can provision reverse proxies for hosted
//! services (e.g. point `app.example.com` at a container's `127.0.0.1:8080`)
//! without editing the main Caddyfile by hand. Each site is a small
//! `*.caddy` file with a `reverse_proxy` block; the main Caddyfile is
//! expected to `import` the managed directory.
//!
//! Registered behind the `reverse-proxy` cargo feature. Mutating actions
//! (`write`, `delete`) accept `dry_run` and an optional `reload` flag that
//! triggers `systemctl reload caddy.service` after the change.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{
    AgentModule,
    module_support::{
        ModuleInfo, ModuleStatus, handle_metadata, parse_payload, safe_join, unsupported_action,
    },
};

/// Agent module that manages Caddy reverse proxy snippets.
///
/// Stateless: all persisted state lives as `*.caddy` files under the
/// configured `config_dir`, with site metadata embedded in a header comment
/// so the module can list and round-trip sites without a separate database.
pub struct ReverseProxyModule;

/// Static descriptor published via the shared `capabilities` and `plan`
/// actions; one source of truth for the module's name, feature gate, and
/// supported actions.
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
    privileged_actions: &["write", "delete", "reload"],
};

/// Directory where managed site snippets live. Overridable per request via
/// `config_dir` (used by tests and deployments that keep Caddy config
/// elsewhere). The main Caddyfile is expected to `import` this directory.
const DEFAULT_CONFIG_DIR: &str = "/etc/caddy/tetra-sites";
/// Sentinel first line written to every managed file. `list_sites` uses this
/// to distinguish Tetra-managed snippets from any other `*.caddy` files the
/// operator may have placed in the same directory, so we never touch files
/// we didn't write.
const MANAGED_HEADER: &str = "# Managed by Tetra reverse_proxy";

#[derive(Debug, Deserialize)]
struct BasePayload {
    config_dir: Option<PathBuf>,
}

/// Payload for `render` and `write`: describes one site to proxy.
///
/// `tls` defaults to `true` because Caddy provisions automatic HTTPS by
/// default; callers opt out for local-only sites or when TLS is terminated
/// upstream.
#[derive(Debug, Deserialize)]
struct SitePayload {
    config_dir: Option<PathBuf>,
    domain: String,
    upstream: String,
    #[serde(default = "default_tls")]
    tls: bool,
    #[serde(default)]
    dry_run: bool,
    /// Reload Caddy after the change. The dashboard typically sets this so a
    /// single command applies the new config; batch operations may prefer to
    /// reload once at the end via the dedicated `reload` action.
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

/// In-memory and on-disk representation of a managed site. Serialized as
/// JSON into a `# tetra: ...` comment header in the snippet so `list_sites`
/// can recover the original parameters without re-parsing the Caddy block.
#[derive(Debug, Serialize, Deserialize)]
struct SiteMetadata {
    domain: String,
    upstream: String,
    tls: bool,
}

/// Dispatches reverse-proxy actions. Mutating actions persist `*.caddy`
/// files under `config_dir` and optionally reload Caddy.
impl AgentModule for ReverseProxyModule {
    fn info(&self) -> ModuleInfo {
        INFO
    }

    fn handle(&self, action: &str, payload: Value, _user: Option<&str>) -> Result<Value> {
        // Answer the shared `capabilities`/`plan` metadata actions first.
        if let Some(response) = handle_metadata(INFO, action, payload.clone())? {
            return Ok(response);
        }

        match action {
            "list" => {
                let payload: BasePayload = parse_payload(payload)?;
                let config_dir = config_dir(payload.config_dir);
                Ok(json!({ "config_dir": config_dir, "sites": list_sites(&config_dir)? }))
            }
            // `render` only produces the snippet text; it does not touch disk.
            // Use `write` to persist (and optionally reload).
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
                // `safe_join` rejects absolute paths and `..` traversal, so
                // even a hostile domain (already blocked by `validate_domain`)
                // cannot escape `config_dir` — defense in depth.
                let path = safe_join(&config_dir, &filename)?;
                let contents = render_site(&site)?;

                if !payload.dry_run {
                    fs::create_dir_all(&config_dir)
                        .with_context(|| format!("failed to create `{}`", config_dir.display()))?;
                    fs::write(&path, &contents)
                        .with_context(|| format!("failed to write `{}`", path.display()))?;
                }

                // Reload only when explicitly requested, so callers can batch
                // writes and reload once at the end.
                // Persisting the site and reloading Caddy are separate outcomes.
                // A container or development userspace may not run systemd even
                // though the managed file was written successfully, so preserve
                // the write result and report reload failure as structured data.
                let (reload, reload_error) = if payload.reload {
                    match reload_caddy(action, payload.dry_run, _user) {
                        Ok(result) => (Some(result), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                } else {
                    (None, None)
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
                    "reload_error": reload_error,
                }))
            }
            "delete" => {
                let payload: DeletePayload = parse_payload(payload)?;
                let config_dir = config_dir(payload.config_dir);
                let domain = validate_domain(&payload.domain)?;
                let filename = site_filename(&domain);
                let path = safe_join(&config_dir, &filename)?;

                // Guard on `path.exists()` so deleting a missing site is a
                // no-op rather than an error.
                if !payload.dry_run && path.exists() {
                    fs::remove_file(&path)
                        .with_context(|| format!("failed to delete `{}`", path.display()))?;
                }

                // As with writes, deletion is successful once the managed file
                // is removed. A missing systemd bus is returned as a reload
                // warning instead of making the persisted deletion look failed.
                let (reload, reload_error) = if payload.reload {
                    match reload_caddy(action, payload.dry_run, _user) {
                        Ok(result) => (Some(result), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                } else {
                    (None, None)
                };

                Ok(json!({
                    "config_dir": config_dir,
                    "filename": filename,
                    "path": path,
                    "deleted": !payload.dry_run,
                    "dry_run": payload.dry_run,
                    "reload": reload,
                    "reload_error": reload_error,
                }))
            }
            "reload" => {
                let payload: ReloadPayload = parse_payload(payload)?;
                reload_caddy(action, payload.dry_run, _user)
            }
            _ => unsupported_action(INFO.name, action),
        }
    }
}

/// Serde default for `tls`. Caddy auto-provisions HTTPS, so TLS is the
/// safe default; callers must explicitly opt out.
const fn default_tls() -> bool {
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

/// Normalize and validate a DNS domain for use as a Caddy site label.
///
/// Lowercases and strips the trailing dot (FQDN form), then enforces RFC
/// length limits and allowed characters. This is primarily a safety check:
/// the domain is interpolated into a Caddy block, so rejecting `/`, `:`,
/// and non-DNS characters prevents Caddy config injection.
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

/// Validate an upstream address such as `127.0.0.1:8080`.
///
/// As with the domain, the upstream is interpolated into a Caddy block, so
/// newlines and `{`/`}` are rejected to prevent block injection. Requiring
/// a `:` catches the common mistake of omitting the port.
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

    Ok(upstream.to_owned())
}

/// Map a (validated) domain to a safe on-disk snippet filename.
///
/// `*` (wildcard site labels) becomes `wildcard` and `.` becomes `_`, so
/// `*.example.com` -> `wildcard_example_com.caddy`. The domain is already
/// validated to contain only `[a-z0-9-.]`, so no further escaping is needed.
fn site_filename(domain: &str) -> String {
    format!(
        "{}.caddy",
        domain.replace('*', "wildcard").replace('.', "_")
    )
}

/// Render a single site snippet.
///
/// The `# tetra: {metadata}` line embeds `SiteMetadata` as JSON so
/// `list_sites` can round-trip the original parameters. TLS off emits
/// `auto_https off` to disable Caddy's automatic certificate provisioning
/// for this site.
fn render_site(site: &SiteMetadata) -> Result<String> {
    let metadata = serde_json::to_string(site)?;
    let tls = if site.tls { "" } else { "\n\tauto_https off" };
    Ok(format!(
        "{MANAGED_HEADER}\n# tetra: {metadata}\n{} {{{tls}\n\treverse_proxy {}\n}}\n",
        site.domain, site.upstream
    ))
}

/// List Tetra-managed sites in `config_dir`.
///
/// Skips any `*.caddy` file that doesn't start with `MANAGED_HEADER`, so
/// operator-authored files in the same directory are neither reported nor
/// touched. Metadata is recovered from the `# tetra:` comment line; files
/// without a parseable header are silently ignored. Results are sorted by
/// domain for stable dashboard output.
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

/// Reload Caddy via systemd so new or removed sites take effect without
/// restarting (which would drop active connections). Dry runs report the
/// command that would run without invoking systemctl.
fn reload_caddy(action: &str, dry_run: bool, user: Option<&str>) -> Result<Value> {
    crate::cmd!({ &INFO, action, user } (dry_run) "systemctl" ["reload", "caddy.service"] json)
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
                None,
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
                None,
            )
            .unwrap();

        assert_eq!(response["written"], false);
        assert!(!dir.path().join("demo_example_com.caddy").exists());
    }
}
