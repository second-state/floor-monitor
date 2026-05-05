//! `v4l2-ctl` subprocess runner trait + implementations.
//!
//! - [`V4l2CtlRunner`] is the trait the rest of the PTZ code depends on.
//! - [`RealRunner`] (Linux only) shells out to `tokio::process::Command`.
//! - [`FakeV4l2CtlRunner`] captures the exact `args` it was called with
//!   and returns canned responses popped from a `VecDeque`. Tests use it
//!   to exercise both the parser and the dispatch logic without ever
//!   touching a real binary.

use super::PtzError;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;

#[async_trait]
pub trait V4l2CtlRunner: Send + Sync {
    /// Invoke `v4l2-ctl` with the given args. Returns stdout on success.
    async fn run(&self, args: &[&str]) -> Result<String, PtzError>;
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
        let fut = Command::new("v4l2-ctl").args(args).output();
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
    captured: Mutex<Vec<Vec<String>>>,
    responses: Mutex<VecDeque<Result<String, PtzError>>>,
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
