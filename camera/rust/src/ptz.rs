//! UVC PTZ control for the Rust camera client.
//!
//! Shells out to `v4l2-ctl` (Linux `v4l-utils`) behind a `CommandRunner` seam so
//! the argv-building and parsing logic is unit-testable without hardware. On
//! non-Linux hosts or when `v4l2-ctl` is missing, `build_ptz` returns `NoopPtz`
//! and `detect_controls` yields an empty set, so no PTZ capability is advertised.

use serde::Deserialize;
use std::collections::HashMap;

/// One V4L2 integer control's range, parsed from `--list-ctrls`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct V4l2Control {
    pub min: i64,
    pub max: i64,
    pub step: i64,
    pub default: i64,
    pub value: i64,
}

/// The pan/tilt/zoom controls a device exposes, keyed by V4L2 control name.
#[derive(Debug, Clone, Default)]
pub struct V4l2Controls {
    pub controls: HashMap<String, V4l2Control>,
}

impl V4l2Controls {
    pub fn has(&self, name: &str) -> bool {
        self.controls.contains_key(name)
    }
    pub fn is_empty(&self) -> bool {
        self.controls.is_empty()
    }
}

/// Parse `v4l2-ctl --list-ctrls` output, keeping only pan/tilt/zoom controls.
///
/// Lines look like:
/// `    pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0`
pub fn parse_v4l2_controls(text: &str) -> V4l2Controls {
    let mut controls = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        let Some(name) = head.split_whitespace().next() else {
            continue;
        };
        if !(name.starts_with("pan_") || name.starts_with("tilt_") || name.starts_with("zoom_")) {
            continue;
        }
        let mut ctrl = V4l2Control::default();
        for tok in tail.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                if let Ok(n) = v.parse::<i64>() {
                    match k {
                        "min" => ctrl.min = n,
                        "max" => ctrl.max = n,
                        "step" => ctrl.step = n,
                        "default" => ctrl.default = n,
                        "value" => ctrl.value = n,
                        _ => {}
                    }
                }
            }
        }
        controls.insert(name.to_string(), ctrl);
    }
    V4l2Controls { controls }
}

/// Map detected controls to advertised capabilities.
pub fn capabilities_from_controls(c: &V4l2Controls) -> Vec<String> {
    let mut caps = Vec::new();
    if c.has("pan_absolute")
        || c.has("pan_relative")
        || c.has("tilt_absolute")
        || c.has("tilt_relative")
    {
        caps.push("ptz".to_string());
        caps.push("patrol".to_string());
    }
    if c.has("zoom_absolute") || c.has("zoom_relative") {
        caps.push("zoom".to_string());
    }
    caps
}

/// Advertised capabilities = configured ∪ detected (configured first, deduped).
pub fn resolve_capabilities(configured: &[String], detected: &[String]) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();
    for c in configured.iter().chain(detected.iter()) {
        if !caps.contains(c) {
            caps.push(c.clone());
        }
    }
    caps
}

/// `[ptz]` config block (shared key names with the Python client).
#[derive(Debug, Clone, Deserialize)]
pub struct PtzConfig {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "default_step_pan")]
    pub step_pan: i64,
    #[serde(default = "default_step_tilt")]
    pub step_tilt: i64,
    #[serde(default = "default_step_zoom")]
    pub step_zoom: i64,
    #[serde(default)]
    pub invert_pan: bool,
    #[serde(default)]
    pub invert_tilt: bool,
    #[serde(default)]
    pub invert_zoom: bool,
    #[serde(default = "default_patrol_steps")]
    pub patrol_steps: u32,
    #[serde(default = "default_patrol_dwell")]
    pub patrol_dwell_sec: f64,
}

fn default_step_pan() -> i64 {
    3600
}
fn default_step_tilt() -> i64 {
    1800
}
fn default_step_zoom() -> i64 {
    50
}
fn default_patrol_steps() -> u32 {
    4
}
fn default_patrol_dwell() -> f64 {
    1.5
}

impl Default for PtzConfig {
    fn default() -> Self {
        PtzConfig {
            device: None,
            step_pan: default_step_pan(),
            step_tilt: default_step_tilt(),
            step_zoom: default_step_zoom(),
            invert_pan: false,
            invert_tilt: false,
            invert_zoom: false,
            patrol_steps: default_patrol_steps(),
            patrol_dwell_sec: default_patrol_dwell(),
        }
    }
}

/// Which physical axis a command targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Pan,
    Tilt,
    Zoom,
}

/// Step direction. `Pos` = pan_right / tilt_up / zoom_in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Neg,
    Pos,
}

/// Map a server `direction` string to (axis, direction).
pub fn parse_direction(direction: &str) -> Option<(Axis, Dir)> {
    match direction {
        "pan_left" => Some((Axis::Pan, Dir::Neg)),
        "pan_right" => Some((Axis::Pan, Dir::Pos)),
        "tilt_down" => Some((Axis::Tilt, Dir::Neg)),
        "tilt_up" => Some((Axis::Tilt, Dir::Pos)),
        "zoom_out" => Some((Axis::Zoom, Dir::Neg)),
        "zoom_in" => Some((Axis::Zoom, Dir::Pos)),
        _ => None,
    }
}

/// A motor controller. `step` moves one increment on an axis.
pub trait Ptz: Send {
    fn step(&mut self, axis: Axis, dir: Dir) -> Result<(), String>;
    fn home(&mut self) -> Result<(), String>;
}

/// Fallback for cameras with no PTZ hardware (also non-Linux). Errors if driven;
/// the server only routes movement to cameras that advertised the capability, so
/// this path is defensive.
pub struct NoopPtz;

impl Ptz for NoopPtz {
    fn step(&mut self, _axis: Axis, _dir: Dir) -> Result<(), String> {
        Err("no PTZ hardware on this client".to_string())
    }
    fn home(&mut self) -> Result<(), String> {
        Err("no PTZ hardware on this client".to_string())
    }
}

/// Seam over the `v4l2-ctl` process so tests can inject a fake.
pub trait CommandRunner: Send {
    /// Run `v4l2-ctl <args>`; return stdout on success or stderr/message on error.
    fn run(&self, args: &[String]) -> Result<String, String>;
}

/// Real runner: synchronously invokes `v4l2-ctl`.
pub struct V4l2CtlRunner;

impl CommandRunner for V4l2CtlRunner {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let output = std::process::Command::new("v4l2-ctl")
            .args(args)
            .output()
            .map_err(|e| format!("failed to run v4l2-ctl: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_PTZ: &str = "
Camera Controls
        pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
       tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
       zoom_absolute 0x009a090d (int)    : min=100 max=400 step=1 default=100 value=100
";
    const ZOOM_ONLY: &str = "
       zoom_absolute 0x009a090d (int)    : min=100 max=500 step=1 default=100 value=100
power_line_frequency 0x00980918 (menu)   : min=0 max=2 default=1 value=1
";
    const BCC950_RELATIVE: &str = "
        pan_relative 0x009a0904 (int)    : min=-1 max=1 step=1 default=0 value=0 flags=write-only
       tilt_relative 0x009a0905 (int)    : min=-1 max=1 step=1 default=0 value=0 flags=write-only
";

    #[test]
    fn parses_absolute_ranges() {
        let c = parse_v4l2_controls(FULL_PTZ);
        assert_eq!(c.controls["pan_absolute"].max, 36000);
        assert_eq!(c.controls["zoom_absolute"].min, 100);
        assert!(!c.has("power_line_frequency"));
    }

    #[test]
    fn ignores_non_ptz_controls() {
        let c = parse_v4l2_controls(ZOOM_ONLY);
        assert!(c.has("zoom_absolute"));
        assert_eq!(c.controls.len(), 1);
    }

    #[test]
    fn full_ptz_advertises_ptz_patrol_zoom() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(FULL_PTZ));
        assert_eq!(caps, vec!["ptz", "patrol", "zoom"]);
    }

    #[test]
    fn zoom_only_advertises_zoom() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(ZOOM_ONLY));
        assert_eq!(caps, vec!["zoom"]);
    }

    #[test]
    fn relative_pan_tilt_advertises_ptz() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(BCC950_RELATIVE));
        assert_eq!(caps, vec!["ptz", "patrol"]);
    }

    #[test]
    fn resolve_is_additive_and_deduped() {
        let detected = vec!["ptz".to_string(), "patrol".to_string()];
        let configured = vec!["zoom".to_string(), "ptz".to_string()];
        assert_eq!(
            resolve_capabilities(&configured, &detected),
            vec!["zoom", "ptz", "patrol"]
        );
        assert_eq!(resolve_capabilities(&[], &detected), vec!["ptz", "patrol"]);
    }

    #[test]
    fn ptz_config_defaults() {
        let c = PtzConfig::default();
        assert_eq!(c.step_pan, 3600);
        assert_eq!(c.patrol_steps, 4);
        assert!((c.patrol_dwell_sec - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn direction_maps_to_axis_and_dir() {
        assert_eq!(parse_direction("pan_left"), Some((Axis::Pan, Dir::Neg)));
        assert_eq!(parse_direction("zoom_in"), Some((Axis::Zoom, Dir::Pos)));
        assert_eq!(parse_direction("tilt_up"), Some((Axis::Tilt, Dir::Pos)));
        assert_eq!(parse_direction("bogus"), None);
    }

    #[test]
    fn noop_ptz_reports_unsupported() {
        let mut p = NoopPtz;
        assert!(p.step(Axis::Pan, Dir::Pos).is_err());
        assert!(p.home().is_err());
    }
}
