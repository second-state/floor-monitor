use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub vlm: VlmConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub asr: AsrConfig,
    #[serde(default)]
    pub llm: LlmConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VlmConfig {
    /// API endpoint URL. Supports OpenAI-compatible (`/v1/chat/completions`)
    /// and Ollama native (`/api/generate`). Auto-detected from URL path.
    pub api_url: String,
    /// Optional API key for authenticated endpoints (OpenAI, cloud providers).
    pub api_key: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TelegramConfig {
    pub bot_token: Option<String>,
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MonitorConfig {
    #[serde(default = "default_profile")]
    pub default_profile: String,
    #[serde(default = "default_summary_window")]
    pub summary_window_min: u32,
    #[serde(default = "default_alert_consecutive")]
    pub alert_consecutive: u32,
    #[serde(default = "default_alert_cooldown")]
    pub alert_cooldown_sec: f64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            default_profile: default_profile(),
            summary_window_min: default_summary_window(),
            alert_consecutive: default_alert_consecutive(),
            alert_cooldown_sec: default_alert_cooldown(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AsrConfig {
    /// Whisper-compatible ASR endpoint (e.g. /v1/audio/transcriptions).
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_asr_model")]
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LlmConfig {
    /// OpenAI-compatible chat completions endpoint for intent classification.
    pub api_url: Option<String>,
    pub api_key: Option<String>,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_llm_temperature")]
    pub temperature: f32,
}

fn default_asr_model() -> String {
    "whisper-1".to_string()
}
fn default_llm_model() -> String {
    "qwen2.5:3b".to_string()
}
fn default_llm_max_tokens() -> u32 {
    150
}
fn default_llm_temperature() -> f32 {
    0.0
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3456
}
fn default_model() -> String {
    "qwen2.5-vl:3b".to_string()
}
fn default_max_tokens() -> u32 {
    200
}
fn default_temperature() -> f32 {
    0.1
}
fn default_profile() -> String {
    "kid".to_string()
}
fn default_summary_window() -> u32 {
    30
}
fn default_alert_consecutive() -> u32 {
    2
}
fn default_alert_cooldown() -> f64 {
    120.0
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
