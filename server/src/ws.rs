use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::state::{AppState, CameraState, FrameResult};

/// WebSocket upgrade handler for camera clients.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_camera_ws(socket, state))
}

/// Messages sent from camera client to server.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum CameraMessage {
    /// Camera registers itself with an ID and name.
    #[serde(rename = "register")]
    Register { camera_id: String, name: String },
    /// Camera sends a JPEG frame (base64-encoded).
    #[serde(rename = "frame")]
    Frame { camera_id: String, jpeg_b64: String },
}

/// Messages sent from server to camera client.
#[derive(Serialize)]
#[serde(tag = "type")]
enum ServerMessage {
    /// Acknowledge registration.
    #[serde(rename = "registered")]
    Registered { camera_id: String },
    /// Inference result for a frame.
    #[serde(rename = "result")]
    Result {
        camera_id: String,
        frame_no: u64,
        text: String,
        infer_secs: f64,
    },
    /// Error message.
    #[serde(rename = "error")]
    Error { message: String },
}

async fn handle_camera_ws(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut camera_id: Option<String> = None;

    info!("New WebSocket connection");

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(b)) => {
                // Binary messages: treat as raw JPEG frame if camera is registered
                if let Some(ref cid) = camera_id {
                    let jpeg_bytes = b.to_vec();
                    process_frame(&state, cid, &jpeg_bytes, &mut sender).await;
                }
                continue;
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket closed by client");
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                warn!("WebSocket error: {}", e);
                break;
            }
        };

        let parsed: CameraMessage = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                let err = ServerMessage::Error {
                    message: format!("Invalid message: {}", e),
                };
                let _ = sender
                    .send(Message::Text(serde_json::to_string(&err).unwrap().into()))
                    .await;
                continue;
            }
        };

        match parsed {
            CameraMessage::Register {
                camera_id: cid,
                name,
            } => {
                info!("Camera registered: {} ({})", cid, name);
                camera_id = Some(cid.clone());

                let mut cameras = state.cameras.write().await;
                cameras
                    .entry(cid.clone())
                    .or_insert_with(|| CameraState::new(cid.clone(), name));
                cameras.get_mut(&cid).unwrap().running = true;

                let ack = ServerMessage::Registered { camera_id: cid };
                let _ = sender
                    .send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
                    .await;
            }
            CameraMessage::Frame {
                camera_id: cid,
                jpeg_b64,
            } => {
                let jpeg_bytes = match base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &jpeg_b64,
                ) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!("Invalid base64 in frame: {}", e);
                        continue;
                    }
                };
                process_frame(&state, &cid, &jpeg_bytes, &mut sender).await;
            }
        }
    }

    // Cleanup: mark camera as not running
    if let Some(cid) = camera_id {
        info!("Camera {} disconnected", cid);
        let mut cameras = state.cameras.write().await;
        if let Some(cam) = cameras.get_mut(&cid) {
            cam.running = false;
        }
    }
}

async fn process_frame(
    state: &AppState,
    camera_id: &str,
    jpeg_bytes: &[u8],
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) {
    // Update latest frame
    let frame_no = {
        let mut cameras = state.cameras.write().await;
        if let Some(cam) = cameras.get_mut(camera_id) {
            cam.latest_frame = Some(jpeg_bytes.to_vec());
            cam.frame_no += 1;
            cam.frame_no
        } else {
            warn!("Frame from unregistered camera {}", camera_id);
            return;
        }
    };

    // Get the prompt — use the default monitor profile prompt
    let profile_id = &state.config.monitor.default_profile;
    let prompt = state
        .monitor_profiles
        .get(profile_id)
        .map(|p| p.prompt.as_str())
        .unwrap_or("Describe what you see in this image.");

    // Run VLM inference
    let (text, infer_secs) = match state.vlm.infer(jpeg_bytes, prompt).await {
        Ok(r) => r,
        Err(e) => {
            warn!("VLM inference failed for frame {}: {}", frame_no, e);
            (format!("[Error] {}", e), 0.0)
        }
    };

    // Parse structured JSON if possible
    let parsed_json = crate::monitor::parse_vlm_json(&text);

    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let result = FrameResult {
        camera_id: camera_id.to_string(),
        frame_no,
        time: now,
        infer_secs,
        model: state.vlm.model_name().to_string(),
        text: text.clone(),
        parsed_json,
    };

    // Store result
    {
        let mut cameras = state.cameras.write().await;
        if let Some(cam) = cameras.get_mut(camera_id) {
            cam.results.push(result.clone());
            // Keep last 200 results
            if cam.results.len() > 200 {
                cam.results.drain(..cam.results.len() - 200);
            }
        }
    }

    // Broadcast to SSE subscribers
    let event_json = serde_json::to_string(&result).unwrap_or_default();
    let _ = state.events_tx.send(event_json);

    // Send result back to camera client
    let reply = ServerMessage::Result {
        camera_id: camera_id.to_string(),
        frame_no,
        text,
        infer_secs,
    };
    let _ = sender
        .send(Message::Text(serde_json::to_string(&reply).unwrap().into()))
        .await;
}
