# Knowledge Base

Technical pitfalls, learnings, and gotchas for the Floor Monitor project.

## Rust / Cargo

### Axum Extractor Ordering

Axum extractors must be ordered correctly in handler signatures. `State` should come last or use `extract::State`. When using `WebSocketUpgrade`, it must appear before `State` in the parameter list.

### Axum SSE Streams

`axum::response::Sse::new()` requires a stream of `Result<Event, _>`, not `Result<String, _>`. Always yield `axum::response::sse::Event::default().data(...)` instead of raw strings.

### Cargo fmt Before Clippy

Always run `cargo fmt --all` before `cargo clippy`. Clippy sometimes reports style issues that fmt would fix, and fmt changes can introduce new clippy warnings. The CI checks both sequentially.

### Clippy Doc Comments

Clippy enforces `doc-lazy-continuation` — multi-line doc comments must have consistent indentation for continuation lines. Keep doc comments on a single line or properly indent continuations.

## WebSocket Protocol

### Message Types

The camera-to-server WebSocket protocol supports two frame delivery methods:
1. **JSON text messages** with base64-encoded JPEG (`{"type":"frame","camera_id":"...","jpeg_b64":"..."}`)
2. **Binary messages** — raw JPEG bytes (camera must be registered first; the server associates binary frames with the most recently registered camera on that connection)

### Camera Registration

A camera must send a `{"type":"register","camera_id":"...","name":"..."}` message before sending frames. The server acknowledges with `{"type":"registered","camera_id":"..."}`. Without registration, frames are dropped.

### Connection Lifecycle

When a WebSocket disconnects, the server marks the camera as `running: false` but retains its state (frames, results). This allows the dashboard to show historical data and the camera to reconnect without losing context.

## VLM Integration

### OpenAI-Compatible API Only

All API sections (`[vlm]`, `[llm]`, `[asr]`) use the standard OpenAI API format exclusively. There is no Ollama-native format support. If using Ollama, point to its OpenAI-compatible endpoint (`http://localhost:11434/v1/chat/completions`).

### OpenAI Vision Format

Images are sent as base64 data URLs in the `content` array:
```json
{"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,..."}}
```

Not all OpenAI-compatible servers support vision. Ensure your model supports image input.

### Text-Only VLM Calls

For summary generation (no image needed), we send a 1x1 grey JPEG placeholder. VLMs require an image input even for text-only tasks. The prompt instructs the model to ignore the image.

### Inference Timeout

Both OpenAI and Ollama calls use a 120-second timeout. Large models on slow hardware may need this entire window. The server sends the result back to the camera client only after inference completes — there's no streaming.

## Monitor Profiles

### JSON Parsing Tolerance

VLM output is not always clean JSON. The parser:
1. Tries `serde_json::from_str` on the full text
2. Falls back to finding the first `{...}` block via string search
3. Returns `None` if neither works

This handles common VLM quirks: leading/trailing text, markdown code fences, explanatory prose around the JSON.

### Risk Level Normalization

The `risk_level` field is normalized to lowercase and must be one of: `none`, `low`, `medium`, `high`. Any other value defaults to `none`. This prevents VLM hallucinations from triggering false alerts.

## Testing

### E2E Test Architecture

E2E tests start a real Axum server on a random port (`TcpListener::bind("127.0.0.1:0")`), connect simulated camera clients via WebSocket, and validate the full pipeline. No mock VLM is used — the VLM endpoint is set to an unreachable URL, and the server gracefully handles the inference error.

To test with a real VLM, set `FLOOR_MONITOR_E2E_VLM=1` and configure `FLOOR_MONITOR_VLM_URL` / `FLOOR_MONITOR_VLM_MODEL`.

### Test JPEG Images

Tests use pre-encoded minimal JPEG byte arrays (2x2 pixels) rather than runtime image generation. This avoids pulling in image encoding dependencies in tests and keeps test startup fast.

### Template Directory Resolution

Tests may run from the `server/` directory or the repo root. The template loader tries both `templates/` and `server/templates/` paths. In CI, always `cd server` before running tests.

## Camera Clients

### Shared Config Format

Both Python and Rust camera clients read the same `camera.toml` file (TOML format). This ensures config portability. The Python client supports both local and RTSP cameras; the Rust client currently supports local cameras only (RTSP requires FFmpeg bindings not included).

### RTSP Buffer Draining

OpenCV's FFmpeg backend buffers ~5 decoded RTSP frames. After camera movement (PTZ), the next few `read()` calls return stale pre-movement frames. The Python client's `grab_fresh_frame()` drains the buffer by calling `grab()` multiple times before `retrieve()`. The Rust client doesn't implement this yet.

### Reconnection

Both clients implement auto-reconnect with a 5-second backoff. If the server goes down, the camera client keeps retrying. On reconnect, the camera re-registers — the server creates or updates the camera state.

## CI

### ARM Linux Runners

CI uses `ubuntu-24.04-arm` runners. The `nokhwa` crate (camera capture in the Rust client) may require system libraries (`v4l2`, `libclang`) on Linux that aren't available in CI. The CI only builds/tests the server, not the Rust camera client.

### Rust Cache

CI uses `Swatinem/rust-cache@v2` with `workspaces: server` to cache only the server's target directory. This avoids caching the camera client's separate target directory.
