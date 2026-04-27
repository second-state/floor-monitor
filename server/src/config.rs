use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub vlm: VlmConfig,
    /// `[llm]` is **required**. Drives intent classification *and* the
    /// periodic summary generation, so a text-completion model is always
    /// needed even when Telegram is not configured.
    pub llm: LlmConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub monitor: MonitorConfig,
    #[serde(default)]
    pub asr: AsrConfig,
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
    /// OpenAI-compatible API endpoint (e.g. `/v1/chat/completions`).
    pub api_url: String,
    /// Optional API key for authenticated endpoints (OpenAI, cloud providers).
    pub api_key: Option<String>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TelegramConfig {
    pub bot_token: Option<String>,
    /// Single chat ID (for backward compatibility). Use `chat_ids` for multiple.
    pub chat_id: Option<String>,
    /// List of chat IDs to send alerts/summaries to and accept messages from.
    #[serde(default)]
    pub chat_ids: Vec<String>,
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
    /// Number of recent per-frame scene descriptions injected as context
    /// when answering visual questions from the web UI or Telegram.
    #[serde(default = "default_context_window_frames")]
    pub context_window_frames: u32,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            default_profile: default_profile(),
            summary_window_min: default_summary_window(),
            alert_consecutive: default_alert_consecutive(),
            alert_cooldown_sec: default_alert_cooldown(),
            context_window_frames: default_context_window_frames(),
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

#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    /// OpenAI-compatible chat completions endpoint. **Required.** Used for
    /// both intent classification (Telegram) and periodic activity summaries.
    pub api_url: String,
    pub api_key: Option<String>,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_max_tokens")]
    pub max_tokens: u32,
    pub temperature: Option<f32>,
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
fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3456
}
fn default_model() -> String {
    "Qwen/Qwen2.5-VL-3B-Instruct".to_string()
}
fn default_max_tokens() -> u32 {
    200
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
fn default_context_window_frames() -> u32 {
    30
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        if config.llm.api_url.trim().is_empty() {
            return Err("[llm].api_url must be set (the LLM is used for both \
                        intent classification and periodic summaries)"
                .into());
        }
        Ok(config)
    }
}
