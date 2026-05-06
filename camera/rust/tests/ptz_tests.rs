//! Integration tests for the `Ptz` trait dispatcher and `handle_command`
//! glue. Uses `FakePtz` to verify trait method invocations and
//! `commands::dispatch` / `commands::build_ack` to verify protocol shape
//! without standing up a real WebSocket.

use floor_monitor_camera::commands::{build_ack, dispatch, CommandCtx};
use floor_monitor_camera::config::{CameraConfig, PatrolConfig, PtzConfig};
use floor_monitor_camera::ptz::{
    self,
    detect::parse_list_ctrls,
    fake::{FakePtz, PtzCall},
    noop::NoopPtz,
    patrol::PatrolHandle,
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

fn fast_patrol_cfg() -> PatrolConfig {
    PatrolConfig {
        sweep_steps: 1,
        dwell_ms: 0,
        return_home: false,
    }
}

/// Build a `CommandCtx` for tests. The patrol slot is held in a
/// caller-owned `Option<PatrolHandle>` so tests can reuse it across
/// successive `dispatch` calls (mimicking the main loop).
fn ctx<'a>(
    ptz: Arc<dyn Ptz>,
    slot: &'a mut Option<PatrolHandle>,
    cfg: &'a PatrolConfig,
) -> CommandCtx<'a> {
    CommandCtx {
        ptz,
        patrol_slot: slot,
        patrol_cfg: cfg,
    }
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
async fn execute_ptz_missing_direction_returns_missing_direction() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let err = ptz::execute_ptz(&p, &json!({})).await.unwrap_err();
    assert!(matches!(err, PtzError::MissingDirection));
    // The message reaching the server's command_ack is actionable.
    assert_eq!(err.to_string(), "missing 'direction' parameter");
}

#[tokio::test]
async fn execute_ptz_non_string_direction_returns_missing_direction() {
    // params.direction is not a string (here a number) — same shape as
    // missing entirely from the client's perspective.
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let err = ptz::execute_ptz(&p, &json!({"direction": 42}))
        .await
        .unwrap_err();
    assert!(matches!(err, PtzError::MissingDirection));
}

// ---- NoopPtz protocol-level behavior ---------------------------------

#[tokio::test]
async fn dispatch_ptz_pan_left_with_noop_acks_success() {
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let mut slot = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "ptz", &json!({"direction": "pan_left"})).await;
    assert!(success);
    assert_eq!(msg, "pan_left ok");
}

#[tokio::test]
async fn dispatch_patrol_without_pan_capability_acks_as_no_op() {
    // NoopPtz reports caps.pan = false. Patrol should ack success without
    // spawning a task — matches the documented fallback behavior for
    // hardware-less clients (macOS, [ptz] enabled = false, failed detection).
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let mut slot = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "patrol", &json!({})).await;
    assert!(success);
    assert!(msg.contains("acknowledged"));
    // No task was spawned — fallback path skips start_patrol entirely.
    assert!(slot.is_none());
}

#[tokio::test]
async fn dispatch_patrol_starts_task_and_acks_started() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let mut slot: Option<PatrolHandle> = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "patrol", &json!({})).await;
    assert!(success);
    assert_eq!(msg, "patrol_started");
    assert!(slot.is_some());
    // Wait for the patrol to finish naturally.
    if let Some(h) = slot.take() {
        h.join().await;
    }
    // sweep_steps=1 → 1 left + 2 right + 1 left = 4 calls.
    assert_eq!(f.calls().len(), 4);
}

#[tokio::test]
async fn dispatch_second_patrol_cancels_first() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let mut slot: Option<PatrolHandle> = None;
    // Slow patrol so we can preempt it.
    let cfg = PatrolConfig {
        sweep_steps: 5,
        dwell_ms: 100_000,
        return_home: false,
    };
    let mut c = ctx(p.clone(), &mut slot, &cfg);
    let _ = dispatch(&mut c, "patrol", &json!({})).await;
    // Drop ctx so we can rebind; the patrol_slot lives across.
    drop(c);
    // Give the first patrol a moment to issue at least one pan.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let first_calls = f.calls().len();
    // Send another patrol — should cancel the previous and start fresh.
    let mut c2 = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c2, "patrol", &json!({})).await;
    drop(c2);
    assert!(success);
    assert_eq!(msg, "patrol_started");
    // Cleanup the second handle so the test exits promptly.
    if let Some(h) = slot.take() {
        h.cancel().await;
    }
    // First patrol got at most a couple of pans before being cancelled,
    // so total calls is bounded.
    let total = f.calls().len();
    assert!(
        total >= first_calls && total < 100,
        "expected bounded calls, got first={} total={}",
        first_calls,
        total
    );
}

// ---- dispatch error paths -------------------------------------------

#[tokio::test]
async fn dispatch_unknown_action_acks_failure() {
    let p: Arc<dyn Ptz> = Arc::new(NoopPtz);
    let mut slot = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "dance", &json!({})).await;
    assert!(!success);
    assert_eq!(msg, "Unknown action: dance");
}

#[tokio::test]
async fn dispatch_ptz_unknown_direction_acks_failure() {
    let f = fake();
    let p: Arc<dyn Ptz> = f.clone();
    let mut slot = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "ptz", &json!({"direction": "diagonal_warp"})).await;
    assert!(!success);
    assert!(msg.contains("diagonal_warp"));
    assert!(f.calls().is_empty());
}

#[tokio::test]
async fn dispatch_ptz_when_trait_fails_acks_failure_with_error_text() {
    let f = fake();
    f.fail_next();
    let p: Arc<dyn Ptz> = f.clone();
    let mut slot = None;
    let cfg = fast_patrol_cfg();
    let mut c = ctx(p, &mut slot, &cfg);
    let (success, msg) = dispatch(&mut c, "ptz", &json!({"direction": "pan_left"})).await;
    assert!(!success);
    assert!(msg.contains("v4l2-ctl") || msg.contains("forced failure"));
    // The trait was still invoked once.
    assert_eq!(f.calls(), vec![PtzCall::Pan(PanDir::Left)]);
}

// ---- Wire format ----------------------------------------------------

#[test]
fn build_ack_has_required_fields() {
    let ack = build_ack("cam-1", "ptz", true, "pan_left ok");
    // Pin the protocol contract (the fields the server's CommandAck
    // deserializer reads), not the implementation's exact JSON shape.
    // The deserializer ignores unknown fields, so future forwards-
    // compatible additions to the ack must not break this test.
    assert_eq!(ack["type"], "command_ack");
    assert_eq!(ack["camera_id"], "cam-1");
    assert_eq!(ack["action"], "ptz");
    assert_eq!(ack["success"], true);
    assert_eq!(ack["message"], "pan_left ok");
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
async fn absolute_writes_are_snapped_to_v4l2_step_lattice() {
    // Hardware reports step=12 (e.g. a degree-based pan with 12-unit
    // increments). Configured pan_step = 3600 (the BCC950 default) is
    // wildly wrong here — the v4l2 driver would reject any write that
    // isn't on the min + N*12 lattice. After snap, the actual --set-ctrl
    // value lands on a legal lattice point.
    const FINE_GRAIN: &str = "
                   pan_absolute 0x009a0908 (int) : min=-180 max=180 step=12 default=0 value=0
                  tilt_absolute 0x009a0909 (int) : min=-90 max=90 step=10 default=0 value=0
";
    let h = harness(FINE_GRAIN, cfg());
    h.ptz.pan(PanDir::Right).await.unwrap();
    let cmd = &h.runner.captured()[0][2];
    // 0 + 3600 clamped to 180, then snapped to nearest min+N*12: 180 is
    // -180 + 30*12 = 180, exactly on lattice.
    assert_eq!(cmd, "--set-ctrl=pan_absolute=180");

    // Tilt: 0 + 1800 clamped to 90, snap to nearest min+N*10: -90 + 18*10 = 90.
    h.ptz.tilt(TiltDir::Up).await.unwrap();
    let cmd2 = &h.runner.captured()[1][2];
    assert_eq!(cmd2, "--set-ctrl=tilt_absolute=90");
}

#[tokio::test]
async fn absolute_writes_do_not_overflow_with_huge_step() {
    // Misconfigured pan_step = i32::MAX. Naive guard + sign*step would
    // overflow i32 (panic in debug, wrap in release). Saturating
    // arithmetic should clamp the intermediate to i32::MAX, then snap
    // pulls it back to range.max (which is on-lattice).
    const NORMAL_RANGE: &str = "
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=0
";
    let huge = PtzConfig {
        pan_step: i32::MAX,
        tilt_step: i32::MAX,
        ..PtzConfig::default()
    };
    let h = harness(NORMAL_RANGE, huge);
    // pan_right: should clamp to max=36000 without panic.
    h.ptz.pan(PanDir::Right).await.unwrap();
    let cmd = &h.runner.captured()[0][2];
    assert_eq!(cmd, "--set-ctrl=pan_absolute=36000");
    // pan_left from 36000: target = 36000 + (-1)*MAX → saturates to MIN
    // → snap clamps to -36000.
    h.ptz.pan(PanDir::Left).await.unwrap();
    let cmd = &h.runner.captured()[1][2];
    assert_eq!(cmd, "--set-ctrl=pan_absolute=-36000");
}

#[tokio::test]
async fn absolute_writes_snap_when_seed_is_off_lattice() {
    // Camera left at value=15 but step=10, so 15 is off-lattice. After
    // a single pan_left (step=3600 cfg), target = 15 - 3600 = -3585,
    // clamped to min=-100, snapped to -100 (which is on-lattice).
    const SEEDED_OFF_LATTICE: &str = "
                   pan_absolute 0x009a0908 (int) : min=-100 max=100 step=10 default=0 value=15
                  tilt_absolute 0x009a0909 (int) : min=-100 max=100 step=10 default=0 value=0
";
    let h = harness(SEEDED_OFF_LATTICE, cfg());
    h.ptz.pan(PanDir::Left).await.unwrap();
    let cmd = &h.runner.captured()[0][2];
    assert_eq!(cmd, "--set-ctrl=pan_absolute=-100");
}

#[tokio::test]
async fn absolute_initial_position_seeded_from_v4l2_value() {
    // Camera left at pan_absolute=14400 from a previous session. First
    // pan_left should write 14400 - 3600 = 10800, NOT -3600 (which would
    // be the case if we assumed the camera starts at 0).
    const PRESET: &str = "
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=14400
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=-3600
";
    let h = harness(PRESET, cfg());
    h.ptz.pan(PanDir::Left).await.unwrap();
    assert_eq!(h.runner.captured()[0][2], "--set-ctrl=pan_absolute=10800");
    h.ptz.tilt(TiltDir::Up).await.unwrap();
    // tilt: -3600 + 1800 (default tilt_step) = -1800
    assert_eq!(h.runner.captured()[1][2], "--set-ctrl=tilt_absolute=-1800");
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
async fn capabilities_zoom_false_when_driver_does_not_implement_zoom() {
    // Camera exposes zoom_absolute (parser would set caps.zoom=true), but
    // V4l2CtlPtz doesn't override Ptz::zoom — the default impl returns
    // Unsupported. capabilities().zoom must reflect that runtime contract,
    // not the parser's data-level inference.
    const PAN_TILT_ZOOM: &str = "
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=0
                  zoom_absolute 0x009a090d (int) : min=100 max=500 step=1 default=100 value=100
";
    let h = harness(PAN_TILT_ZOOM, cfg());
    let c = h.ptz.capabilities();
    assert!(c.pan);
    assert!(c.tilt);
    assert!(
        !c.zoom,
        "zoom must be false: V4l2CtlPtz does not implement Ptz::zoom"
    );
    // Sanity: zoom() actually returns Unsupported.
    use floor_monitor_camera::ptz::ZoomDir;
    let err = h.ptz.zoom(ZoomDir::In).await.unwrap_err();
    assert!(matches!(err, PtzError::Unsupported("zoom")));
}

#[tokio::test]
async fn capabilities_home_false_when_driver_picks_relative_over_absolute() {
    // Both pan_relative AND pan_absolute (and same for tilt) are present
    // in --list-ctrls. AxisCtrl::from_parsed prefers Relative, so
    // home() will return Unsupported even though the parser sees both
    // *_absolute names. capabilities().home must reflect the runtime
    // contract — false here, not true.
    const BOTH_MODES: &str = "
                   pan_relative 0x009a0904 (int) : min=-1 max=1 step=1 default=0 value=0
                  tilt_relative 0x009a0905 (int) : min=-1 max=1 step=1 default=0 value=0
                   pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0
                  tilt_absolute 0x009a0909 (int) : min=-18000 max=18000 step=1800 default=0 value=0
";
    let h = harness(BOTH_MODES, cfg());
    let c = h.ptz.capabilities();
    assert!(c.pan);
    assert!(c.tilt);
    assert!(
        !c.home,
        "home should be false because driver picked Relative mode"
    );
    // Sanity: home() actually returns Unsupported.
    let err = h.ptz.home().await.unwrap_err();
    assert!(matches!(err, PtzError::Unsupported("home")));
}

#[tokio::test]
async fn make_ptz_helper_smoke() {
    // Sanity that the simpler constructor compiles and yields expected caps.
    let p = make_ptz(RELATIVE_CTRLS, cfg());
    assert!(p.capabilities().pan);
}

// ---- build_with_runner factory branches ----------------------------

fn camera_cfg() -> CameraConfig {
    // Minimal config; we only need device_index to exist for the
    // device_path helper inside build_with_runner.
    toml::from_str::<CameraConfig>(
        r#"
id = "test"
name = "Test"
"#,
    )
    .unwrap()
}

#[tokio::test]
async fn build_when_disabled_returns_noop() {
    let runner = FakeV4l2CtlRunner::with_response(RELATIVE_CTRLS);
    let cfg = PtzConfig {
        enabled: false,
        ..PtzConfig::default()
    };
    let ptz = ptz::build_with_runner(&cfg, &camera_cfg(), runner).await;
    // NoopPtz reports all-false capabilities.
    assert!(!ptz.capabilities().pan);
    assert!(!ptz.capabilities().tilt);
}

#[tokio::test]
async fn build_when_runner_errors_returns_noop() {
    let runner = FakeV4l2CtlRunner::with_error(PtzError::V4l2("not found".into()));
    let ptz = ptz::build_with_runner(&PtzConfig::default(), &camera_cfg(), runner).await;
    assert!(!ptz.capabilities().pan);
    assert!(!ptz.capabilities().tilt);
}

#[tokio::test]
async fn build_when_runner_times_out_returns_noop() {
    let runner = FakeV4l2CtlRunner::with_error(PtzError::Timeout);
    let ptz = ptz::build_with_runner(&PtzConfig::default(), &camera_cfg(), runner).await;
    assert!(!ptz.capabilities().pan);
}

#[tokio::test]
async fn build_when_no_pan_or_tilt_detected_returns_noop() {
    // brightness/contrast only — no PTZ controls.
    let runner = FakeV4l2CtlRunner::with_response(
        "
                       brightness 0x00980900 (int) : min=0 max=255 step=1 default=128 value=128
",
    );
    let ptz = ptz::build_with_runner(&PtzConfig::default(), &camera_cfg(), runner).await;
    assert!(!ptz.capabilities().pan);
    assert!(!ptz.capabilities().tilt);
}

#[tokio::test]
async fn build_when_pan_and_tilt_detected_returns_v4l2_ctl_ptz() {
    let runner = FakeV4l2CtlRunner::with_response(RELATIVE_CTRLS);
    let ptz = ptz::build_with_runner(&PtzConfig::default(), &camera_cfg(), runner).await;
    // Capabilities forwarded from V4l2CtlPtz.
    assert!(ptz.capabilities().pan);
    assert!(ptz.capabilities().tilt);
    // Behavior check: a pan call should reach the underlying runner.
    // (We can't easily inspect the runner here because build_with_runner
    // takes ownership, but capabilities being non-default is sufficient
    // proof V4l2CtlPtz was selected — NoopPtz reports all-false.)
}

#[tokio::test]
async fn build_with_only_pan_detected_still_returns_v4l2_ctl_ptz() {
    // pan_relative without tilt: build() admits this (caps.pan || caps.tilt),
    // but advertised() returns [] so the server won't route to it via
    // send_command_to_any_camera. Direct send_camera_command still
    // exercises the controller, where pan succeeds and tilt fails honestly.
    let runner = FakeV4l2CtlRunner::with_response(
        "
                   pan_relative 0x009a0904 (int) : min=-1 max=1 step=1 default=0 value=0
",
    );
    let ptz = ptz::build_with_runner(&PtzConfig::default(), &camera_cfg(), runner).await;
    assert!(ptz.capabilities().pan);
    assert!(!ptz.capabilities().tilt);
}
