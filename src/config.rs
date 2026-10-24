use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub language: String,
    pub providers: ProvidersConfig,
    pub poll_interval_secs: u64,
    pub display: DisplayConfig,
    pub position: PositionConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            language: "system".to_string(),
            providers: ProvidersConfig::default(),
            poll_interval_secs: 120,
            display: DisplayConfig::default(),
            position: PositionConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvidersConfig {
    pub chatgpt: ChatGptConfig,
    pub zai: ZaiConfig,
    pub deepseek: DeepSeekConfig,
    pub claude: ClaudeConfig,
    pub antigravity: AntigravityConfig,
    pub copilot: CopilotConfig,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            chatgpt: ChatGptConfig::default(),
            zai: ZaiConfig::default(),
            deepseek: DeepSeekConfig::default(),
            claude: ClaudeConfig::default(),
            antigravity: AntigravityConfig::default(),
            copilot: CopilotConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatGptConfig {
    pub enabled: bool,
    pub auth_path: String,
    pub window: String,
}

impl Default for ChatGptConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_path: String::new(),
            window: "most_constrained".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ZaiConfig {
    pub enabled: bool,
    pub account: String,
    pub show_expiry: bool,
    pub refresh_token: bool,
}

impl Default for ZaiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            account: "auto".to_string(),
            show_expiry: true,
            refresh_token: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DeepSeekConfig {
    pub enabled: bool,
    pub api_key_env: String,
    pub currency: String,
    pub use_platform_token: bool,
}

impl Default for DeepSeekConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            currency: "USD".to_string(),
            use_platform_token: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub enabled: bool,
    pub auth_path: String,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auth_path: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AntigravityConfig {
    pub enabled: bool,
}

impl Default for AntigravityConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CopilotConfig {
    pub enabled: bool,
}

impl Default for CopilotConfig {
    fn default() -> Self {
        Self { enabled: false }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub metric: String,
    pub labels: String,
    pub dock: String,
    pub compact: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            metric: "remaining".to_string(),
            labels: "full".to_string(),
            dock: "taskbar".to_string(),
            compact: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PositionConfig {
    pub taskbar_index: usize,
    pub offset: i32,
}

impl Default for PositionConfig {
    fn default() -> Self {
        Self {
            taskbar_index: 0,
            offset: 0,
        }
    }
}

impl Config {
    pub fn sanitize(&mut self) {
        self.poll_interval_secs = self.poll_interval_secs.clamp(30, 3600);
        self.position.taskbar_index = self.position.taskbar_index.min(8);
        self.position.offset = self.position.offset.max(0);
        if self.display.labels != "short" {
            self.display.labels = "full".to_string();
        }
        if self.display.dock != "floating" {
            self.display.dock = "taskbar".to_string();
        }
        if self.providers.chatgpt.window != "nearest_reset"
            && self.providers.chatgpt.window != "primary"
            && self.providers.chatgpt.window != "secondary"
        {
            self.providers.chatgpt.window = "most_constrained".to_string();
        }
        if self.providers.deepseek.currency.trim().is_empty() {
            self.providers.deepseek.currency = "USD".to_string();
        }
    }

    pub fn deepseek_key(&self) -> Option<String> {
        let env_name = if self.providers.deepseek.api_key_env.trim().is_empty() {
            "DEEPSEEK_API_KEY"
        } else {
            self.providers.deepseek.api_key_env.trim()
        };
        if let Ok(value) = std::env::var(env_name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        None
    }
}

pub fn config_path() -> PathBuf {
    if let Ok(value) = std::env::var("ALM_CONFIG") {
        let value = value.trim();
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let portable = dir.join("config.json");
            if portable.is_file() {
                return portable;
            }
        }
    }

    let appdata = std::env::var("APPDATA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));

    appdata.join("AgentLimitMonitor").join("config.json")
}

pub fn load() -> (Config, PathBuf) {
    let path = config_path();
    let mut config = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(config) => config,
            Err(error) => {
                crate::diagnose::log_error(
                    &format!("unable to parse config {}; using defaults", path.display()),
                    error,
                );
                Config::default()
            }
        },
        Err(_) => Config::default(),
    };
    config.sanitize();
    (config, path)
}

pub fn save(config: &Config, path: &Path) -> PathBuf {
    let Ok(text) = serde_json::to_string_pretty(config) else {
        crate::diagnose::log("unable to serialize config");
        return path.to_path_buf();
    };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(path, &text).is_ok() {
        return path.to_path_buf();
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let fallback = dir.join("config.json");
            if std::fs::write(&fallback, &text).is_ok() {
                crate::diagnose::log(format!(
                    "config path {} was not writable; using {}",
                    path.display(),
                    fallback.display()
                ));
                return fallback;
            }
        }
    }

    crate::diagnose::log_error(
        &format!("unable to save config {}", path.display()),
        std::io::Error::last_os_error(),
    );
    path.to_path_buf()
}
