//! Layered configuration resolution for Taime.
//!
//! Resolution order (highest priority wins):
//!   1. `TAIME_API_URL` env var (and `TAIME_EXTERNAL_BACKEND` / `TAIME_BACKEND_CMD`)
//!   2. project-local `./.taimerc`            (JSON)
//!   3. user `~/.taime/config.json`           (JSON)
//!   4. built-in default `http://127.0.0.1:9889`
//!
//! Rust owns this config and is the single source of truth for "where is the
//! backend". The resolved value is handed to React via the `get_api_url`
//! command and injected into the Python sidecar via `CAO_API_HOST/PORT`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 9889;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedConfig {
    pub api_url: String,
    pub ws_url: String,
    pub host: String,
    pub port: u16,
    pub external_backend: bool,
    /// Optional override for the backend launch command (default: `cao-server`).
    /// Not serialized to the frontend.
    #[serde(skip)]
    pub backend_cmd: Option<String>,
    /// Where the effective value came from (diagnostics only).
    pub source: String,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    api_url: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    external_backend: Option<bool>,
    #[serde(default)]
    backend_cmd: Option<String>,
}

/// Parse a `scheme://host:port` (or bare `host:port` / `host`) into (host, port).
fn parse_host_port(url: &str) -> Option<(String, u16)> {
    let rest = url.rsplit("://").next()?; // strip scheme if present
    let authority = rest.split('/').next()?; // drop any path
    if let Some(idx) = authority.rfind(':') {
        let host = &authority[..idx];
        let port = authority[idx + 1..].parse::<u16>().ok()?;
        if host.is_empty() {
            return None;
        }
        Some((host.to_string(), port))
    } else if !authority.is_empty() {
        Some((authority.to_string(), DEFAULT_PORT))
    } else {
        None
    }
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn merge(
    fc: &FileConfig,
    host: &mut String,
    port: &mut u16,
    external: &mut bool,
    backend_cmd: &mut Option<String>,
) {
    if let Some(u) = fc.api_url.as_deref() {
        if let Some((h, p)) = parse_host_port(u) {
            *host = h;
            *port = p;
        }
    }
    if let Some(h) = &fc.host {
        *host = h.clone();
    }
    if let Some(p) = fc.port {
        *port = p;
    }
    if let Some(e) = fc.external_backend {
        *external = e;
    }
    if let Some(c) = &fc.backend_cmd {
        *backend_cmd = Some(c.clone());
    }
}

/// Pure resolver — takes the raw inputs so it can be unit-tested.
pub fn resolve_from(
    home_json: Option<&str>,
    project_json: Option<&str>,
    env: &HashMap<String, String>,
) -> ResolvedConfig {
    let mut host = DEFAULT_HOST.to_string();
    let mut port = DEFAULT_PORT;
    let mut external = false;
    let mut backend_cmd: Option<String> = None;
    let mut source = "default".to_string();

    if let Some(j) = home_json {
        if let Ok(fc) = serde_json::from_str::<FileConfig>(j) {
            merge(&fc, &mut host, &mut port, &mut external, &mut backend_cmd);
            source = "~/.taime/config.json".to_string();
        }
    }
    if let Some(j) = project_json {
        if let Ok(fc) = serde_json::from_str::<FileConfig>(j) {
            merge(&fc, &mut host, &mut port, &mut external, &mut backend_cmd);
            source = "./.taimerc".to_string();
        }
    }

    // Env overrides (highest priority).
    if let Some(u) = env.get("TAIME_API_URL") {
        if let Some((h, p)) = parse_host_port(u) {
            host = h;
            port = p;
            source = "TAIME_API_URL".to_string();
        }
    }
    if let Some(h) = env.get("CAO_API_HOST") {
        host = h.clone();
    }
    if let Some(p) = env.get("CAO_API_PORT") {
        if let Ok(pp) = p.parse::<u16>() {
            port = pp;
        }
    }
    if let Some(e) = env.get("TAIME_EXTERNAL_BACKEND") {
        if truthy(e) {
            external = true;
            source = format!("{source} + TAIME_EXTERNAL_BACKEND");
        }
    }
    if let Some(c) = env.get("TAIME_BACKEND_CMD") {
        backend_cmd = Some(c.clone());
    }

    ResolvedConfig {
        api_url: format!("http://{host}:{port}"),
        ws_url: format!("ws://{host}:{port}"),
        host,
        port,
        external_backend: external,
        backend_cmd,
        source,
    }
}

/// Read the real environment + config files and resolve.
pub fn resolve() -> ResolvedConfig {
    let home_json = dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".taime").join("config.json")).ok());
    let project_json = std::env::current_dir()
        .ok()
        .and_then(|d| std::fs::read_to_string(d.join(".taimerc")).ok());
    let env: HashMap<String, String> = std::env::vars().collect();
    resolve_from(home_json.as_deref(), project_json.as_deref(), &env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_when_nothing() {
        let c = resolve_from(None, None, &env(&[]));
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 9889);
        assert!(!c.external_backend);
        assert_eq!(c.api_url, "http://127.0.0.1:9889");
        assert_eq!(c.ws_url, "ws://127.0.0.1:9889");
        assert_eq!(c.source, "default");
    }

    #[test]
    fn project_overrides_home() {
        let c = resolve_from(
            Some(r#"{"port": 9000}"#),
            Some(r#"{"port": 9100}"#),
            &env(&[]),
        );
        assert_eq!(c.port, 9100);
        assert_eq!(c.source, "./.taimerc");
    }

    #[test]
    fn env_url_overrides_files() {
        let c = resolve_from(
            Some(r#"{"port": 9000}"#),
            Some(r#"{"port": 9100}"#),
            &env(&[("TAIME_API_URL", "http://127.0.0.1:9222")]),
        );
        assert_eq!(c.port, 9222);
        assert_eq!(c.source, "TAIME_API_URL");
    }

    #[test]
    fn external_flag_from_env() {
        let c = resolve_from(None, None, &env(&[("TAIME_EXTERNAL_BACKEND", "1")]));
        assert!(c.external_backend);
    }

    #[test]
    fn backend_cmd_override() {
        let c = resolve_from(
            None,
            Some(r#"{"backend_cmd": "uv run cao-server"}"#),
            &env(&[]),
        );
        assert_eq!(c.backend_cmd.as_deref(), Some("uv run cao-server"));
    }

    #[test]
    fn parse_variants() {
        assert_eq!(
            parse_host_port("http://localhost:1234"),
            Some(("localhost".to_string(), 1234))
        );
        assert_eq!(
            parse_host_port("127.0.0.1:8080"),
            Some(("127.0.0.1".to_string(), 8080))
        );
        assert_eq!(
            parse_host_port("example.com"),
            Some(("example.com".to_string(), 9889))
        );
        assert_eq!(parse_host_port(""), None);
    }
}
