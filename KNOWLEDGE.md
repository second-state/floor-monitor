# Knowledge Base

Technical pitfalls, learnings, and gotchas for the Floor Monitor project.

## Rust / Cargo

### Axum Extractor Ordering

Axum extractors must be ordered correctly in handler signatures. `State` should come last or use `extract::State`. When using `WebSocketUpgrade`, it must appear before `State` in the parameter list.

### Axum SSE Streams

`axum::response::Sse::new()` requires a stream of `Result<Event, _>`, not `Result<String, _>`. Always yield `axum::response::sse::Event::default().data(...)` instead of raw strings.

### SSE Behind Cloudflare / Reverse Proxies

Plain `axum::response::Sse` works on localhost but breaks when fronted by Cloudflare, nginx, or similar. Three fixes are needed:

- **15-second keep-alive**: Cloudflare drops idle connections at ~100 seconds. Without traffic between events the browser sees a silent disconnect and reconnects in a tight loop. Use `Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("heartbeat"))` so SSE comments flow during quiet periods.
- **Proxy-buffering headers**: axum sets `Cache-Control: no-cache` by default but not the rest. Override `Cache-Control` to `no-cache, no-transform` (the `no-transform` blocks Cloudflare from rewriting/compressing the body) and add `X-Accel-Buffering: no` (nginx response-buffer disable).
- **Client-side reconnect + backfill**: don't rely on `EventSource` auto-reconnect — close the source on `onerror` and call `connectSSE()` after a 3s timeout. After reconnect, refetch any persistent state (e.g. summaries) from a REST endpoint to recover events emitted while disconnected. Frame results are transient and don't need backfill.

In `server/src/routes.rs::api_events`, the response is built with `.into_response()` so headers can be mutated; the dashboard JS uses a 3s reconnect delay and a 60s `/api/summaries` poll for eventual consistency.

### Cargo fmt Before Clippy

Always run `cargo fmt --all` before `cargo clippy`. Clippy sometimes reports style issues that fmt would fix, and fmt changes can introduce new clippy warnings. The CI checks both sequentially.

### Clippy Doc Comments

Clippy enforces `doc-lazy-continuation` — multi-line doc comments must have consistent indentation for continuation lines. Keep doc comments on a single line or properly indent continuations.

## OpenAI-Compatible API

### All Endpoints Are OpenAI Format

All API sections (`[vlm]`, `[llm]`, `[asr]`) use the standard OpenAI API format exclusively. There is no Ollama-native or other proprietary format support. If using Ollama, point to its OpenAI-compatible endpoint (`http://localhost:11434/v1/chat/completions`).

### VLM Vision Format

Images are sent as base64 data URLs in the `content` array:
```json
{"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,..."}}
```
Not all OpenAI-compatible servers support vision. Ensure your model supports image input.

### API Key Handling

All three sections (`[vlm]`, `[llm]`, `[asr]`) accept an optional `api_key`. When present and non-empty, it's sent as `Authorization: Bearer <key>`. When absent, no auth header is sent. This allows both authenticated cloud endpoints and local servers that need no auth.

### Temperature Is Optional

The `temperature` field in `[vlm]` and `[llm]` is `Option<f32>`. When omitted, the field is not sent in the API request (via `skip_serializing_if`), letting the provider use its default. This is important because some providers (e.g. OpenAI reasoning models) have deprecated user-set temperatures.

### max_tokens

`max_tokens` limits VLM/LLM response length in tokens. For structured JSON monitor output, 200 is sufficient. For intent classification, 150 is enough. Lower values = faster response and lower cost on cloud APIs. The server processes every frame, so keeping this low matters for throughput.

### Text-Only Paths Use `[llm]`, Not `[vlm]`

Text-only inference (the periodic summary scheduler and Telegram history-summary Q&A) used to call `VlmClient::infer_text_only`, which shipped a hard-coded 1×1 placeholder JPEG so it could reuse the vision payload format. Some VLM providers validate image payloads and reject obvious dummies with a 400 (`"The image data you provided does not represent a valid image"`), silently breaking summaries even when per-frame inference worked fine. Both paths now go through `LlmClient::complete`, and `[llm]` is a required config section. `Config::load` enforces non-empty `api_url` at startup. `[llm]` and `[vlm]` can point at the same OpenAI-compatible endpoint, or `[llm]` can target a smaller text-only model for cheaper/faster summaries.

### Invalid JSON Responses

VLM and LLM clients read the HTTP response as text first, then parse with `serde_json::from_str`. If the provider returns a 200 with non-JSON body (HTML error page, malformed response), the error is logged with a truncated body snippet and returned as `Err` — never a panic. String truncation uses `.chars().take(N)` to prevent panics on multi-byte UTF-8.

## WebSocket Protocol

### Message Types

Camera-to-server:
- `register` — `{camera_id, name, capabilities}` — camera announces itself
- `frame` — `{camera_id, jpeg_b64}` — base64-encoded JPEG frame
- Binary message — raw JPEG bytes (after registration)
- `command_ack` — `{camera_id, action, success, message}` — acknowledge a command

Server-to-camera:
- `registered` — `{camera_id}` — acknowledge registration
- `result` — `{camera_id, frame_no, text, infer_secs}` — VLM inference result
- `command` — `{camera_id, action, params}` — PTZ or patrol command
- `error` — `{message}` — error message

### Camera Capabilities

Cameras report capabilities on registration (e.g. `["ptz", "patrol"]`). The server stores these in `CameraState.capabilities`. When the Telegram bot dispatches a PTZ or patrol command via `send_command_to_any_camera()`, it first looks for a camera with the matching capability. If no camera supports the requested action, a clear error is returned. Fixed cameras (Mac webcam, RTSP without PTZ) register with empty capabilities and are never sent movement commands.

### Command Channel Architecture

Each camera has an `mpsc::UnboundedSender<String>` stored in `CameraState.cmd_tx`. On WebSocket connection, a forwarding task spawns that reads from the channel and writes to the WebSocket. This allows any part of the server (Telegram handler, API endpoint) to send commands to a specific camera without holding the WebSocket sender directly. On disconnect, `cmd_tx` is set to `None`.

### Connection Lifecycle

When a WebSocket disconnects, the server marks the camera as `running: false` and sets `cmd_tx: None`, but retains the camera state (frames, results). This allows the dashboard to show historical data and the camera to reconnect without losing context.

## Monitor Profiles

### External TOML Files

Profiles are loaded from `server/profiles/*.toml` at startup. Each file defines `id`, `name`, `prompt`, `summary_intro`, and `danger_categories`. To add a new profile, drop a `.toml` file in the directory and set `default_profile` in config. No recompilation needed.

### Fallback Defaults

If the `profiles/` directory doesn't exist or is empty, the server falls back to built-in default profiles (kid, security). The external files are the canonical source; the built-in defaults are a safety net.

### JSON Parsing Tolerance

VLM output is not always clean JSON. The `parse_vlm_json` function:
1. Tries `serde_json::from_str` on the full text
2. Falls back to finding the first `{...}` block via string search
3. Returns `None` if neither works — never panics

### Risk Level Normalization

The `risk_level` field is normalized to lowercase and must be one of: `none`, `low`, `medium`, `high`. Any other value defaults to `none`. This prevents VLM hallucinations from triggering false alerts.

## Alert Pipeline

### How Alerts Fire

`AlertTracker` counts consecutive frames where `risk_level` is `"high"` or `"medium"`. When the count reaches `alert_consecutive` (default 2), an `AlertEvent` is sent via `mpsc` channel. A consumer task in `main.rs` sends the alert to all Telegram chats with the frame photo.

### Cooldown

After an alert fires for a camera, a per-camera cooldown (`alert_cooldown_sec`, default 120) prevents repeated alerts for the same ongoing situation.

### Summary Scheduler

A background `tokio::spawn` task fires every `summary_window_min` minutes. It collects recent frame results, builds a text digest, and calls `vlm.infer_text_only()` with the profile's `summary_intro` to generate a natural-language summary. The summary is pushed to all Telegram chats.

## Telegram Bot

### Multi-Chat Support

The `[telegram]` config supports both `chat_id` (single, string) and `chat_ids` (list of strings). Both are merged and deduplicated at startup. All messages (alerts, summaries, replies) are sent to every configured chat. Only incoming messages from listed chats are processed; others are silently dropped.

### Voice Messages

Voice messages (OGG files) are handled by:
1. Downloading via Telegram `getFile` API → file URL → raw bytes
2. Converting OGG to 16kHz mono WAV via `ffmpeg` (temp files with UUID names)
3. Sending WAV to the ASR endpoint (Whisper-compatible multipart POST)
4. Feeding the transcribed text through the normal message handler

If ffmpeg is not installed or conversion fails, the original OGG is sent to the ASR endpoint as a fallback.

### Intent Classification

When `[llm]` is configured, incoming text (or transcribed voice) is classified by the LLM into an `Intent` enum: `VisualQuestion`, `HistorySummary`, `Snapshot`, `Patrol`, `PtzControl`, `Help`, `Status`. The LLM is prompted to output a single-line JSON, which is parsed tolerantly (strips code fences, finds first `{...}` block).

When `[llm]` is not configured, the bot falls back to keyword matching (`/help`, `/snapshot`, `pan left`, etc.).

### Reactive Only

The Telegram bot only responds to incoming messages. It does not stream frames or analysis results. The only proactive behaviors are alerts (from the alert pipeline) and periodic summaries (from the summary scheduler).

## Camera Clients

### Shared Config Format

Both Python and Rust camera clients read the same `camera.toml` file (TOML format). The Python client supports both local and RTSP cameras; the Rust client currently supports local cameras only.

### Capabilities

The `capabilities` field in `camera.toml` is an optional list of strings (e.g. `["ptz", "patrol"]`). It's sent during WebSocket registration and determines whether the server will route movement commands to this camera. Fixed cameras should omit this field or set it to `[]`.

### Command Handling

After receiving an inference result, camera clients poll for pending command messages with a short timeout. Commands have `action` ("ptz", "patrol") and `params` (e.g. `{"direction": "pan_left"}`). The client sends a `command_ack` response. The Python client logs commands but does not implement actual motor control — that depends on the camera hardware.

### Reconnection

Both clients implement auto-reconnect with a 5-second backoff. On reconnect, the camera re-registers with its capabilities.

## Testing

### E2E Test Architecture

E2E tests start a real Axum server on a random port (`TcpListener::bind("127.0.0.1:0")`), connect simulated camera clients via WebSocket, and validate the full pipeline. No mock VLM is used — the VLM endpoint is set to an unreachable URL, and the server gracefully handles the inference error.

To test with a real VLM, set `FLOOR_MONITOR_E2E_VLM=1` and configure `FLOOR_MONITOR_VLM_URL` / `FLOOR_MONITOR_VLM_MODEL`.

### Test JPEG Images

Tests use pre-encoded minimal JPEG byte arrays (2x2 pixels) rather than runtime image generation. This keeps test startup fast and avoids image-crate dependencies in tests.

### Template Directory Resolution

Tests may run from the `server/` directory or the repo root. The template loader tries both `templates/` and `server/templates/` paths. The profile loader similarly tries both `profiles/` and `server/profiles/`. In CI, always `cd server` before running tests.

## CI

### ARM Linux Runners

CI uses `ubuntu-24.04-arm` runners. The server build and all tests run on ARM. The Rust camera client (`nokhwa` crate) may require system libraries on Linux that aren't available in CI, so CI only builds/tests the server.

### Rust Cache

CI uses `Swatinem/rust-cache@v2` with `workspaces: server` to cache only the server's target directory.
