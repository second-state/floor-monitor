//! `FakePtz` — a call-recording test double for the [`Ptz`] trait.
//!
//! Tests construct a `FakePtz`, hand it out as `Arc<dyn Ptz>`, and after
//! the system-under-test runs they inspect `calls()` to assert the right
//! sequence of trait method invocations. `fail_next()` flips the next
//! call to return `PtzError::V4l2(...)` so failure paths can be tested
//! without standing up a real subprocess.

use super::{PanDir, Ptz, PtzCapabilities, PtzError, TiltDir, ZoomDir};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtzCall {
    Pan(PanDir),
    Tilt(TiltDir),
    Zoom(ZoomDir),
    Home,
}

#[derive(Debug, Default)]
pub struct FakePtz {
    caps: PtzCapabilities,
    calls: Mutex<Vec<PtzCall>>,
    fail_next: AtomicBool,
}

impl FakePtz {
    pub fn with_caps(caps: PtzCapabilities) -> Self {
        Self {
            caps,
            calls: Mutex::new(Vec::new()),
            fail_next: AtomicBool::new(false),
        }
    }

    /// Snapshot the recorded call list. Cheap (clones a small Vec).
    pub fn calls(&self) -> Vec<PtzCall> {
        self.calls.lock().expect("FakePtz mutex poisoned").clone()
    }

    /// Make the next trait call return `PtzError::V4l2("forced failure")`.
    pub fn fail_next(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    fn record(&self, call: PtzCall) -> Result<(), PtzError> {
        self.calls
            .lock()
            .expect("FakePtz mutex poisoned")
            .push(call);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            Err(PtzError::V4l2("forced failure".to_string()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Ptz for FakePtz {
    fn capabilities(&self) -> PtzCapabilities {
        self.caps
    }
    async fn pan(&self, dir: PanDir) -> Result<(), PtzError> {
        self.record(PtzCall::Pan(dir))
    }
    async fn tilt(&self, dir: TiltDir) -> Result<(), PtzError> {
        self.record(PtzCall::Tilt(dir))
    }
    async fn zoom(&self, dir: ZoomDir) -> Result<(), PtzError> {
        self.record(PtzCall::Zoom(dir))
    }
    async fn home(&self) -> Result<(), PtzError> {
        self.record(PtzCall::Home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_pan_in_order() {
        let f = FakePtz::default();
        f.pan(PanDir::Left).await.unwrap();
        f.pan(PanDir::Right).await.unwrap();
        assert_eq!(
            f.calls(),
            vec![PtzCall::Pan(PanDir::Left), PtzCall::Pan(PanDir::Right)]
        );
    }

    #[tokio::test]
    async fn records_tilt_in_order() {
        let f = FakePtz::default();
        f.tilt(TiltDir::Up).await.unwrap();
        f.tilt(TiltDir::Down).await.unwrap();
        assert_eq!(
            f.calls(),
            vec![PtzCall::Tilt(TiltDir::Up), PtzCall::Tilt(TiltDir::Down)]
        );
    }

    #[tokio::test]
    async fn fail_next_returns_error_then_recovers() {
        let f = FakePtz::default();
        f.fail_next();
        let err = f.pan(PanDir::Left).await.unwrap_err();
        assert!(matches!(err, PtzError::V4l2(_)));
        // Subsequent call succeeds again.
        f.pan(PanDir::Right).await.unwrap();
        assert_eq!(f.calls().len(), 2);
    }

    #[test]
    fn capabilities_match_with_caps() {
        let caps = PtzCapabilities {
            pan: true,
            tilt: true,
            ..Default::default()
        };
        let f = FakePtz::with_caps(caps);
        assert_eq!(f.capabilities(), caps);
    }
}
