//! Parser + detection tests for `v4l2-ctl --list-ctrls` output.
//!
//! Three captured fixtures cover the realistic shape of the output:
//!
//! - `BCC950_LIKE` — Logitech BCC950: `pan_speed`/`tilt_speed` plus absolute
//!   limits. Speed-mode is the BCC950's preferred control surface.
//! - `C920_LIKE` — Logitech C920/C922: `pan_absolute`, `tilt_absolute`,
//!   `zoom_absolute` (no relative).
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
                   pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
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
fn parse_c920_extracts_pan_tilt_zoom_absolute_only() {
    let p = parse_list_ctrls(C920_LIKE);
    assert!(p.has("pan_absolute"));
    assert!(p.has("tilt_absolute"));
    assert!(p.has("zoom_absolute"));
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
fn caps_from_c920_advertises_ptz_and_patrol_includes_zoom() {
    let caps = PtzCapabilities::from_controls(&parse_list_ctrls(C920_LIKE));
    assert!(caps.pan);
    assert!(caps.tilt);
    assert!(caps.zoom);
    assert!(caps.home);
    // Even though zoom is true, advertised() still only returns ptz/patrol
    // because the server has no zoom intents/UI.
    assert_eq!(caps.advertised(), vec!["ptz", "patrol"]);
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
    let detected = PtzCapabilities {
        pan: true,
        ..Default::default()
    };
    let user: Vec<String> = vec![];
    assert_eq!(
        resolve_advertised_capabilities(&user, detected),
        vec!["ptz", "patrol"]
    );
}

#[test]
fn resolve_empty_user_no_detection_advertises_nothing() {
    let user: Vec<String> = vec![];
    let detected = PtzCapabilities::default();
    assert!(resolve_advertised_capabilities(&user, detected).is_empty());
}
