//! Parser + detection tests for `v4l2-ctl --list-ctrls` output.
//!
//! Four captured fixtures cover the realistic shape of the output:
//!
//! - `BCC950_LIKE` — Logitech BCC950 with both speed and absolute pan/tilt.
//!   Real BCC950 firmware varies; this fixture is the "rich" case.
//! - `C920_LIKE` — Logitech C920/C922: zoom-only. Per the issue and the
//!   hardware verification log, real C920 cameras expose `zoom_absolute`
//!   but no pan/tilt controls. Used as the negative case (advertise `[]`).
//! - `BCC950_RELATIVE_ONLY` — relative pan/tilt only, the BCC950 path
//!   most users see.
//! - `NO_PTZ` — generic webcam: only `brightness`/`contrast`.

use floor_monitor_camera::ptz::detect::{self, parse_list_ctrls, resolve_advertised_capabilities};
use floor_monitor_camera::ptz::v4l2ctl::FakeV4l2CtlRunner;
use floor_monitor_camera::ptz::{PtzCapabilities, PtzError};

const BCC950_LIKE: &str = "
User Controls

                     brightness 0x00980900 (int)    : min=0 max=255 step=1 default=128 value=128
                       contrast 0x00980901 (int)    : min=0 max=255 step=1 default=32 value=32

Camera Controls

                      pan_speed 0x009a0920 (int)    : min=-4 max=4 step=1 default=0 value=0
                     tilt_speed 0x009a0921 (int)    : min=-4 max=4 step=1 default=0 value=0
                   pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
";

const C920_LIKE: &str = "
User Controls
                     brightness 0x00980900 (int)    : min=0 max=255 step=1 default=128 value=128
Camera Controls
                  zoom_absolute 0x009a090d (int)    : min=100 max=500 step=1 default=100 value=100
";

const NO_PTZ: &str = "
User Controls
                     brightness 0x00980900 (int)    : min=0 max=255 step=1 default=128 value=128
                       contrast 0x00980901 (int)    : min=0 max=255 step=1 default=32 value=32
                     saturation 0x00980902 (int)    : min=0 max=255 step=1 default=128 value=128
";

const BCC950_RELATIVE_ONLY: &str = "
                   pan_relative 0x009a0904 (int)    : min=-1 max=1 step=1 default=0 value=0
                  tilt_relative 0x009a0905 (int)    : min=-1 max=1 step=1 default=0 value=0
";

// ---- Parser tests --------------------------------------------------

#[test]
fn parse_bcc950_extracts_pan_and_tilt_speed_plus_absolute() {
    let p = parse_list_ctrls(BCC950_LIKE);
    assert!(p.has("pan_speed"));
    assert!(p.has("tilt_speed"));
    assert!(p.has("pan_absolute"));
    assert!(p.has("tilt_absolute"));
    assert!(!p.has("zoom_absolute"));
    assert!(p.has("brightness"));
}

#[test]
fn parse_bcc950_extracts_min_max_step_for_pan_absolute() {
    let p = parse_list_ctrls(BCC950_LIKE);
    let r = p.range("pan_absolute").unwrap();
    assert_eq!(r.min, -36000);
    assert_eq!(r.max, 36000);
    assert_eq!(r.step, 3600);
}

#[test]
fn parse_extracts_current_value_when_present() {
    let fixture = "
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=14400
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=-3600
";
    let p = parse_list_ctrls(fixture);
    assert_eq!(p.value("pan_absolute"), Some(14400));
    assert_eq!(p.value("tilt_absolute"), Some(-3600));
}

#[test]
fn parse_value_absent_returns_none() {
    let p = parse_list_ctrls("");
    assert_eq!(p.value("pan_absolute"), None);
}

#[test]
fn parse_c920_extracts_zoom_only() {
    let p = parse_list_ctrls(C920_LIKE);
    assert!(p.has("zoom_absolute"));
    assert!(!p.has("pan_absolute"));
    assert!(!p.has("tilt_absolute"));
    assert!(!p.has("pan_relative"));
    assert!(!p.has("pan_speed"));
}

#[test]
fn parse_c920_extracts_zoom_range() {
    let p = parse_list_ctrls(C920_LIKE);
    let r = p.range("zoom_absolute").unwrap();
    assert_eq!(r.min, 100);
    assert_eq!(r.max, 500);
    assert_eq!(r.step, 1);
}

#[test]
fn parse_no_ptz_finds_no_pan_tilt_zoom() {
    let p = parse_list_ctrls(NO_PTZ);
    assert!(!p.has("pan_absolute"));
    assert!(!p.has("tilt_absolute"));
    assert!(!p.has("zoom_absolute"));
    assert!(!p.has("pan_speed"));
    assert!(!p.has("pan_relative"));
}

#[test]
fn parse_empty_string_returns_empty() {
    let p = parse_list_ctrls("");
    assert!(p.names.is_empty());
    assert!(p.ranges.is_empty());
}

#[test]
fn parse_garbage_does_not_panic() {
    let p = parse_list_ctrls("\u{0}\u{1} random\nbinary  \tdata\n\nno colons\n");
    // No control lines (none have a colon), so empty.
    assert!(p.names.is_empty());
}

#[test]
fn parse_skips_section_headers() {
    let p = parse_list_ctrls("User Controls\n\nCamera Controls\n");
    assert!(p.names.is_empty());
}

// ---- PtzCapabilities::from_controls --------------------------------

#[test]
fn caps_from_bcc950_advertises_ptz_and_patrol() {
    let caps = PtzCapabilities::from_controls(&parse_list_ctrls(BCC950_LIKE));
    assert!(caps.pan);
    assert!(caps.tilt);
    assert!(!caps.zoom);
    assert!(caps.home); // pan_absolute + tilt_absolute both present
    assert_eq!(caps.advertised(), vec!["ptz", "patrol"]);
}

#[test]
fn caps_from_c920_zoom_only_advertises_nothing() {
    // Real C920 has only zoom_absolute — no pan/tilt motors.
    // Per docs/PTZ_HARDWARE_LOG.md this is the negative regression case:
    // the server has no zoom UI/intent, so the camera should advertise [].
    let caps = PtzCapabilities::from_controls(&parse_list_ctrls(C920_LIKE));
    assert!(!caps.pan);
    assert!(!caps.tilt);
    assert!(caps.zoom);
    assert!(!caps.home);
    assert!(caps.advertised().is_empty());
}

#[test]
fn caps_from_bcc950_relative_only_no_home() {
    let caps = PtzCapabilities::from_controls(&parse_list_ctrls(BCC950_RELATIVE_ONLY));
    assert!(caps.pan);
    assert!(caps.tilt);
    assert!(!caps.home); // no pan_absolute/tilt_absolute
}

#[test]
fn caps_from_no_ptz_advertises_nothing() {
    let caps = PtzCapabilities::from_controls(&parse_list_ctrls(NO_PTZ));
    assert!(!caps.pan);
    assert!(!caps.tilt);
    assert!(!caps.zoom);
    assert!(caps.advertised().is_empty());
}

// ---- detect() end-to-end through the runner trait ------------------

#[tokio::test]
async fn detect_runs_v4l2_ctl_with_correct_args() {
    let r = FakeV4l2CtlRunner::with_response(BCC950_LIKE);
    let _ = detect::detect(&r, "/dev/video2").await;
    let captured = r.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0],
        vec![
            "-d".to_string(),
            "/dev/video2".to_string(),
            "--list-ctrls".to_string()
        ]
    );
}

#[tokio::test]
async fn detect_returns_caps_from_runner_output() {
    let r = FakeV4l2CtlRunner::with_response(BCC950_LIKE);
    let caps = detect::detect(&r, "/dev/video0").await;
    assert!(caps.pan);
    assert!(caps.tilt);
}

#[tokio::test]
async fn detect_when_runner_errors_returns_empty_caps() {
    let r = FakeV4l2CtlRunner::with_error(PtzError::V4l2("not found".to_string()));
    let caps = detect::detect(&r, "/dev/video0").await;
    assert_eq!(caps, PtzCapabilities::default());
}

#[tokio::test]
async fn detect_when_runner_times_out_returns_empty_caps() {
    let r = FakeV4l2CtlRunner::with_error(PtzError::Timeout);
    let caps = detect::detect(&r, "/dev/video0").await;
    assert_eq!(caps, PtzCapabilities::default());
}

// ---- resolve_advertised_capabilities -------------------------------

#[test]
fn resolve_user_supplied_overrides_detected() {
    let detected = PtzCapabilities {
        pan: true,
        tilt: true,
        ..Default::default()
    };
    let user = vec!["custom".to_string()];
    assert_eq!(
        resolve_advertised_capabilities(&user, detected),
        vec!["custom"]
    );
}

#[test]
fn resolve_empty_user_uses_detected() {
    // Detected pan AND tilt → advertises both wire caps.
    let detected = PtzCapabilities {
        pan: true,
        tilt: true,
        ..Default::default()
    };
    let user: Vec<String> = vec![];
    assert_eq!(
        resolve_advertised_capabilities(&user, detected),
        vec!["ptz", "patrol"]
    );
}

#[test]
fn resolve_empty_user_with_pan_only_advertises_nothing() {
    // Pan-only is a negative case: server gates pan/tilt behind the same
    // "ptz" capability, so advertising it would route tilt commands the
    // camera can't drive.
    let detected = PtzCapabilities {
        pan: true,
        ..Default::default()
    };
    let user: Vec<String> = vec![];
    assert!(resolve_advertised_capabilities(&user, detected).is_empty());
}

#[test]
fn resolve_empty_user_no_detection_advertises_nothing() {
    let user: Vec<String> = vec![];
    let detected = PtzCapabilities::default();
    assert!(resolve_advertised_capabilities(&user, detected).is_empty());
}
