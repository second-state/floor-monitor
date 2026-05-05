//! Hardware-gated PTZ tests. Skipped unless `FLOOR_MONITOR_PTZ_HW=1`.
//!
//! These tests only run on a Linux host with a real UVC PTZ webcam
//! plugged in (default `/dev/video0`, override via `FLOOR_MONITOR_PTZ_DEV`).
//! CI never runs them — the env var gate ensures `cargo test` skips them
//! cleanly even when the test binary is built.
//!
//! Each test is also `#[ignore]`d so plain `cargo test` won't pick them
//! up; opt in with `cargo test -- --ignored`.

#[cfg(target_os = "linux")]
use floor_monitor_camera::config::PtzConfig;
#[cfg(target_os = "linux")]
use floor_monitor_camera::ptz::detect::{self, parse_list_ctrls};
#[cfg(target_os = "linux")]
use floor_monitor_camera::ptz::{PanDir, Ptz};

#[cfg(target_os = "linux")]
fn hw_enabled() -> bool {
    std::env::var("FLOOR_MONITOR_PTZ_HW").as_deref() == Ok("1")
}

#[cfg(target_os = "linux")]
fn hw_device() -> String {
    std::env::var("FLOOR_MONITOR_PTZ_DEV").unwrap_or_else(|_| "/dev/video0".into())
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real PTZ hardware; gate with FLOOR_MONITOR_PTZ_HW=1"]
async fn hw_detect_capabilities_returns_nonempty() {
    if !hw_enabled() {
        eprintln!("FLOOR_MONITOR_PTZ_HW != 1; skipping");
        return;
    }
    use floor_monitor_camera::ptz::v4l2ctl::RealRunner;
    let device = hw_device();
    let caps = detect::detect(&RealRunner, &device).await;
    assert!(
        caps.pan || caps.tilt || caps.zoom,
        "no PTZ controls detected on {} — wrong device?",
        device
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires real PTZ hardware; gate with FLOOR_MONITOR_PTZ_HW=1"]
async fn hw_pan_left_moves_motor() {
    if !hw_enabled() {
        eprintln!("FLOOR_MONITOR_PTZ_HW != 1; skipping");
        return;
    }
    use floor_monitor_camera::ptz::v4l2ctl::{RealRunner, V4l2CtlPtz, V4l2CtlRunner};
    let device = hw_device();
    let runner = RealRunner;
    let out = runner
        .run(&["-d", &device, "--list-ctrls"])
        .await
        .expect("v4l2-ctl --list-ctrls failed");
    let parsed = parse_list_ctrls(&out);
    let cfg = PtzConfig::default();
    let ptz = V4l2CtlPtz::new(runner, device.clone(), &parsed, &cfg);
    if !ptz.capabilities().pan {
        eprintln!("device {} has no pan control; skipping", device);
        return;
    }
    ptz.pan(PanDir::Left).await.expect("pan_left failed");
    // Visual confirmation only — no programmatic way to verify the motor
    // actually moved without a webcam-of-the-webcam setup.
    eprintln!("pan_left issued on {}; check the camera moved.", device);
}

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "Linux-only"]
fn hw_tests_only_on_linux() {
    // Placeholder so the test binary compiles on macOS / Windows.
}
