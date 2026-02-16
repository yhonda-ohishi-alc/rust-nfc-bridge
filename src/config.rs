use clap::Parser;
use serde::Deserialize;
use tracing::info;

/// CLI arguments (parsed in console mode; service mode uses config file only)
#[derive(Parser, Debug, Clone)]
#[command(name = "nfc-bridge")]
#[command(about = "NFC reader bridge - broadcasts card UIDs via WebSocket")]
pub struct AppArgs {
    /// Run in console mode instead of Windows Service mode
    #[arg(long, default_value_t = false)]
    pub console: bool,

    /// Path to config file (TOML)
    #[arg(long)]
    pub config: Option<String>,

    /// WebSocket server port (overrides config file)
    #[arg(long)]
    pub port: Option<u16>,

    /// NFC polling interval in milliseconds (overrides config file)
    #[arg(long)]
    pub poll_interval_ms: Option<u64>,

    /// Cooldown period in milliseconds (overrides config file)
    #[arg(long)]
    pub cooldown_ms: Option<u64>,

    /// WebSocket bind address (overrides config file)
    #[arg(long)]
    pub bind_addr: Option<String>,
}

/// Runtime configuration (merged from file defaults + CLI overrides)
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,

    #[serde(default = "default_cooldown")]
    pub cooldown_ms: u64,

    #[serde(default = "default_bind_addr")]
    pub bind_addr: String,

    #[serde(default)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub log_dir: String,
}

fn default_port() -> u16 {
    9876
}
fn default_poll_interval() -> u64 {
    200
}
fn default_cooldown() -> u64 {
    3000
}
fn default_bind_addr() -> String {
    "127.0.0.1".to_string()
}

impl Config {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.bind_addr, self.port)
    }

    /// Load config from file, then apply CLI overrides
    pub fn from_args_and_file(args: &AppArgs) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = if let Some(ref path) = args.config {
            let content = std::fs::read_to_string(path)?;
            info!("Loaded config from {}", path);
            toml::from_str(&content)?
        } else {
            Self::load_default_locations()?
        };

        // CLI args override file values
        if let Some(port) = args.port {
            config.port = port;
        }
        if let Some(ms) = args.poll_interval_ms {
            config.poll_interval_ms = ms;
        }
        if let Some(ms) = args.cooldown_ms {
            config.cooldown_ms = ms;
        }
        if let Some(ref addr) = args.bind_addr {
            config.bind_addr = addr.clone();
        }

        Ok(config)
    }

    /// Load from standard locations (service mode uses this)
    pub fn load_default_locations() -> Result<Self, Box<dyn std::error::Error>> {
        // Check next to executable first
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let exe_config = exe_dir.join("nfc-bridge.toml");
                if exe_config.exists() {
                    let content = std::fs::read_to_string(&exe_config)?;
                    info!("Loaded config from {}", exe_config.display());
                    return Ok(toml::from_str(&content)?);
                }
            }
        }

        // Fall back to defaults
        Ok(Config {
            port: default_port(),
            poll_interval_ms: default_poll_interval(),
            cooldown_ms: default_cooldown(),
            bind_addr: default_bind_addr(),
            log_dir: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = Config::load_default_locations().unwrap();
        assert_eq!(config.port, 9876);
        assert_eq!(config.poll_interval_ms, 200);
        assert_eq!(config.cooldown_ms, 3000);
        assert_eq!(config.bind_addr, "127.0.0.1");
        assert_eq!(config.addr(), "127.0.0.1:9876");
    }

    #[test]
    fn config_from_toml() {
        let toml_str = r#"
            port = 8080
            bind_addr = "0.0.0.0"
            poll_interval_ms = 100
            cooldown_ms = 5000
        "#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.port, 8080);
        assert_eq!(config.bind_addr, "0.0.0.0");
        assert_eq!(config.poll_interval_ms, 100);
        assert_eq!(config.cooldown_ms, 5000);
    }

    #[test]
    fn config_defaults_from_empty_toml() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.port, 9876);
        assert_eq!(config.bind_addr, "127.0.0.1");
        assert_eq!(config.poll_interval_ms, 200);
        assert_eq!(config.cooldown_ms, 3000);
    }

    #[test]
    fn cli_overrides_file_config() {
        let args = AppArgs::parse_from([
            "test",
            "--console",
            "--port",
            "8080",
            "--bind-addr",
            "0.0.0.0",
        ]);
        assert!(args.console);
        assert_eq!(args.port, Some(8080));
        assert_eq!(args.bind_addr, Some("0.0.0.0".to_string()));
        assert_eq!(args.poll_interval_ms, None);
    }
}
