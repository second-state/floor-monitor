# Floor Monitor

Real-time camera monitoring with Vision-Language Model (VLM) analysis. Camera
clients stream frames to a central server via WebSocket; the server runs VLM
inference on each frame, serves a live web dashboard, and optionally pushes
high-priority alerts and periodic summaries via Telegram.

**Key design:** the server never touches cameras directly. It receives JPEG frames
over WebSocket from one or more camera clients, which can run on the same machine
or anywhere on the network.


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

- **Generic VLM/LLM backend** — Any OpenAI-compatible `/v1/chat/completions` endpoint
  (vLLM, OpenAI, Ollama via its OpenAI endpoint, etc.).
- **Monitor profiles** — Domain-specific structured JSON prompts: Kid, Office,
  Retail Store, Home Security. Alerts on high-risk frames; periodic summaries.
- **Web dashboard** — Live camera preview, streaming analysis results via SSE.
  All HTML/CSS/JS in editable template files — no Rust recompile needed.
- **Telegram bot** — `/snapshot`, `/status`, `/help`, and free-form visual
  questions about the live feed.
- **Dual camera clients** — Python (USB + RTSP) and Rust (USB only), sharing
  the same `camera.toml` config format.
- **Multi-camera** — Multiple camera clients can connect to a single server
  simultaneously.

## Quick Start

### Requirements

- Rust 1.75+ (for the server)
- Python 3.11+ (for the Python camera client)
- A VLM backend serving an OpenAI-compatible `/v1/chat/completions` endpoint.
  Options: [vLLM](https://github.com/vllm-project/vllm),
  [Ollama](https://ollama.com) (with its OpenAI-compatible endpoint),
  OpenAI API, or any compatible provider.

### 1. Start a VLM backend

```bash
# Option A: vLLM with Qwen (recommended for local)
pip install vllm
vllm serve Qwen/Qwen2.5-VL-3B-Instruct

# Option B: Ollama (use its OpenAI-compatible endpoint)
ollama pull qwen2.5-vl:3b
ollama serve
# Then set api_url = "http://localhost:11434/v1/chat/completions" in config.toml
```

### 2. Build and start the server

```bash
cd server
cp config.toml.example config.toml
# Edit config.toml — set your VLM endpoint, Telegram tokens, etc.

cargo build --release
./target/release/floor-monitor-server
```

The server starts on `http://0.0.0.0:3456` by default.

### 3. Start a camera client

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

### 4. Open the dashboard

Browse to [http://127.0.0.1:3456/dashboard](http://127.0.0.1:3456/dashboard).
Analysis results stream in real-time as the camera client sends frames.

## Configuration

### Server (`server/config.toml`)

```toml
[server]
host = "0.0.0.0"
port = 3456

[vlm]
# vLLM with Qwen
api_url = "http://localhost:8000/v1/chat/completions"
model = "Qwen/Qwen2.5-VL-3B-Instruct"

# Or OpenAI cloud
# api_url = "https://api.openai.com/v1/chat/completions"
# api_key = "sk-..."
# model = "gpt-4o-mini"

max_tokens = 200
temperature = 0.1

[telegram]
# bot_token = "123456:ABC-DEF..."
# chat_id = "12345678"

# [asr]
# api_url = "https://api.openai.com/v1/audio/transcriptions"
# api_key = "sk-..."
# model = "whisper-1"

# [llm]
# api_url = "http://localhost:8000/v1/chat/completions"
# model = "Qwen/Qwen2.5-3B-Instruct"

[monitor]
default_profile = "kid"        # kid | office | retail | security
summary_window_min = 30
alert_consecutive = 2
alert_cooldown_sec = 120
```

All API sections (`[vlm]`, `[llm]`, `[asr]`) use OpenAI-compatible endpoints.

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
```

Both Python and Rust camera clients read this same file.

## API Reference

| Endpoint | Method | Description |
|---|---|---|
| `/dashboard` | GET | Web UI dashboard |
| `/ws` | WebSocket | Camera client connection |
| `/api/cameras` | GET | JSON list of connected cameras |
| `/api/results` | GET | Recent analysis results (all cameras) |
| `/api/snapshot/{camera_id}` | GET | Latest JPEG frame for a camera |
| `/api/events` | GET (SSE) | Server-Sent Events stream for live updates |

### WebSocket Protocol

**Camera → Server:**
```json
{"type": "register", "camera_id": "cam1", "name": "Living Room"}
{"type": "frame", "camera_id": "cam1", "jpeg_b64": "<base64>"}
```
Or send raw JPEG as a binary WebSocket message (after registration).

**Server → Camera:**
```json
{"type": "registered", "camera_id": "cam1"}
{"type": "result", "camera_id": "cam1", "frame_no": 42, "text": "...", "infer_secs": 1.23}
```

## Monitor Profiles

| Profile | Focus | Alert Examples |
|---|---|---|
| **Kid Monitor** | Child safety | Roughhousing, climbing, near windows, playing with outlets/sharp objects |
| **Office Monitor** | Workplace safety | Injury, conflict, fire/smoke, intruders |
| **Retail Store** | Operations | Unattended customers, staff negligence, cleanliness, conflicts |
| **Home Security** | Intrusion/safety | Strangers, forced entry, fire, glass breakage |

Each profile instructs the VLM to output structured JSON with `activity`,
`risk_level`, and `risk_reason` fields. High-risk detections trigger Telegram
alerts after N consecutive frames (configurable).

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

Not yet chosen. Treat as "all rights reserved" for now.
