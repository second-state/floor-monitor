//! Integration tests for the `Ptz` trait dispatcher and `handle_command`
//! glue. Uses `FakePtz` to verify trait method invocations and
//! `commands::dispatch` / `commands::build_ack` to verify protocol shape
//! without standing up a real WebSocket.

use floor_monitor_camera::commands::{build_ack, dispatch};
use floor_monitor_camera::ptz::{
    self,
    fake::{FakePtz, PtzCall},
    noop::NoopPtz,
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
