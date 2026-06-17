# UVC PTZ + Zoom Support — Design

**Date:** 2026-06-16
**Branch:** `feat/uvc-ptz-zoom`
**Tracking issue:** [second-state/floor-monitor#1](https://github.com/second-state/floor-monitor/issues/1)
**Reference work:** ONVIF PTZ in the Python client (commit `a24c18a`, PR #5)

## 1. Goal

Extend real camera control from ONVIF network cameras to **UVC USB webcams** over the
standard V4L2 control interface, and add **zoom** as a first-class movement type across
the whole system.

After this lands:
- A UVC PTZ webcam (Logitech BCC950 / PTZ Pro 2 / Brio) plugged into a host running
  either camera client physically pans, tilts, and zooms from the dashboard and Telegram.
- Cameras auto-advertise only the capabilities their hardware actually exposes.
- Zoom-only webcams (C920/C922) advertise `["zoom"]`; full PTZ webcams advertise
  `["ptz", "patrol", "zoom"]`.

## 2. Scope (decisions locked during brainstorming)

| Decision | Choice |
|---|---|
| Zoom | **Included end-to-end** (protocol, server, dashboard, LLM, Telegram, both clients) |
| Clients | **Both** Rust and Python camera clients get UVC/V4L2 PTZ |
| Capabilities | **Auto-detect** via `v4l2-ctl --list-ctrls`, with explicit `camera.toml` override |
| Patrol | **Blocking sequence** inside the command handler (parity with Python ONVIF) |
| Dashboard | **Disable** a control group when no connected camera advertises that capability |
| ONVIF zoom | **Yes** — the Python ONVIF controller also gains zoom |
| V4L2 targeting | **Prefer absolute** (read → ±step → clamp → set); **fall back to relative** |
| Dependency | **Shell out to `v4l2-ctl`** behind a trait/class (both clients) |

### Out of scope (documented follow-ups)
- macOS / Windows native UVC PTZ (`IOUSBHostInterface`, DirectShow). Non-Linux falls
  back to no-op; the build still compiles everywhere.
- `uvcdynctrl` / Logitech vendor-specific control codes (PTZ Pro 2 quirk). Standard
  V4L2 controls only for this PR; documented as a known limitation.
- Cancelable async patrol; per-camera control panels in the dashboard.
- The `v4l` Rust crate / direct ioctls (kept swappable behind the trait for later).

## 3. Protocol extension

Zoom is modelled as its **own action and capability**, consistent with the existing
`action == capability` routing in `server/src/ws.rs`.

| action | params | capability | directions |
|---|---|---|---|
| `ptz` (existing) | `{ "direction": <d> }` | `ptz` | `pan_left`, `pan_right`, `tilt_up`, `tilt_down` |
| `zoom` (**new**) | `{ "direction": <d> }` | `zoom` | `zoom_in`, `zoom_out` |
| `patrol` (existing) | `{}` | `patrol` | — |

The server already routes commands generically (`has_capability(action)`), so **no
routing/dispatch change is needed** for zoom on the server — only the LLM intent, the
Telegram handler, and the dashboard buttons must learn the new action.

Capability strings a client may advertise: `ptz`, `patrol`, `zoom` (any subset).

## 4. Shared configuration — new `[ptz]` block

Read identically by **both** clients (per the CLAUDE.md shared-config rule). Drives the
V4L2 controller only; ONVIF keeps its existing `[onvif]` block.

```toml
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

`[ptz]` is optional. When absent and the device exposes PTZ controls, sensible defaults
above are used.

## 5. Capability auto-detection (shared logic, per client)

On startup (Linux only), the client runs `v4l2-ctl -d <device> --list-ctrls` and parses
which controls exist plus their `min`/`max`/`step`/`value` for absolute controls.

Detected controls → advertised capabilities:

| Controls present | Capabilities |
|---|---|
| `pan_absolute`/`pan_relative` **or** `tilt_absolute`/`tilt_relative` | `ptz`, `patrol` |
| `zoom_absolute`/`zoom_relative`/`zoom_continuous` | `zoom` |

Resolution rules:
- Explicit `capabilities = [...]` in `[camera]` **overrides** detection entirely
  (force or suppress), matching the existing override semantics.
- On non-Linux, or when `v4l2-ctl` is absent/errors, detection yields nothing and the
  client advertises only what is explicitly configured.

**The parser is a pure function** `parse_v4l2_controls(text) -> Controls` so it is fully
unit-tested with captured sample outputs (C920 zoom-only, BCC950 relative, full PTZ)
without hardware.

## 6. Rust camera client (`camera/rust/src/main.rs`)

### Trait
```rust
enum Axis { Pan, Tilt, Zoom }
enum Dir  { Neg, Pos }   // pan_left/down/zoom_out = Neg; pan_right/up/zoom_in = Pos

trait Ptz: Send {
    fn step(&mut self, axis: Axis, dir: Dir) -> Result<(), String>;
    fn home(&mut self) -> Result<(), String>;
}
```

### Implementations
- **`V4l2CtlPtz`** — holds device path, per-axis step + invert, and the parsed control
  table (which controls exist, min/max for absolute). Per `step`:
  - If an `*_absolute` control exists: `--get-ctrl` current → `current ± step` →
    clamp to `min..=max` → `--set-ctrl`. Tracks position via the camera itself
    (re-reads each time, so it stays correct if moved out-of-band).
  - Else if a `*_relative` control exists: `--set-ctrl <ctrl>=±step` (momentary).
  - `home()` sets absolute controls to their `default`; no-op for relative-only.
  - The argv builder and target math are **pure functions** behind a `CommandRunner`
    seam, so a fake runner records argv in tests; only the real `tokio::process::Command`
    call is untested.
- **`NoopPtz`** — current behavior; selected on non-Linux or no PTZ hardware.

### Wiring
- Build `Box<dyn Ptz>` once at startup from detected controls + `[ptz]` config; thread
  it into the frame/drain loop alongside the writer.
- `handle_command`:
  - `"ptz"` → map direction → (axis, dir) → `ptz.step(...)`.
  - `"zoom"` → map `zoom_in/zoom_out` → `ptz.step(Zoom, ...)`.
  - `"patrol"` → blocking sequence: `pan_left × patrol_steps`, dwell, `pan_right ×
    2·patrol_steps`, dwell, `pan_left × patrol_steps`. Symmetric, returns near start.
  - On hardware error reply `success=false` with the `v4l2-ctl` stderr string.
- Capabilities sent in the `register` message come from detection + override.

## 7. Python camera client (`camera/python/camera_client.py`)

- **New `V4l2PtzController`** mirroring `OnvifPtzController`'s interface: `move(direction)`,
  `patrol()`, `stop()` — plus zoom directions. Shells out to `v4l2-ctl` with the same
  absolute-prefer/relative-fallback logic; argv built by a pure helper for tests.
- **ONVIF gains zoom**: `OnvifPtzController.move` accepts `zoom_in/zoom_out` and sends a
  `Zoom` velocity in `ContinuousMove` (currently PanTilt-only); advertise `zoom` when the
  profile exposes a continuous-zoom velocity space (or `[onvif].zoom_speed` is set).
- **`build_ptz_controller(cfg)`**: `[onvif].enabled` → ONVIF; else local source with
  detected V4L2 controls or a `[ptz]` block → `V4l2PtzController`; else `None`.
- **`resolve_capabilities`** extended to add `zoom` (alongside `ptz`/`patrol`) whenever
  the active controller reports zoom support.
- Shared `parse_v4l2_controls` logic lives in one helper, unit-tested with pytest.

## 8. Server changes

- **`src/llm.rs`** — add `Intent::ZoomControl { direction }` (`zoom_in`/`zoom_out`),
  add a zoom line to `SYSTEM_PROMPT`, and add manual shortcuts (`/zoom in`, `zoom out`).
- **`src/telegram.rs`** — dispatch `ZoomControl` → `send_command_to_any_camera("zoom",
  {direction})`, with a 🔍 confirmation message mirroring the PTZ handler.
- **Routing (`src/ws.rs`)** — unchanged; `zoom` flows through the existing
  capability-matched router.

## 9. Dashboard

- **`templates/dashboard.html`** — add a zoom control group with **+ / −** buttons
  (`sendZoom('zoom_in')` / `sendZoom('zoom_out')`).
- **`static/js/dashboard.js`** — add `sendZoom(d)`; fetch `/api/cameras` (already returns
  `capabilities`) and **disable** the PTZ group, patrol button, and zoom group when no
  connected, running camera advertises the matching capability. Re-evaluate on the SSE
  refresh cycle so controls enable/disable as cameras connect.
- **`static/css/style.css`** — styling for the zoom buttons and a `disabled` group state.

## 10. Error handling

- Camera client: any `v4l2-ctl` non-zero exit → `command_ack` with `success=false` and the
  stderr text. No `unwrap()`; controller construction failures degrade to `NoopPtz`
  (Rust) / `None` (Python), exactly like ONVIF init failure today.
- Server: unchanged — capability-miss already returns a graceful error the dashboard and
  Telegram surface.
- Dashboard: disabled controls prevent most no-capable-camera cases; any residual error
  is shown in the existing `#ptz-result` line.

## 11. Testing

Tests live in **three separate projects** — the `server/` crate, the `camera/rust/`
binary crate, and the Python client — so they go in three places:

- **Camera client — Rust** (`camera/rust/`, inline `#[cfg(test)] mod tests`): the
  camera crate has no test suite today, so add an inline module covering
  `parse_v4l2_controls` over captured samples; direction→axis/dir mapping; absolute
  target = clamp(current±step); capability resolution incl. override; `[ptz]` config
  parse; `NoopPtz` no-ops; `V4l2CtlPtz` argv via a fake `CommandRunner`.
- **Camera client — Python** (`camera/python/`, pytest): `parse_v4l2_controls`,
  direction mapping, capability resolution, and `V4l2PtzController` argv via an injected
  fake runner.
- **Server** (`server/tests/unit_tests.rs`): LLM manual-parse + `Intent` (de)serialization
  for the new `zoom_control` variant.
- Existing server checks — `cargo fmt`/`clippy`/build/`cargo test`/`e2e_tests` — must
  stay green. The camera Rust crate must also build clean and pass its new inline tests.

### Manual hardware bench matrix (issue #1 — CI cannot cover)
- [ ] Logitech C920/C922 — capabilities reduce to `["zoom"]`; dashboard disables pan/tilt
      group when it's the only camera; zoom in/out moves.
- [ ] Logitech BCC950 — relative pan/tilt path; verify wraparound limits.
- [ ] Logitech PTZ Pro 2 — note `uvcdynctrl` fallback need; document.
- [ ] No-PTZ camera — `NoopPtz`; server capability check rejects dispatch (no regression).

## 12. Deviations from issue #1 (with rationale)

- **Zoom is a separate `zoom` action/capability** (issue grouped it under the `Ptz`
  trait). Required because the server routes by `action == capability`; a zoom-only camera
  must match a `zoom` command, not `ptz`.
- **Patrol is a blocking sequence**, not a cancelable async task — parity with the Python
  ONVIF client and far simpler; async cancelation is a documented follow-up.
- **Dashboard disables unsupported controls** instead of per-camera hiding — fits the
  existing global-control model (server picks the first capable camera); per-camera panels
  are a larger UI rework left as follow-up.
- **Both clients**, not Rust-only — the user asked for parity; ONVIF also gains zoom.

## 13. Implementation phases (each a separate, checks-passing commit)

1. **Foundation** — `[ptz]` config (both clients) + shared `parse_v4l2_controls` +
   capability-resolution logic, with full unit tests. No behavior wired yet.
2. **Rust client** — `Ptz` trait, `V4l2CtlPtz`, `NoopPtz`, capability auto-detect on
   register, `handle_command` wiring (ptz/zoom/patrol) + tests.
3. **Python client** — `V4l2PtzController`, ONVIF zoom, `build_ptz_controller` selection,
   `resolve_capabilities` + pytest.
4. **Server** — `Intent::ZoomControl` in `llm.rs` + Telegram dispatch + tests.
5. **Dashboard** — zoom buttons + capability-aware enable/disable + CSS.
6. **Docs & PR** — update `camera.toml.example`, `CLAUDE.md`, `KNOWLEDGE.md`, `README`;
   run the full check suite; open the PR referencing issue #1.
