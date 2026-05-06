//! `v4l2-ctl` subprocess runner trait + implementations + the
//! [`V4l2CtlPtz`] controller that drives real UVC PTZ hardware.
//!
//! - [`V4l2CtlRunner`] is the trait the rest of the PTZ code depends on.
//! - [`RealRunner`] (Linux only) shells out to `tokio::process::Command`.
//! - [`FakeV4l2CtlRunner`] captures the exact `args` it was called with
//!   and returns canned responses popped from a `VecDeque`. Tests use it
//!   to exercise both the parser and the dispatch logic without ever
//!   touching a real binary.
//! - [`V4l2CtlPtz`] selects per-axis between relative-mode and
//!   absolute-mode based on detected controls and translates pan/tilt
//!   commands into `--set-ctrl=...` invocations.

use super::detect::{ControlRange, ParsedControls};
use super::{PanDir, Ptz, PtzCapabilities, PtzError, TiltDir};
use crate::config::PtzConfig;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AsyncMutex;

#[async_trait]
pub trait V4l2CtlRunner: Send + Sync {
    /// Invoke `v4l2-ctl` with the given args. Returns stdout on success.
    async fn run(&self, args: &[&str]) -> Result<String, PtzError>;
}

/// `Arc<R>` is a `V4l2CtlRunner` whenever `R` is, so tests can hold an
/// `Arc<FakeV4l2CtlRunner>` for inspection while passing a clone into
/// `V4l2CtlPtz::new` for ownership.
#[async_trait]
impl<R: V4l2CtlRunner + ?Sized> V4l2CtlRunner for Arc<R> {
    async fn run(&self, args: &[&str]) -> Result<String, PtzError> {
        (**self).run(args).await
    }
}

/// Production runner. Spawns `v4l2-ctl` via `tokio::process::Command`
/// with a 2-second timeout. Linux-only.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct RealRunner;

#[cfg(target_os = "linux")]
#[async_trait]
impl V4l2CtlRunner for RealRunner {
    async fn run(&self, args: &[&str]) -> Result<String, PtzError> {
        use tokio::process::Command;
        use tokio::time::timeout;
        // kill_on_drop(true) wires SIGKILL to the child's Drop. When our
        // 2-second timeout fires the spawned future is dropped along with
        // its Child, and the v4l2-ctl process is reaped — without this
        // flag, a wedged USB device would leak one orphan v4l2-ctl per
        // timed-out PTZ command on long-running clients.
        let fut = Command::new("v4l2-ctl")
            .args(args)
            .kill_on_drop(true)
            .output();
        let output = timeout(std::time::Duration::from_secs(2), fut)
            .await
            .map_err(|_| PtzError::Timeout)?
            .map_err(|e| PtzError::Io(e.to_string()))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(PtzError::V4l2(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }
}

/// Test double that records every `args` array it's called with and
/// returns canned responses. Construct with [`FakeV4l2CtlRunner::with_response`]
/// for a single canned reply, or push more via [`FakeV4l2CtlRunner::push`].
#[derive(Debug, Default)]
pub struct FakeV4l2CtlRunner {
    captured: StdMutex<Vec<Vec<String>>>,
    responses: StdMutex<VecDeque<Result<String, PtzError>>>,
}

impl FakeV4l2CtlRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_response(stdout: impl Into<String>) -> Self {
        let r = Self::default();
        r.push(Ok(stdout.into()));
        r
    }

    pub fn with_error(err: PtzError) -> Self {
        let r = Self::default();
        r.push(Err(err));
        r
    }

    pub fn push(&self, response: Result<String, PtzError>) {
        self.responses
            .lock()
            .expect("FakeV4l2CtlRunner mutex poisoned")
            .push_back(response);
    }

    /// Snapshot of every `args` array passed to `run()`, in invocation order.
    pub fn captured(&self) -> Vec<Vec<String>> {
        self.captured
            .lock()
            .expect("FakeV4l2CtlRunner mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl V4l2CtlRunner for FakeV4l2CtlRunner {
    async fn run(&self, args: &[&str]) -> Result<String, PtzError> {
        self.captured
            .lock()
            .expect("FakeV4l2CtlRunner mutex poisoned")
            .push(args.iter().map(|s| s.to_string()).collect());
        let mut q = self
            .responses
            .lock()
            .expect("FakeV4l2CtlRunner mutex poisoned");
        // If the test didn't push enough responses, default to empty stdout.
        q.pop_front().unwrap_or_else(|| Ok(String::new()))
    }
}

// ---- V4l2CtlPtz -----------------------------------------------------

/// Per-axis control mode chosen at startup from the parsed v4l2 controls.
/// Relative wins over absolute (BCC950 needs relative; absolute requires
/// us to track position which is fragile if another tool also drives the
/// camera).
#[derive(Debug)]
pub enum AxisCtrl {
    /// No supported control on this axis. `pan`/`tilt` will return
    /// [`PtzError::Unsupported`].
    None,
    /// `pan_relative` / `tilt_relative` available. We send `±1` per click;
    /// the camera firmware decides how far one detent is.
    Relative,
    /// `pan_absolute` / `tilt_absolute` available. We track the current
    /// position in `current` and write `current ± step` clamped to `range`.
    /// The `tokio::sync::Mutex` serializes the (read-prev, write-cmd,
    /// update-tracking) triple so concurrent calls (e.g. patrol on a
    /// spawned task plus a user-issued pan from the WS loop) can't race.
    Absolute {
        current: AsyncMutex<i32>,
        range: ControlRange,
    },
}

impl AxisCtrl {
    /// Inspect parsed controls for the given axis (`"pan"` or `"tilt"`)
    /// and choose the best mode. For absolute mode, seeds the tracked
    /// position from the camera's current `value=N` so the first command
    /// after startup moves one step from where the lens actually is —
    /// not from a synthetic zero.
    pub fn from_parsed(prefix: &str, p: &ParsedControls) -> Self {
        let rel = format!("{}_relative", prefix);
        if p.has(&rel) {
            return Self::Relative;
        }
        let abs = format!("{}_absolute", prefix);
        if p.has(&abs) {
            let range = p.range(&abs).unwrap_or(ControlRange {
                min: i32::MIN / 2,
                max: i32::MAX / 2,
                step: 1,
            });
            let initial = p.value(&abs).unwrap_or(0);
            return Self::Absolute {
                current: AsyncMutex::new(initial),
                range,
            };
        }
        Self::None
    }
}

/// Drives UVC PTZ hardware via `v4l2-ctl --set-ctrl=...`. Generic over the
/// runner so tests can inject [`FakeV4l2CtlRunner`]. Production code uses
/// `V4l2CtlPtz<RealRunner>` (Linux only).
#[derive(Debug)]
pub struct V4l2CtlPtz<R: V4l2CtlRunner> {
    runner: R,
    device: String,
    pan: AxisCtrl,
    tilt: AxisCtrl,
    pan_step: i32,
    tilt_step: i32,
    invert_pan: bool,
    invert_tilt: bool,
    caps: PtzCapabilities,
}

impl<R: V4l2CtlRunner> V4l2CtlPtz<R> {
    pub fn new(runner: R, device: String, parsed: &ParsedControls, cfg: &PtzConfig) -> Self {
        let pan = AxisCtrl::from_parsed("pan", parsed);
        let tilt = AxisCtrl::from_parsed("tilt", parsed);
        // Override caps to match what the driver can actually do, not
        // just what `--list-ctrls` parsed. The trait's capabilities() is
        // the runtime contract; consumers branching on it must not see
        // a `true` for a method whose default impl returns Unsupported.
        // - `home`: only true if BOTH axes ended up in Absolute mode
        //   (parser-level inference would say true whenever both
        //   pan_absolute and tilt_absolute exist, but if pan_relative
        //   was also present AxisCtrl::from_parsed picks Relative and
        //   home() returns Unsupported).
        // - `zoom`: V4l2CtlPtz doesn't override Ptz::zoom yet (zoom is
        //   out of scope per issue #1), so the trait's default impl
        //   returns Unsupported. Force the cap off until zoom_absolute /
        //   zoom_relative driving lands.
        let mut caps = PtzCapabilities::from_controls(parsed);
        caps.home =
            matches!(pan, AxisCtrl::Absolute { .. }) && matches!(tilt, AxisCtrl::Absolute { .. });
        caps.zoom = false;
        Self {
            runner,
            device,
            pan,
            tilt,
            pan_step: cfg.pan_step,
            tilt_step: cfg.tilt_step,
            invert_pan: cfg.invert_pan,
            invert_tilt: cfg.invert_tilt,
            caps,
        }
    }

    /// Per-axis dispatch. Splits the runner call out so `pan`/`tilt`
    /// stay a one-liner each. Absolute mode holds the position lock
    /// across the subprocess await so a concurrent call (e.g. patrol
    /// from a spawned task + a user pan from the WS loop) waits its
    /// turn instead of computing from a stale `prev`.
    async fn drive_axis(
        &self,
        axis: &AxisCtrl,
        ctrl_prefix: &'static str,
        sign: i32,
        step: i32,
    ) -> Result<(), PtzError> {
        match axis {
            AxisCtrl::None => Err(PtzError::Unsupported(ctrl_prefix)),
            AxisCtrl::Relative => {
                let arg = format!("--set-ctrl={}_relative={}", ctrl_prefix, sign);
                self.runner.run(&["-d", &self.device, &arg]).await?;
                Ok(())
            }
            AxisCtrl::Absolute { current, range } => {
                let mut guard = current.lock().await;
                let next = (*guard + sign * step).clamp(range.min, range.max);
                let arg = format!("--set-ctrl={}_absolute={}", ctrl_prefix, next);
                self.runner.run(&["-d", &self.device, &arg]).await?;
                *guard = next;
                Ok(())
            }
        }
    }
}

#[async_trait]
impl<R: V4l2CtlRunner + 'static> Ptz for V4l2CtlPtz<R> {
    fn capabilities(&self) -> PtzCapabilities {
        self.caps
    }

    async fn pan(&self, dir: PanDir) -> Result<(), PtzError> {
        let base = match dir {
            PanDir::Left => -1,
            PanDir::Right => 1,
        };
        let sign = if self.invert_pan { -base } else { base };
        self.drive_axis(&self.pan, "pan", sign, self.pan_step).await
    }

    async fn tilt(&self, dir: TiltDir) -> Result<(), PtzError> {
        let base = match dir {
            TiltDir::Up => 1,
            TiltDir::Down => -1,
        };
        let sign = if self.invert_tilt { -base } else { base };
        self.drive_axis(&self.tilt, "tilt", sign, self.tilt_step)
            .await
    }

    async fn home(&self) -> Result<(), PtzError> {
        let (AxisCtrl::Absolute { current: pan, .. }, AxisCtrl::Absolute { current: tilt, .. }) =
            (&self.pan, &self.tilt)
        else {
            return Err(PtzError::Unsupported("home"));
        };
        // Acquire both axis locks before the subprocess call. Lock order
        // (pan → tilt) is fixed so concurrent home + pan + tilt cannot
        // deadlock; only these two locks exist on this controller.
        let mut pan_guard = pan.lock().await;
        let mut tilt_guard = tilt.lock().await;
        self.runner
            .run(&[
                "-d",
                &self.device,
                "--set-ctrl=pan_absolute=0,tilt_absolute=0",
            ])
            .await?;
        *pan_guard = 0;
        *tilt_guard = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_runner_captures_args_and_returns_canned() {
        let r = FakeV4l2CtlRunner::with_response("hello\n");
        let out = r.run(&["-d", "/dev/video0", "--list-ctrls"]).await.unwrap();
        assert_eq!(out, "hello\n");
        assert_eq!(
            r.captured(),
            vec![vec![
                "-d".to_string(),
                "/dev/video0".to_string(),
                "--list-ctrls".to_string()
            ]]
        );
    }

    #[tokio::test]
    async fn fake_runner_pops_canned_in_order() {
        let r = FakeV4l2CtlRunner::new();
        r.push(Ok("first".to_string()));
        r.push(Ok("second".to_string()));
        assert_eq!(r.run(&["a"]).await.unwrap(), "first");
        assert_eq!(r.run(&["b"]).await.unwrap(), "second");
        // Past the queue: defaults to empty stdout (does not panic).
        assert_eq!(r.run(&["c"]).await.unwrap(), "");
    }

    #[tokio::test]
    async fn fake_runner_propagates_canned_error() {
        let r = FakeV4l2CtlRunner::with_error(PtzError::Timeout);
        let err = r.run(&["x"]).await.unwrap_err();
        assert!(matches!(err, PtzError::Timeout));
    }
}
