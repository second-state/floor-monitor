//! Integration tests for the HTTP API endpoints.
//!
//! These tests start a real Axum server and make HTTP requests against it.
//! VLM inference is not tested here (no model server running); we test the
//! WebSocket protocol, snapshot API, camera listing, and SSE events.

use std::sync::Arc;
use tokio::net::TcpListener;

/// Create a test AppState with a mock config (no real VLM backend).
fn test_state() -> Arc<floor_monitor_server::state::AppState> {
    let config = floor_monitor_server::config::Config {
        server: floor_monitor_server::config::ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0, // will bind to random port
        },
        vlm: floor_monitor_server::config::VlmConfig {
            api_url: "http://localhost:99999/api/generate".to_string(), // unreachable
            api_key: None,
            model: "test-model".to_string(),
            max_tokens: 100,
            temperature: 0.1,
        },
        telegram: Default::default(),
        monitor: Default::default(),
    };
    Arc::new(floor_monitor_server::state::AppState::new(config))
}

/// Build the Axum app (same as main.rs but without Telegram).
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

async fn start_test_server() -> (String, Arc<floor_monitor_server::state::AppState>) {
    let state = test_state();
    let app = test_app(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{}", addr), state)
}

#[tokio::test]
async fn test_index_redirect() {
    let (base_url, _state) = start_test_server().await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let resp = client.get(&base_url).send().await.unwrap();
    assert_eq!(resp.status(), 303); // redirect
    assert!(resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("/dashboard"));
}

#[tokio::test]
async fn test_api_cameras_empty() {
    let (base_url, _state) = start_test_server().await;
    let resp = reqwest::get(format!("{}/api/cameras", base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn test_api_results_empty() {
    let (base_url, _state) = start_test_server().await;
    let resp = reqwest::get(format!("{}/api/results", base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_api_snapshot_not_found() {
    let (base_url, _state) = start_test_server().await;
    let resp = reqwest::get(format!("{}/api/snapshot/nonexistent", base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn test_api_cameras_after_adding() {
    let (_base_url, state) = start_test_server().await;

    // Simulate a camera registering
    {
        let mut cameras = state.cameras.write().await;
        cameras.insert(
            "test-cam".to_string(),
            floor_monitor_server::state::CameraState::new(
                "test-cam".to_string(),
                "Test Camera".to_string(),
            ),
        );
        cameras.get_mut("test-cam").unwrap().running = true;
    }

    let resp = reqwest::get(format!("{}/api/cameras", _base_url))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    assert_eq!(body.len(), 1);
    assert_eq!(body[0]["camera_id"], "test-cam");
    assert_eq!(body[0]["name"], "Test Camera");
    assert_eq!(body[0]["running"], true);
}

#[tokio::test]
async fn test_api_snapshot_with_frame() {
    let (_base_url, state) = start_test_server().await;

    // Add a camera with a fake JPEG frame
    let fake_jpeg = vec![0xFF, 0xD8, 0xFF, 0xD9]; // minimal JPEG markers
    {
        let mut cameras = state.cameras.write().await;
        let mut cam = floor_monitor_server::state::CameraState::new(
            "cam1".to_string(),
            "Camera 1".to_string(),
        );
        cam.latest_frame = Some(fake_jpeg.clone());
        cam.running = true;
        cameras.insert("cam1".to_string(), cam);
    }

    let resp = reqwest::get(format!("{}/api/snapshot/cam1", _base_url))
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
    let body = resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), &fake_jpeg);
}

#[tokio::test]
async fn test_websocket_register() {
    let (base_url, _state) = start_test_server().await;
    let ws_url = base_url.replace("http://", "ws://") + "/ws";

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("WebSocket connect failed");

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Message;

    // Send register message
    let register = serde_json::json!({
        "type": "register",
        "camera_id": "ws-test-cam",
        "name": "WS Test Camera",
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .unwrap();

    // Read ack
    let msg = ws.next().await.unwrap().unwrap();
    let text = msg.into_text().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(resp["type"], "registered");
    assert_eq!(resp["camera_id"], "ws-test-cam");

    // Verify camera appears in state
    let cameras = _state.cameras.read().await;
    assert!(cameras.contains_key("ws-test-cam"));
    assert!(cameras["ws-test-cam"].running);
}
