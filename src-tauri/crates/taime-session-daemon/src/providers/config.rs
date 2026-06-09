//! TOML-backed provider defaults (`~/.taime/providers.toml`) over a built-in
//! baseline, plus the gemini `settings.json` and grok `config.toml`
//! mutate/restore helpers.
//!
//! The TOML holds only the **data** worth overriding per machine: the binary
//! (path or PATH name), the static base args, the model flag, and any extra env.
//! Behavior (command shape, MCP mechanism, status regexes) is code in the
//! adapters. User entries override built-ins per provider id; unknown keys are
//! ignored so the file can carry forward across daemon versions.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde::Deserialize;

/// Per-provider overridable data.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderDefaults {
    /// Binary to launch (PATH name or absolute path).
    pub binary: String,
    /// Static base args every launch carries (e.g. codex `--no-alt-screen`).
    #[serde(default)]
    pub base_args: Vec<String>,
    /// The flag that precedes a model name (`--model` for all four today).
    #[serde(default = "default_model_flag")]
    pub model_flag: String,
    /// Extra env applied to the child (rarely needed; e.g. forcing a config dir).
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_model_flag() -> String {
    "--model".to_string()
}

impl ProviderDefaults {
    /// A hard fallback for an id with no built-in/user entry: binary == id with a
    /// reasonable model flag. Keeps the registry total even for typo'd ids (the
    /// registry still gates on known ids before this is reached).
    fn fallback(id: &str) -> ProviderDefaults {
        let binary = match id {
            "claude_code" => "claude",
            "codex" => "codex",
            "gemini_cli" => "gemini",
            "grok_cli" => "grok",
            other => other,
        }
        .to_string();
        ProviderDefaults {
            binary,
            base_args: Vec::new(),
            model_flag: default_model_flag(),
            env: HashMap::new(),
        }
    }
}

/// The whole `[providers.*]` table.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default)]
    providers: HashMap<String, ProviderDefaults>,
}

/// Built-in defaults — the source of truth when no user file is present. Kept as
/// a parsed const so adding a provider is a one-line table edit.
const BUILTIN_TOML: &str = r#"
[providers.claude_code]
# Permission flag (--dangerously-skip-permissions / --permission-mode) is dynamic
# in the adapter, so base_args is empty here.
binary = "claude"
base_args = []
model_flag = "--model"

[providers.codex]
binary = "codex"
# --no-alt-screen: run inline (scrollback = history). --disable shell_snapshot:
# avoid SIGTTIN in a non-interactive PTY. The --yolo/--profile choice is dynamic.
base_args = ["--no-alt-screen", "--disable", "shell_snapshot"]
model_flag = "--model"

[providers.gemini_cli]
binary = "gemini"
base_args = ["--yolo", "--sandbox", "false"]
model_flag = "--model"

[providers.grok_cli]
binary = "grok"
base_args = ["--always-approve"]
model_flag = "--model"
"#;

impl ProvidersConfig {
    /// Parse the built-in defaults (infallible: the const is tested).
    pub fn builtin() -> ProvidersConfig {
        toml::from_str(BUILTIN_TOML).expect("built-in providers.toml parses")
    }

    /// Built-ins, overlaid with `~/.taime/providers.toml` if present + parseable.
    /// A malformed user file is ignored (logged) rather than failing every spawn.
    pub fn load() -> ProvidersConfig {
        let mut cfg = ProvidersConfig::builtin();
        if let Some(home) = dirs::home_dir() {
            let path = home.join(".taime").join("providers.toml");
            if let Ok(text) = std::fs::read_to_string(&path) {
                match toml::from_str::<ProvidersConfig>(&text) {
                    Ok(user) => {
                        for (id, defaults) in user.providers {
                            cfg.providers.insert(id, defaults);
                        }
                    }
                    Err(e) => {
                        eprintln!("[taime-daemon] ignoring malformed providers.toml: {e}");
                    }
                }
            }
        }
        cfg
    }

    /// Defaults for `id` (built-in/user entry, else a binary==id fallback).
    pub fn get(&self, id: &str) -> ProviderDefaults {
        self.providers
            .get(id)
            .cloned()
            .unwrap_or_else(|| ProviderDefaults::fallback(id))
    }
}

/// Hold an exclusive advisory lock on a settings file for the duration of a
/// read-modify-write. Two gemini agents spawning at once share `~/.gemini/
/// settings.json`, so an unlocked read-modify-write (or a spawn racing a
/// cleanup) would clobber each other's `mcpServers` entries. The lock lives on a
/// sibling `.taime-settings.lock` and releases when this guard drops.
struct SettingsLock {
    #[cfg(unix)]
    _file: std::fs::File,
}

fn lock_settings(path: &Path) -> io::Result<SettingsLock> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = path.with_file_name(".taime-settings.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        // Blocking exclusive lock so concurrent spawns serialize; released when
        // `file` (and thus the fd) drops.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(SettingsLock { _file: file })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(SettingsLock {})
    }
}

/// Merge an `mcpServers` map into a JSON settings file (gemini), creating the file
/// and parent dir if needed. Existing unrelated keys are preserved. Serialized
/// against concurrent spawns/cleanups via [`lock_settings`].
pub fn merge_json_mcp_servers(
    path: &Path,
    servers: &[(String, serde_json::Value)],
) -> io::Result<()> {
    let _lock = lock_settings(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut root: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().expect("object");
    let mcp = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !mcp.is_object() {
        *mcp = serde_json::json!({});
    }
    let mcp_obj = mcp.as_object_mut().expect("mcpServers object");
    for (name, value) in servers {
        mcp_obj.insert(name.clone(), value.clone());
    }
    let pretty = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, pretty)
}

/// Remove named keys from `mcpServers` in a JSON settings file; drop the
/// `mcpServers` object if it becomes empty. No-op if the file is gone. (Inverse of
/// [`merge_json_mcp_servers`] — the gemini cleanup.)
pub fn remove_json_mcp_servers(path: &Path, names: &[String]) -> io::Result<()> {
    let _lock = lock_settings(path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let mut root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    if let Some(obj) = root.as_object_mut() {
        let now_empty = if let Some(mcp) = obj.get_mut("mcpServers").and_then(|m| m.as_object_mut()) {
            for n in names {
                mcp.remove(n);
            }
            mcp.is_empty()
        } else {
            false
        };
        if now_empty {
            obj.remove("mcpServers");
        }
    }
    let pretty = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, pretty)
}

/// Merge `[mcp_servers.<name>]` sections into a TOML config file (grok
/// `~/.grok/config.toml`), creating the file and parent dir if needed. Edited
/// with `toml_edit` so the user's existing keys, comments, and formatting
/// survive. Unlike the JSON twin, an UNPARSEABLE existing file is an error,
/// not a clobber — config.toml carries user models/API keys, so we must never
/// rewrite it from scratch. Serialized via [`lock_settings`].
pub fn merge_toml_mcp_servers(
    path: &Path,
    servers: &[taime_protocol::McpServerConfig],
) -> io::Result<()> {
    use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};
    let _lock = lock_settings(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{path:?}: {e}")))?;
    if !doc.contains_key("mcp_servers") || !doc["mcp_servers"].is_table() {
        let mut t = Table::new();
        // Implicit: render only the `[mcp_servers.<name>]` child headers (the
        // shape `grok mcp add` writes), not a bare `[mcp_servers]`.
        t.set_implicit(true);
        doc.insert("mcp_servers", Item::Table(t));
    }
    let mcp = doc["mcp_servers"].as_table_mut().expect("mcp_servers table");
    for s in servers {
        let mut t = Table::new();
        t.insert("command", toml_edit::value(s.command.clone()));
        let mut args = Array::new();
        for a in &s.args {
            args.push(a.clone());
        }
        t.insert("args", toml_edit::value(args));
        let mut env = InlineTable::new();
        for (k, v) in &s.env {
            env.insert(k, Value::from(v.clone()));
        }
        t.insert("env", toml_edit::value(env));
        mcp.insert(&s.name, Item::Table(t));
    }
    std::fs::write(path, doc.to_string())
}

/// Remove named `[mcp_servers.<name>]` sections from a TOML config file; drop
/// the parent table if it becomes empty. No-op if the file is gone or
/// unparseable. (Inverse of [`merge_toml_mcp_servers`] — the grok cleanup.)
pub fn remove_toml_mcp_servers(path: &Path, names: &[String]) -> io::Result<()> {
    use toml_edit::DocumentMut;
    let _lock = lock_settings(path)?;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let mut doc: DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };
    let now_empty = if let Some(mcp) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) {
        for n in names {
            mcp.remove(n);
        }
        mcp.is_empty()
    } else {
        false
    };
    if now_empty {
        doc.remove("mcp_servers");
    }
    std::fs::write(path, doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_the_four_providers() {
        let cfg = ProvidersConfig::builtin();
        for id in ["claude_code", "codex", "gemini_cli", "grok_cli"] {
            let d = cfg.get(id);
            assert!(!d.binary.is_empty(), "{id} has a binary");
            assert_eq!(d.model_flag, "--model");
        }
        assert_eq!(
            cfg.get("codex").base_args,
            vec!["--no-alt-screen", "--disable", "shell_snapshot"]
        );
    }

    #[test]
    fn unknown_id_falls_back_to_binary_named_after_id() {
        let cfg = ProvidersConfig::builtin();
        assert_eq!(cfg.get("zzz").binary, "zzz");
    }

    #[test]
    fn merge_then_remove_roundtrips_gemini_settings() {
        let dir = std::env::temp_dir().join(format!("taime-cfg-test-{}", unsafe { libc::getpid() }));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // Pre-existing unrelated key must survive.
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();

        let servers = vec![(
            "cao".to_string(),
            serde_json::json!({"command":"cao-mcp-server","args":[],"env":{"CAO_TERMINAL_ID":"t1"}}),
        )];
        merge_json_mcp_servers(&path, &servers).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["mcpServers"]["cao"]["command"], "cao-mcp-server");

        remove_json_mcp_servers(&path, &["cao".to_string()]).unwrap();
        let restored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored["theme"], "dark");
        assert!(restored.get("mcpServers").is_none(), "empty mcpServers removed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_then_remove_roundtrips_grok_config_toml() {
        let dir = std::env::temp_dir().join(format!("taime-cfg-toml-{}", unsafe { libc::getpid() }));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // User content — including a comment — must survive byte-for-byte.
        let user = "# my settings\n[ui]\nyolo = false # keep\n";
        std::fs::write(&path, user).unwrap();

        let servers = vec![taime_protocol::McpServerConfig {
            name: "taime".into(),
            command: "/bin/taime-session-daemon".into(),
            args: vec!["--mcp-stdio".into()],
            env: vec![
                ("TAIME_MCP_TOKEN".into(), "tok".into()),
                ("CAO_TERMINAL_ID".into(), "t1".into()),
            ],
        }];
        merge_toml_mcp_servers(&path, &servers).unwrap();
        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(merged.starts_with(user), "user content + formatting preserved:\n{merged}");
        assert!(merged.contains("[mcp_servers.taime]"), "section header shape:\n{merged}");
        assert!(merged.contains("command = \"/bin/taime-session-daemon\""));
        assert!(merged.contains("args = [\"--mcp-stdio\"]"));
        assert!(merged.contains("TAIME_MCP_TOKEN = \"tok\""));
        assert!(merged.contains("CAO_TERMINAL_ID = \"t1\""));

        remove_toml_mcp_servers(&path, &["taime".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), user, "restored exactly");

        // An unparseable config must error, never be clobbered.
        std::fs::write(&path, "this is [not toml").unwrap();
        assert!(merge_toml_mcp_servers(&path, &servers).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is [not toml");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
