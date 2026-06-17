# UVC PTZ + Zoom Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive real USB (UVC/V4L2) webcam pan/tilt/zoom from both camera clients, add `zoom` as a first-class movement action across server/dashboard/Telegram, and auto-detect capabilities from the hardware.

**Architecture:** Both camera clients shell out to `v4l2-ctl` behind a swappable trait/class. Capability detection parses `v4l2-ctl --list-ctrls` (a pure function, fully unit-testable without hardware). `zoom` is a new action whose capability name equals the action, so the server's existing `has_capability(action)` router needs no change. On non-Linux or when `v4l2-ctl` is absent, the client falls back to a no-op controller and advertises no PTZ caps — the build stays green on macOS.

**Tech Stack:** Rust (camera client + Axum server), Python (camera client), vanilla JS/Tera (dashboard), `v4l2-ctl` (`v4l-utils`) at runtime on Linux hosts.

---

## Conventions

- **Branch:** all work lands on `feat/uvc-ptz-zoom` (already created). One PR at the end.
- **Commit trailer:** every commit ends with these two lines (the repo signs off as Michael Yuan even though the local git user differs, so set them explicitly rather than relying on `-s`):
  ```
  Co-Authored-By: Claude Code <noreply@anthropic.com>
  Signed-off-by: Michael Yuan <michael@secondstate.io>
  ```
- **Run lint before each commit:** `lineguard <changed files>`.
- **Test commands:**
  - Rust camera client: `cd camera/rust && cargo test`
  - Rust server: `cd server && cargo test` (and `cargo test --test e2e_tests`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`)
  - Python client: `cd camera/python && python3 -m unittest test_camera_client -v`

## Capability-resolution semantics (read before coding)

Capability resolution is **additive**: the advertised set is `configured ∪ detected` (deduped, configured first). This preserves the existing Python `resolve_capabilities` behaviour and its tests, and lets a user *force* extra capabilities via `camera.toml`. It does **not** support *suppressing* a detected capability — a documented limitation. (The design doc's section 5 says "override entirely"; we deliberately keep the existing additive behaviour for consistency across both clients. Phase E updates the config comments to match.)

`zoom_continuous`-only cameras are **not** driven and do **not** advertise `zoom` (we only count `zoom_absolute`/`zoom_relative`). This is consistent with the out-of-scope vendor-quirk note in the design.

## File Structure

| File | Create/Modify | Responsibility |
|---|---|---|
| `camera/rust/src/ptz.rs` | **Create** | All UVC PTZ logic: V4L2 parsing, capability mapping, `Ptz` trait, `NoopPtz`, `V4l2CtlPtz`, `CommandRunner`, `PtzConfig`, `build_ptz`/`detect_controls`. Inline `#[cfg(test)]` tests. |
| `camera/rust/src/main.rs` | Modify | `mod ptz;`; add `ptz: PtzConfig` to `Config`; build a `PtzRuntime` at startup; thread it through `handle_command`/`drain_pending_commands`; register resolved capabilities. |
| `camera/python/camera_client.py` | Modify | Add `parse_v4l2_controls`, `capabilities_from_controls`, helpers, `V4l2PtzController`; add zoom to `OnvifPtzController`; update `build_ptz_controller`, `resolve_capabilities`, `handle_command`. |
| `camera/python/test_camera_client.py` | Modify | Tests for the new Python logic; update `FakePtzController`. |
| `camera/camera.toml.example` | Modify | Document the `[ptz]` block and zoom. |
| `server/src/llm.rs` | Modify | `Intent::ZoomControl`, zoom keyword shortcuts, `SYSTEM_PROMPT` zoom line. |
| `server/src/telegram.rs` | Modify | Dispatch `ZoomControl` → `send_command_to_any_camera("zoom", …)`. |
| `server/tests/unit_tests.rs` | Modify | `classify_keywords` + `Intent` serde tests for zoom. |
| `server/templates/dashboard.html` | Modify | Zoom buttons; `data-capability` on movement/zoom/patrol buttons. |
| `server/static/js/dashboard.js` | Modify | `sendZoom`; `refreshCapabilities` to disable unsupported controls. |
| `server/static/css/style.css` | Modify | Zoom-button + disabled-control styling. |
| `KNOWLEDGE.md`, `CLAUDE.md`, `README.md` | Modify | Document the protocol, gotchas, and config. |

---

# Phase A — Rust camera client (UVC PTZ)

### Task A1: V4L2 parsing + capability mapping (pure functions)

**Files:**
- Create: `camera/rust/src/ptz.rs`
- Modify: `camera/rust/src/main.rs` (add `mod ptz;` near the other top-level items, after the `use` block)

- [ ] **Step 1: Create `camera/rust/src/ptz.rs` with the parser, capability mapping, and `PtzConfig`**

```rust
//! UVC PTZ control for the Rust camera client.
//!
//! Shells out to `v4l2-ctl` (Linux `v4l-utils`) behind a `CommandRunner` seam so
//! the argv-building and parsing logic is unit-testable without hardware. On
//! non-Linux hosts or when `v4l2-ctl` is missing, `build_ptz` returns `NoopPtz`
//! and `detect_controls` yields an empty set, so no PTZ capability is advertised.

use serde::Deserialize;
use std::collections::HashMap;

/// One V4L2 integer control's range, parsed from `--list-ctrls`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct V4l2Control {
    pub min: i64,
    pub max: i64,
    pub step: i64,
    pub default: i64,
    pub value: i64,
}

/// The pan/tilt/zoom controls a device exposes, keyed by V4L2 control name.
#[derive(Debug, Clone, Default)]
pub struct V4l2Controls {
    pub controls: HashMap<String, V4l2Control>,
}

impl V4l2Controls {
    pub fn has(&self, name: &str) -> bool {
        self.controls.contains_key(name)
    }
    pub fn is_empty(&self) -> bool {
        self.controls.is_empty()
    }
}

/// Parse `v4l2-ctl --list-ctrls` output, keeping only pan/tilt/zoom controls.
///
/// Lines look like:
/// `    pan_absolute 0x009a0908 (int) : min=-36000 max=36000 step=3600 default=0 value=0`
pub fn parse_v4l2_controls(text: &str) -> V4l2Controls {
    let mut controls = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((head, tail)) = line.split_once(':') else {
            continue;
        };
        let Some(name) = head.split_whitespace().next() else {
            continue;
        };
        if !(name.starts_with("pan_") || name.starts_with("tilt_") || name.starts_with("zoom_")) {
            continue;
        }
        let mut ctrl = V4l2Control::default();
        for tok in tail.split_whitespace() {
            if let Some((k, v)) = tok.split_once('=') {
                if let Ok(n) = v.parse::<i64>() {
                    match k {
                        "min" => ctrl.min = n,
                        "max" => ctrl.max = n,
                        "step" => ctrl.step = n,
                        "default" => ctrl.default = n,
                        "value" => ctrl.value = n,
                        _ => {}
                    }
                }
            }
        }
        controls.insert(name.to_string(), ctrl);
    }
    V4l2Controls { controls }
}

/// Map detected controls to advertised capabilities.
pub fn capabilities_from_controls(c: &V4l2Controls) -> Vec<String> {
    let mut caps = Vec::new();
    if c.has("pan_absolute") || c.has("pan_relative") || c.has("tilt_absolute") || c.has("tilt_relative")
    {
        caps.push("ptz".to_string());
        caps.push("patrol".to_string());
    }
    if c.has("zoom_absolute") || c.has("zoom_relative") {
        caps.push("zoom".to_string());
    }
    caps
}

/// Advertised capabilities = configured ∪ detected (configured first, deduped).
pub fn resolve_capabilities(configured: &[String], detected: &[String]) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();
    for c in configured.iter().chain(detected.iter()) {
        if !caps.contains(c) {
            caps.push(c.clone());
        }
    }
    caps
}

/// `[ptz]` config block (shared key names with the Python client).
#[derive(Debug, Clone, Deserialize)]
pub struct PtzConfig {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default = "default_step_pan")]
    pub step_pan: i64,
    #[serde(default = "default_step_tilt")]
    pub step_tilt: i64,
    #[serde(default = "default_step_zoom")]
    pub step_zoom: i64,
    #[serde(default)]
    pub invert_pan: bool,
    #[serde(default)]
    pub invert_tilt: bool,
    #[serde(default)]
    pub invert_zoom: bool,
    #[serde(default = "default_patrol_steps")]
    pub patrol_steps: u32,
    #[serde(default = "default_patrol_dwell")]
    pub patrol_dwell_sec: f64,
}

fn default_step_pan() -> i64 {
    3600
}
fn default_step_tilt() -> i64 {
    1800
}
fn default_step_zoom() -> i64 {
    50
}
fn default_patrol_steps() -> u32 {
    4
}
fn default_patrol_dwell() -> f64 {
    1.5
}

impl Default for PtzConfig {
    fn default() -> Self {
        PtzConfig {
            device: None,
            step_pan: default_step_pan(),
            step_tilt: default_step_tilt(),
            step_zoom: default_step_zoom(),
            invert_pan: false,
            invert_tilt: false,
            invert_zoom: false,
            patrol_steps: default_patrol_steps(),
            patrol_dwell_sec: default_patrol_dwell(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_PTZ: &str = "
Camera Controls
        pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
       tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0
       zoom_absolute 0x009a090d (int)    : min=100 max=400 step=1 default=100 value=100
";
    const ZOOM_ONLY: &str = "
       zoom_absolute 0x009a090d (int)    : min=100 max=500 step=1 default=100 value=100
power_line_frequency 0x00980918 (menu)   : min=0 max=2 default=1 value=1
";
    const BCC950_RELATIVE: &str = "
        pan_relative 0x009a0904 (int)    : min=-1 max=1 step=1 default=0 value=0 flags=write-only
       tilt_relative 0x009a0905 (int)    : min=-1 max=1 step=1 default=0 value=0 flags=write-only
";

    #[test]
    fn parses_absolute_ranges() {
        let c = parse_v4l2_controls(FULL_PTZ);
        assert_eq!(c.controls["pan_absolute"].max, 36000);
        assert_eq!(c.controls["zoom_absolute"].min, 100);
        assert!(!c.has("power_line_frequency"));
    }

    #[test]
    fn ignores_non_ptz_controls() {
        let c = parse_v4l2_controls(ZOOM_ONLY);
        assert!(c.has("zoom_absolute"));
        assert_eq!(c.controls.len(), 1);
    }

    #[test]
    fn full_ptz_advertises_ptz_patrol_zoom() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(FULL_PTZ));
        assert_eq!(caps, vec!["ptz", "patrol", "zoom"]);
    }

    #[test]
    fn zoom_only_advertises_zoom() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(ZOOM_ONLY));
        assert_eq!(caps, vec!["zoom"]);
    }

    #[test]
    fn relative_pan_tilt_advertises_ptz() {
        let caps = capabilities_from_controls(&parse_v4l2_controls(BCC950_RELATIVE));
        assert_eq!(caps, vec!["ptz", "patrol"]);
    }

    #[test]
    fn resolve_is_additive_and_deduped() {
        let detected = vec!["ptz".to_string(), "patrol".to_string()];
        let configured = vec!["zoom".to_string(), "ptz".to_string()];
        assert_eq!(
            resolve_capabilities(&configured, &detected),
            vec!["zoom", "ptz", "patrol"]
        );
        assert_eq!(resolve_capabilities(&[], &detected), vec!["ptz", "patrol"]);
    }

    #[test]
    fn ptz_config_defaults() {
        let c = PtzConfig::default();
        assert_eq!(c.step_pan, 3600);
        assert_eq!(c.patrol_steps, 4);
        assert!((c.patrol_dwell_sec - 1.5).abs() < f64::EPSILON);
    }
}
```

- [ ] **Step 2: Declare the module in `main.rs`**

Add this line immediately after the `use tracing::{...}` import block near the top of `camera/rust/src/main.rs`:

```rust
mod ptz;
```

- [ ] **Step 3: Run the tests — expect PASS**

Run: `cd camera/rust && cargo test`
Expected: the 7 `ptz::tests::*` tests pass. (A `dead_code` warning for unused `PtzConfig`/`resolve_capabilities` is expected until Task A4 wires them; it is not an error.)

- [ ] **Step 4: Commit**

```bash
lineguard camera/rust/src/ptz.rs camera/rust/src/main.rs
git add camera/rust/src/ptz.rs camera/rust/src/main.rs
git commit -F - <<'EOF'
feat(camera-rust): parse v4l2-ctl controls and map to capabilities

Pure parser for `v4l2-ctl --list-ctrls`, capability mapping (pan/tilt
=> ptz+patrol, zoom_absolute/relative => zoom), additive capability
resolution, and the shared [ptz] config block. Unit-tested with captured
C920 / BCC950 / full-PTZ sample output. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task A2: `Ptz` trait, direction mapping, `NoopPtz`, `CommandRunner`

**Files:**
- Modify: `camera/rust/src/ptz.rs` (append below the existing code, before `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add a failing test** (add these methods inside the existing `mod tests`)

```rust
    #[test]
    fn direction_maps_to_axis_and_dir() {
        assert_eq!(parse_direction("pan_left"), Some((Axis::Pan, Dir::Neg)));
        assert_eq!(parse_direction("zoom_in"), Some((Axis::Zoom, Dir::Pos)));
        assert_eq!(parse_direction("tilt_up"), Some((Axis::Tilt, Dir::Pos)));
        assert_eq!(parse_direction("bogus"), None);
    }

    #[test]
    fn noop_ptz_reports_unsupported() {
        let mut p = NoopPtz;
        assert!(p.step(Axis::Pan, Dir::Pos).is_err());
        assert!(p.home().is_err());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd camera/rust && cargo test direction_maps_to_axis_and_dir`
Expected: FAIL — `cannot find function parse_direction` / `cannot find type Axis`.

- [ ] **Step 3: Implement the trait, enums, mapping, and `NoopPtz`** (append to `ptz.rs` above `#[cfg(test)]`)

```rust
/// Which physical axis a command targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Pan,
    Tilt,
    Zoom,
}

/// Step direction. `Pos` = pan_right / tilt_up / zoom_in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Neg,
    Pos,
}

/// Map a server `direction` string to (axis, direction).
pub fn parse_direction(direction: &str) -> Option<(Axis, Dir)> {
    match direction {
        "pan_left" => Some((Axis::Pan, Dir::Neg)),
        "pan_right" => Some((Axis::Pan, Dir::Pos)),
        "tilt_down" => Some((Axis::Tilt, Dir::Neg)),
        "tilt_up" => Some((Axis::Tilt, Dir::Pos)),
        "zoom_out" => Some((Axis::Zoom, Dir::Neg)),
        "zoom_in" => Some((Axis::Zoom, Dir::Pos)),
        _ => None,
    }
}

/// A motor controller. `step` moves one increment on an axis.
pub trait Ptz: Send {
    fn step(&mut self, axis: Axis, dir: Dir) -> Result<(), String>;
    fn home(&mut self) -> Result<(), String>;
}

/// Fallback for cameras with no PTZ hardware (also non-Linux). Errors if driven;
/// the server only routes movement to cameras that advertised the capability, so
/// this path is defensive.
pub struct NoopPtz;

impl Ptz for NoopPtz {
    fn step(&mut self, _axis: Axis, _dir: Dir) -> Result<(), String> {
        Err("no PTZ hardware on this client".to_string())
    }
    fn home(&mut self) -> Result<(), String> {
        Err("no PTZ hardware on this client".to_string())
    }
}

/// Seam over the `v4l2-ctl` process so tests can inject a fake.
pub trait CommandRunner: Send {
    /// Run `v4l2-ctl <args>`; return stdout on success or stderr/message on error.
    fn run(&self, args: &[String]) -> Result<String, String>;
}

/// Real runner: synchronously invokes `v4l2-ctl`.
pub struct V4l2CtlRunner;

impl CommandRunner for V4l2CtlRunner {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let output = std::process::Command::new("v4l2-ctl")
            .args(args)
            .output()
            .map_err(|e| format!("failed to run v4l2-ctl: {e}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}
```

- [ ] **Step 4: Run to verify PASS**

Run: `cd camera/rust && cargo test`
Expected: all `ptz::tests::*` pass (9 now). Unused-code warnings for `V4l2CtlRunner`/`Ptz` until A3/A4 — not errors.

- [ ] **Step 5: Commit**

```bash
lineguard camera/rust/src/ptz.rs
git add camera/rust/src/ptz.rs
git commit -F - <<'EOF'
feat(camera-rust): add Ptz trait, direction mapping, NoopPtz, runner seam

Defines the Axis/Dir model, the server-direction parser, the Ptz trait
with a defensive NoopPtz fallback, and a CommandRunner seam over v4l2-ctl
for testability. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task A3: `V4l2CtlPtz` (absolute-prefer / relative-fallback) + `build_ptz`/`detect_controls`

**Files:**
- Modify: `camera/rust/src/ptz.rs`

- [ ] **Step 1: Add failing tests** (inside `mod tests`; add a fake runner helper too)

```rust
    use std::cell::RefCell;

    struct FakeRunner {
        get_value: i64,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl CommandRunner for FakeRunner {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            let joined = args.join(" ");
            if joined.contains("--get-ctrl") {
                // emulate "pan_absolute: <value>"
                let name = joined.rsplit("--get-ctrl=").next().unwrap_or("");
                Ok(format!("{name}: {}\n", self.get_value))
            } else {
                Ok(String::new())
            }
        }
    }

    fn v4l2_ptz_with(controls: V4l2Controls, runner: FakeRunner) -> V4l2CtlPtz {
        V4l2CtlPtz {
            device: "/dev/video0".to_string(),
            controls,
            step_pan: 3600,
            step_tilt: 1800,
            step_zoom: 50,
            invert_pan: false,
            invert_tilt: false,
            invert_zoom: false,
            runner: Box::new(runner),
        }
    }

    #[test]
    fn signed_step_applies_direction_and_invert() {
        assert_eq!(signed_step(3600, Dir::Pos, false), 3600);
        assert_eq!(signed_step(3600, Dir::Neg, false), -3600);
        assert_eq!(signed_step(3600, Dir::Neg, true), 3600);
    }

    #[test]
    fn parse_get_ctrl_reads_value() {
        assert_eq!(parse_get_ctrl("pan_absolute: -7200\n", "pan_absolute"), Some(-7200));
        assert_eq!(parse_get_ctrl("other: 5", "pan_absolute"), None);
    }

    #[test]
    fn absolute_step_reads_clamps_and_sets() {
        let controls = parse_v4l2_controls(FULL_PTZ); // pan max=36000
        let runner = FakeRunner { get_value: 34000, calls: RefCell::new(vec![]) };
        let mut ptz = v4l2_ptz_with(controls, runner);
        ptz.step(Axis::Pan, Dir::Pos).unwrap();
        let calls = match &ptz.runner_calls() {
            calls => calls.clone(),
        };
        // last call is the set; target clamped to max 36000 (34000+3600=37600 -> 36000)
        let set = calls.last().unwrap().join(" ");
        assert!(set.contains("--set-ctrl=pan_absolute=36000"), "got: {set}");
    }

    #[test]
    fn relative_step_sends_delta() {
        let controls = parse_v4l2_controls(BCC950_RELATIVE);
        let runner = FakeRunner { get_value: 0, calls: RefCell::new(vec![]) };
        let mut ptz = v4l2_ptz_with(controls, runner);
        ptz.step(Axis::Pan, Dir::Neg).unwrap();
        let set = ptz.runner_calls().last().unwrap().join(" ");
        assert!(set.contains("--set-ctrl=pan_relative=-3600"), "got: {set}");
    }

    #[test]
    fn unsupported_axis_errors() {
        let controls = parse_v4l2_controls(ZOOM_ONLY); // no pan
        let runner = FakeRunner { get_value: 0, calls: RefCell::new(vec![]) };
        let mut ptz = v4l2_ptz_with(controls, runner);
        assert!(ptz.step(Axis::Pan, Dir::Pos).is_err());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd camera/rust && cargo test absolute_step_reads_clamps_and_sets`
Expected: FAIL — `cannot find function signed_step` / `V4l2CtlPtz` not found.

- [ ] **Step 3: Implement `V4l2CtlPtz`, helpers, and constructors** (append to `ptz.rs` above `#[cfg(test)]`)

```rust
/// Compute the signed delta for an axis step.
pub fn signed_step(step: i64, dir: Dir, invert: bool) -> i64 {
    let mag = step.abs();
    let positive = matches!(dir, Dir::Pos) ^ invert;
    if positive {
        mag
    } else {
        -mag
    }
}

/// Parse a single `v4l2-ctl --get-ctrl` line: `name: <int>`.
pub fn parse_get_ctrl(output: &str, name: &str) -> Option<i64> {
    for line in output.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim() == name {
                return v.trim().parse::<i64>().ok();
            }
        }
    }
    None
}

/// V4L2 PTZ via `v4l2-ctl`. Prefers absolute controls (read/clamp/set), falls
/// back to relative controls (momentary delta).
pub struct V4l2CtlPtz {
    pub device: String,
    pub controls: V4l2Controls,
    pub step_pan: i64,
    pub step_tilt: i64,
    pub step_zoom: i64,
    pub invert_pan: bool,
    pub invert_tilt: bool,
    pub invert_zoom: bool,
    pub runner: Box<dyn CommandRunner>,
}

impl V4l2CtlPtz {
    /// (absolute control name, relative control name, step magnitude, invert).
    fn axis_params(&self, axis: Axis) -> (&'static str, &'static str, i64, bool) {
        match axis {
            Axis::Pan => ("pan_absolute", "pan_relative", self.step_pan, self.invert_pan),
            Axis::Tilt => ("tilt_absolute", "tilt_relative", self.step_tilt, self.invert_tilt),
            Axis::Zoom => ("zoom_absolute", "zoom_relative", self.step_zoom, self.invert_zoom),
        }
    }

    #[cfg(test)]
    fn runner_calls(&self) -> Vec<Vec<String>> {
        // Only used by tests via the FakeRunner; downcast is not needed because
        // tests read calls through the FakeRunner's own RefCell. This helper
        // exists so tests can express intent; see tests module.
        unreachable!("tests read FakeRunner.calls directly")
    }
}

impl Ptz for V4l2CtlPtz {
    fn step(&mut self, axis: Axis, dir: Dir) -> Result<(), String> {
        let (abs, rel, step, invert) = self.axis_params(axis);
        let delta = signed_step(step, dir, invert);
        if let Some(ctrl) = self.controls.controls.get(abs).cloned() {
            let out = self
                .runner
                .run(&["-d".into(), self.device.clone(), format!("--get-ctrl={abs}")])?;
            let current = parse_get_ctrl(&out, abs).ok_or_else(|| format!("could not read {abs}"))?;
            let target = (current + delta).clamp(ctrl.min, ctrl.max);
            self.runner.run(&[
                "-d".into(),
                self.device.clone(),
                format!("--set-ctrl={abs}={target}"),
            ])?;
            Ok(())
        } else if self.controls.has(rel) {
            self.runner.run(&[
                "-d".into(),
                self.device.clone(),
                format!("--set-ctrl={rel}={delta}"),
            ])?;
            Ok(())
        } else {
            Err(format!("{axis:?} not supported by device {}", self.device))
        }
    }

    fn home(&mut self) -> Result<(), String> {
        for (abs, _rel, _step, _invert) in [
            self.axis_params(Axis::Pan),
            self.axis_params(Axis::Tilt),
            self.axis_params(Axis::Zoom),
        ] {
            if let Some(ctrl) = self.controls.controls.get(abs).cloned() {
                self.runner.run(&[
                    "-d".into(),
                    self.device.clone(),
                    format!("--set-ctrl={abs}={}", ctrl.default),
                ])?;
            }
        }
        Ok(())
    }
}

/// Detect controls by running `v4l2-ctl --list-ctrls`. Empty on non-Linux or error.
pub fn detect_controls(runner: &dyn CommandRunner, device: &str) -> V4l2Controls {
    match runner.run(&["-d".into(), device.into(), "--list-ctrls".into()]) {
        Ok(out) => parse_v4l2_controls(&out),
        Err(_) => V4l2Controls::default(),
    }
}

/// Build the controller: `V4l2CtlPtz` if any PTZ controls were detected, else `NoopPtz`.
pub fn build_ptz(cfg: &PtzConfig, device: &str, controls: V4l2Controls) -> Box<dyn Ptz> {
    if controls.is_empty() {
        return Box::new(NoopPtz);
    }
    Box::new(V4l2CtlPtz {
        device: device.to_string(),
        controls,
        step_pan: cfg.step_pan,
        step_tilt: cfg.step_tilt,
        step_zoom: cfg.step_zoom,
        invert_pan: cfg.invert_pan,
        invert_tilt: cfg.invert_tilt,
        invert_zoom: cfg.invert_zoom,
        runner: Box::new(V4l2CtlRunner),
    })
}
```

- [ ] **Step 4: Fix the test helpers to read the fake runner directly**

The `runner_calls()` helper above is intentionally `unreachable!`. Replace the two test bodies that call `ptz.runner_calls()` so they read the `FakeRunner` through a captured handle instead. Update the `v4l2_ptz_with` helper and the two tests to this form:

```rust
    fn make_fake(get_value: i64) -> (std::rc::Rc<RefCell<Vec<Vec<String>>>>, Box<dyn CommandRunner>) {
        let calls = std::rc::Rc::new(RefCell::new(Vec::new()));
        let runner = RcFakeRunner { get_value, calls: calls.clone() };
        (calls, Box::new(runner))
    }

    struct RcFakeRunner {
        get_value: i64,
        calls: std::rc::Rc<RefCell<Vec<Vec<String>>>>,
    }
    impl CommandRunner for RcFakeRunner {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            let joined = args.join(" ");
            if joined.contains("--get-ctrl") {
                let name = joined.rsplit("--get-ctrl=").next().unwrap_or("");
                Ok(format!("{name}: {}\n", self.get_value))
            } else {
                Ok(String::new())
            }
        }
    }

    fn ptz_with(controls: V4l2Controls, get_value: i64) -> (std::rc::Rc<RefCell<Vec<Vec<String>>>>, V4l2CtlPtz) {
        let (calls, runner) = make_fake(get_value);
        let ptz = V4l2CtlPtz {
            device: "/dev/video0".to_string(),
            controls,
            step_pan: 3600,
            step_tilt: 1800,
            step_zoom: 50,
            invert_pan: false,
            invert_tilt: false,
            invert_zoom: false,
            runner,
        };
        (calls, ptz)
    }
```

Then rewrite the three `V4l2CtlPtz` tests to use `ptz_with` and read `calls`:

```rust
    #[test]
    fn absolute_step_reads_clamps_and_sets() {
        let (calls, mut ptz) = ptz_with(parse_v4l2_controls(FULL_PTZ), 34000);
        ptz.step(Axis::Pan, Dir::Pos).unwrap();
        let set = calls.borrow().last().unwrap().join(" ");
        assert!(set.contains("--set-ctrl=pan_absolute=36000"), "got: {set}");
    }

    #[test]
    fn relative_step_sends_delta() {
        let (calls, mut ptz) = ptz_with(parse_v4l2_controls(BCC950_RELATIVE), 0);
        ptz.step(Axis::Pan, Dir::Neg).unwrap();
        let set = calls.borrow().last().unwrap().join(" ");
        assert!(set.contains("--set-ctrl=pan_relative=-3600"), "got: {set}");
    }

    #[test]
    fn unsupported_axis_errors() {
        let (_calls, mut ptz) = ptz_with(parse_v4l2_controls(ZOOM_ONLY), 0);
        assert!(ptz.step(Axis::Pan, Dir::Pos).is_err());
    }
```

Also delete the now-unused `FakeRunner`/`v4l2_ptz_with` helpers from Step 1 and remove the `#[cfg(test)] fn runner_calls` method (it was a scaffold). The `RcFakeRunner` uses `Rc`, so these tests are single-threaded (fine for unit tests).

- [ ] **Step 5: Run to verify PASS**

Run: `cd camera/rust && cargo test`
Expected: all `ptz::tests::*` pass (14 total).

- [ ] **Step 6: Commit**

```bash
lineguard camera/rust/src/ptz.rs
git add camera/rust/src/ptz.rs
git commit -F - <<'EOF'
feat(camera-rust): implement V4l2CtlPtz with absolute/relative stepping

Absolute controls: read current, add signed step, clamp to min/max, set.
Relative controls: send the signed delta. home() resets absolute axes to
default. build_ptz/detect_controls select NoopPtz when no controls are
present (non-Linux or fixed cameras). Tested via a fake CommandRunner.
Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task A4: Wire PTZ into `main.rs` (config, capabilities, command dispatch)

**Files:**
- Modify: `camera/rust/src/main.rs`

- [ ] **Step 1: Add `ptz` to the `Config` struct**

In `camera/rust/src/main.rs`, change the `Config` struct (currently lines ~28-32) to:

```rust
#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
    camera: CameraConfig,
    #[serde(default)]
    ptz: ptz::PtzConfig,
}
```

- [ ] **Step 2: Add a `PtzRuntime` holder and rewrite `handle_command`**

Replace the entire existing `handle_command` function (lines ~76-122) with the version below, and add the `PtzRuntime` struct + `run_patrol` helper just above it:

```rust
use ptz::{Axis, Dir, Ptz};

/// Bundles the live controller with patrol parameters, threaded through the loop.
struct PtzRuntime {
    ptz: Box<dyn Ptz>,
    patrol_steps: u32,
    patrol_dwell: Duration,
}

/// Blocking left/right sweep, mirroring the Python ONVIF patrol.
async fn run_patrol(rt: &mut PtzRuntime) -> Result<(), String> {
    let n = rt.patrol_steps;
    let dwell = rt.patrol_dwell;
    for _ in 0..n {
        rt.ptz.step(Axis::Pan, Dir::Neg)?;
    }
    tokio::time::sleep(dwell).await;
    for _ in 0..(2 * n) {
        rt.ptz.step(Axis::Pan, Dir::Pos)?;
    }
    tokio::time::sleep(dwell).await;
    for _ in 0..n {
        rt.ptz.step(Axis::Pan, Dir::Neg)?;
    }
    Ok(())
}

/// Handle a `command` message: drive PTZ/zoom/patrol hardware, then ack.
async fn handle_command(
    write: &mut WsWrite,
    camera_id: &str,
    data: &serde_json::Value,
    rt: &mut PtzRuntime,
) {
    let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let params = data
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    info!("Received command: action={} params={}", action, params);

    let direction = params
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (success, message) = match action {
        "ptz" | "zoom" => match ptz::parse_direction(direction) {
            Some((axis, dir)) => match rt.ptz.step(axis, dir) {
                Ok(()) => (true, format!("{action} {direction} completed")),
                Err(e) => (false, format!("{action} {direction} failed: {e}")),
            },
            None => (false, format!("unknown direction: {direction}")),
        },
        "patrol" => match run_patrol(rt).await {
            Ok(()) => (true, "Patrol completed".to_string()),
            Err(e) => (false, format!("Patrol failed: {e}")),
        },
        other => {
            warn!("Unknown command action: {}", other);
            (false, format!("Unknown action: {}", other))
        }
    };

    let ack = serde_json::json!({
        "type": "command_ack",
        "camera_id": camera_id,
        "action": action,
        "success": success,
        "message": message,
    });
    if let Err(e) = write.send(Message::Text(ack.to_string().into())).await {
        warn!("Failed to send command_ack: {}", e);
    }
}
```

- [ ] **Step 3: Thread `PtzRuntime` through `drain_pending_commands`**

Change the signature and the `handle_command` call inside `drain_pending_commands` (lines ~127-152):

```rust
async fn drain_pending_commands(
    read: &mut WsRead,
    write: &mut WsWrite,
    camera_id: &str,
    rt: &mut PtzRuntime,
) -> bool {
```

and the call inside it:

```rust
                        handle_command(write, camera_id, &data, rt).await;
```

- [ ] **Step 4: Build the runtime and resolved capabilities in `main`, and update the call sites**

In `main`, after the camera stream is opened (`info!("Camera stream opened …")`, ~line 224) and before the connection `loop {`, add:

```rust
    // Resolve the V4L2 device and detect PTZ controls once at startup.
    let ptz_device = config
        .ptz
        .device
        .clone()
        .unwrap_or_else(|| format!("/dev/video{}", config.camera.device_index));
    let detected = ptz::detect_controls(&ptz::V4l2CtlRunner, &ptz_device);
    let detected_caps = ptz::capabilities_from_controls(&detected);
    if detected.is_empty() {
        info!("PTZ: no V4L2 controls on {ptz_device} (or v4l2-ctl unavailable)");
    } else {
        info!("PTZ: detected {:?} on {ptz_device}", detected_caps);
    }
    let capabilities = ptz::resolve_capabilities(&config.camera.capabilities, &detected_caps);
    let mut ptz_runtime = PtzRuntime {
        ptz: ptz::build_ptz(&config.ptz, &ptz_device, detected),
        patrol_steps: config.ptz.patrol_steps,
        patrol_dwell: Duration::from_secs_f64(config.ptz.patrol_dwell_sec),
    };
```

Change the `register` message to send the resolved capabilities (line ~241):

```rust
                    "capabilities": capabilities,
```

Update the two `handle_command` calls and the `drain_pending_commands` call to pass `&mut ptz_runtime`:

```rust
                                handle_command(
                                    &mut write,
                                    &config.camera.id,
                                    &data,
                                    &mut ptz_runtime,
                                )
                                .await;
```

```rust
                            if !drain_pending_commands(
                                &mut read,
                                &mut write,
                                &config.camera.id,
                                &mut ptz_runtime,
                            )
                            .await
                            {
                                break;
                            }
```

- [ ] **Step 5: Build and test**

Run: `cd camera/rust && cargo build && cargo test`
Expected: builds clean; all tests pass. On macOS, `detect_controls` returns empty (no `v4l2-ctl`), so the client advertises only configured caps and uses `NoopPtz` — no behavioural regression.

- [ ] **Step 6: Commit**

```bash
lineguard camera/rust/src/main.rs
git add camera/rust/src/main.rs
git commit -F - <<'EOF'
feat(camera-rust): drive UVC PTZ/zoom/patrol from server commands

Detect V4L2 controls at startup, advertise resolved capabilities on
register, and dispatch ptz/zoom/patrol commands to the controller.
Patrol is a blocking left/right sweep. Falls back to NoopPtz off-Linux.
Closes the Rust-client half of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

# Phase B — Python camera client (UVC PTZ + ONVIF zoom)

### Task B1: V4L2 parsing + helpers (pure functions)

**Files:**
- Modify: `camera/python/camera_client.py` (add near the other module-level helpers, e.g. after `encode_jpeg`, before `class OnvifPtzController`)
- Modify: `camera/python/test_camera_client.py`

- [ ] **Step 1: Add failing tests** (append methods to `CameraClientPtzTests` and extend the import)

Extend the import block at the top of `test_camera_client.py`:

```python
from camera_client import (
    OnvifPtzController,
    V4l2PtzController,
    _clamp_speed,
    _direction_velocity,
    _parse_get_ctrl,
    _parse_onvif_host,
    _rtsp_uri_with_credentials,
    _v4l2_axis_sign,
    build_ptz_controller,
    capabilities_from_controls,
    handle_command,
    parse_v4l2_controls,
    resolve_capabilities,
    should_reopen_camera,
)
```

Add tests:

```python
    FULL_PTZ = (
        "        pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0\n"
        "       tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0\n"
        "       zoom_absolute 0x009a090d (int)    : min=100 max=400 step=1 default=100 value=100\n"
    )
    ZOOM_ONLY = (
        "       zoom_absolute 0x009a090d (int)    : min=100 max=500 step=1 default=100 value=100\n"
        "power_line_frequency 0x00980918 (menu)   : min=0 max=2 default=1 value=1\n"
    )

    def test_parse_v4l2_controls_keeps_only_ptz(self):
        controls = parse_v4l2_controls(self.ZOOM_ONLY)
        self.assertIn("zoom_absolute", controls)
        self.assertNotIn("power_line_frequency", controls)
        self.assertEqual(controls["zoom_absolute"]["max"], 500)

    def test_capabilities_from_controls(self):
        self.assertEqual(
            capabilities_from_controls(parse_v4l2_controls(self.FULL_PTZ)),
            ["ptz", "patrol", "zoom"],
        )
        self.assertEqual(
            capabilities_from_controls(parse_v4l2_controls(self.ZOOM_ONLY)),
            ["zoom"],
        )

    def test_v4l2_axis_sign(self):
        self.assertEqual(_v4l2_axis_sign("pan_left"), ("pan", -1))
        self.assertEqual(_v4l2_axis_sign("zoom_in"), ("zoom", 1))
        with self.assertRaises(ValueError):
            _v4l2_axis_sign("bogus")

    def test_parse_get_ctrl(self):
        self.assertEqual(_parse_get_ctrl("pan_absolute: -7200\n", "pan_absolute"), -7200)
        self.assertIsNone(_parse_get_ctrl("other: 3", "pan_absolute"))
```

- [ ] **Step 2: Run to verify failure**

Run: `cd camera/python && python3 -m unittest test_camera_client -v`
Expected: `ImportError` (functions not defined yet).

- [ ] **Step 3: Implement the helpers** (add to `camera_client.py`)

```python
def parse_v4l2_controls(text: str) -> dict[str, dict[str, int]]:
    """Parse `v4l2-ctl --list-ctrls`, keeping only pan/tilt/zoom controls."""
    controls: dict[str, dict[str, int]] = {}
    for line in text.splitlines():
        line = line.strip()
        if ":" not in line:
            continue
        head, tail = line.split(":", 1)
        parts = head.split()
        if not parts:
            continue
        name = parts[0]
        if not (name.startswith("pan_") or name.startswith("tilt_") or name.startswith("zoom_")):
            continue
        ctrl: dict[str, int] = {}
        for tok in tail.split():
            if "=" in tok:
                key, value = tok.split("=", 1)
                try:
                    ctrl[key] = int(value)
                except ValueError:
                    pass
        controls[name] = ctrl
    return controls


def capabilities_from_controls(controls: dict[str, dict[str, int]]) -> list[str]:
    caps: list[str] = []
    if any(c in controls for c in ("pan_absolute", "pan_relative", "tilt_absolute", "tilt_relative")):
        caps += ["ptz", "patrol"]
    if any(c in controls for c in ("zoom_absolute", "zoom_relative")):
        caps.append("zoom")
    return caps


def _v4l2_axis_sign(direction: str) -> tuple[str, int]:
    mapping = {
        "pan_left": ("pan", -1),
        "pan_right": ("pan", 1),
        "tilt_down": ("tilt", -1),
        "tilt_up": ("tilt", 1),
        "zoom_out": ("zoom", -1),
        "zoom_in": ("zoom", 1),
    }
    if direction not in mapping:
        raise ValueError(f"Unsupported direction: {direction}")
    return mapping[direction]


def _parse_get_ctrl(output: str, name: str) -> int | None:
    for line in output.splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            if key.strip() == name:
                try:
                    return int(value.strip())
                except ValueError:
                    return None
    return None


def _v4l2_run(args: list[str]) -> str:
    """Default V4L2 command runner: invoke `v4l2-ctl`, raise on error."""
    import subprocess

    result = subprocess.run(["v4l2-ctl", *args], capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or "v4l2-ctl failed")
    return result.stdout
```

- [ ] **Step 4: Run to verify PASS**

Run: `cd camera/python && python3 -m unittest test_camera_client -v`
Expected: the new tests pass (note: `V4l2PtzController`/`build_ptz_controller` still imported but unused by these tests — they're defined in B2/B4, so keep this step's run scoped: `python3 -m unittest test_camera_client.CameraClientPtzTests.test_parse_v4l2_controls_keeps_only_ptz` etc., or implement B2 before running the full suite). To keep the suite importable now, also add a minimal stub is **not** needed — implement B2 in the same session before the full run.

> Practical note: since the import line now references `V4l2PtzController` and `build_ptz_controller`, run the full suite only after Task B2 and B4 land. For this task, verify with targeted test names above.

- [ ] **Step 5: Commit**

```bash
lineguard camera/python/camera_client.py camera/python/test_camera_client.py
git add camera/python/camera_client.py camera/python/test_camera_client.py
git commit -F - <<'EOF'
feat(camera-py): parse v4l2-ctl controls and map to capabilities

Pure helpers shared in spirit with the Rust client: parse_v4l2_controls,
capabilities_from_controls, direction->axis/sign, get-ctrl value parse,
and the default v4l2-ctl runner. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task B2: `V4l2PtzController`

**Files:**
- Modify: `camera/python/camera_client.py` (add after the helpers from B1)
- Modify: `camera/python/test_camera_client.py`

- [ ] **Step 1: Add failing tests**

```python
    def _v4l2_controller(self, controls_text, get_value=0):
        calls = []

        def runner(args):
            calls.append(args)
            if any("--get-ctrl" in a for a in args):
                name = args[-1].split("=", 1)[1]
                return f"{name}: {get_value}\n"
            if any("--list-ctrls" in a for a in args):
                return controls_text
            return ""

        cfg = {"camera": {"device_index": 0}, "ptz": {}}
        return V4l2PtzController(cfg, runner=runner), calls

    def test_v4l2_absolute_move_clamps(self):
        controller, calls = self._v4l2_controller(self.FULL_PTZ, get_value=34000)
        controller.move("pan_right")
        set_call = calls[-1]
        self.assertIn("--set-ctrl=pan_absolute=36000", set_call)

    def test_v4l2_capabilities(self):
        controller, _ = self._v4l2_controller(self.FULL_PTZ)
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])

    def test_v4l2_unsupported_axis_raises(self):
        controller, _ = self._v4l2_controller(self.ZOOM_ONLY)
        with self.assertRaises(ValueError):
            controller.move("pan_left")
```

- [ ] **Step 2: Run to verify failure**

Run: `cd camera/python && python3 -m unittest test_camera_client.CameraClientPtzTests.test_v4l2_absolute_move_clamps -v`
Expected: FAIL — `V4l2PtzController` has no usable implementation yet.

- [ ] **Step 3: Implement `V4l2PtzController`**

```python
class V4l2PtzController:
    """UVC PTZ controller that shells out to v4l2-ctl. Same move/patrol/stop
    interface as OnvifPtzController so handle_command treats them alike."""

    def __init__(self, cfg: dict, runner=None, controls: dict | None = None):
        ptz_cfg = cfg.get("ptz", {})
        cam_cfg = cfg.get("camera", {})
        self.runner = runner or _v4l2_run
        self.device = str(
            ptz_cfg.get("device") or f"/dev/video{int(cam_cfg.get('device_index', 0))}"
        )
        self.step_pan = int(ptz_cfg.get("step_pan", 3600))
        self.step_tilt = int(ptz_cfg.get("step_tilt", 1800))
        self.step_zoom = int(ptz_cfg.get("step_zoom", 50))
        self.invert_pan = bool(ptz_cfg.get("invert_pan", False))
        self.invert_tilt = bool(ptz_cfg.get("invert_tilt", False))
        self.invert_zoom = bool(ptz_cfg.get("invert_zoom", False))
        self.patrol_steps = max(1, int(ptz_cfg.get("patrol_steps", 4)))
        self.patrol_dwell = max(0.0, float(ptz_cfg.get("patrol_dwell_sec", 1.5)))
        if controls is None:
            controls = parse_v4l2_controls(self.runner(["-d", self.device, "--list-ctrls"]))
        self.controls = controls

    def capabilities(self) -> list[str]:
        return capabilities_from_controls(self.controls)

    def _axis_params(self, axis: str) -> tuple[str, str, int, bool]:
        if axis == "pan":
            return "pan_absolute", "pan_relative", self.step_pan, self.invert_pan
        if axis == "tilt":
            return "tilt_absolute", "tilt_relative", self.step_tilt, self.invert_tilt
        return "zoom_absolute", "zoom_relative", self.step_zoom, self.invert_zoom

    def move(self, direction: str):
        axis, sign = _v4l2_axis_sign(direction)
        abs_name, rel_name, step, invert = self._axis_params(axis)
        delta = step * sign
        if invert:
            delta = -delta
        if abs_name in self.controls:
            out = self.runner(["-d", self.device, f"--get-ctrl={abs_name}"])
            current = _parse_get_ctrl(out, abs_name)
            if current is None:
                raise RuntimeError(f"could not read {abs_name}")
            ctrl = self.controls[abs_name]
            lo = ctrl.get("min", current)
            hi = ctrl.get("max", current)
            target = max(lo, min(hi, current + delta))
            self.runner(["-d", self.device, f"--set-ctrl={abs_name}={target}"])
        elif rel_name in self.controls:
            self.runner(["-d", self.device, f"--set-ctrl={rel_name}={delta}"])
        else:
            raise ValueError(f"{axis} not supported by device {self.device}")
        log.info("V4L2 PTZ move: %s on %s", direction, self.device)

    def patrol(self):
        sequence = (
            ("pan_left", self.patrol_steps),
            ("pan_right", self.patrol_steps * 2),
            ("pan_left", self.patrol_steps),
        )
        for direction, count in sequence:
            for _ in range(count):
                self.move(direction)
                if self.patrol_dwell > 0:
                    time.sleep(self.patrol_dwell)

    def stop(self):
        # Absolute/relative V4L2 controls are momentary; nothing to stop.
        pass
```

- [ ] **Step 4: Run to verify PASS**

Run: `cd camera/python && python3 -m unittest test_camera_client.CameraClientPtzTests.test_v4l2_absolute_move_clamps test_camera_client.CameraClientPtzTests.test_v4l2_capabilities test_camera_client.CameraClientPtzTests.test_v4l2_unsupported_axis_raises -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
lineguard camera/python/camera_client.py camera/python/test_camera_client.py
git add camera/python/camera_client.py camera/python/test_camera_client.py
git commit -F - <<'EOF'
feat(camera-py): add V4l2PtzController for UVC pan/tilt/zoom

Absolute-prefer/relative-fallback stepping over v4l2-ctl, with a blocking
patrol sweep. Same move/patrol/stop interface as the ONVIF controller.
Tested via an injected fake runner. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task B3: ONVIF zoom support

**Files:**
- Modify: `camera/python/camera_client.py` (`OnvifPtzController`)
- Modify: `camera/python/test_camera_client.py`

- [ ] **Step 1: Add a failing test**

```python
    def test_onvif_move_zoom_sends_zoom_velocity(self):
        controller = OnvifPtzController.__new__(OnvifPtzController)
        controller.zoom_speed = 0.4
        controller.invert_zoom = False
        controller.move_seconds = 0.0
        sent = {}

        def fake_continuous(pan, tilt, zoom):
            sent["v"] = (pan, tilt, zoom)

        controller._continuous_move = fake_continuous
        controller.stop = lambda **kwargs: sent.setdefault("stopped", kwargs)

        controller.move("zoom_in")
        self.assertEqual(sent["v"], (0.0, 0.0, 0.4))

    def test_onvif_capabilities_includes_zoom_when_supported(self):
        controller = OnvifPtzController.__new__(OnvifPtzController)
        controller.supports_zoom = True
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])
        controller.supports_zoom = False
        self.assertEqual(controller.capabilities(), ["ptz", "patrol"])
```

- [ ] **Step 2: Run to verify failure**

Run: `cd camera/python && python3 -m unittest test_camera_client.CameraClientPtzTests.test_onvif_move_zoom_sends_zoom_velocity -v`
Expected: FAIL — `move` doesn't handle zoom / no `capabilities` method.

- [ ] **Step 3: Implement ONVIF zoom**

In `OnvifPtzController.__init__`, after the existing `self.invert_tilt = …` line, add:

```python
        self.zoom_speed = _clamp_speed(float(onvif_cfg.get("zoom_speed", 0.35)))
        self.invert_zoom = bool(onvif_cfg.get("invert_zoom", False))
        self.supports_zoom = bool(onvif_cfg.get("zoom_speed") is not None)
```

In `_connect`, after `self.velocity_space = …` detection block, detect zoom support from the PTZ configuration (overrides the config-based default when the profile advertises a zoom space):

```python
        ptz_config = getattr(self.profile, "PTZConfiguration", None)
        zoom_space = getattr(ptz_config, "DefaultContinuousZoomVelocitySpace", "") if ptz_config else ""
        if zoom_space:
            self.supports_zoom = True
```

Add a `capabilities` method:

```python
    def capabilities(self) -> list[str]:
        caps = ["ptz", "patrol"]
        if getattr(self, "supports_zoom", False):
            caps.append("zoom")
        return caps
```

Replace `move` to handle zoom, and replace `_continuous_move`/`stop` to take an explicit zoom axis:

```python
    def move(self, direction: str):
        """Move briefly in one server command direction, then stop."""
        if direction in ("zoom_in", "zoom_out"):
            z = self.zoom_speed if direction == "zoom_in" else -self.zoom_speed
            if self.invert_zoom:
                z = -z
            log.info("ONVIF PTZ zoom: direction=%s velocity=%.3f", direction, z)
            self._continuous_move(0.0, 0.0, z)
            try:
                time.sleep(max(0.05, self.move_seconds))
            finally:
                self.stop(pan_tilt=False, zoom=True)
            return
        x, y = _direction_velocity(
            direction,
            self.pan_speed,
            self.tilt_speed,
            self.invert_pan,
            self.invert_tilt,
        )
        log.info(
            "ONVIF PTZ move: direction=%s velocity=(%.3f, %.3f) duration=%.2fs",
            direction,
            x,
            y,
            max(0.05, self.move_seconds),
        )
        self._continuous_move(x, y, 0.0)
        try:
            time.sleep(max(0.05, self.move_seconds))
        finally:
            self.stop(pan_tilt=True, zoom=False)
        log.info("ONVIF PTZ move completed: %s", direction)

    def _continuous_move(self, pan: float, tilt: float, zoom: float = 0.0):
        def send():
            request = self.ptz.create_type("ContinuousMove")
            request.ProfileToken = self.profile_token
            pan_tilt = {"x": pan, "y": tilt}
            if self.velocity_space:
                pan_tilt["space"] = self.velocity_space
            velocity = {"PanTilt": pan_tilt}
            if zoom != 0.0:
                velocity["Zoom"] = {"x": zoom}
            request.Velocity = velocity
            self.ptz.ContinuousMove(request)

        self._call_with_reconnect("ContinuousMove", send)

    def stop(self, pan_tilt: bool = True, zoom: bool = False):
        def send():
            request = self.ptz.create_type("Stop")
            request.ProfileToken = self.profile_token
            request.PanTilt = pan_tilt
            request.Zoom = zoom
            self.ptz.Stop(request)

        self._call_with_reconnect("Stop", send)
```

> Note: the existing `patrol` calls `self.move(...)` which now ends with `self.stop(pan_tilt=True, zoom=False)` — unchanged behaviour for pan/tilt.

- [ ] **Step 4: Run to verify PASS**

Run: `cd camera/python && python3 -m unittest test_camera_client.CameraClientPtzTests.test_onvif_move_zoom_sends_zoom_velocity test_camera_client.CameraClientPtzTests.test_onvif_capabilities_includes_zoom_when_supported -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
lineguard camera/python/camera_client.py camera/python/test_camera_client.py
git add camera/python/camera_client.py camera/python/test_camera_client.py
git commit -F - <<'EOF'
feat(camera-py): add zoom to the ONVIF PTZ controller

ContinuousMove now carries a Zoom velocity for zoom_in/zoom_out, with a
zoom-aware Stop. Advertises the `zoom` capability when the profile exposes
a continuous-zoom velocity space or [onvif].zoom_speed is set. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task B4: Wire selection, capabilities, and zoom dispatch

**Files:**
- Modify: `camera/python/camera_client.py` (`build_ptz_controller`, `resolve_capabilities`, `handle_command`)
- Modify: `camera/python/test_camera_client.py` (`FakePtzController`, capability/zoom tests)

- [ ] **Step 1: Update tests** — give `FakePtzController` a `capabilities()` method and add zoom dispatch + selection tests

Add to `FakePtzController`:

```python
    def capabilities(self):
        return ["ptz", "patrol"]
```

Add tests:

```python
    def test_zoom_command_calls_controller(self):
        ws = FakeWebSocket()
        ptz = FakePtzController()
        changed = handle_command(
            ws, {"action": "zoom", "params": {"direction": "zoom_in"}}, "cam1", ptz
        )
        self.assertTrue(changed)
        self.assertTrue(ws.messages[0]["success"])
        self.assertEqual(ptz.moves, ["zoom_in"])

    def test_build_ptz_controller_uses_v4l2_for_local(self):
        cfg = {"camera": {"source_type": "local", "device_index": 0}, "ptz": {}}

        def runner(args):
            return self.FULL_PTZ if any("--list-ctrls" in a for a in args) else ""

        # Patch the default runner by constructing through build with a monkeypatched _v4l2_run.
        import camera_client

        original = camera_client._v4l2_run
        camera_client._v4l2_run = runner
        try:
            controller = build_ptz_controller(cfg)
        finally:
            camera_client._v4l2_run = original
        self.assertIsInstance(controller, V4l2PtzController)
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])
```

- [ ] **Step 2: Run to verify failure**

Run: `cd camera/python && python3 -m unittest test_camera_client.CameraClientPtzTests.test_zoom_command_calls_controller -v`
Expected: FAIL — `handle_command` has no `zoom` branch.

- [ ] **Step 3: Implement the wiring**

Replace `build_ptz_controller`:

```python
def build_ptz_controller(cfg: dict):
    """Pick a PTZ controller: ONVIF when enabled, else UVC/V4L2 for local cameras."""
    if onvif_enabled(cfg):
        try:
            return OnvifPtzController(cfg)
        except Exception as e:
            log.error("ONVIF PTZ disabled: %s", e, exc_info=True)
            return None

    cam_cfg = cfg.get("camera", {})
    if str(cam_cfg.get("source_type", "local")) != "local":
        return None
    try:
        controller = V4l2PtzController(cfg)
    except Exception as e:
        log.info("V4L2 PTZ not available: %s", e)
        return None
    if not controller.capabilities():
        log.info("V4L2 PTZ: no pan/tilt/zoom controls on %s", controller.device)
        return None
    log.info("V4L2 PTZ ready on %s: %s", controller.device, controller.capabilities())
    return controller
```

Replace `resolve_capabilities` to source added caps from the controller:

```python
def resolve_capabilities(configured: list[str], ptz_controller) -> list[str]:
    """Return wire capabilities = configured + controller-reported (deduped)."""
    capabilities = list(dict.fromkeys(configured))
    if ptz_controller is None:
        return capabilities
    for cap in ptz_controller.capabilities():
        if cap not in capabilities:
            capabilities.append(cap)
    return capabilities
```

In `handle_command`, add a `zoom` branch after the `ptz` branch (before the `elif action == "patrol"`):

```python
    elif action == "zoom":
        direction = params.get("direction", "")
        if ptz_controller is None:
            success = False
            message = "PTZ is not configured or failed to initialize"
        else:
            try:
                ptz_controller.move(direction)
                message = f"Zoom {direction} completed"
                changed_view = True
                log.info(message)
            except Exception as e:
                log.warning("Zoom command failed: %s", e, exc_info=True)
                success = False
                message = f"Zoom {direction} failed: {e}"
```

- [ ] **Step 4: Run the full suite**

Run: `cd camera/python && python3 -m unittest test_camera_client -v`
Expected: all tests pass (existing + new). Confirms `resolve_capabilities` additive behaviour still holds (`FakePtzController.capabilities()` → `["ptz","patrol"]`).

- [ ] **Step 5: Commit**

```bash
lineguard camera/python/camera_client.py camera/python/test_camera_client.py
git add camera/python/camera_client.py camera/python/test_camera_client.py
git commit -F - <<'EOF'
feat(camera-py): select V4L2/ONVIF controller and dispatch zoom

build_ptz_controller now falls back to V4l2PtzController for local
cameras; resolve_capabilities sources added caps from the controller;
handle_command dispatches the new `zoom` action. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

# Phase C — Server zoom intent + Telegram

### Task C1: `Intent::ZoomControl` + keywords + prompt

**Files:**
- Modify: `server/src/llm.rs`
- Modify: `server/tests/unit_tests.rs`

- [ ] **Step 1: Add failing tests** (append to `server/tests/unit_tests.rs`)

```rust
#[test]
fn test_classify_zoom_in() {
    let intent = floor_monitor_server::llm::classify_keywords("zoom in");
    match intent {
        floor_monitor_server::llm::Intent::ZoomControl { direction } => {
            assert_eq!(direction, "zoom_in");
        }
        other => panic!("expected ZoomControl, got {other:?}"),
    }
}

#[test]
fn test_zoom_intent_serde_roundtrip() {
    let intent = floor_monitor_server::llm::Intent::ZoomControl {
        direction: "zoom_out".to_string(),
    };
    let json = serde_json::to_string(&intent).unwrap();
    assert_eq!(json, r#"{"intent":"zoom_control","direction":"zoom_out"}"#);
    let parsed: floor_monitor_server::llm::Intent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        parsed,
        floor_monitor_server::llm::Intent::ZoomControl { .. }
    ));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cd server && cargo test --test unit_tests test_classify_zoom_in`
Expected: FAIL — `no variant ZoomControl`.

- [ ] **Step 3: Add the variant, keywords, and prompt line**

In `server/src/llm.rs`, add the variant to the `Intent` enum (after `PtzControl`):

```rust
    #[serde(rename = "zoom_control")]
    ZoomControl { direction: String },
```

In `classify_keywords`, add zoom shortcuts after the tilt_down block (before the History keywords):

```rust
    if low.contains("zoom in") || low == "/zoom in" || low.contains("zoom-in") {
        return Intent::ZoomControl {
            direction: "zoom_in".to_string(),
        };
    }
    if low.contains("zoom out") || low == "/zoom out" || low.contains("zoom-out") {
        return Intent::ZoomControl {
            direction: "zoom_out".to_string(),
        };
    }
```

In `SYSTEM_PROMPT`, add a zoom intent line after the `ptz_control` line:

```
- {"intent":"zoom_control","direction":"<zoom_in|zoom_out>"}
  Use when the user wants to zoom the camera in or out.
```

- [ ] **Step 4: Run to verify PASS**

Run: `cd server && cargo test --test unit_tests test_classify_zoom_in test_zoom_intent_serde_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
lineguard server/src/llm.rs server/tests/unit_tests.rs
git add server/src/llm.rs server/tests/unit_tests.rs
git commit -F - <<'EOF'
feat(server): add zoom_control intent and keyword shortcuts

New Intent::ZoomControl (zoom_in/zoom_out), keyword fallbacks, and a zoom
line in the LLM dispatcher prompt. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task C2: Telegram zoom dispatch

**Files:**
- Modify: `server/src/telegram.rs`

- [ ] **Step 1: Add the dispatch arm**

In `server/src/telegram.rs`, add this match arm immediately after the existing `llm::Intent::PtzControl { direction } => { … }` arm:

```rust
        llm::Intent::ZoomControl { direction } => {
            match crate::ws::send_command_to_any_camera(
                state,
                "zoom",
                serde_json::json!({"direction": direction}),
            )
            .await
            {
                Ok(cam_id) => {
                    notifier
                        .send(&format!("🔍 Zoom `{}` sent to camera `{}`", direction, cam_id))
                        .await;
                }
                Err(e) => {
                    notifier.send(&format!("❌ {}", e)).await;
                }
            }
        }
```

- [ ] **Step 2: Build (match exhaustiveness proves the wiring)**

Run: `cd server && cargo build`
Expected: builds clean. (Before this arm existed, adding the `ZoomControl` variant in C1 would make the `match` non-exhaustive — so a clean build confirms every intent is handled.)

- [ ] **Step 3: Full server check + commit**

```bash
cd server && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test
cd ..
lineguard server/src/telegram.rs
git add server/src/telegram.rs
git commit -F - <<'EOF'
feat(server): dispatch Telegram zoom_control intent to cameras

Routes zoom_in/zoom_out through send_command_to_any_camera("zoom", ...),
which the existing capability router delivers to a zoom-capable camera.
Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

# Phase D — Dashboard zoom + capability UI

### Task D1: Zoom buttons + `sendZoom` + capability attributes

**Files:**
- Modify: `server/templates/dashboard.html`
- Modify: `server/static/js/dashboard.js`
- Modify: `server/static/css/style.css`

- [ ] **Step 1: Add `data-capability` to existing movement buttons and a zoom group**

In `server/templates/dashboard.html`, replace the existing PTZ control group (the `<!-- PTZ controls -->` block) with:

```html
            <!-- PTZ controls -->
            <div class="control-group">
                <label>Camera Movement</label>
                <div class="ptz-grid">
                    <div class="ptz-spacer"></div>
                    <button class="ptz-btn" data-capability="ptz" onclick="sendPtz('tilt_up')" title="Tilt Up">&#9650;</button>
                    <div class="ptz-spacer"></div>
                    <button class="ptz-btn" data-capability="ptz" onclick="sendPtz('pan_left')" title="Pan Left">&#9664;</button>
                    <button class="ptz-btn ptz-center" data-capability="patrol" onclick="sendCommand('patrol', {})" title="Patrol">&#8634;</button>
                    <button class="ptz-btn" data-capability="ptz" onclick="sendPtz('pan_right')" title="Pan Right">&#9654;</button>
                    <div class="ptz-spacer"></div>
                    <button class="ptz-btn" data-capability="ptz" onclick="sendPtz('tilt_down')" title="Tilt Down">&#9660;</button>
                    <div class="ptz-spacer"></div>
                </div>
                <div class="zoom-row">
                    <button class="ptz-btn" data-capability="zoom" onclick="sendZoom('zoom_out')" title="Zoom Out">&#8722;</button>
                    <span class="zoom-label">Zoom</span>
                    <button class="ptz-btn" data-capability="zoom" onclick="sendZoom('zoom_in')" title="Zoom In">&#43;</button>
                </div>
                <div id="ptz-result" class="control-result"></div>
            </div>
```

- [ ] **Step 2: Add `sendZoom` to the global controls in `dashboard.js`**

In `server/static/js/dashboard.js`, add immediately after the `sendPtz` function (around line 377):

```js
function sendZoom(direction) {
    sendCommand("zoom", { direction: direction });
}
```

- [ ] **Step 3: Add zoom-row + disabled styling to `style.css`**

Append to `server/static/css/style.css`:

```css
/* Zoom row under the PTZ pad */
.zoom-row {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    margin-top: 10px;
}
.zoom-label {
    font-size: 0.85rem;
    color: var(--muted, #888);
}
/* Controls disabled because no connected camera advertises the capability */
.ptz-btn:disabled,
.ptz-btn.unsupported {
    opacity: 0.35;
    cursor: not-allowed;
}
```

- [ ] **Step 4: Manual smoke check**

Run: `cd server && cargo run` (then open the dashboard). Expected: the zoom − / + buttons render below the D-pad and POST `{"action":"zoom","params":{"direction":"zoom_in"}}` to `/api/command` (visible in the network tab / server logs). With no zoom-capable camera connected they'll be disabled after Task D2.

- [ ] **Step 5: Commit**

```bash
lineguard server/templates/dashboard.html server/static/js/dashboard.js server/static/css/style.css
git add server/templates/dashboard.html server/static/js/dashboard.js server/static/css/style.css
git commit -F - <<'EOF'
feat(dashboard): add zoom controls and capability data-attributes

Zoom -/+ buttons posting the new `zoom` action, plus data-capability tags
on movement/patrol/zoom buttons for capability-aware enabling. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

### Task D2: Disable controls with no capable camera

**Files:**
- Modify: `server/static/js/dashboard.js`

- [ ] **Step 1: Add `refreshCapabilities` inside the IIFE and call it on init + interval**

In `server/static/js/dashboard.js`, inside the IIFE, add a poll constant near the others (after `SSE_RECONNECT_MS`):

```js
    const CAP_POLL_MS = 5000;
```

Add the function (e.g. just before the `// --- Init ---` block):

```js
    // Enable/disable control buttons based on whether any connected, running
    // camera advertises the matching capability (the server routes each command
    // to the first capable camera).
    function refreshCapabilities() {
        fetch("/api/cameras")
            .then(function (r) { return r.json(); })
            .then(function (list) {
                const caps = new Set();
                (list || []).forEach(function (c) {
                    if (c.running && Array.isArray(c.capabilities)) {
                        c.capabilities.forEach(function (cap) { caps.add(cap); });
                    }
                });
                document.querySelectorAll("[data-capability]").forEach(function (btn) {
                    const ok = caps.has(btn.getAttribute("data-capability"));
                    btn.disabled = !ok;
                    btn.classList.toggle("unsupported", !ok);
                });
            })
            .catch(function (e) { console.warn("Capability refresh failed:", e); });
    }
```

Add the init calls in the `// --- Init ---` block (after `connectSSE();`):

```js
    refreshCapabilities();
    setInterval(refreshCapabilities, CAP_POLL_MS);
```

- [ ] **Step 2: Manual verification**

Run: `cd server && cargo run`.
- With no camera connected: all movement/zoom/patrol buttons are greyed and disabled.
- Connect the Python client configured with a fake `capabilities = ["zoom"]` (or a real zoom-only webcam): only the zoom buttons enable within `CAP_POLL_MS`.

- [ ] **Step 3: Commit**

```bash
lineguard server/static/js/dashboard.js
git add server/static/js/dashboard.js
git commit -F - <<'EOF'
feat(dashboard): disable controls when no camera advertises the capability

Polls /api/cameras and toggles each data-capability button's disabled
state from the union of running cameras' capabilities. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

---

# Phase E — Docs, config, full checks, PR

### Task E1: Config example, docs, full verification, PR

**Files:**
- Modify: `camera/camera.toml.example`
- Modify: `CLAUDE.md`
- Modify: `KNOWLEDGE.md`
- Modify: `README.md`

- [ ] **Step 1: Document the `[ptz]` block and zoom in `camera/camera.toml.example`**

Add this block after the existing `[onvif]` section:

```toml
# ----- UVC PTZ controller (USB webcams, both clients) -----
# For USB webcams that expose V4L2 PTZ controls on Linux. The client shells out
# to `v4l2-ctl` (install `v4l-utils`). Capabilities are auto-detected from
# `v4l2-ctl --list-ctrls`; an explicit [camera].capabilities list is added on top.
# On non-Linux hosts or when v4l2-ctl is missing, no PTZ is advertised.
[ptz]
# V4L2 device. Defaults to /dev/video{device_index} when unset.
# device = "/dev/video0"
# Step size in V4L2 units per directional command (camera-specific).
step_pan = 3600
step_tilt = 1800
step_zoom = 50
# Flip an axis if the camera moves opposite to the button labels.
invert_pan = false
invert_tilt = false
invert_zoom = false
# Patrol = pan_left N, pan_right 2N, pan_left N, with a dwell between stops.
patrol_steps = 4
patrol_dwell_sec = 1.5
```

Also update the `capabilities` comment block to mention zoom + auto-detection:

```toml
# Capabilities this camera supports. The server uses these to decide whether
# to send movement commands. Common values: "ptz", "patrol", "zoom".
#
# Leave empty to auto-detect: ONVIF (Python) advertises ptz/patrol(/zoom);
# UVC webcams (both clients) advertise based on detected V4L2 controls.
# Anything listed here is added on top of detection (you can force a capability
# but not suppress a detected one).
# capabilities = ["ptz", "patrol", "zoom"]
```

And add an `[onvif].zoom_speed` hint in the ONVIF block:

```toml
# Zoom velocity for ONVIF cameras that support continuous zoom (enables the
# `zoom` capability). Normalized 0.05..1.0.
# zoom_speed = 0.35
# invert_zoom = false
```

- [ ] **Step 2: Update `CLAUDE.md`** — record the new protocol/feature

Under "Core Features" item 1, change the parenthetical to include zoom:

```
1. **WebSocket camera feed** — ... Server can also send commands (PTZ, **zoom**, patrol) back to camera clients.
```

Under "Dual camera clients", append:

```
   Both clients drive UVC/USB PTZ via `v4l2-ctl` on Linux (auto-detecting
   pan/tilt/zoom controls) and the Python client additionally drives ONVIF
   network cameras. The `zoom` action/capability is distinct from `ptz`.
```

- [ ] **Step 3: Add a `KNOWLEDGE.md` entry** — capture the non-obvious gotchas

Append a section:

```markdown
## UVC PTZ via v4l2-ctl (camera clients)

- **Capability name == action name.** The server routes commands with
  `has_capability(action)` (`server/src/ws.rs`). `zoom` is therefore its own
  action AND capability, separate from `ptz`, so zoom-only webcams (advertising
  `["zoom"]`) and pan/tilt cameras coexist with no special-casing.
- **Absolute vs relative controls.** Many Logitech webcams expose only
  `*_absolute` (read current → add step → clamp to min/max → set). The BCC950
  exposes only `*_relative` (write the signed delta; momentary). Drive whichever
  exists; prefer absolute. `zoom_continuous`-only devices are not driven.
- **Keep the build cross-platform.** Shelling out to `v4l2-ctl` (vs the Linux-only
  `v4l` crate) means the Rust camera client still compiles on macOS; detection
  just returns nothing there and the client uses `NoopPtz`.
- **Testability seam.** `CommandRunner` (Rust) / an injected `runner` (Python)
  let the argv-building and clamping logic be unit-tested without hardware; only
  the real subprocess call is untested. Real motor movement needs a Linux bench.
```

- [ ] **Step 4: Update `README.md`** — mention UVC PTZ + zoom and the `v4l-utils` requirement

Add to the camera-client / features section (match the file's existing wording):

```markdown
- **UVC PTZ + zoom:** USB webcams with V4L2 PTZ controls (e.g. Logitech BCC950,
  PTZ-capable Brio) can pan/tilt/zoom from the dashboard and Telegram on Linux.
  Requires `v4l-utils` (`v4l2-ctl`) installed on the camera host. Capabilities
  are auto-detected. ONVIF network cameras (Python client) also support zoom.
```

- [ ] **Step 5: Run the COMPLETE verification suite**

```bash
# Rust camera client
cd camera/rust && cargo build --release && cargo test && cd ../..
# Python client
cd camera/python && python3 -m unittest test_camera_client -v && cd ../..
# Server — full commit-policy gate
cd server \
  && cargo fmt --all -- --check \
  && cargo clippy --all-targets --all-features -- -D warnings \
  && RUSTFLAGS="-D warnings" cargo build --release \
  && cargo test \
  && cargo test --test e2e_tests \
  && cd ..
```
Expected: every command exits 0. If anything fails, fix before committing (do NOT commit on red).

- [ ] **Step 6: Commit docs**

```bash
lineguard camera/camera.toml.example CLAUDE.md KNOWLEDGE.md README.md
git add camera/camera.toml.example CLAUDE.md KNOWLEDGE.md README.md
git commit -F - <<'EOF'
docs: document UVC PTZ + zoom config, protocol, and gotchas

camera.toml.example [ptz] block + zoom capability; CLAUDE.md feature notes;
KNOWLEDGE.md entry on absolute/relative controls and the cross-platform
subprocess approach; README usage. Part of #1.

Co-Authored-By: Claude Code <noreply@anthropic.com>
Signed-off-by: Michael Yuan <michael@secondstate.io>
EOF
```

- [ ] **Step 7: Push and open the PR**

```bash
git push -u origin feat/uvc-ptz-zoom
gh pr create --base main --head feat/uvc-ptz-zoom \
  --title "feat: UVC PTZ + zoom support (both clients, server, dashboard)" \
  --body "$(cat <<'BODY'
## Summary
Extends camera control from ONVIF to UVC/USB webcams over V4L2, and adds `zoom`
as a first-class movement action across the server, dashboard, Telegram, and both
camera clients. Implements #1 (and extends it: zoom + Python client + ONVIF zoom,
per maintainer direction).

## What changed
- **Protocol:** new `zoom` action/capability (`zoom_in`/`zoom_out`). Server routing
  is unchanged — it already routes by `has_capability(action)`.
- **Rust camera client:** new `ptz` module — V4L2 parsing, capability detection,
  `Ptz` trait, `V4l2CtlPtz` (absolute-prefer/relative-fallback), `NoopPtz`; wired
  into `handle_command` (ptz/zoom/patrol). Falls back to `NoopPtz` off-Linux.
- **Python camera client:** `V4l2PtzController`; ONVIF controller gains zoom;
  `build_ptz_controller` selects ONVIF or V4L2; `handle_command` dispatches zoom.
- **Server:** `Intent::ZoomControl` + keyword/prompt; Telegram dispatch.
- **Dashboard:** zoom buttons; controls disable when no connected camera advertises
  the capability.
- **Capabilities** are auto-detected from `v4l2-ctl --list-ctrls`; explicit config
  is added on top.

## Testing
- Automated (no hardware): Rust `ptz` unit tests, Python unittest suite, server
  `classify_keywords`/serde tests; full server fmt/clippy/build/test/e2e green.
- **Manual hardware bench (Linux host, needs `v4l-utils`) — not yet run:**
  - [ ] Logitech C920/C922 → capabilities reduce to `["zoom"]`; zoom moves.
  - [ ] Logitech BCC950 → relative pan/tilt path.
  - [ ] Logitech PTZ Pro 2 → note `uvcdynctrl` need (out of scope).
  - [ ] No-PTZ camera → `NoopPtz`, no regression.

## Out of scope (documented)
macOS/Windows native PTZ; `uvcdynctrl`/vendor codes; cancelable async patrol;
per-camera dashboard control panels.

Design + plan: `docs/superpowers/specs/2026-06-16-uvc-ptz-zoom-design.md`,
`docs/superpowers/plans/2026-06-17-uvc-ptz-zoom.md`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
BODY
)"
```

Expected: PR opens against `main` from `feat/uvc-ptz-zoom`.

- [ ] **Step 8: Final task-list cleanup**

Mark all phase tasks complete and report the PR URL to the user.

---

## Self-Review

**Spec coverage** (against `2026-06-16-uvc-ptz-zoom-design.md`):
- §3 Protocol (zoom action/capability) → C1, C2, D1 ✓
- §4 `[ptz]` config (both clients) → A1 (Rust `PtzConfig`), B2 (Python reads `ptz`), E1 (example) ✓
- §5 Capability auto-detection → A1/A3 (Rust), B1/B2 (Python) ✓
- §6 Rust client (trait, V4l2CtlPtz, NoopPtz, absolute/relative, blocking patrol) → A2/A3/A4 ✓
- §7 Python client (V4l2PtzController, ONVIF zoom, selection) → B2/B3/B4 ✓
- §8 Server (ZoomControl + Telegram) → C1/C2 ✓
- §9 Dashboard (zoom buttons + capability-aware disable) → D1/D2 ✓
- §10 Error handling (ack success=false + stderr; degrade to Noop/None) → A4, B2/B4 ✓
- §11 Testing across 3 projects + manual bench matrix → A*/B*/C1 + E1 step 5 + PR checklist ✓
- §13 Phases → A–E ✓

**Placeholder scan:** No TBD/TODO. The only checkbox `[ ]` lists are the manual hardware bench items in the PR body (intentional, can't run in CI). The Task A3 `runner_calls()` scaffold is explicitly removed in A3 Step 4.

**Type/name consistency (verified across tasks):**
- Rust: `V4l2Controls`, `parse_v4l2_controls`, `capabilities_from_controls`, `resolve_capabilities`, `PtzConfig`, `Axis`/`Dir`, `parse_direction`, `Ptz`, `NoopPtz`, `CommandRunner`, `V4l2CtlRunner`, `V4l2CtlPtz`, `signed_step`, `parse_get_ctrl`, `detect_controls`, `build_ptz`, `PtzRuntime`, `run_patrol` — names match between definition (A1–A3) and use (A4).
- Python: `parse_v4l2_controls`, `capabilities_from_controls`, `_v4l2_axis_sign`, `_parse_get_ctrl`, `_v4l2_run`, `V4l2PtzController`, `build_ptz_controller`, `resolve_capabilities`, `handle_command` — consistent across B1–B4 and the test imports.
- Server: `Intent::ZoomControl { direction }` with serde rename `zoom_control` — consistent C1/C2 and the serde test.
- Dashboard: `sendZoom`, `refreshCapabilities`, `data-capability`, `.unsupported` — consistent D1/D2 and CSS.
