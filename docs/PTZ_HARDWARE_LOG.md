# PTZ Hardware Verification Log

This file tracks **manual** end-to-end PTZ verification on real UVC PTZ
webcams. CI cannot help here — the runner has no hardware. Add a row when
you bench-test a camera.

## Matrix

| Camera | OS | v4l2-ctl ver | Mode | Pan | Tilt | Patrol | Notes | Commit | Tester | Date |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Logitech BCC950 | TBD | TBD | relative | ⏳ | ⏳ | ⏳ | TBD | TBD | TBD | TBD |
| Logitech C920 (zoom-only) | TBD | TBD | absolute | n/a | n/a | n/a | should advertise `[]` (regression check) | TBD | TBD | TBD |
| Logitech PTZ Pro 2 | TBD | TBD | TBD | ⏳ | ⏳ | ⏳ | may need cameractrls / uvcdynctrl per KNOWLEDGE.md | TBD | TBD | TBD |
| Mac built-in (negative) | macOS | n/a | NoopPtz | n/a | n/a | n/a | regression check: capabilities `[]` | TBD | TBD | TBD |

Legend: `⏳ pending`, `✅ verified`, `❌ failed`, `n/a` not applicable.

## Per-camera procedure

For each camera, on a Linux host with the camera plugged in:

1. **Confirm device path:**
   ```bash
   ls /dev/video*
   ```
   If the camera is not at `/dev/video0`, set `[ptz] device = "/dev/videoN"`
   in `camera.toml` (or pass via `[camera] device_index = N`).

2. **Inspect controls:**
   ```bash
   v4l2-ctl --device=/dev/video0 --list-ctrls
   ```
   Look for `pan_relative`, `pan_absolute`, `tilt_relative`, `tilt_absolute`,
   `zoom_absolute`. Note which axes are present.

3. **Start the server:** `cd server && cargo run --release`.

4. **Start the Rust camera client:**
   ```bash
   cd camera/rust && cargo run --release -- ../camera.toml
   ```
   In the startup log, look for `PTZ caps advertised: [...]`. It should
   match what `v4l2-ctl --list-ctrls` showed.

5. **Click each direction in the dashboard** at `http://localhost:3456`:
   - `▲` (tilt_up): camera should tilt up.
   - `◀` (pan_left): camera should pan left.
   - `▶` (pan_right): camera should pan right.
   - `▼` (tilt_down): camera should tilt down.
   - `↺` (patrol): camera should sweep left-right-left.

   Each click should produce a log line like:
   ```
   dispatch: action=ptz params={"direction":"pan_left"}
   ```
   on the camera client and a server-side log of the `command_ack`.

6. **Telegram path** (if Telegram is configured):
   Send `pan left` to the bot — should ack within a couple of seconds.

7. **Hardware-gated unit tests** (optional):
   ```bash
   FLOOR_MONITOR_PTZ_HW=1 cargo test --test hw_tests -- --ignored --nocapture
   ```

8. **Record outcome** by adding a row to the Matrix table above with the
   commit hash and your handle.

## Negative-path checks

- **Mac built-in webcam** (or Linux host without v4l-utils): `PTZ caps
  advertised: []` should appear in the startup log; the dashboard's PTZ
  panel still shows up but server-side capability filtering prevents
  commands from being routed.
- **Camera with zoom only (C920)**: should advertise `[]` because no zoom
  intent exists server-side today. Regression check: was previously sent
  zoom commands? It shouldn't be.

## Known caveats

See **KNOWLEDGE.md** § Camera Clients § UVC PTZ for vendor-specific
caveats (BCC950 relative-only, PTZ Pro 2 needs vendor extensions, sign
convention, etc.).
