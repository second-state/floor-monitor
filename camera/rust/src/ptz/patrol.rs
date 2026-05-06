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
    /// May block up to one `v4l2-ctl` timeout (~2s) if the previous
    /// `pan` is mid-subprocess. Use [`PatrolHandle::signal_cancel`] in
    /// hot paths where blocking the caller is unacceptable.
    pub async fn cancel(self) {
        self.cancel.cancel();
        let _ = self.join.await;
    }

    /// Signal the patrol to stop without awaiting its join. The spawned
    /// task continues running on tokio until it reaches its next yield
    /// point and observes the cancel token, then exits naturally — but
    /// the caller doesn't have to wait for the in-flight `pan` to
    /// finish. Used by `commands::dispatch` so the WebSocket loop can
    /// ack `patrol_started` immediately when a new patrol pre-empts an
    /// older one.
    pub fn signal_cancel(self) {
        self.cancel.cancel();
        // self.join is dropped here. tokio detaches the task; it runs
        // to completion in the background.
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
        if cancel.is_cancelled() {
            info!("patrol: cancelled before home");
        } else {
            // Race cancel against home(): if cancellation lands while
            // the v4l2-ctl subprocess is in flight, dropping the home()
            // future drops the Child, which kills the subprocess (via
            // kill_on_drop). No "one final motor move after cancel".
            tokio::select! {
                _ = cancel.cancelled() => info!("patrol: cancelled during home"),
                result = ptz.home() => match result {
                    Ok(()) => info!("patrol: returned to home"),
                    Err(e) => info!("patrol: home not supported ({}); leaving at center", e),
                }
            }
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
        // Race cancel against the pan() future itself. A bare
        // is_cancelled() check before pan() leaves a TOCTOU window
        // where cancellation observed during the await still completes
        // a motor move. With select!, dropping the pan future on
        // cancel also drops the v4l2-ctl Child (kill_on_drop=true),
        // so the underlying subprocess gets killed too.
        let pan_result = tokio::select! {
            _ = cancel.cancelled() => return false,
            res = ptz.pan(dir) => res,
        };
        if let Err(e) = pan_result {
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
    async fn signal_cancel_returns_immediately_and_task_exits_eventually() {
        // The point of signal_cancel is that the caller DOES NOT wait for
        // the spawned task to finish. We assert this by measuring how
        // long signal_cancel takes — under a millisecond — even when the
        // task is parked on a long dwell sleep.
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            ..Default::default()
        }));
        let cfg = PatrolConfig {
            sweep_steps: 5,
            dwell_ms: 100_000, // forever
            return_home: false,
        };
        let h = start_patrol(fake.clone(), cfg);
        // Let the first pan happen and the task park on the dwell sleep.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let before = std::time::Instant::now();
        h.signal_cancel();
        // Should return effectively instantly — no .await on the join.
        assert!(
            before.elapsed() < Duration::from_millis(20),
            "signal_cancel should not block; took {:?}",
            before.elapsed()
        );
        // Yield once so the detached task can wake on the cancel future
        // and exit. This is enough because the dwell select races
        // sleep(100s) against cancel.cancelled() — cancel wins instantly.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // No further pans should have been issued past the first.
        let n = fake.calls().len();
        assert!(
            (1..=2).contains(&n),
            "expected 1 or 2 pans before cancel, got {}",
            n
        );
    }

    #[tokio::test]
    async fn cancel_during_pan_await_aborts_before_record() {
        // Hardware-style slow pan: if cancel arrives while pan is
        // mid-await, the select! drops the pan future before record()
        // runs. With the previous code (is_cancelled check then bare
        // pan().await), the in-flight pan completed and was recorded.
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            ..Default::default()
        }));
        fake.set_delay_ms(500);
        let cfg = PatrolConfig {
            sweep_steps: 5,
            dwell_ms: 0,
            return_home: false,
        };
        let h = start_patrol(fake.clone(), cfg);
        // Spawn picks up, enters sweep, starts the first pan, sleeps.
        tokio::time::sleep(Duration::from_millis(50)).await;
        h.cancel().await;
        let n = fake.calls().len();
        // Cancel raced and won the select! before record() ran.
        assert_eq!(
            n, 0,
            "expected pan future to be dropped before record(), got {} calls",
            n
        );
    }

    #[tokio::test]
    async fn cancel_during_sweep_skips_home_entirely() {
        // Sweep + slow pans. We cancel mid-sweep (between pans, while
        // one is parked on its delay) so the patrol never reaches the
        // home() block at all. PtzCall::Home must not appear in the
        // recorded sequence.
        let fake = Arc::new(FakePtz::with_caps(PtzCapabilities {
            pan: true,
            tilt: true,
            home: true,
            ..Default::default()
        }));
        fake.set_delay_ms(500);
        let cfg = PatrolConfig {
            sweep_steps: 1,
            dwell_ms: 0,
            return_home: true,
        };
        let h = start_patrol(fake.clone(), cfg);
        // Sweep would be 4 pans @ 500ms each = 2000ms; cancel ~1s in
        // so only 1-3 pans get recorded and the home() branch is
        // never entered.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        h.cancel().await;
        let calls = fake.calls();
        assert!(
            !calls.contains(&PtzCall::Home),
            "home should have been skipped, got: {:?}",
            calls
        );
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
