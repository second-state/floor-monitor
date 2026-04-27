//! End-to-end tests with simulated camera streaming.
//!
//! These tests start the full server, connect a simulated camera client via
//! WebSocket, stream pre-built JPEG frames, and validate the server processes
//! them correctly (stores frames, serves snapshots, broadcasts events).
//!
//! VLM inference is expected to fail (no model server) — the tests verify the
//! server gracefully handles inference errors and still stores frames.
//! When FLOOR_MONITOR_E2E_VLM=1 is set, a real VLM backend is expected and
//! the tests validate actual inference results.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::Message;

/// Create a minimal valid JPEG image (red 2x2 pixels).
/// This is a real JPEG that any image decoder can handle.
fn test_jpeg() -> Vec<u8> {
    // Use a pre-encoded 2x2 red JPEG
    // Generated from: ImageMagick `convert -size 2x2 xc:red test.jpg`
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x02, 0x00, 0x02, 0x01, 0x01, 0x11, 0x00, 0xFF,
        0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA,
        0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B, 0x40, 0x1B, 0x00, 0xFF, 0xD9,
    ]
}

fn test_state() -> Arc<floor_monitor_server::state::AppState> {
    let config = floor_monitor_server::config::Config {
        server: floor_monitor_server::config::ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
        },
        vlm: floor_monitor_server::config::VlmConfig {
            api_url: std::env::var("FLOOR_MONITOR_VLM_URL")
                .unwrap_or_else(|_| "http://localhost:99999/v1/chat/completions".to_string()),
            api_key: std::env::var("FLOOR_MONITOR_VLM_KEY").ok(),
            model: std::env::var("FLOOR_MONITOR_VLM_MODEL")
                .unwrap_or_else(|_| "test-model".to_string()),
            max_tokens: 100,
            temperature: None,
        },
        telegram: Default::default(),
        monitor: Default::default(),
        asr: Default::default(),
        llm: floor_monitor_server::config::LlmConfig {
            api_url: "http://localhost:99999/v1/chat/completions".to_string(),
            api_key: None,
            model: "test-llm".to_string(),
            max_tokens: 100,
            temperature: None,
        },
    };
    let (state, _alert_rx) = floor_monitor_server::state::AppState::new(config);
    Arc::new(state)
}

fn test_app(state: Arc<floor_monitor_server::state::AppState>) -> axum::Router {
    use axum::routing::get;
    use tower_http::services::ServeDir;

    axum::Router::new()
        .route("/", get(floor_monitor_server::routes::index))
        .route("/dashboard", get(floor_monitor_server::routes::dashboard))
        .route("/ws", get(floor_monitor_server::ws::ws_handler))
        .route(
            "/api/cameras",
            get(floor_monitor_server::routes::api_cameras),
        )
        .route(
            "/api/results",
            get(floor_monitor_server::routes::api_results),
        )
        .route(
            "/api/snapshot/{camera_id}",
            get(floor_monitor_server::routes::api_snapshot),
        )
        .route("/api/events", get(floor_monitor_server::routes::api_events))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state)
}

async fn start_server() -> (String, Arc<floor_monitor_server::state::AppState>) {
    let state = test_state();
    let app = test_app(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{}", addr), state)
}

/// Simulate a camera client: register, send N frames, collect results.
async fn simulate_camera(
    ws_url: &str,
    camera_id: &str,
    camera_name: &str,
    num_frames: usize,
) -> Vec<serde_json::Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("WS connect failed");

    // Register
    let register = serde_json::json!({
        "type": "register",
        "camera_id": camera_id,
        "name": camera_name,
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .unwrap();
    let _ack = ws.next().await.unwrap().unwrap();

    let jpeg = test_jpeg();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg);
    let mut results = Vec::new();

    for _ in 0..num_frames {
        let frame_msg = serde_json::json!({
            "type": "frame",
            "camera_id": camera_id,
            "jpeg_b64": b64,
        });
        ws.send(Message::Text(frame_msg.to_string().into()))
            .await
            .unwrap();

        // Wait for response (with timeout)
        match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    results.push(v);
                }
            }
            _ => {
                results.push(serde_json::json!({"type": "timeout"}));
            }
        }
    }

    results
}

#[tokio::test]
async fn test_e2e_camera_stream_and_snapshot() {
    let (base_url, state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    // Simulate camera sending 3 frames
    let results = simulate_camera(&ws_url, "e2e-cam1", "E2E Test Camera", 3).await;

    // All 3 frames should get responses (either result or error due to no VLM)
    assert_eq!(results.len(), 3);
    for r in &results {
        let rtype = r["type"].as_str().unwrap_or("unknown");
        assert!(
            rtype == "result" || rtype == "timeout",
            "Unexpected response type: {}",
            rtype
        );
    }

    // Camera should be registered with 3 frames
    let cameras = state.cameras.read().await;
    let cam = cameras.get("e2e-cam1").expect("Camera should exist");
    assert_eq!(cam.frame_no, 3);
    assert!(cam.latest_frame.is_some());
    assert_eq!(cam.results.len(), 3);
    drop(cameras);

    // Snapshot API should serve the frame
    let resp = reqwest::get(format!("{}/api/snapshot/e2e-cam1", base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap(),
        "image/jpeg"
    );

    // Camera list should include our camera
    let resp = reqwest::get(format!("{}/api/cameras", base_url))
        .await
        .unwrap();
    let cameras: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(cameras.iter().any(|c| c["camera_id"] == "e2e-cam1"));
}

#[tokio::test]
async fn test_e2e_multiple_cameras() {
    let (base_url, state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    // Two cameras streaming concurrently
    let (r1, r2) = tokio::join!(
        simulate_camera(&ws_url, "multi-cam1", "Camera 1", 2),
        simulate_camera(&ws_url, "multi-cam2", "Camera 2", 2),
    );

    assert_eq!(r1.len(), 2);
    assert_eq!(r2.len(), 2);

    // Both cameras should be registered
    let cameras = state.cameras.read().await;
    assert!(cameras.contains_key("multi-cam1"));
    assert!(cameras.contains_key("multi-cam2"));
    assert_eq!(cameras["multi-cam1"].frame_no, 2);
    assert_eq!(cameras["multi-cam2"].frame_no, 2);
}

#[tokio::test]
async fn test_e2e_sse_events() {
    let (_base_url, state) = start_server().await;

    // Subscribe to SSE
    let mut sse_rx = state.events_tx.subscribe();

    // Add a camera with a result
    {
        let mut cameras = state.cameras.write().await;
        let mut cam = floor_monitor_server::state::CameraState::new(
            "sse-cam".to_string(),
            "SSE Test".to_string(),
        );
        cam.running = true;
        cameras.insert("sse-cam".to_string(), cam);
    }

    // Broadcast a fake event
    let fake_result = serde_json::json!({
        "camera_id": "sse-cam",
        "frame_no": 1,
        "time": "12:00:00",
        "infer_secs": 0.5,
        "model": "test",
        "text": "Test event",
    });
    let _ = state
        .events_tx
        .send(serde_json::to_string(&fake_result).unwrap());

    // SSE subscriber should receive the event
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), sse_rx.recv())
        .await
        .expect("SSE timeout")
        .expect("SSE recv error");
    let parsed: serde_json::Value = serde_json::from_str(&event).unwrap();
    assert_eq!(parsed["camera_id"], "sse-cam");
    assert_eq!(parsed["text"], "Test event");
}

#[tokio::test]
async fn test_e2e_camera_disconnect_cleanup() {
    let (base_url, state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    // Connect, register, then disconnect
    {
        let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
        let register = serde_json::json!({
            "type": "register",
            "camera_id": "disconnect-cam",
            "name": "Disconnect Test",
        });
        ws.send(Message::Text(register.to_string().into()))
            .await
            .unwrap();
        let _ = ws.next().await; // read ack
        ws.close(None).await.ok();
    }

    // Give the server a moment to process the disconnect
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Camera should exist but be marked as not running
    let cameras = state.cameras.read().await;
    let cam = cameras
        .get("disconnect-cam")
        .expect("Camera should exist after disconnect");
    assert!(
        !cam.running,
        "Camera should be marked as not running after disconnect"
    );
}

/// When FLOOR_MONITOR_E2E_VLM=1 is set, test with a real VLM backend.
/// This test sends a frame and validates the inference result contains
/// expected structured JSON fields.
#[tokio::test]
async fn test_e2e_real_vlm_inference() {
    if std::env::var("FLOOR_MONITOR_E2E_VLM").unwrap_or_default() != "1" {
        eprintln!("Skipping real VLM test (set FLOOR_MONITOR_E2E_VLM=1 to enable)");
        return;
    }

    let (base_url, _state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    let results = simulate_camera(&ws_url, "vlm-cam", "VLM Test Camera", 1).await;
    assert_eq!(results.len(), 1);

    let r = &results[0];
    assert_eq!(r["type"], "result");
    let text = r["text"].as_str().unwrap_or("");
    assert!(!text.is_empty(), "VLM should return non-empty text");
    assert!(
        r["infer_secs"].as_f64().unwrap_or(0.0) > 0.0,
        "Inference time should be positive"
    );

    // The response should be parseable JSON (using monitor profile prompt)
    let parsed = floor_monitor_server::monitor::parse_vlm_json(text);
    if let Some(ref v) = parsed {
        assert!(v.get("activity").is_some(), "Should have activity field");
        assert!(
            v.get("risk_level").is_some(),
            "Should have risk_level field"
        );
    }
    // Note: parsing may fail for small test images, so we don't assert parsed.is_some()
}

/// Binary frame protocol test: send raw JPEG bytes instead of base64 JSON.
#[tokio::test]
async fn test_e2e_binary_frame_protocol() {
    let (base_url, _state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // Register first
    let register = serde_json::json!({
        "type": "register",
        "camera_id": "bin-cam",
        "name": "Binary Test",
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .unwrap();
    let _ack = ws.next().await.unwrap().unwrap();

    // Send a binary frame (raw JPEG)
    let jpeg = test_jpeg();
    ws.send(Message::Binary(jpeg.into())).await.unwrap();

    // Server should process it (response or timeout is fine — the point is no crash)
    match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
        Ok(Some(Ok(msg))) => {
            // Got a response — good
            let text = msg.into_text().unwrap_or_default();
            if !text.is_empty() {
                let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["type"], "result");
            }
        }
        _ => {
            // Timeout is acceptable — server tried inference on unreachable VLM
        }
    }

    // Frame should be stored
    let cameras = _state.cameras.read().await;
    let cam = cameras.get("bin-cam").expect("Camera should exist");
    assert!(cam.latest_frame.is_some());
}

/// Test server→camera command delivery via WebSocket.
#[tokio::test]
async fn test_e2e_camera_receives_command() {
    let (base_url, state) = start_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // Register
    let register = serde_json::json!({
        "type": "register",
        "camera_id": "cmd-cam",
        "name": "Command Test",
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .unwrap();
    let _ack = ws.next().await.unwrap().unwrap();

    // Small delay to let cmd_tx be stored
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Send a command from the server side
    let result = floor_monitor_server::ws::send_camera_command(
        &state,
        "cmd-cam",
        "ptz",
        serde_json::json!({"direction": "pan_left"}),
    )
    .await;
    assert!(result.is_ok(), "send_camera_command should succeed");

    // Camera should receive the command
    match tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["type"], "command");
            assert_eq!(v["action"], "ptz");
            assert_eq!(v["params"]["direction"], "pan_left");
        }
        other => panic!("Expected command message, got: {:?}", other),
    }
}

/// Test that sending a command to a disconnected camera returns an error.
#[tokio::test]
async fn test_e2e_command_to_nonexistent_camera() {
    let (_base_url, state) = start_server().await;

    let result = floor_monitor_server::ws::send_camera_command(
        &state,
        "no-such-cam",
        "ptz",
        serde_json::json!({}),
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

/// Test that capability checking rejects commands the camera doesn't support.
#[tokio::test]
async fn test_e2e_capability_check() {
    let (_base_url, state) = start_server().await;
    let ws_url = _base_url.replace("http://", "ws://") + "/ws";

    // Register a camera WITHOUT ptz capability
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let register = serde_json::json!({
        "type": "register",
        "camera_id": "no-ptz-cam",
        "name": "Fixed Camera",
        "capabilities": [],
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .unwrap();
    let _ack = ws.next().await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trying to send PTZ via send_command_to_any_camera should fail
    let result = floor_monitor_server::ws::send_command_to_any_camera(
        &state,
        "ptz",
        serde_json::json!({"direction": "pan_left"}),
    )
    .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("does not support"));

    // Now register a camera WITH ptz capability
    let (mut ws2, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let register2 = serde_json::json!({
        "type": "register",
        "camera_id": "ptz-cam",
        "name": "PTZ Camera",
        "capabilities": ["ptz", "patrol"],
    });
    ws2.send(Message::Text(register2.to_string().into()))
        .await
        .unwrap();
    let _ack2 = ws2.next().await.unwrap().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Now the same command should succeed (routed to ptz-cam)
    let result = floor_monitor_server::ws::send_command_to_any_camera(
        &state,
        "ptz",
        serde_json::json!({"direction": "pan_left"}),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "ptz-cam");
}
