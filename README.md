# Floor Monitor

Real-time camera monitoring with Vision-Language Model (VLM) analysis. Camera
clients stream frames to a central server via WebSocket; the server runs VLM
inference on each frame, serves a live web dashboard, and optionally pushes
high-priority alerts and periodic summaries via Telegram.

**Key design:** the server never touches cameras directly. It receives JPEG frames
over WebSocket from one or more camera clients, which can run on the same machine
or anywhere on the network. All AI backends (VLM, LLM, ASR) are accessed through
OpenAI-compatible APIs — the server itself loads no models.

## Architecture

```
┌─────────────────────────┐         ┌───────────────────────────────────┐
│  Camera Client          │         │  Server (Rust / Axum)             │
│  (Python or Rust)       │         │                                   │
│                         │  WS     │  /ws       WebSocket handler      │
│  USB / RTSP capture ────┼────────▶│  /dashboard  Web UI (Tera + SSE) │
│  JPEG encode + send     │◀────────┤  /api/*    REST endpoints         │
│  Receive results        │  Result │  VLM client  (OpenAI-compatible)  │
└─────────────────────────┘         │  Telegram bot (optional)          │
                                    └───────────────────────────────────┘
```

## Features

- **Generic VLM/LLM backend** — Any OpenAI-compatible `/v1/chat/completions`
  endpoint. No vendor lock-in.
- **Monitor profiles** — Domain-specific structured JSON prompts: Kid, Office,
  Retail Store, Home Security. Alerts on high-risk frames; periodic summaries.
- **Web dashboard** — Live camera preview, streaming analysis results via SSE.
  All HTML/CSS/JS in editable template files — no Rust recompile needed.
- **Telegram bot** — Text and voice messages. Voice transcribed via ASR.
  LLM-based intent classification routes to visual Q&A, snapshots, PTZ control,
  patrol, history summaries.
- **Camera control** — Server sends PTZ and patrol commands to capable cameras
  via WebSocket. Cameras report capabilities on registration; fixed cameras
  (e.g. Mac webcam) are never sent movement commands. The Rust client drives
  real UVC PTZ hardware on Linux when `v4l-utils` is installed (auto-detected
  on startup); macOS and Windows still acknowledge but do not move motors.
  See [`docs/PTZ_HARDWARE_LOG.md`](docs/PTZ_HARDWARE_LOG.md) for the
  verified-hardware matrix.
- **Dual camera clients** — Python (USB + RTSP) and Rust (USB only, with UVC
  PTZ on Linux), sharing the same `camera.toml` config format.
- **Multi-camera** — Multiple camera clients can connect simultaneously.

## Quick Start

### Requirements

- Rust 1.75+ (for the server)
- Python 3.11+ (for the Python camera client)
- An OpenAI-compatible VLM endpoint (see [Local Inference](#local-inference) below)

### 1. Build and start the server

```bash
cd server
cp config.toml.example config.toml
```

Edit `config.toml` — point `[vlm]` at your OpenAI-compatible endpoint:

```toml
[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
model = "Qwen/Qwen2.5-VL-3B-Instruct"
max_tokens = 200
# temperature = 0.1  # optional; omit to use provider default
```

Build and run:

```bash
cargo build --release
./target/release/floor-monitor-server
```

The server starts on `http://0.0.0.0:3456` by default.

### 2. Start a camera client

**Python (recommended — supports USB and RTSP cameras):**

```bash
cd camera
cp camera.toml.example camera.toml
# Edit camera.toml — set camera source and server URL

cd python
python -m venv .venv
.venv/bin/pip install -r requirements.txt
.venv/bin/python camera_client.py
```

**Rust (USB cameras only):**

```bash
cd camera/rust
cargo build --release
./target/release/floor-monitor-camera ../camera.toml
```

### 3. Open the dashboard

Browse to [http://127.0.0.1:3456/dashboard](http://127.0.0.1:3456/dashboard).
Analysis results stream in real-time as the camera client sends frames.

## Configuration

### Server (`server/config.toml`)

```toml
[server]
host = "0.0.0.0"
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
model = "Qwen/Qwen2.5-VL-3B-Instruct"
# api_key = "sk-..."   # if your endpoint requires authentication
max_tokens = 200
# temperature = 0.1  # optional; omit to use provider default

[telegram]
# bot_token = "123456:ABC-DEF..."
# chat_id = "12345678"                 # single chat
# chat_ids = ["12345678", "87654321"]  # or multiple chats

# [asr]  # for Telegram voice messages (requires ffmpeg)
# api_url = "https://api.openai.com/v1/audio/transcriptions"
# api_key = "sk-..."
# model = "whisper-1"

[llm]  # required — drives intent classification AND periodic summaries
api_url = "http://localhost:8000/v1/chat/completions"
# api_key = "sk-..."
model = "Qwen/Qwen2.5-3B-Instruct"

[monitor]
default_profile = "kid"        # kid | office | retail | security
summary_window_min = 30
alert_consecutive = 2
alert_cooldown_sec = 120
```

All API sections (`[vlm]`, `[llm]`, `[asr]`) use standard OpenAI-compatible
endpoints. See `config.toml.example` for full documentation.

### Camera (`camera/camera.toml`)

```toml
[server]
ws_url = "ws://127.0.0.1:3456/ws"

[camera]
id = "cam-livingroom"
name = "Living Room Camera"
source_type = "local"       # "local" or "rtsp"
device_index = 0            # for local cameras
# rtsp_url = "rtsp://user:pass@192.168.1.10:554/stream1"  # for RTSP
interval = 2.0
max_dimension = 768
jpeg_quality = 85
# capabilities = ["ptz", "patrol"]  # for PTZ-capable cameras
```

Both Python and Rust camera clients read this same file.

## Local Inference

The server works with any OpenAI-compatible `/v1/chat/completions` endpoint.
For local (on-device) inference, you can use:

### vLLM

```bash
pip install vllm
vllm serve Qwen/Qwen2.5-VL-3B-Instruct
```

Then set in `config.toml`:
```toml
[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
model = "Qwen/Qwen2.5-VL-3B-Instruct"
```

### Ollama

Ollama exposes an OpenAI-compatible endpoint alongside its native API.

```bash
ollama pull qwen2.5-vl:3b
ollama serve
```

Then set in `config.toml`:
```toml
[vlm]
api_url = "http://localhost:11434/v1/chat/completions"
model = "qwen2.5-vl:3b"
```

### Cloud providers

Any OpenAI-compatible cloud endpoint works (OpenAI, Together, Groq, etc.):

```toml
[vlm]
api_url = "https://api.openai.com/v1/chat/completions"
api_key = "sk-..."
model = "gpt-4o-mini"
```

## Telegram Bot Setup

### 1. Create a bot

Message [@BotFather](https://t.me/BotFather) on Telegram and send `/newbot`.
Follow the prompts to get your **bot token** (e.g. `123456:ABC-DEF...`).

### 2. Find your chat ID

Send any message to your new bot, then open this URL in a browser
(replace `<TOKEN>` with your bot token):

```
https://api.telegram.org/bot<TOKEN>/getUpdates
```

Look for `"chat":{"id":12345678,...}` in the JSON response. That number
is your chat ID.

For a **group chat**, add the bot to the group, send a message in the
group, then check `getUpdates` again. Group chat IDs are negative numbers
(e.g. `-1001234567890`).

### 3. Configure

```toml
[telegram]
bot_token = "123456:ABC-DEF..."
chat_id = "12345678"
```

To send alerts and summaries to multiple people or groups:

```toml
[telegram]
bot_token = "123456:ABC-DEF..."
chat_ids = ["12345678", "-1001234567890"]
```

The bot sends alerts, summaries, and replies to all listed chats.
Only messages from those chats are accepted — others are ignored.

## API Reference

| Endpoint | Method | Description |
|---|---|---|
| `/dashboard` | GET | Web UI dashboard |
| `/ws` | WebSocket | Camera client connection |
| `/api/cameras` | GET | JSON list of connected cameras (includes capabilities) |
| `/api/results` | GET | Recent analysis results (all cameras) |
| `/api/snapshot/{camera_id}` | GET | Latest JPEG frame for a camera |
| `/api/events` | GET (SSE) | Server-Sent Events stream for live updates |

### WebSocket Protocol

**Camera → Server:**
```json
{"type": "register", "camera_id": "cam1", "name": "Living Room", "capabilities": ["ptz", "patrol"]}
{"type": "frame", "camera_id": "cam1", "jpeg_b64": "<base64>"}
```
Or send raw JPEG as a binary WebSocket message (after registration).

**Server → Camera:**
```json
{"type": "registered", "camera_id": "cam1"}
{"type": "result", "camera_id": "cam1", "frame_no": 42, "text": "...", "infer_secs": 1.23}
{"type": "command", "camera_id": "cam1", "action": "ptz", "params": {"direction": "pan_left"}}
```

## Monitor Profiles

Profiles are VLM prompts stored as TOML files in `server/profiles/`. Each
profile tells the VLM how to analyze a frame for a specific domain.

### Built-in profiles

| Profile | File | Focus |
|---|---|---|
| Kid Monitor | `profiles/kid.toml` | Child safety — roughhousing, climbing, sharp objects |
| Office Monitor | `profiles/office.toml` | Workplace — injury, conflict, fire, intruders |
| Retail Store | `profiles/retail.toml` | Operations — unattended customers, cleanliness |
| Home Security | `profiles/security.toml` | Intrusion — strangers, forced entry, fire |

### Profile format

Each `.toml` file contains:

```toml
id = "my-profile"                  # unique ID, referenced in config.toml
name = "My Custom Profile"         # display name
danger_categories = ["fire", "intruder"]  # high-risk categories

summary_intro = """
Instructions for generating periodic activity summaries."""

prompt = """
Instructions for the VLM. Must tell it to output structured JSON with
at least: activity, risk_level, risk_reason fields."""
```

### Creating a custom profile

1. Copy an existing profile: `cp profiles/kid.toml profiles/warehouse.toml`
2. Edit the `id`, `name`, `prompt`, `summary_intro`, and `danger_categories`
3. Set `default_profile = "warehouse"` in `config.toml`
4. Restart the server — no recompilation needed

### Selecting a profile

```toml
[monitor]
default_profile = "kid"   # must match the id field in a profiles/*.toml file
```

### How alerts work

Each profile's `prompt` instructs the VLM to output JSON with `risk_level`
(`"none"`, `"low"`, `"medium"`, `"high"`) and `risk_reason` fields. When
the server sees N consecutive high-risk frames (configurable via
`alert_consecutive`), it sends a Telegram alert with the frame photo.
A per-camera cooldown (`alert_cooldown_sec`) prevents alert spam.

## Development

### Run tests

```bash
cd server
cargo test                          # all tests
cargo test --test e2e_tests         # e2e only
```

### CI

GitHub Actions runs on ARM Linux (`ubuntu-24.04-arm`):
1. `cargo fmt --check`
2. `cargo clippy -- -D warnings`
3. `cargo build --release`
4. `cargo test`
5. `cargo test --test e2e_tests`

### Project structure

See [CLAUDE.md](CLAUDE.md) for the full directory layout and development guidelines.

## Prior Art

This project is a restructured version of
[VLM Camera](https://github.com/Drlucaslu/vlm-camera), originally a monolithic
Python + Gradio application. The restructuring separates the camera capture
(client) from the analysis server, replaces Gradio with a Rust/Axum web server
with editable templates, and makes the VLM backend configurable via standard
OpenAI-compatible APIs.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).
