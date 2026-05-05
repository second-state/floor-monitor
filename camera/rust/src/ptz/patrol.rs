//! Cancellable patrol task.
//!
//! A patrol is a multi-step sweep across the pan axis: pan left, dwell,
//! pan back through center to right, dwell, pan back to start, then
//! optionally `home()`. Running it inline on the WebSocket task would
//! freeze frame capture for several seconds, so we spawn it on `tokio` and
//! cancel via [`tokio_util::sync::CancellationToken`].
//!
//! [`PatrolHandle`] owns the cancel token plus the spawned `JoinHandle`.
//! Drop the handle to keep the task running; call [`PatrolHandle::cancel`]
//! to signal it and await orderly shutdown.

use super::{PanDir, Ptz};
use crate::config::PatrolConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Debug)]
pub struct PatrolHandle {
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl PatrolHandle {
    /// Signal the patrol to stop and await orderly shutdown. The patrol
    /// finishes any in-flight `pan` call but does not start the next one.
    pub async fn cancel(self) {
        self.cancel.cancel();
        let _ = self.join.await;
    }

    /// Test-only: wait for the patrol to complete naturally.
    pub async fn join(self) {
        let _ = self.join.await;
    }

    /// True if the cancel signal has been delivered.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Spawn a patrol task. Returns immediately with a handle the caller
/// uses to cancel or join.
pub fn start_patrol(ptz: Arc<dyn Ptz>, cfg: PatrolConfig) -> PatrolHandle {
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        run_patrol(ptz, cfg, cancel_for_task).await;
    });
    PatrolHandle { cancel, join }
}

/// Drive one full patrol sweep, checking `cancel` between every pan call.
async fn run_patrol(ptz: Arc<dyn Ptz>, cfg: PatrolConfig, cancel: CancellationToken) {
    let dwell = Duration::from_millis(cfg.dwell_ms);
    let n = cfg.sweep_steps;

    // Phase 1: sweep left N steps.
    if !sweep(&ptz, PanDir::Left, n, dwell, &cancel).await {
        return;
    }
    // Phase 2: sweep right 2N steps (cross center to the right edge).
    if !sweep(&ptz, PanDir::Right, n.saturating_mul(2), dwell, &cancel).await {
        return;
    }
    // Phase 3: sweep left N steps to land back at center.
    if !sweep(&ptz, PanDir::Left, n, dwell, &cancel).await {
        return;
    }

    if cfg.return_home {
        match ptz.home().await {
            Ok(()) => info!("patrol: returned to home"),
            Err(e) => info!("patrol: home not supported ({}); leaving at center", e),
        }
    }
    info!("patrol: complete");
}

/// One side of a sweep. Returns `true` if it ran to completion, `false`
/// if cancelled mid-flight.
async fn sweep(
    ptz: &Arc<dyn Ptz>,
    dir: PanDir,
    steps: u32,
    dwell: Duration,
    cancel: &CancellationToken,
) -> bool {
    for _ in 0..steps {
        if cancel.is_cancelled() {
            return false;
        }
        if let Err(e) = ptz.pan(dir).await {
            warn!("patrol: pan failed ({}); aborting", e);
            return false;
        }
        tokio::select! {
            _ = cancel.cancelled() => return false,
            _ = tokio::time::sleep(dwell) => {}
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ptz::fake::{FakePtz, PtzCall};
    use crate::ptz::PtzCapabilities;

    fn fast_cfg() -> PatrolConfig {
        PatrolConfig {
            sweep_steps: 2,
            dwell_ms: 0,
            return_home: false,
        }
    }

    #[tokio::test]
    async fn full_sweep_records_left_right_left_sequence() {
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            tilt: true,
            ..Default::default()
        }));
        let h = start_patrol(fake.clone(), fast_cfg());
        h.join().await;
        let calls = fake.calls();
        // sweep_steps=2 → 2 left + 4 right + 2 left = 8 calls.
        assert_eq!(calls.len(), 8, "got {:?}", calls);
        assert_eq!(
            calls[0..2],
            vec![PtzCall::Pan(PanDir::Left), PtzCall::Pan(PanDir::Left)][..]
        );
        assert_eq!(
            calls[2..6],
            vec![
                PtzCall::Pan(PanDir::Right),
                PtzCall::Pan(PanDir::Right),
                PtzCall::Pan(PanDir::Right),
                PtzCall::Pan(PanDir::Right)
            ][..]
        );
        assert_eq!(
            calls[6..8],
            vec![PtzCall::Pan(PanDir::Left), PtzCall::Pan(PanDir::Left)][..]
        );
    }

    #[tokio::test]
    async fn return_home_calls_home_when_enabled() {
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            tilt: true,
            home: true,
            ..Default::default()
        }));
        let cfg = PatrolConfig {
            sweep_steps: 1,
            dwell_ms: 0,
            return_home: true,
        };
        start_patrol(fake.clone(), cfg).join().await;
        assert_eq!(fake.calls().last(), Some(&PtzCall::Home));
    }

    #[tokio::test]
    async fn cancellation_halts_mid_sweep() {
        // Long dwell so we have time to cancel between pans.
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            ..Default::default()
        }));
        let cfg = PatrolConfig {
            sweep_steps: 5,
            dwell_ms: 100_000, // effectively forever for the test
            return_home: false,
        };
        let h = start_patrol(fake.clone(), cfg);
        // Give the spawned task a moment to make its first pan and then
        // park on the dwell sleep.
        tokio::time::sleep(Duration::from_millis(50)).await;
        h.cancel().await;
        // First pan was issued; the cancellation should have stopped any
        // further sweep work.
        let n_calls = fake.calls().len();
        assert!(
            (1..=2).contains(&n_calls),
            "expected 1 or 2 pans before cancel, got {}",
            n_calls
        );
    }

    #[tokio::test]
    async fn pan_failure_aborts_patrol() {
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            ..Default::default()
        }));
        fake.fail_next();
        let h = start_patrol(fake.clone(), fast_cfg());
        h.join().await;
        // Only the first (failing) pan was recorded.
        assert_eq!(fake.calls().len(), 1);
    }

    #[tokio::test]
    async fn cancel_before_first_pan_records_zero_calls() {
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            ..Default::default()
        }));
        let cfg = PatrolConfig {
            sweep_steps: 5,
            dwell_ms: 100_000,
            return_home: false,
        };
        let h = start_patrol(fake.clone(), cfg);
        // Cancel immediately, before the spawned task has a chance to run.
        // Note: the very first iteration of `sweep` checks `is_cancelled()`
        // before pan, so cancellation here may or may not catch the first
        // pan depending on scheduling. Either zero or one pan is acceptable.
        h.cancel().await;
        let n = fake.calls().len();
        assert!(n <= 1, "expected at most 1 pan before cancel, got {}", n);
    }
}
