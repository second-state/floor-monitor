//! TOML configuration shared with the Python camera client (`camera.toml`).

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub camera: CameraConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub ws_url: String,
}

#[derive(Debug, Deserialize)]
pub struct CameraConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source_type: String,
    #[serde(default)]
    pub device_index: u32,
    #[serde(default = "default_interval")]
    pub interval: f64,
    #[serde(default = "default_max_dim")]
    pub max_dimension: u32,
    #[serde(default = "default_quality")]
    pub jpeg_quality: u8,
    /// Capabilities advertised on registration (e.g. "ptz", "patrol").
    /// The server uses these to decide which cameras can receive movement
    /// commands. Leave empty for fixed cameras with no PTZ hardware.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

fn default_interval() -> f64 {
    2.0
}
fn default_max_dim() -> u32 {
    768
}
fn default_quality() -> u8 {
    85
}

pub fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}
