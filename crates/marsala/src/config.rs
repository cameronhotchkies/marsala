use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use config::{Config, Environment, File, FileFormat};
use serde::{Deserialize, Serialize};

const DEFAULT_CONFIG_FILE: &str = "marsala.toml";
const ENV_LIST_SEPARATOR: &str = ",";
const MITM_ALLOW_HOSTS_ENV_KEY: &str = "mitm.allow_hosts";

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
                .list_separator(ENV_LIST_SEPARATOR)
                .with_list_parse_key(MITM_ALLOW_HOSTS_ENV_KEY)
                .try_parsing(true),
        );

        let config: Self = builder
            .build()
            .context("failed to build configuration")?
            .try_deserialize()
            .context("failed to deserialize configuration")?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to render configuration")
    }

    pub fn to_active_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(&ActiveConfigView {
            server: &self.server,
            openai: &self.openai,
            logging: &self.logging,
            proxy: &self.proxy,
        })
        .context("failed to render active configuration")
    }

    pub fn validate(&self) -> Result<()> {
        self.mitm.validate(&self.proxy)
    }

    pub fn mitm_connect_action_for_host(&self, host: &str) -> MitmConnectAction {
        self.mitm.connect_action_for_host(&self.proxy, host)
    }
}

#[derive(Debug, Serialize)]
struct ActiveConfigView<'a> {
    server: &'a ServerConfig,
    openai: &'a OpenAiConfig,
    logging: &'a LoggingConfig,
    proxy: &'a ProxyConfig,
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
    pub auth_mode: OpenAiAuthMode,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            auth_mode: OpenAiAuthMode::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiAuthMode {
    ConfiguredApiKey,
    InboundAuthorization,
}

impl Default for OpenAiAuthMode {
    fn default() -> Self {
        Self::ConfiguredApiKey
    }
}

impl OpenAiAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfiguredApiKey => "configured_api_key",
            Self::InboundAuthorization => "inbound_authorization",
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
            log_bodies: false,
            redact_secrets: true,
            capture_stream_chunks: false,
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
    pub ca_cert_path: PathBuf,
    pub ca_key_path: PathBuf,
}

impl Default for MitmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_action: "tunnel".to_string(),
            allow_hosts: Vec::new(),
            ca_cert_path: PathBuf::from("certs/marsala-ca.pem"),
            ca_key_path: PathBuf::from("certs/marsala-ca-key.pem"),
        }
    }
}

impl MitmConfig {
    fn validate(&self, proxy: &ProxyConfig) -> Result<()> {
        if self.default_action != "tunnel" {
            bail!("mitm.default_action currently supports only \"tunnel\"");
        }

        for host in &self.allow_hosts {
            validate_mitm_allow_host(host)?;
        }

        if self.enabled {
            if !proxy.enabled {
                bail!("mitm.enabled=true requires proxy.enabled=true");
            }
            if self.allow_hosts.is_empty() {
                bail!("mitm.enabled=true requires at least one mitm.allow_hosts entry");
            }
            if self.ca_cert_path.as_os_str().is_empty() {
                bail!("mitm.ca_cert_path must not be empty when MITM is enabled");
            }
            if self.ca_key_path.as_os_str().is_empty() {
                bail!("mitm.ca_key_path must not be empty when MITM is enabled");
            }
        }

        Ok(())
    }

    pub fn connect_action_for_host(&self, proxy: &ProxyConfig, host: &str) -> MitmConnectAction {
        if proxy.enabled
            && self.enabled
            && self
                .allow_hosts
                .iter()
                .any(|allow_host| allow_host.as_str() == host)
        {
            MitmConnectAction::Mitm
        } else {
            MitmConnectAction::Tunnel
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MitmConnectAction {
    Mitm,
    Tunnel,
}

fn validate_mitm_allow_host(host: &str) -> Result<()> {
    if host.is_empty() {
        bail!("mitm.allow_hosts entries must not be empty");
    }
    if host != host.trim() {
        bail!("mitm.allow_hosts entries must not contain surrounding whitespace");
    }
    if host.contains("://") || host.contains('/') {
        bail!("mitm.allow_hosts entries must be hostnames, not URLs");
    }
    if host.contains(':') {
        bail!("mitm.allow_hosts entries must not include ports");
    }
    if host.contains('*') {
        bail!("mitm.allow_hosts entries must be exact hosts, not wildcards");
    }
    if host.chars().any(char::is_whitespace) {
        bail!("mitm.allow_hosts entries must not contain whitespace");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const ENV_PREFIX: &str = "MARSALA__";
    const CONFIG_SELECTOR_ENV: &str = "MARSALA_CONFIG";

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

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(OsString, OsString)>,
    }

    impl EnvGuard {
        fn isolate_marsala() -> Self {
            let lock = ENV_LOCK.lock().expect("env lock");
            let saved: Vec<_> = env::vars_os()
                .filter(|(key, _)| is_marsala_env_key(key))
                .collect();

            for (key, _) in &saved {
                env::remove_var(key);
            }

            Self { _lock: lock, saved }
        }

        fn set(&self, key: &str, value: &str) {
            env::set_var(key, value);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let current_keys: Vec<_> = env::vars_os()
                .map(|(key, _)| key)
                .filter(is_marsala_env_key)
                .collect();

            for key in current_keys {
                env::remove_var(key);
            }

            for (key, value) in &self.saved {
                env::set_var(key, value);
            }
        }
    }

    fn is_marsala_env_key(key: &OsString) -> bool {
        let key = key.to_string_lossy();
        key == CONFIG_SELECTOR_ENV || key.starts_with(ENV_PREFIX)
    }

    fn error_chain(error: &anyhow::Error) -> String {
        format!("{error:#}")
    }

    #[test]
    fn default_config_matches_phase_one_expectations() {
        let config = AppConfig::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8787);
        assert_eq!(config.openai.base_url, "https://api.openai.com/v1");
        assert_eq!(config.openai.auth_mode, OpenAiAuthMode::ConfiguredApiKey);
        assert!(config.logging.enabled);
        assert!(!config.logging.log_bodies);
        assert!(config.logging.redact_secrets);
        assert!(!config.logging.capture_stream_chunks);
        assert_eq!(config.rewrite.mode, "off");
        assert_eq!(config.rewrite.streaming, "passthrough");
        assert!(!config.proxy.enabled);
        assert!(!config.mitm.enabled);
    }

    #[test]
    fn config_loads_from_file_and_env() {
        let env_guard = EnvGuard::isolate_marsala();
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

        env_guard.set("MARSALA__SERVER__HOST", "0.0.0.0");
        env_guard.set("MARSALA__LOGGING__LOG_BODIES", "false");

        let config = AppConfig::load(Some(&config_path)).expect("load config");

        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 9999);
        assert_eq!(config.logging.path, PathBuf::from("custom/events.jsonl"));
        assert!(!config.logging.log_bodies);
        assert!(config.logging.redact_secrets);
    }

    #[test]
    fn env_string_override_preserves_scalar_server_host() {
        let env_guard = EnvGuard::isolate_marsala();

        env_guard.set("MARSALA__SERVER__HOST", "0.0.0.0");

        let config = AppConfig::load(None).expect("load config");

        assert_eq!(config.server.host, "0.0.0.0");
    }

    #[test]
    fn env_enum_override_parses_openai_auth_mode() {
        let env_guard = EnvGuard::isolate_marsala();

        env_guard.set("MARSALA__OPENAI__AUTH_MODE", "inbound_authorization");

        let config = AppConfig::load(None).expect("load config");

        assert_eq!(
            config.openai.auth_mode,
            OpenAiAuthMode::InboundAuthorization
        );
    }

    #[test]
    fn env_list_override_parses_mitm_allow_hosts() {
        let env_guard = EnvGuard::isolate_marsala();

        env_guard.set("MARSALA__MITM__ALLOW_HOSTS", "a,b");

        let config = AppConfig::load(None).expect("load config");

        assert_eq!(
            config.mitm.allow_hosts,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn mitm_config_shape_includes_ca_paths() {
        let config = AppConfig::default();

        assert_eq!(
            config.mitm.ca_cert_path,
            PathBuf::from("certs/marsala-ca.pem")
        );
        assert_eq!(
            config.mitm.ca_key_path,
            PathBuf::from("certs/marsala-ca-key.pem")
        );
    }

    #[test]
    fn mitm_enabled_requires_proxy_and_allowlist() {
        let _env_guard = EnvGuard::isolate_marsala();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
[mitm]
enabled = true
allow_hosts = ["api.openai.com"]
"#,
        )
        .expect("write config");

        let error =
            AppConfig::load(Some(&config_path)).expect_err("mitm enabled without proxy must fail");
        let message = error_chain(&error);

        assert!(message.contains("proxy.enabled=true"));
    }

    #[test]
    fn mitm_enabled_requires_non_empty_allowlist() {
        let _env_guard = EnvGuard::isolate_marsala();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let config_path = tempdir.path().join("marsala.toml");
        fs::write(
            &config_path,
            r#"
[proxy]
enabled = true

[mitm]
enabled = true
"#,
        )
        .expect("write config");

        let error =
            AppConfig::load(Some(&config_path)).expect_err("mitm enabled without hosts must fail");
        let message = error_chain(&error);

        assert!(message.contains("mitm.allow_hosts"));
    }

    #[test]
    fn mitm_allow_hosts_are_exact_hostnames() {
        let _env_guard = EnvGuard::isolate_marsala();
        let tempdir = tempfile::tempdir().expect("tempdir");

        for host in [
            "",
            " api.openai.com",
            "https://api.openai.com",
            "api.openai.com:443",
            "*.openai.com",
            "api.openai.com/v1",
        ] {
            let config_path = tempdir.path().join("marsala.toml");
            fs::write(
                &config_path,
                format!(
                    r#"
[mitm]
allow_hosts = ["{host}"]
"#
                ),
            )
            .expect("write config");

            assert!(
                AppConfig::load(Some(&config_path)).is_err(),
                "expected invalid host to fail: {host}"
            );
        }
    }

    #[test]
    fn mitm_connect_action_requires_proxy_enabled_mitm_enabled_and_exact_host() {
        let proxy_enabled = ProxyConfig {
            enabled: true,
            ..ProxyConfig::default()
        };
        let proxy_disabled = ProxyConfig::default();
        let mitm_enabled = MitmConfig {
            enabled: true,
            allow_hosts: vec!["api.openai.com".to_string()],
            ..MitmConfig::default()
        };
        let mitm_disabled = MitmConfig {
            allow_hosts: vec!["api.openai.com".to_string()],
            ..MitmConfig::default()
        };

        assert_eq!(
            mitm_enabled.connect_action_for_host(&proxy_enabled, "api.openai.com"),
            MitmConnectAction::Mitm
        );
        assert_eq!(
            mitm_enabled.connect_action_for_host(&proxy_enabled, "chatgpt.com"),
            MitmConnectAction::Tunnel
        );
        assert_eq!(
            mitm_enabled.connect_action_for_host(&proxy_enabled, "sub.api.openai.com"),
            MitmConnectAction::Tunnel
        );
        assert_eq!(
            mitm_enabled.connect_action_for_host(&proxy_disabled, "api.openai.com"),
            MitmConnectAction::Tunnel
        );
        assert_eq!(
            mitm_disabled.connect_action_for_host(&proxy_enabled, "api.openai.com"),
            MitmConnectAction::Tunnel
        );
    }

    #[test]
    fn cli_config_selector_env_does_not_feed_app_config_loader() {
        let env_guard = EnvGuard::isolate_marsala();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let _cwd = CurrentDirGuard::change_to(tempdir.path());

        env_guard.set("MARSALA_CONFIG", "/tmp/cli-only-config.toml");
        env_guard.set("MARSALA__SERVER__PORT", "9797");

        let config = AppConfig::load(None).expect("load config without env collision");

        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9797);
    }

    #[test]
    fn config_rejects_unknown_top_level_key() {
        let _env_guard = EnvGuard::isolate_marsala();
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
        let _env_guard = EnvGuard::isolate_marsala();
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
        let _env_guard = EnvGuard::isolate_marsala();
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
        let _env_guard = EnvGuard::isolate_marsala();
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
    fn active_render_includes_openai_proxy_and_omits_later_phase_sections() {
        let rendered = AppConfig::default()
            .to_active_toml_string()
            .expect("render active config");

        assert!(rendered.contains("[server]"));
        assert!(rendered.contains("[openai]"));
        assert!(rendered.contains("[logging]"));
        assert!(rendered.contains("[proxy]"));
        assert!(rendered.contains("log_bodies = false"));
        assert!(!rendered.contains("[rewrite]"));
        assert!(!rendered.contains("[tool_capture]"));
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
