//! Tests for `[ptz]` and `[ptz.patrol]` block parsing.

use floor_monitor_camera::config::{device_path, Config, PtzConfig};

const MINIMAL_TOML: &str = r#"
[server]
ws_url = "ws://127.0.0.1:3456/ws"

[camera]
id = "cam-1"
name = "Test"
"#;

const FULL_TOML: &str = r#"
[server]
ws_url = "ws://127.0.0.1:3456/ws"

[camera]
id = "cam-2"
name = "PTZ Cam"
device_index = 2
capabilities = ["ptz", "patrol"]

[ptz]
enabled = false
device = "/dev/video7"
pan_step = 1800
tilt_step = 900
invert_pan = true
invert_tilt = true

[ptz.patrol]
sweep_steps = 5
dwell_ms = 1500
return_home = false
"#;

#[test]
fn no_ptz_block_uses_defaults() {
    let cfg: Config = toml::from_str(MINIMAL_TOML).unwrap();
    assert!(cfg.ptz.enabled);
    assert_eq!(cfg.ptz.device, None);
    assert_eq!(cfg.ptz.pan_step, 3600);
    assert_eq!(cfg.ptz.tilt_step, 1800);
    assert!(!cfg.ptz.invert_pan);
    assert!(!cfg.ptz.invert_tilt);
    assert_eq!(cfg.ptz.patrol.sweep_steps, 3);
    assert_eq!(cfg.ptz.patrol.dwell_ms, 800);
    assert!(cfg.ptz.patrol.return_home);
}

#[test]
fn ptz_block_with_overrides_applied() {
    let cfg: Config = toml::from_str(FULL_TOML).unwrap();
    assert!(!cfg.ptz.enabled);
    assert_eq!(cfg.ptz.device.as_deref(), Some("/dev/video7"));
    assert_eq!(cfg.ptz.pan_step, 1800);
    assert_eq!(cfg.ptz.tilt_step, 900);
    assert!(cfg.ptz.invert_pan);
    assert!(cfg.ptz.invert_tilt);
    assert_eq!(cfg.ptz.patrol.sweep_steps, 5);
    assert_eq!(cfg.ptz.patrol.dwell_ms, 1500);
    assert!(!cfg.ptz.patrol.return_home);
}

#[test]
fn capabilities_override_preserved() {
    let cfg: Config = toml::from_str(FULL_TOML).unwrap();
    assert_eq!(cfg.camera.capabilities, vec!["ptz", "patrol"]);
}

#[test]
fn capabilities_default_empty_for_minimal_config() {
    let cfg: Config = toml::from_str(MINIMAL_TOML).unwrap();
    assert!(cfg.camera.capabilities.is_empty());
}

#[test]
fn device_path_falls_back_to_device_index() {
    let cfg: Config = toml::from_str(MINIMAL_TOML).unwrap();
    // device_index defaults to 0
    assert_eq!(device_path(&cfg.camera, &cfg.ptz), "/dev/video0");
}

#[test]
fn device_path_uses_index_when_no_override() {
    let cfg: Config = toml::from_str(
        r#"
[server]
ws_url = "ws://127.0.0.1:3456/ws"

[camera]
id = "c"
name = "n"
device_index = 4
"#,
    )
    .unwrap();
    assert_eq!(device_path(&cfg.camera, &cfg.ptz), "/dev/video4");
}

#[test]
fn device_path_uses_explicit_override() {
    let cfg: Config = toml::from_str(FULL_TOML).unwrap();
    assert_eq!(device_path(&cfg.camera, &cfg.ptz), "/dev/video7");
}

#[test]
fn ptz_config_default_is_enabled() {
    let p = PtzConfig::default();
    assert!(p.enabled);
    assert_eq!(p.pan_step, 3600);
    assert!(!p.invert_pan);
}
