"""
Floor Monitor — Camera Client (Python)

Captures frames from a local webcam or RTSP network camera and streams
them to the floor-monitor server via WebSocket.

Configuration is read from camera.toml (shared format with the Rust client).

Usage:
    python camera_client.py [path/to/camera.toml]
"""

import base64
import json
import logging
import os
import sys
import time
from io import BytesIO
from pathlib import Path

import cv2
import toml
import websockets.sync.client as ws_sync
from PIL import Image

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


def open_camera(cfg: dict) -> cv2.VideoCapture:
    """Open camera based on configuration."""
    cam_cfg = cfg["camera"]
    source_type = cam_cfg.get("source_type", "local")

    if source_type == "rtsp":
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


def grab_frame(cap: cv2.VideoCapture) -> Image.Image | None:
    """Read a frame and convert to PIL Image."""
    ret, frame = cap.read()
    if not ret:
        return None
    rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
    return Image.fromarray(rgb)


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


def handle_command(websocket, data: dict, camera_id: str):
    """Handle a command message from the server."""
    action = data.get("action", "")
    params = data.get("params", {})
    log.info("Received command: action=%s params=%s", action, params)

    success = True
    message = "OK"

    if action == "ptz":
        direction = params.get("direction", "")
        log.info("PTZ command: %s (not implemented in this client)", direction)
        message = f"PTZ {direction} acknowledged (no PTZ hardware)"
    elif action == "patrol":
        log.info("Patrol command (not implemented in this client)")
        message = "Patrol acknowledged (no PTZ hardware)"
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
    capabilities = cam_cfg.get("capabilities", [])

    cap = open_camera(cfg)

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
                    pil = grab_frame(cap)
                    if pil is None:
                        log.warning("No frame, retrying...")
                        time.sleep(0.5)
                        continue

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

                    # Wait for server response (inference result)
                    try:
                        result = websocket.recv(timeout=120)
                        data = json.loads(result)
                        if data.get("type") == "result":
                            log.info(
                                "Frame %d: infer=%.2fs — %s",
                                data.get("frame_no", frame_no),
                                data.get("infer_secs", 0),
                                (data.get("text", ""))[:80],
                            )
                        elif data.get("type") == "command":
                            handle_command(websocket, data, camera_id)
                    except Exception as e:
                        log.warning("No response for frame %d: %s", frame_no, e)

                    # Check for any pending command messages
                    try:
                        while True:
                            extra = websocket.recv(timeout=0.01)
                            extra_data = json.loads(extra)
                            if extra_data.get("type") == "command":
                                handle_command(websocket, extra_data, camera_id)
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
