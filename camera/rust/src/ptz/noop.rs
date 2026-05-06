//! `NoopPtz` — the always-success no-op used for cameras without motors
//! (e.g. Mac built-in webcam) and as the macOS / disabled-config fallback.
//!
//! It returns `Ok(())` so the server still receives `success: true` in the
//! `command_ack`, matching the pre-PR behavior where the Rust client
//! acknowledged commands but never moved a motor.

use super::{PanDir, Ptz, PtzCapabilities, PtzError, TiltDir, ZoomDir};
use async_trait::async_trait;

#[derive(Debug, Default)]
pub struct NoopPtz;

#[async_trait]
impl Ptz for NoopPtz {
    fn capabilities(&self) -> PtzCapabilities {
        PtzCapabilities::default()
    }
    async fn pan(&self, _dir: PanDir) -> Result<(), PtzError> {
        Ok(())
    }
    async fn tilt(&self, _dir: TiltDir) -> Result<(), PtzError> {
        Ok(())
    }
    async fn zoom(&self, _dir: ZoomDir) -> Result<(), PtzError> {
        Ok(())
    }
    async fn home(&self) -> Result<(), PtzError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_all_false() {
        let n = NoopPtz;
        assert_eq!(n.capabilities(), PtzCapabilities::default());
    }

    #[tokio::test]
    async fn pan_is_ok() {
        let n = NoopPtz;
        assert!(n.pan(PanDir::Left).await.is_ok());
        assert!(n.pan(PanDir::Right).await.is_ok());
    }

    #[tokio::test]
    async fn tilt_is_ok() {
        let n = NoopPtz;
        assert!(n.tilt(TiltDir::Up).await.is_ok());
        assert!(n.tilt(TiltDir::Down).await.is_ok());
    }

    #[tokio::test]
    async fn zoom_and_home_are_ok() {
        let n = NoopPtz;
        assert!(n.zoom(ZoomDir::In).await.is_ok());
        assert!(n.home().await.is_ok());
    }
}
