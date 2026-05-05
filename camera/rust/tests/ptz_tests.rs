//! Integration tests for the `Ptz` trait dispatcher and `handle_command`
//! glue. Uses `FakePtz` to verify trait method invocations and
//! `commands::dispatch` / `commands::build_ack` to verify protocol shape
//! without standing up a real WebSocket.

use floor_monitor_camera::commands::{build_ack, dispatch};
use floor_monitor_camera::config::PtzConfig;
use floor_monitor_camera::ptz::{
    self,
    detect::parse_list_ctrls,
    fake::{FakePtz, PtzCall},
    noop::NoopPtz,
    v4l2ctl::{FakeV4l2CtlRunner, V4l2CtlPtz},
    PanDir, Ptz, PtzCapabilities, PtzError, TiltDir,
};
use serde_json::json;
use std::sync::Arc;

fn fake() -> Arc<FakePtz> {
    Arc::new(FakePtz::with_caps(PtzCapabilities {
        pan: true,
        tilt: true,
        ..Default::default()
    }))
}

// ---- Direction string mapping ----------------------------------------

#[tokio::test]
async fn execute_ptz_pan_left_calls_pan_with_left() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let msg = ptz::execute_ptz(&p, &json!({"direction": "pan_left"}))
        .await
        .unwrap();
    assert_eq!(msg, "pan_left ok");
    assert_eq!(f.calls(), vec![PtzCall::Pan(PanDir::Left)]);
}

#[tokio::test]
async fn execute_ptz_pan_right_calls_pan_with_right() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    ptz::execute_ptz(&p, &json!({"direction": "pan_right"}))
        .await
        .unwrap();
    assert_eq!(f.calls(), vec![PtzCall::Pan(PanDir::Right)]);
}

#[tokio::test]
async fn execute_ptz_tilt_up_calls_tilt_with_up() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    ptz::execute_ptz(&p, &json!({"direction": "tilt_up"}))
        .await
        .unwrap();
    assert_eq!(f.calls(), vec![PtzCall::Tilt(TiltDir::Up)]);
}

#[tokio::test]
async fn execute_ptz_tilt_down_calls_tilt_with_down() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    ptz::execute_ptz(&p, &json!({"direction": "tilt_down"}))
        .await
        .unwrap();
    assert_eq!(f.calls(), vec![PtzCall::Tilt(TiltDir::Down)]);
}

#[tokio::test]
async fn execute_ptz_unknown_direction_returns_bad_direction() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let err = ptz::execute_ptz(&p, &json!({"direction": "diagonal_warp"}))
        .await
        .unwrap_err();
    assert!(matches!(err, PtzError::BadDirection(s) if s == "diagonal_warp"));
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn execute_ptz_missing_direction_returns_bad_direction_empty() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let err = ptz::execute_ptz(&p, &json!({})).await.unwrap_err();
    assert!(matches!(err, PtzError::BadDirection(s) if s.is_empty()));
}

// ---- NoopPtz protocol-level behavior ---------------------------------

#[tokio::test]
async fn dispatch_ptz_pan_left_with_noop_acks_success() {
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let (success, msg) = dispatch(&p, "ptz", &json!({"direction": "pan_left"})).await;
    assert!(success);
    assert_eq!(msg, "pan_left ok");
}

#[tokio::test]
async fn dispatch_patrol_with_noop_acks_success() {
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let (success, msg) = dispatch(&p, "patrol", &json!({})).await;
    assert!(success);
    assert!(msg.contains("patrol"));
}

// ---- dispatch error paths -------------------------------------------

#[tokio::test]
async fn dispatch_unknown_action_acks_failure() {
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let (success, msg) = dispatch(&p, "dance", &json!({})).await;
    assert!(!success);
    assert_eq!(msg, "Unknown action: dance");
}

#[tokio::test]
async fn dispatch_ptz_unknown_direction_acks_failure() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let (success, msg) = dispatch(&p, "ptz", &json!({"direction": "diagonal_warp"})).await;
    assert!(!success);
    assert!(msg.contains("diagonal_warp"));
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn dispatch_ptz_when_trait_fails_acks_failure_with_error_text() {
    let f = fake();
    f.fail_next();
    let p: Arc<dyn Ptz> = f.clone();
    let (success, msg) = dispatch(&p, "ptz", &json!({"direction": "pan_left"})).await;
    assert!(!success);
    assert!(msg.contains("v4l2-ctl") || msg.contains("forced failure"));
    // The trait was still invoked once.
    assert_eq!(f.calls(), vec![PtzCall::Pan(PanDir::Left)]);
}

// ---- Wire format ----------------------------------------------------

#[test]
fn build_ack_has_required_fields() {
    let ack = build_ack("cam-1", "ptz", true, "pan_left ok");
    assert_eq!(ack["type"], "command_ack");
    assert_eq!(ack["camera_id"], "cam-1");
    assert_eq!(ack["action"], "ptz");
    assert_eq!(ack["success"], true);
    assert_eq!(ack["message"], "pan_left ok");
    // Exactly five fields — no surprise additions.
    let obj = ack.as_object().unwrap();
    assert_eq!(obj.len(), 5);
}

#[test]
fn build_ack_failure_shape() {
    let ack = build_ack("cam-1", "ptz", false, "unknown direction 'zzz'");
    assert_eq!(ack["success"], false);
    assert!(ack["message"].as_str().unwrap().contains("zzz"));
}

// ---- V4l2CtlPtz against FakeV4l2CtlRunner ---------------------------

const RELATIVE_CTRLS: &str = "
                   pan_relative 0x009a0904 (int) : min=-1 max=1 step=1 default=0 value=0
                  tilt_relative 0x009a0905 (int) : min=-1 max=1 step=1 default=0 value=0
";

const ABSOLUTE_CTRLS: &str = "
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=0
";

const NO_PTZ_CTRLS: &str = "
                       brightness 0x00980900 (int) : min=0 max=255 step=1 default=128 value=128
";

fn cfg() -> PtzConfig {
    PtzConfig::default()
}

fn cfg_inverted() -> PtzConfig {
    PtzConfig {
        invert_pan: true,
        invert_tilt: true,
        ..PtzConfig::default()
    }
}

fn make_ptz(ctrls: &str, cfg: PtzConfig) -> V4l2CtlPtz<FakeV4l2CtlRunner> {
    let parsed = parse_list_ctrls(ctrls);
    V4l2CtlPtz::new(
        FakeV4l2CtlRunner::new(),
        "/dev/video0".to_string(),
        &parsed,
        &cfg,
    )
}

// We hold the FakeV4l2CtlRunner in an `Arc` so the test can inspect
// captured args while the V4l2CtlPtz also owns a clone for issuing
// commands. The `impl V4l2CtlRunner for Arc<R>` blanket lives in the lib.
struct Harness {
    ptz: V4l2CtlPtz<Arc<FakeV4l2CtlRunner>>,
    runner: Arc<FakeV4l2CtlRunner>,
}

fn harness(ctrls: &str, cfg: PtzConfig) -> Harness {
    let parsed = parse_list_ctrls(ctrls);
    let runner = Arc::new(FakeV4l2CtlRunner::new());
    let ptz = V4l2CtlPtz::new(runner.clone(), "/dev/video0".to_string(), &parsed, &cfg);
    Harness { ptz, runner }
}

#[tokio::test]
async fn relative_pan_left_invokes_pan_relative_negative_one() {
    let h = harness(RELATIVE_CTRLS, cfg());
    h.ptz.pan(PanDir::Left).await.unwrap();
    let calls = h.runner.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        vec![
            "-d".to_string(),
            "/dev/video0".to_string(),
            "--set-ctrl=pan_relative=-1".to_string()
        ]
    );
}

#[tokio::test]
async fn relative_pan_right_invokes_pan_relative_positive_one() {
    let h = harness(RELATIVE_CTRLS, cfg());
    h.ptz.pan(PanDir::Right).await.unwrap();
    let calls = h.runner.captured();
    assert_eq!(calls[0][2], "--set-ctrl=pan_relative=1");
}

#[tokio::test]
async fn relative_tilt_up_invokes_tilt_relative_positive_one() {
    let h = harness(RELATIVE_CTRLS, cfg());
    h.ptz.tilt(TiltDir::Up).await.unwrap();
    let calls = h.runner.captured();
    assert_eq!(calls[0][2], "--set-ctrl=tilt_relative=1");
}

#[tokio::test]
async fn relative_tilt_down_invokes_tilt_relative_negative_one() {
    let h = harness(RELATIVE_CTRLS, cfg());
    h.ptz.tilt(TiltDir::Down).await.unwrap();
    let calls = h.runner.captured();
    assert_eq!(calls[0][2], "--set-ctrl=tilt_relative=-1");
}

#[tokio::test]
async fn absolute_pan_left_writes_negative_step() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    h.ptz.pan(PanDir::Left).await.unwrap();
    assert_eq!(h.runner.captured()[0][2], "--set-ctrl=pan_absolute=-3600");
}

#[tokio::test]
async fn absolute_pan_left_twice_tracks_position() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    h.ptz.pan(PanDir::Left).await.unwrap();
    h.ptz.pan(PanDir::Left).await.unwrap();
    let calls = h.runner.captured();
    // First call: 0 - 3600 = -3600. Second call: -3600 - 3600 = -7200.
    assert_eq!(calls[0][2], "--set-ctrl=pan_absolute=-3600");
    assert_eq!(calls[1][2], "--set-ctrl=pan_absolute=-7200");
}

#[tokio::test]
async fn absolute_pan_clamps_at_min() {
    // Range is min=-36000. Default pan_step is 3600. After 11 lefts we reach
    // -39600 which should clamp to -36000.
    let h = harness(ABSOLUTE_CTRLS, cfg());
    for _ in 0..11 {
        h.ptz.pan(PanDir::Left).await.unwrap();
    }
    let last = h.runner.captured().last().unwrap()[2].clone();
    assert_eq!(last, "--set-ctrl=pan_absolute=-36000");
}

#[tokio::test]
async fn absolute_tilt_up_uses_tilt_step_not_pan_step() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    h.ptz.tilt(TiltDir::Up).await.unwrap();
    // Default tilt_step is 1800.
    assert_eq!(h.runner.captured()[0][2], "--set-ctrl=tilt_absolute=1800");
}

#[tokio::test]
async fn invert_pan_flips_direction() {
    let h = harness(RELATIVE_CTRLS, cfg_inverted());
    h.ptz.pan(PanDir::Left).await.unwrap();
    // pan_left with invert_pan=true → pan_relative=+1
    assert_eq!(h.runner.captured()[0][2], "--set-ctrl=pan_relative=1");
}

#[tokio::test]
async fn invert_tilt_flips_direction() {
    let h = harness(RELATIVE_CTRLS, cfg_inverted());
    h.ptz.tilt(TiltDir::Up).await.unwrap();
    // tilt_up with invert_tilt=true → tilt_relative=-1
    assert_eq!(h.runner.captured()[0][2], "--set-ctrl=tilt_relative=-1");
}

#[tokio::test]
async fn home_writes_zero_to_both_absolute_axes() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    h.ptz.home().await.unwrap();
    let calls = h.runner.captured();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][2], "--set-ctrl=pan_absolute=0,tilt_absolute=0");
}

#[tokio::test]
async fn home_resets_tracked_position() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    h.ptz.pan(PanDir::Right).await.unwrap();
    h.ptz.pan(PanDir::Right).await.unwrap();
    h.ptz.home().await.unwrap();
    h.ptz.pan(PanDir::Right).await.unwrap();
    let calls = h.runner.captured();
    // After home, the next pan_right starts from 0 again.
    assert_eq!(calls.last().unwrap()[2], "--set-ctrl=pan_absolute=3600");
}

#[tokio::test]
async fn home_on_relative_only_returns_unsupported() {
    let h = harness(RELATIVE_CTRLS, cfg());
    let err = h.ptz.home().await.unwrap_err();
    assert!(matches!(err, PtzError::Unsupported("home")));
    // Runner should not have been called.
    assert!(h.runner.captured().is_empty());
}

#[tokio::test]
async fn pan_on_no_ptz_returns_unsupported() {
    let h = harness(NO_PTZ_CTRLS, cfg());
    let err = h.ptz.pan(PanDir::Left).await.unwrap_err();
    assert!(matches!(err, PtzError::Unsupported("pan")));
    assert!(h.runner.captured().is_empty());
}

#[tokio::test]
async fn runner_error_propagates_as_v4l2_error() {
    let parsed = parse_list_ctrls(RELATIVE_CTRLS);
    let runner = Arc::new(FakeV4l2CtlRunner::new());
    runner.push(Err(PtzError::V4l2("device busy".to_string())));
    let ptz = V4l2CtlPtz::new(runner.clone(), "/dev/video0".to_string(), &parsed, &cfg());
    let err = ptz.pan(PanDir::Left).await.unwrap_err();
    assert!(matches!(err, PtzError::V4l2(s) if s.contains("device busy")));
}

#[tokio::test]
async fn capabilities_match_parsed_controls_for_absolute() {
    let h = harness(ABSOLUTE_CTRLS, cfg());
    let c = h.ptz.capabilities();
    assert!(c.pan);
    assert!(c.tilt);
    assert!(c.home);
}

#[tokio::test]
async fn capabilities_match_parsed_controls_for_relative_only() {
    let h = harness(RELATIVE_CTRLS, cfg());
    let c = h.ptz.capabilities();
    assert!(c.pan);
    assert!(c.tilt);
    assert!(!c.home);
}

#[tokio::test]
async fn make_ptz_helper_smoke() {
    // Sanity that the simpler constructor compiles and yields expected caps.
    let p = make_ptz(RELATIVE_CTRLS, cfg());
    assert!(p.capabilities().pan);
}
