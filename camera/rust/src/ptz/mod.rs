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
use tracing::info;

pub mod detect;
pub mod fake;
pub mod noop;
pub mod patrol;
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
    /// strings the server filters on. Pan AND tilt must BOTH be present
    /// to advertise `"ptz"`/`"patrol"`: the server gates all four
    /// directional commands (`pan_left`/`pan_right`/`tilt_up`/`tilt_down`)
    /// behind a single `"ptz"` capability, so a pan-only or tilt-only
    /// device would silently receive commands it can't drive. Zoom-only
    /// cameras also advertise nothing because the server has no zoom
    /// UI/intent today.
    pub fn advertised(&self) -> Vec<String> {
        if self.pan && self.tilt {
            vec!["ptz".to_string(), "patrol".to_string()]
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PtzError {
    #[error("PTZ disabled in config")]
    Disabled,
    #[error("unsupported on this hardware: {0}")]
    Unsupported(&'static str),
    #[error("missing 'direction' parameter")]
    MissingDirection,
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

/// Construct the appropriate [`Ptz`] implementation for this host. On
/// Linux, probes `v4l2-ctl --list-ctrls` for pan/tilt controls and
/// returns a [`v4l2ctl::V4l2CtlPtz`] when something supported is found.
/// Falls back to [`noop::NoopPtz`] on macOS, on Linux without v4l-utils,
/// when detection fails, or when `cfg.enabled = false`.
pub async fn build(
    cfg: &crate::config::PtzConfig,
    camera_cfg: &crate::config::CameraConfig,
) -> Arc<dyn Ptz> {
    #[cfg(target_os = "linux")]
    {
        return build_with_runner(cfg, camera_cfg, v4l2ctl::RealRunner).await;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cfg, camera_cfg);
        Arc::new(noop::NoopPtz)
    }
}

/// Inner of [`build`] with the runner injected, so tests can drive the
/// branches with a [`v4l2ctl::FakeV4l2CtlRunner`] and assert which
/// implementation comes back.
pub async fn build_with_runner<R>(
    cfg: &crate::config::PtzConfig,
    camera_cfg: &crate::config::CameraConfig,
    runner: R,
) -> Arc<dyn Ptz>
where
    R: v4l2ctl::V4l2CtlRunner + 'static,
{
    if !cfg.enabled {
        info!("PTZ disabled in config; using NoopPtz");
        return Arc::new(noop::NoopPtz);
    }
    let device = crate::config::device_path(camera_cfg, cfg);
    let args = ["-d", device.as_str(), "--list-ctrls"];
    match v4l2ctl::V4l2CtlRunner::run(&runner, &args).await {
        Ok(out) => {
            let parsed = detect::parse_list_ctrls(&out);
            let caps = PtzCapabilities::from_controls(&parsed);
            if caps.pan || caps.tilt {
                info!("PTZ: detected on {} (caps {:?})", device, caps);
                return Arc::new(v4l2ctl::V4l2CtlPtz::new(runner, device, &parsed, cfg));
            }
            info!("PTZ: {} has no pan/tilt controls; using NoopPtz", device);
        }
        Err(e) => {
            info!(
                "PTZ: v4l2-ctl probe of {} failed ({}); using NoopPtz",
                device, e
            );
        }
    }
    Arc::new(noop::NoopPtz)
}

/// Dispatch a `ptz` action's `direction` string to the appropriate trait
/// method. Returns a short human-readable success message on `Ok` for the
/// `command_ack` payload.
///
/// Zoom directions are intentionally **not** routed: the issue declares
/// zoom out of scope today (no UI button or LLM intent emits `zoom_in`/
/// `zoom_out`), and `V4l2CtlPtz` doesn't override `Ptz::zoom`, so any
/// `zoom_*` direction would unconditionally fail with `Unsupported`.
/// Treat zoom strings as unknown directions until the server-side
/// vocabulary catches up.
pub async fn execute_ptz(
    ptz: &Arc<dyn Ptz>,
    params: &serde_json::Value,
) -> Result<String, PtzError> {
    let direction = params.get("direction").and_then(|v| v.as_str());
    match direction {
        Some("pan_left") => ptz.pan(PanDir::Left).await.map(|_| "pan_left ok".into()),
        Some("pan_right") => ptz.pan(PanDir::Right).await.map(|_| "pan_right ok".into()),
        Some("tilt_up") => ptz.tilt(TiltDir::Up).await.map(|_| "tilt_up ok".into()),
        Some("tilt_down") => ptz.tilt(TiltDir::Down).await.map(|_| "tilt_down ok".into()),
        Some("home") => ptz.home().await.map(|_| "home ok".into()),
        Some(other) => Err(PtzError::BadDirection(other.to_string())),
        // Field absent or non-string. Distinct error so the ack message
        // is actionable ("missing 'direction' parameter") instead of
        // the confusing "unknown direction ''".
        None => Err(PtzError::MissingDirection),
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
    fn advertised_pan_only_is_empty() {
        // Pan without tilt: server would still send tilt_up/tilt_down
        // because the wire capability is the single "ptz" gate.
        let caps = PtzCapabilities {
            pan: true,
            ..Default::default()
        };
        assert!(caps.advertised().is_empty());
    }

    #[test]
    fn advertised_tilt_only_is_empty() {
        let caps = PtzCapabilities {
            tilt: true,
            ..Default::default()
        };
        assert!(caps.advertised().is_empty());
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

    #[test]
    fn advertised_pan_and_tilt_includes_ptz_and_patrol() {
        let caps = PtzCapabilities {
            pan: true,
            tilt: true,
            ..Default::default()
        };
        assert_eq!(caps.advertised(), vec!["ptz", "patrol"]);
    }
}
