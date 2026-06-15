"""
Floor Monitor — Camera Client (Python)

Captures frames from a local webcam or RTSP network camera and streams
them to the floor-monitor server via WebSocket.

Configuration is read from camera.toml (shared format with the Rust client).

Usage:
    python camera_client.py [path/to/camera.toml]
"""

from __future__ import annotations

import base64
import json
import logging
import os
import sys
import time
from io import BytesIO
from pathlib import Path
from typing import Any
from urllib.parse import quote, urlparse, urlunparse

import cv2
import toml
import websockets.sync.client as ws_sync
from PIL import Image

try:
    from onvif import ONVIFCamera
except ImportError:  # pragma: no cover - exercised when optional dependency is absent
    ONVIFCamera = None

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("camera-client")


def load_config(path: str) -> dict:
    """Load camera.toml configuration."""
    with open(path, encoding="utf-8") as f:
        return toml.load(f)


def open_camera(cfg: dict, ptz_controller: Any | None = None) -> cv2.VideoCapture:
    """Open camera based on configuration."""
    cam_cfg = cfg["camera"]
    source_type = cam_cfg.get("source_type", "local")

    if source_type == "rtsp":
        if cam_cfg.get("rtsp_from_onvif_profile", False):
            if ptz_controller is None:
                raise RuntimeError(
                    "[camera] rtsp_from_onvif_profile requires [onvif].enabled = true"
                )
            url = _rtsp_uri_with_credentials(
                ptz_controller.get_stream_uri(),
                ptz_controller.username,
                ptz_controller.password,
            )
        else:
            url = cam_cfg["rtsp_url"]
        os.environ.setdefault("OPENCV_FFMPEG_CAPTURE_OPTIONS", "rtsp_transport;tcp")
        log.info("Opening RTSP camera: %s", url.split("@")[-1] if "@" in url else url)
        cap = cv2.VideoCapture(url, cv2.CAP_FFMPEG)
    else:
        idx = int(cam_cfg.get("device_index", 0))
        log.info("Opening local camera index %d", idx)
        cap = cv2.VideoCapture(idx)

    if not cap.isOpened():
        raise RuntimeError(f"Failed to open camera (source_type={source_type})")

    ret, frame = cap.read()
    if ret:
        h, w = frame.shape[:2]
        log.info("Camera opened: %dx%d", w, h)
    else:
        log.warning("Camera opened but test frame read failed")

    return cap


def reopen_camera_capture(
    cap: cv2.VideoCapture,
    cfg: dict,
    ptz_controller: Any | None = None,
    reason: str = "requested",
) -> cv2.VideoCapture:
    log.info("Reopening camera capture: %s", reason)
    cap.release()
    return open_camera(cfg, ptz_controller)


def grab_frame(cap: cv2.VideoCapture, flush_frames: int = 0) -> Image.Image | None:
    """Read a frame and convert to PIL Image."""
    grabbed = False
    for _ in range(max(0, flush_frames)):
        if not cap.grab():
            break
        grabbed = True
    if grabbed:
        ret, frame = cap.retrieve()
    else:
        ret, frame = cap.read()
    if not ret:
        return None
    rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
    return Image.fromarray(rgb)


def should_reopen_camera(consecutive_failures: int, threshold: int) -> bool:
    """Return true when repeated read failures should trigger a reconnect."""
    return threshold > 0 and consecutive_failures >= threshold


def resize_image(img: Image.Image, max_dim: int) -> Image.Image:
    """Resize preserving aspect ratio."""
    if max(img.size) > max_dim:
        img = img.copy()
        img.thumbnail((max_dim, max_dim), Image.Resampling.LANCZOS)
    return img


def encode_jpeg(img: Image.Image, quality: int = 85) -> bytes:
    """Encode PIL Image as JPEG bytes."""
    buf = BytesIO()
    img.save(buf, format="JPEG", quality=quality)
    return buf.getvalue()


class OnvifPtzController:
    """Synchronous ONVIF PTZ controller used by the Python camera client."""

    def __init__(self, cfg: dict):
        if ONVIFCamera is None:
            raise RuntimeError(
                "ONVIF PTZ is enabled but onvif-zeep is not installed; "
                "run `pip install -r camera/python/requirements.txt`"
            )

        onvif_cfg = cfg.get("onvif", {})
        self.onvif_cfg = onvif_cfg
        raw_host = str(onvif_cfg.get("host") or cfg["camera"].get("onvif_host", ""))
        if not raw_host:
            raise ValueError("[onvif] host is required when ONVIF PTZ is enabled")

        self.scheme, hostname, parsed_port = _parse_onvif_host(raw_host, onvif_cfg)
        self.host = f"https://{hostname}" if self.scheme == "https" else hostname
        default_port = 443 if self.scheme == "https" else 80
        self.port = int(onvif_cfg.get("port") or parsed_port or default_port)
        self.username = str(onvif_cfg.get("username", ""))
        password_env = onvif_cfg.get("password_env")
        if password_env and os.environ.get(str(password_env)):
            self.password = os.environ[str(password_env)]
        else:
            self.password = str(onvif_cfg.get("password", ""))

        if not self.username or not self.password:
            raise ValueError("[onvif] username and password are required")

        self.profile_index = int(onvif_cfg.get("profile_index", 0))
        self.move_seconds = float(onvif_cfg.get("move_seconds", 0.35))
        self.pan_speed = _clamp_speed(float(onvif_cfg.get("pan_speed", 0.35)))
        self.tilt_speed = _clamp_speed(float(onvif_cfg.get("tilt_speed", 0.35)))
        self.invert_pan = bool(onvif_cfg.get("invert_pan", False))
        self.invert_tilt = bool(onvif_cfg.get("invert_tilt", False))
        self.patrol_steps = max(1, int(onvif_cfg.get("patrol_steps", 3)))
        self.patrol_pause_seconds = max(0.0, float(onvif_cfg.get("patrol_pause_seconds", 0.15)))
        self.configured_velocity_space = str(onvif_cfg.get("velocity_space", ""))
        self.velocity_space = self.configured_velocity_space

        self._connect()

    def _connect(self):
        wsdl_dir = self.onvif_cfg.get("wsdl_dir")
        camera_kwargs = {"wsdl_dir": str(wsdl_dir)} if wsdl_dir else {}
        transport = _build_onvif_transport(self.onvif_cfg)
        if transport is not None:
            camera_kwargs["transport"] = transport

        log.info("Connecting ONVIF PTZ: %s:%d", self.host, self.port)
        self.camera = ONVIFCamera(
            self.host,
            self.port,
            self.username,
            self.password,
            **camera_kwargs,
        )
        self.media = self.camera.create_media_service()
        self.ptz = self.camera.create_ptz_service()
        profiles = self.media.GetProfiles()
        if not profiles:
            raise RuntimeError("ONVIF media service returned no profiles")
        if self.profile_index < 0 or self.profile_index >= len(profiles):
            raise ValueError(
                f"[onvif] profile_index={self.profile_index} out of range "
                f"(profiles={len(profiles)})"
            )
        self.profile = profiles[self.profile_index]
        self.profile_token = _profile_token(self.profile)
        if not self.profile_token:
            raise RuntimeError("ONVIF media profile has no token")
        self.velocity_space = self.configured_velocity_space
        if not self.velocity_space:
            ptz_config = getattr(self.profile, "PTZConfiguration", None)
            self.velocity_space = str(
                getattr(ptz_config, "DefaultContinuousPanTiltVelocitySpace", "") or ""
            )
        log.info(
            "ONVIF PTZ ready: profile_index=%d token=%s",
            self.profile_index,
            self.profile_token,
        )

    def _call_with_reconnect(self, label: str, fn):
        try:
            return fn()
        except Exception as e:
            log.warning("ONVIF PTZ %s failed; reconnecting and retrying once: %s", label, e)
            self._connect()
            return fn()

    def move(self, direction: str):
        """Move briefly in one server command direction, then stop."""
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
        self._continuous_move(x, y)
        try:
            time.sleep(max(0.05, self.move_seconds))
        finally:
            self.stop()
        log.info("ONVIF PTZ move completed: %s", direction)

    def patrol(self):
        """Sweep left and right, then roughly return toward the starting view."""
        sequence = (
            ("pan_left", self.patrol_steps),
            ("pan_right", self.patrol_steps * 2),
            ("pan_left", self.patrol_steps),
        )
        for direction, count in sequence:
            for _ in range(count):
                self.move(direction)
                if self.patrol_pause_seconds > 0:
                    time.sleep(self.patrol_pause_seconds)

    def _continuous_move(self, pan: float, tilt: float):
        def send():
            request = self.ptz.create_type("ContinuousMove")
            request.ProfileToken = self.profile_token
            pan_tilt = {"x": pan, "y": tilt}
            if self.velocity_space:
                pan_tilt["space"] = self.velocity_space
            request.Velocity = {"PanTilt": pan_tilt}
            self.ptz.ContinuousMove(request)

        self._call_with_reconnect("ContinuousMove", send)

    def get_stream_uri(self) -> str:
        """Return the RTSP URI for the currently selected ONVIF media profile."""

        def send():
            request = self.media.create_type("GetStreamUri")
            request.StreamSetup = {
                "Stream": "RTP-Unicast",
                "Transport": {"Protocol": "RTSP"},
            }
            request.ProfileToken = self.profile_token
            return self.media.GetStreamUri(request)

        response = self._call_with_reconnect("GetStreamUri", send)
        uri = getattr(response, "Uri", None)
        if uri is None and isinstance(response, dict):
            uri = response.get("Uri")
        if not uri:
            raise RuntimeError("ONVIF media service returned no RTSP URI")
        return str(uri)

    def stop(self):
        def send():
            request = self.ptz.create_type("Stop")
            request.ProfileToken = self.profile_token
            request.PanTilt = True
            request.Zoom = False
            self.ptz.Stop(request)

        self._call_with_reconnect("Stop", send)


def _clamp_speed(value: float) -> float:
    """Clamp ONVIF velocity to the common normalized range."""
    return max(0.05, min(1.0, abs(value)))


def _direction_velocity(
    direction: str,
    pan_speed: float,
    tilt_speed: float,
    invert_pan: bool,
    invert_tilt: bool,
) -> tuple[float, float]:
    x = 0.0
    y = 0.0
    if direction == "pan_left":
        x = -pan_speed
    elif direction == "pan_right":
        x = pan_speed
    elif direction == "tilt_up":
        y = tilt_speed
    elif direction == "tilt_down":
        y = -tilt_speed
    else:
        raise ValueError(f"Unsupported PTZ direction: {direction}")

    if invert_pan:
        x = -x
    if invert_tilt:
        y = -y
    return x, y


def _rtsp_uri_with_credentials(uri: str, username: str, password: str) -> str:
    parsed = urlparse(uri)
    if not username or not password or "@" in parsed.netloc:
        return uri
    userinfo = f"{quote(username, safe='')}:{quote(password, safe='')}@"
    return urlunparse(parsed._replace(netloc=userinfo + parsed.netloc))


def _parse_onvif_host(raw_host: str, onvif_cfg: dict) -> tuple[str, str, int | None]:
    default_scheme = str(onvif_cfg.get("scheme", "http")).lower()
    candidate = raw_host if "://" in raw_host else f"{default_scheme}://{raw_host}"
    parsed = urlparse(candidate)
    scheme = parsed.scheme.lower()
    if scheme not in {"http", "https"}:
        raise ValueError(f"[onvif] scheme must be http or https, got {scheme}")
    if not parsed.hostname:
        raise ValueError(f"[onvif] invalid host: {raw_host}")
    return scheme, parsed.hostname, parsed.port


def _build_onvif_transport(onvif_cfg: dict):
    timeout = float(onvif_cfg.get("timeout_seconds", 10.0))
    verify_tls = bool(onvif_cfg.get("verify_tls", True))
    if timeout <= 0 and verify_tls:
        return None

    import requests
    from zeep.transports import Transport

    session = requests.Session()
    session.headers.update({"Connection": "close"})
    session.verify = verify_tls
    if not verify_tls:
        import urllib3

        urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    return Transport(session=session, timeout=max(1.0, timeout))


def _profile_token(profile: Any) -> str | None:
    return getattr(profile, "token", None) or getattr(profile, "Token", None)


def onvif_enabled(cfg: dict) -> bool:
    return bool(cfg.get("onvif", {}).get("enabled", False))


def build_ptz_controller(cfg: dict) -> OnvifPtzController | None:
    if not onvif_enabled(cfg):
        return None
    try:
        return OnvifPtzController(cfg)
    except Exception as e:
        log.error("ONVIF PTZ disabled: %s", e, exc_info=True)
        return None


def resolve_capabilities(
    configured: list[str],
    ptz_controller: OnvifPtzController | None,
) -> list[str]:
    """Return wire capabilities, adding PTZ only when ONVIF is ready."""
    capabilities = list(dict.fromkeys(configured))
    if ptz_controller is None:
        return capabilities
    for cap in ("ptz", "patrol"):
        if cap not in capabilities:
            capabilities.append(cap)
    return capabilities


def handle_command(
    websocket,
    data: dict,
    camera_id: str,
    ptz_controller: OnvifPtzController | None = None,
) -> bool:
    """Handle a command message from the server."""
    action = data.get("action", "")
    params = data.get("params", {})
    log.info("Received command: action=%s params=%s", action, params)

    success = True
    message = "OK"
    changed_view = False

    if action == "ptz":
        direction = params.get("direction", "")
        if ptz_controller is None:
            success = False
            message = "ONVIF PTZ is not configured or failed to initialize"
        else:
            try:
                ptz_controller.move(direction)
                message = f"PTZ {direction} completed"
                changed_view = True
                log.info(message)
            except Exception as e:
                log.warning("PTZ command failed: %s", e, exc_info=True)
                success = False
                message = f"PTZ {direction} failed: {e}"
    elif action == "patrol":
        if ptz_controller is None:
            success = False
            message = "ONVIF PTZ is not configured or failed to initialize"
        else:
            try:
                ptz_controller.patrol()
                message = "Patrol completed"
                changed_view = True
                log.info(message)
            except Exception as e:
                log.warning("Patrol command failed: %s", e, exc_info=True)
                success = False
                message = f"Patrol failed: {e}"
    else:
        log.warning("Unknown command action: %s", action)
        success = False
        message = f"Unknown action: {action}"

    # Send acknowledgment
    ack = json.dumps({
        "type": "command_ack",
        "camera_id": camera_id,
        "action": action,
        "success": success,
        "message": message,
    })
    try:
        websocket.send(ack)
    except Exception as e:
        log.warning("Failed to send command ack: %s", e)
    return success and changed_view


def run(config_path: str):
    """Main loop: connect to server, stream frames."""
    cfg = load_config(config_path)
    server_cfg = cfg["server"]
    cam_cfg = cfg["camera"]

    ws_url = server_cfg["ws_url"]
    camera_id = cam_cfg["id"]
    camera_name = cam_cfg["name"]
    interval = float(cam_cfg.get("interval", 2.0))
    max_dim = int(cam_cfg.get("max_dimension", 768))
    jpeg_quality = int(cam_cfg.get("jpeg_quality", 85))
    reopen_after_failures = int(cam_cfg.get("reopen_after_failures", 10))
    rtsp_flush_frames = int(cam_cfg.get("rtsp_flush_frames", 0))
    reopen_after_ptz = bool(
        cam_cfg.get("reopen_after_ptz", cam_cfg.get("source_type", "local") == "rtsp")
    )
    ptz_controller = build_ptz_controller(cfg)
    capabilities = resolve_capabilities(cam_cfg.get("capabilities", []), ptz_controller)

    cap = open_camera(cfg, ptz_controller)
    consecutive_frame_failures = 0

    while True:
        try:
            log.info("Connecting to %s ...", ws_url)
            with ws_sync.connect(ws_url) as websocket:
                # Register
                register_msg = json.dumps({
                    "type": "register",
                    "camera_id": camera_id,
                    "name": camera_name,
                    "capabilities": capabilities,
                })
                websocket.send(register_msg)
                resp = websocket.recv(timeout=10)
                log.info("Server response: %s", resp)

                # Frame loop
                frame_no = 0
                while True:
                    t0 = time.time()
                    pil = grab_frame(cap, rtsp_flush_frames)
                    if pil is None:
                        consecutive_frame_failures += 1
                        log.warning(
                            "No frame (%d/%d), retrying...",
                            consecutive_frame_failures,
                            reopen_after_failures,
                        )
                        if should_reopen_camera(consecutive_frame_failures, reopen_after_failures):
                            log.warning(
                                "Reopening camera after %d failed frame reads",
                                consecutive_frame_failures,
                            )
                            try:
                                cap = reopen_camera_capture(
                                    cap,
                                    cfg,
                                    ptz_controller,
                                    "failed frame reads",
                                )
                                consecutive_frame_failures = 0
                            except Exception as e:
                                log.warning("Camera reopen failed: %s", e)
                        time.sleep(0.5)
                        continue

                    consecutive_frame_failures = 0
                    pil = resize_image(pil, max_dim)
                    jpeg = encode_jpeg(pil, jpeg_quality)
                    b64 = base64.b64encode(jpeg).decode("ascii")

                    frame_msg = json.dumps({
                        "type": "frame",
                        "camera_id": camera_id,
                        "jpeg_b64": b64,
                    })
                    websocket.send(frame_msg)
                    frame_no += 1

                    # Wait for the inference result, handling commands that
                    # arrive while the server is still processing this frame.
                    deadline = time.time() + 120
                    while True:
                        remaining = deadline - time.time()
                        if remaining <= 0:
                            log.warning("No response for frame %d: inference timeout", frame_no)
                            break
                        try:
                            result = websocket.recv(timeout=remaining)
                            data = json.loads(result)
                        except Exception as e:
                            log.warning("No response for frame %d: %s", frame_no, e)
                            break

                        if data.get("type") == "result":
                            log.info(
                                "Frame %d: infer=%.2fs — %s",
                                data.get("frame_no", frame_no),
                                data.get("infer_secs", 0),
                                (data.get("text", ""))[:80],
                            )
                            break
                        if data.get("type") == "command":
                            if handle_command(websocket, data, camera_id, ptz_controller) and reopen_after_ptz:
                                try:
                                    cap = reopen_camera_capture(
                                        cap,
                                        cfg,
                                        ptz_controller,
                                        "PTZ command completed",
                                    )
                                    consecutive_frame_failures = 0
                                except Exception as e:
                                    log.warning("Camera reopen after PTZ failed: %s", e)

                    # Check for any pending command messages
                    try:
                        while True:
                            extra = websocket.recv(timeout=0.01)
                            extra_data = json.loads(extra)
                            if extra_data.get("type") == "command":
                                if handle_command(
                                    websocket,
                                    extra_data,
                                    camera_id,
                                    ptz_controller,
                                ) and reopen_after_ptz:
                                    try:
                                        cap = reopen_camera_capture(
                                            cap,
                                            cfg,
                                            ptz_controller,
                                            "PTZ command completed",
                                        )
                                        consecutive_frame_failures = 0
                                    except Exception as e:
                                        log.warning("Camera reopen after PTZ failed: %s", e)
                    except Exception:
                        pass  # no more pending messages

                    # Wait for remaining interval
                    elapsed = time.time() - t0
                    sleep_time = max(0, interval - elapsed)
                    if sleep_time > 0:
                        time.sleep(sleep_time)

        except (ConnectionRefusedError, OSError) as e:
            log.warning("Connection failed: %s — retrying in 5s", e)
            time.sleep(5)
        except KeyboardInterrupt:
            log.info("Shutting down")
            break
        except Exception as e:
            log.error("Unexpected error: %s — retrying in 5s", e, exc_info=True)
            time.sleep(5)

    cap.release()


if __name__ == "__main__":
    config_file = sys.argv[1] if len(sys.argv) > 1 else str(
        Path(__file__).parent.parent / "camera.toml"
    )
    if not os.path.isfile(config_file):
        print(f"Config not found: {config_file}")
        print("Copy camera.toml.example to camera.toml and edit it.")
        sys.exit(1)
    run(config_file)
