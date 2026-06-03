use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::{Deserialize, Serialize};

const DEFAULT_CONFIG_FILE: &str = "marsala.toml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub openai: OpenAiConfig,
    pub logging: LoggingConfig,
    pub rewrite: RewriteConfig,
    pub tool_capture: ToolCaptureConfig,
    pub proxy: ProxyConfig,
    pub mitm: MitmConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            openai: OpenAiConfig::default(),
            logging: LoggingConfig::default(),
            rewrite: RewriteConfig::default(),
            tool_capture: ToolCaptureConfig::default(),
            proxy: ProxyConfig::default(),
            mitm: MitmConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let defaults = toml::to_string(&Self::default()).context("failed to serialize defaults")?;
        let mut builder = Config::builder()
            .add_source(File::from_str(&defaults, FileFormat::Toml))
            .add_source(File::from(PathBuf::from(DEFAULT_CONFIG_FILE)).required(false));

        if let Some(path) = explicit_path {
            builder = builder.add_source(File::from(path).required(true));
        }

        builder = builder.add_source(
            Environment::with_prefix("MARSALA")
                .prefix_separator("__")
                .separator("__")
                .list_separator(",")
                .try_parsing(true),
        );

        builder
            .build()
            .context("failed to build configuration")?
            .try_deserialize()
            .context("failed to deserialize configuration")
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to render configuration")
    }

    pub fn to_phase_zero_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(&PhaseZeroConfigView {
            server: &self.server,
            logging: &self.logging,
        })
        .context("failed to render Phase 0 configuration")
    }
}

#[derive(Debug, Serialize)]
struct PhaseZeroConfigView<'a> {
    server: &'a ServerConfig,
    logging: &'a LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8787,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub api_key_env: String,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub enabled: bool,
    pub path: PathBuf,
    pub log_bodies: bool,
    pub redact_secrets: bool,
    pub capture_stream_chunks: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("logs/events.jsonl"),
            log_bodies: true,
            redact_secrets: true,
            capture_stream_chunks: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RewriteConfig {
    pub mode: String,
    pub streaming: String,
}

impl Default for RewriteConfig {
    fn default() -> Self {
        Self {
            mode: "off".to_string(),
            streaming: "passthrough".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolCaptureConfig {
    pub enabled: bool,
}

impl Default for ToolCaptureConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_string(),
            port: 8788,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MitmConfig {
    pub enabled: bool,
    pub default_action: String,
    pub allow_hosts: Vec<String>,
}

impl Default for MitmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_action: "tunnel".to_string(),
            allow_hosts: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct CurrentDirGuard {
        original: PathBuf,
    }

    impl CurrentDirGuard {
        fn change_to(path: &Path) -> Self {
            let original = env::current_dir().expect("current dir");
            env::set_current_dir(path).expect("set current dir");
            Self { original }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            env::set_current_dir(&self.original).expect("restore current dir");
        }
    }

    fn error_chain(error: &anyhow::Error) -> String {
        format!("{error:#}")
    }

    #[test]
    fn default_config_matches_phase_zero_expectations() {
        let config = AppConfig::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8787);
        assert_eq!(config.openai.base_url, "https://api.openai.com/v1");
        assert!(config.logging.enabled);
        assert!(config.logging.log_bodies);
        assert!(config.logging.redact_secrets);
        assert!(config.logging.capture_stream_chunks);
        assert_eq!(config.rewrite.mode, "off");
        assert_eq!(config.rewrite.streaming, "passthrough");
        assert!(!config.proxy.enabled);
        assert!(!config.mitm.enabled);
    }

    #[test]
    fn config_loads_from_file_and_env() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
[server]
host = "127.0.0.1"
port = 9999

[logging]
enabled = true
path = "custom/events.jsonl"
log_bodies = true
redact_secrets = true
capture_stream_chunks = true
"#,
        )
        .expect("write config");

        std::env::set_var("MARSALA__SERVER__HOST", "0.0.0.0");
        std::env::set_var("MARSALA__LOGGING__LOG_BODIES", "false");

        let config = AppConfig::load(Some(&config_path)).expect("load config");

        std::env::remove_var("MARSALA__SERVER__HOST");
        std::env::remove_var("MARSALA__LOGGING__LOG_BODIES");

        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9999);
        assert_eq!(config.logging.path, PathBuf::from("custom/events.jsonl"));
        assert!(!config.logging.log_bodies);
        assert!(config.logging.redact_secrets);
    }

    #[test]
    fn cli_config_selector_env_does_not_feed_app_config_loader() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let _cwd = CurrentDirGuard::change_to(tempdir.path());

        env::set_var("MARSALA_CONFIG", "/tmp/cli-only-config.toml");
        env::set_var("MARSALA__SERVER__PORT", "9797");

        let config = AppConfig::load(None).expect("load config without env collision");

        env::remove_var("MARSALA_CONFIG");
        env::remove_var("MARSALA__SERVER__PORT");

        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9797);
    }

    #[test]
    fn config_rejects_unknown_top_level_key() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
unexpected = true
"#,
        )
        .expect("write config");

        let error =
            AppConfig::load(Some(&config_path)).expect_err("unknown top-level key must fail");
        let message = error_chain(&error);

        assert!(message.contains("unexpected"));
        assert!(message.contains("unknown field"));
    }

    #[test]
    fn config_rejects_unknown_nested_key() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
[logging]
enabled = true
path = "custom/events.jsonl"
log_bodies = true
redact_secrets = true
capture_stream_chunks = true
rotate_daily = true
"#,
        )
        .expect("write config");

        let error = AppConfig::load(Some(&config_path)).expect_err("unknown nested key must fail");
        let message = error_chain(&error);

        assert!(message.contains("rotate_daily"));
        assert!(message.contains("unknown field"));
    }

    #[test]
    fn config_rejects_invalid_value_type() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
[server]
port = "not-a-port"
"#,
        )
        .expect("write config");

        let error = AppConfig::load(Some(&config_path)).expect_err("invalid value type must fail");
        let message = error_chain(&error);

        assert!(message.contains("server.port") || message.contains("port"));
    }

    #[test]
    fn config_rejects_invalid_shape() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
server = "127.0.0.1:8787"
"#,
        )
        .expect("write config");

        let error = AppConfig::load(Some(&config_path)).expect_err("invalid table shape must fail");
        let message = error_chain(&error);

        assert!(message.contains("server"));
    }

    #[test]
    fn phase_zero_render_omits_roadmap_sections() {
        let rendered = AppConfig::default()
            .to_phase_zero_toml_string()
            .expect("render phase zero config");

        assert!(rendered.contains("[server]"));
        assert!(rendered.contains("[logging]"));
        assert!(!rendered.contains("[openai]"));
        assert!(!rendered.contains("[rewrite]"));
        assert!(!rendered.contains("[tool_capture]"));
        assert!(!rendered.contains("[proxy]"));
        assert!(!rendered.contains("[mitm]"));
    }

    #[test]
    fn full_render_keeps_roadmap_sections_available() {
        let rendered = AppConfig::default()
            .to_toml_string()
            .expect("render full config");

        assert!(rendered.contains("[openai]"));
        assert!(rendered.contains("[rewrite]"));
        assert!(rendered.contains("[tool_capture]"));
        assert!(rendered.contains("[proxy]"));
        assert!(rendered.contains("[mitm]"));
    }
}
