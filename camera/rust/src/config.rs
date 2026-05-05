//! TOML configuration shared with the Python camera client (`camera.toml`).
//!
//! The `[ptz]` and `[ptz.patrol]` blocks are Rust-client extensions used to
//! configure the UVC PTZ controller. Both are optional — existing
//! `camera.toml` files without them keep working.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub camera: CameraConfig,
    #[serde(default)]
    pub ptz: PtzConfig,
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

/// PTZ controller configuration. All fields default — an absent
/// `[ptz]` block in `camera.toml` is equivalent to the defaults below.
#[derive(Debug, Clone, Deserialize)]
pub struct PtzConfig {
    /// Master switch. `false` forces `NoopPtz` even if v4l2-ctl detects
    /// motors. Useful for debugging or for hosts where you don't want
    /// the camera to physically move.
    #[serde(default = "default_ptz_enabled")]
    pub enabled: bool,

    /// Override the V4L2 device path. Defaults to `/dev/video{device_index}`.
    #[serde(default)]
    pub device: Option<String>,

    /// Step size for one absolute-mode pan button press, in V4L2 units.
    /// Relative-mode cameras (e.g. BCC950 `pan_relative=±1`) ignore this.
    #[serde(default = "default_pan_step")]
    pub pan_step: i32,

    /// Step size for one absolute-mode tilt button press, in V4L2 units.
    #[serde(default = "default_tilt_step")]
    pub tilt_step: i32,

    /// Flip pan sign if the camera moves the opposite direction from
    /// expected. Reference convention: `pan_left` = negative pan delta.
    #[serde(default)]
    pub invert_pan: bool,

    /// Flip tilt sign. Reference convention: `tilt_up` = positive tilt delta.
    #[serde(default)]
    pub invert_tilt: bool,

    #[serde(default)]
    pub patrol: PatrolConfig,
}

impl Default for PtzConfig {
    fn default() -> Self {
        Self {
            enabled: default_ptz_enabled(),
            device: None,
            pan_step: default_pan_step(),
            tilt_step: default_tilt_step(),
            invert_pan: false,
            invert_tilt: false,
            patrol: PatrolConfig::default(),
        }
    }
}

/// Patrol task configuration. Used by the cancellable patrol routine.
#[derive(Debug, Clone, Deserialize)]
pub struct PatrolConfig {
    /// Number of pan steps per side before reversing.
    #[serde(default = "default_sweep_steps")]
    pub sweep_steps: u32,

    /// Pause at each end of the sweep before reversing, in milliseconds.
    #[serde(default = "default_dwell_ms")]
    pub dwell_ms: u64,

    /// Return to home (0,0 absolute) after the sweep, if the camera
    /// supports absolute pan/tilt controls.
    #[serde(default = "default_return_home")]
    pub return_home: bool,
}

impl Default for PatrolConfig {
    fn default() -> Self {
        Self {
            sweep_steps: default_sweep_steps(),
            dwell_ms: default_dwell_ms(),
            return_home: default_return_home(),
        }
    }
}

fn default_ptz_enabled() -> bool {
    true
}
fn default_pan_step() -> i32 {
    3600
}
fn default_tilt_step() -> i32 {
    1800
}
fn default_sweep_steps() -> u32 {
    3
}
fn default_dwell_ms() -> u64 {
    800
}
fn default_return_home() -> bool {
    true
}

/// Compute the V4L2 device path for this config. Returns the explicit
/// `[ptz] device` override if set, otherwise `/dev/video{device_index}`.
pub fn device_path(camera: &CameraConfig, ptz: &PtzConfig) -> String {
    ptz.device
        .clone()
        .unwrap_or_else(|| format!("/dev/video{}", camera.device_index))
}

pub fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}
