//! PTZ control abstractions.
//!
//! The [`Ptz`] trait is the camera client's view of pan-tilt-zoom hardware.
//! Concrete impls live in submodules:
//!
//! - [`noop`] — always-success no-op for cameras without motors (the default).
//! - [`fake`] — call-recording test double.
//! - `v4l2ctl` (added in a later commit) — drives Linux UVC cameras by
//!   shelling out to `v4l2-ctl`.
//!
//! The free function [`execute_ptz`] dispatches a `direction` string from a
//! `params` JSON blob to the appropriate trait method. `handle_command`
//! calls into it; tests can call it directly.

use async_trait::async_trait;
use std::sync::Arc;

pub mod detect;
pub mod fake;
pub mod noop;
pub mod v4l2ctl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanDir {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiltDir {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomDir {
    In,
    Out,
}

/// What the camera can physically do. Constructed at startup either from
/// `v4l2-ctl` capability detection (Linux) or as the all-false default
/// for [`noop::NoopPtz`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PtzCapabilities {
    pub pan: bool,
    pub tilt: bool,
    pub zoom: bool,
    pub home: bool,
}

impl PtzCapabilities {
    /// Translate hardware capabilities into the wire-level capability
    /// strings the server filters on. Pan or tilt presence implies both
    /// `"ptz"` and `"patrol"` (patrol is software-driven on top of pan/tilt).
    /// Zoom-only cameras advertise nothing today because the server's UI
    /// has no zoom buttons or intents yet.
    pub fn advertised(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.pan || self.tilt {
            v.push("ptz".to_string());
            v.push("patrol".to_string());
        }
        v
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PtzError {
    #[error("PTZ disabled in config")]
    Disabled,
    #[error("unsupported on this hardware: {0}")]
    Unsupported(&'static str),
    #[error("unknown direction '{0}'")]
    BadDirection(String),
    #[error("v4l2-ctl failed: {0}")]
    V4l2(String),
    #[error("v4l2-ctl timeout")]
    Timeout,
    #[error("io: {0}")]
    Io(String),
}

#[async_trait]
pub trait Ptz: Send + Sync {
    fn capabilities(&self) -> PtzCapabilities;
    async fn pan(&self, dir: PanDir) -> Result<(), PtzError>;
    async fn tilt(&self, dir: TiltDir) -> Result<(), PtzError>;
    async fn zoom(&self, _dir: ZoomDir) -> Result<(), PtzError> {
        Err(PtzError::Unsupported("zoom"))
    }
    async fn home(&self) -> Result<(), PtzError> {
        Err(PtzError::Unsupported("home"))
    }
}

/// Dispatch a `ptz` action's `direction` string to the appropriate trait
/// method. Returns a short human-readable success message on `Ok` for the
/// `command_ack` payload.
pub async fn execute_ptz(
    ptz: &Arc<dyn Ptz>,
    params: &serde_json::Value,
) -> Result<String, PtzError> {
    let direction = params
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match direction {
        "pan_left" => ptz.pan(PanDir::Left).await.map(|_| "pan_left ok".into()),
        "pan_right" => ptz.pan(PanDir::Right).await.map(|_| "pan_right ok".into()),
        "tilt_up" => ptz.tilt(TiltDir::Up).await.map(|_| "tilt_up ok".into()),
        "tilt_down" => ptz.tilt(TiltDir::Down).await.map(|_| "tilt_down ok".into()),
        "zoom_in" => ptz.zoom(ZoomDir::In).await.map(|_| "zoom_in ok".into()),
        "zoom_out" => ptz.zoom(ZoomDir::Out).await.map(|_| "zoom_out ok".into()),
        "home" => ptz.home().await.map(|_| "home ok".into()),
        other => Err(PtzError::BadDirection(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_empty_when_no_caps() {
        let caps = PtzCapabilities::default();
        assert!(caps.advertised().is_empty());
    }

    #[test]
    fn advertised_pan_only_includes_ptz_and_patrol() {
        let caps = PtzCapabilities {
            pan: true,
            ..Default::default()
        };
        assert_eq!(caps.advertised(), vec!["ptz", "patrol"]);
    }

    #[test]
    fn advertised_tilt_only_includes_ptz_and_patrol() {
        let caps = PtzCapabilities {
            tilt: true,
            ..Default::default()
        };
        assert_eq!(caps.advertised(), vec!["ptz", "patrol"]);
    }

    #[test]
    fn advertised_zoom_only_is_empty() {
        // No zoom UI/intents exist server-side today.
        let caps = PtzCapabilities {
            zoom: true,
            ..Default::default()
        };
        assert!(caps.advertised().is_empty());
    }
}
