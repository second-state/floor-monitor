# Floor Monitor

## Project Overview

A two-project system for real-time camera monitoring with VLM (Vision-Language Model) analysis. Camera clients capture frames and stream them to a central server via WebSocket. The server runs VLM inference, serves a live web dashboard, and optionally pushes alerts/summaries via Telegram.

**Tech Stack:**
- **Server:** Rust, Axum, Tera templates, HTML/CSS/JS (no build step), WebSocket, SSE
- **Camera client (Python):** OpenCV, websockets, TOML config
- **Camera client (Rust):** nokhwa, tokio-tungstenite, TOML config

### Architecture

```
camera/ (Python or Rust)          server/ (Rust/Axum)
┌─────────────────────┐          ┌──────────────────────────────┐
│ Capture frames      │──WebSocket──▶│ WebSocket handler           │
│ (USB / RTSP)        │          │ VLM inference (Ollama/OpenAI) │
│ Encode JPEG         │◀──Result──│ Monitor profiles & alerts    │
│ Send to server      │          │ Telegram bot                 │
└─────────────────────┘          │ Web UI (Tera templates + SSE)│
                                 └──────────────────────────────┘
```

### Core Features

1. **WebSocket camera feed** — Camera clients register, stream JPEG frames; server processes and returns results. Server can also send commands (PTZ, patrol) back to camera clients.
2. **Generic VLM/LLM backend** — Supports any OpenAI-compatible `/v1/chat/completions` endpoint (vLLM, OpenAI, Ollama with `--openai` flag, etc.). Separate `[vlm]` (vision) and `[llm]` (text/intent/summary) configuration. Both are **required**; `[llm]` drives intent classification and periodic activity summaries (so summaries don't depend on Telegram). The two sections can point at the same endpoint.
3. **Web dashboard** — Live camera preview, analysis results via SSE, camera status. All HTML/CSS/JS in editable template files.
4. **Monitor profiles** — Domain-specific structured JSON prompts (Kid / Office / Retail / Home Security) with alert pipeline (consecutive high-risk → Telegram notification) and periodic summary scheduler.
5. **Telegram bot** — Text and voice messages. Voice messages transcribed via ASR (Whisper-compatible API). LLM-based intent classification routes to: visual question, snapshot, patrol, PTZ control, history summary, help, status.
6. **Dual camera clients** — Python (full RTSP + USB support) and Rust (USB only), sharing the same `camera.toml` config format. Both handle server commands (PTZ, patrol).

### Key Architecture Decisions

- **Server-rendered templates** — Tera templates + vanilla JS. No npm/node/webpack. Templates are hot-reloadable without recompiling Rust.
- **OpenAI-compatible APIs only** — All backends (VLM, LLM, ASR) use the standard OpenAI API format. Works with vLLM, OpenAI, Ollama (via its OpenAI-compatible endpoint), or any compliant provider. No model loading in the server process.
- **WebSocket for camera feeds** — Bidirectional: camera sends frames, server sends back inference results and commands. Supports JSON (base64 JPEG) and binary (raw JPEG) frame encoding. Server→camera command channel enables PTZ control and patrol from Telegram.
- **SSE for UI updates** — Dashboard receives live results without polling. Graceful reconnection on disconnect.

For technical pitfalls behind these decisions, always consult **KNOWLEDGE.md** first.

## Reference

- **KNOWLEDGE.md** — Detailed technical pitfalls, learnings, and gotchas for Rust/Cargo, Axum, WebSocket protocol, VLM integration, and testing.
- **server/config.toml.example** — Server configuration reference.
- **camera/camera.toml.example** — Camera client configuration reference.

## Spec & Plan Maintenance

**If any spec or requirement changes, update CLAUDE.md to reflect the change.** This includes new features, modified behavior, removed functionality, API changes, or protocol changes. Treat the code as the final source of truth and keep docs synchronized with it.

## Coding Rules

### Server (Rust)

- **No `unwrap()` in handlers** — Use `?` with proper error types or return `(StatusCode, String)` error tuples.
- **Never hardcode the port** — Read from `config.toml`. Default to 3456 if not set.
- **Never commit `config.toml` or API keys** — They are in `.gitignore`. Provide `config.toml.example` instead.
- **Templates must be editable without recompilation** — All HTML/CSS/JS lives in `server/templates/` and `server/static/`. The Tera engine reloads templates on each request in dev.

### Camera Client

- **Shared config format** — Both Python and Rust clients must read the same `camera.toml` file. Do not add Python-only or Rust-only config keys without a documented fallback. The new `[ptz]` and `[ptz.patrol]` blocks are Rust-only extensions; Python ignores them silently.
- **Auto-reconnect** — Camera clients must handle server disconnections gracefully with backoff retry.
- **Library/binary split (Rust client)** — Protocol code lives in `floor_monitor_camera` (lib); the binary glue with `nokhwa` lives in `main.rs` behind the `camera` feature flag. Run `cargo test --no-default-features` from `camera/rust/` to test the lib without webcam system libs (matches the CI configuration).

For Rust/Cargo, Axum, and WebSocket-specific rules, see **KNOWLEDGE.md**.

## Knowledge Management

When you fix an important bug or discover a non-obvious technical pitfall, add the lesson to **KNOWLEDGE.md**. Focus on the *why* and *how* — not procedural instructions (those belong here). KNOWLEDGE.md should help future agents avoid repeating the same mistakes.

## Development Workflow

### Commit Policy

All commits MUST pass the following checks before being pushed:

```bash
cd server

# 1. Format check — zero diff
cargo fmt --all -- --check

# 2. Clippy — zero warnings (deny all warnings)
cargo clippy --all-targets --all-features -- -D warnings

# 3. Build — zero warnings, zero errors
RUSTFLAGS="-D warnings" cargo build --release

# 4. Unit & integration tests
cargo test

# 5. E2E tests
cargo test --test e2e_tests
```

If any of these fail, do NOT commit. Fix the issues first.

### Commit Signing (DCO)

All commits must be signed with:

```
Signed-off-by: Michael Yuan <michael@secondstate.io>
```

Use `-s` flag: `git commit -s -m "message"`

Co-author line:

```
Co-Authored-By: Claude Code <noreply@anthropic.com>
```

### Test Requirements

- **Unit tests** (`tests/unit_tests.rs`) — Test monitor JSON parsing, profile definitions, config loading.
- **Integration tests** (`tests/api_tests.rs`) — Start a real Axum server, test HTTP endpoints (cameras, results, snapshots, WebSocket register).
- **E2E tests** (`tests/e2e_tests.rs`) — Simulated camera clients streaming frames via WebSocket, multi-camera concurrency, SSE events, disconnect cleanup.
- **Real VLM tests** — Set `FLOOR_MONITOR_E2E_VLM=1` with `FLOOR_MONITOR_VLM_URL` and `FLOOR_MONITOR_VLM_MODEL` to test against a live VLM backend.

### Project Structure

```
floor-monitor/
├── server/                    # Rust/Axum WebSocket server
│   ├── Cargo.toml
│   ├── config.toml.example
│   ├── src/
│   │   ├── main.rs            # Entry point
│   │   ├── lib.rs             # Public modules (for tests)
│   │   ├── config.rs          # TOML config loading
│   │   ├── state.rs           # Shared AppState, FrameResult, CameraState
│   │   ├── vlm.rs             # VLM client (OpenAI-compatible)
│   │   ├── llm.rs             # LLM intent classifier
│   │   ├── asr.rs             # ASR client (Whisper-compatible)
│   │   ├── alert.rs           # Alert tracker + pipeline
│   │   ├── ws.rs              # WebSocket handler + command channel
│   │   ├── routes.rs          # HTTP routes (dashboard, API, SSE)
│   │   ├── monitor.rs         # Profile loader + JSON parsing
│   │   └── telegram.rs        # Telegram bot (text + voice)
│   ├── profiles/              # Monitor profiles (editable TOML)
│   │   ├── kid.toml
│   │   ├── office.toml
│   │   ├── retail.toml
│   │   └── security.toml
│   ├── templates/             # Tera HTML templates
│   │   ├── base.html
│   │   └── dashboard.html
│   ├── static/
│   │   ├── css/style.css
│   │   └── js/dashboard.js
│   └── tests/
│       ├── unit_tests.rs
│       ├── api_tests.rs
│       └── e2e_tests.rs
├── camera/                    # Camera clients
│   ├── camera.toml.example    # Shared config format
│   ├── python/
│   │   ├── camera_client.py
│   │   └── requirements.txt
│   └── rust/
│       ├── Cargo.toml         # [features] camera = nokhwa+image+base64
│       ├── src/
│       │   ├── lib.rs         # protocol library (no nokhwa)
│       │   ├── main.rs        # binary, requires `camera` feature
│       │   ├── config.rs      # Config + PtzConfig + PatrolConfig
│       │   ├── commands.rs    # dispatch, handle_command, CommandCtx
│       │   └── ptz/
│       │       ├── mod.rs     # Ptz trait, build(), execute_ptz()
│       │       ├── noop.rs    # always-success no-op (default fallback)
│       │       ├── fake.rs    # call-recording test double
│       │       ├── detect.rs  # v4l2-ctl --list-ctrls parser
│       │       ├── v4l2ctl.rs # V4l2CtlRunner trait + V4l2CtlPtz
│       │       └── patrol.rs  # cancellable patrol task
│       └── tests/
│           ├── ptz_tests.rs   # trait dispatch + V4l2CtlPtz + patrol
│           ├── caps_tests.rs  # parser fixtures + capability inference
│           └── config_tests.rs # [ptz] block parsing
├── docs/
│   └── PTZ_HARDWARE_LOG.md    # Verified-hardware matrix (manual bench)
├── .github/workflows/ci.yml   # CI: fmt, clippy, build, test, e2e + camera-rust-*
├── CLAUDE.md                  # This file
├── KNOWLEDGE.md               # Technical pitfalls and learnings
└── README.md                  # User-facing documentation
```
