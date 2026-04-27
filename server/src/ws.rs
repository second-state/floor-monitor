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
use tokio::sync::mpsc;
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
    #[serde(rename = "register")]
    Register {
        camera_id: String,
        name: String,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    #[serde(rename = "frame")]
    Frame { camera_id: String, jpeg_b64: String },
    #[serde(rename = "command_ack")]
    CommandAck {
        camera_id: String,
        action: String,
        success: bool,
        #[allow(dead_code)]
        message: Option<String>,
    },
}

/// Messages sent from server to camera client.
#[derive(Serialize)]
#[serde(tag = "type")]
enum ServerMessage {
    #[serde(rename = "registered")]
    Registered { camera_id: String },
    #[serde(rename = "result")]
    Result {
        camera_id: String,
        frame_no: u64,
        text: String,
        infer_secs: f64,
    },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "command")]
    Command {
        camera_id: String,
        action: String,
        params: serde_json::Value,
    },
}

/// Send a command to a connected camera via its WebSocket channel.
pub async fn send_camera_command(
    state: &AppState,
    camera_id: &str,
    action: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let cameras = state.cameras.read().await;
    let cam = cameras
        .get(camera_id)
        .ok_or_else(|| format!("Camera '{}' not found", camera_id))?;
    let tx = cam
        .cmd_tx
        .as_ref()
        .ok_or_else(|| format!("Camera '{}' not connected", camera_id))?;
    let msg = ServerMessage::Command {
        camera_id: camera_id.to_string(),
        action: action.to_string(),
        params,
    };
    let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    tx.send(json).map_err(|e| e.to_string())
}

/// Send a command to the first running camera that supports the required capability.
/// `action` is also used as the capability name (e.g. "ptz", "patrol").
pub async fn send_command_to_any_camera(
    state: &AppState,
    action: &str,
    params: serde_json::Value,
) -> Result<String, String> {
    let cameras = state.cameras.read().await;

    // First try to find a camera with the matching capability
    let cam = cameras
        .values()
        .find(|c| c.running && c.cmd_tx.is_some() && c.has_capability(action))
        .or_else(|| {
            // Fall back to any connected camera (it may handle the command anyway)
            cameras.values().find(|c| c.running && c.cmd_tx.is_some())
        })
        .ok_or_else(|| "No connected camera available".to_string())?;

    if !cam.has_capability(action) {
        return Err(format!(
            "Camera '{}' does not support '{}'. Capabilities: {:?}",
            cam.camera_id, action, cam.capabilities
        ));
    }
    let camera_id = cam.camera_id.clone();
    let tx = cam.cmd_tx.as_ref().unwrap();
    let msg = ServerMessage::Command {
        camera_id: camera_id.clone(),
        action: action.to_string(),
        params,
    };
    let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    tx.send(json).map_err(|e| e.to_string())?;
    Ok(camera_id)
}

async fn handle_camera_ws(socket: WebSocket, state: Arc<AppState>) {
    let (ws_sender, mut receiver) = socket.split();
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));
    let mut camera_id: Option<String> = None;

    // Channel for sending commands to this camera
    let (cmd_tx, mut cmd_rx): (
        mpsc::UnboundedSender<String>,
        mpsc::UnboundedReceiver<String>,
    ) = mpsc::unbounded_channel();

    // Spawn task to forward commands from channel to WebSocket
    let ws_sender_clone = ws_sender.clone();
    let cmd_forwarder = tokio::spawn(async move {
        while let Some(msg) = cmd_rx.recv().await {
            let mut sender = ws_sender_clone.lock().await;
            if sender.send(Message::Text(msg.into())).await.is_err() {
                break;
            }
        }
    });

    info!("New WebSocket connection");

    while let Some(msg) = receiver.next().await {
        let msg = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(b)) => {
                if let Some(ref cid) = camera_id {
                    let jpeg_bytes = b.to_vec();
                    let mut sender = ws_sender.lock().await;
                    process_frame(&state, cid, &jpeg_bytes, &mut *sender).await;
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
                let mut sender = ws_sender.lock().await;
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
                capabilities,
            } => {
                info!(
                    "Camera registered: {} ({}) capabilities={:?}",
                    cid, name, capabilities
                );
                camera_id = Some(cid.clone());

                let mut cameras = state.cameras.write().await;
                cameras
                    .entry(cid.clone())
                    .or_insert_with(|| CameraState::new(cid.clone(), name));
                let cam = cameras.get_mut(&cid).unwrap();
                cam.running = true;
                cam.cmd_tx = Some(cmd_tx.clone());
                cam.capabilities = capabilities;

                let ack = ServerMessage::Registered { camera_id: cid };
                let mut sender = ws_sender.lock().await;
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
                let mut sender = ws_sender.lock().await;
                process_frame(&state, &cid, &jpeg_bytes, &mut *sender).await;
            }
            CameraMessage::CommandAck {
                camera_id: cid,
                action,
                success,
                ..
            } => {
                info!(
                    "Camera {} command ack: action={} success={}",
                    cid, action, success
                );
            }
        }
    }

    // Cleanup: mark camera as not running, remove cmd_tx
    cmd_forwarder.abort();
    if let Some(cid) = camera_id {
        info!("Camera {} disconnected", cid);
        let mut cameras = state.cameras.write().await;
        if let Some(cam) = cameras.get_mut(&cid) {
            cam.running = false;
            cam.cmd_tx = None;
        }
    }
}

async fn process_frame(
    state: &AppState,
    camera_id: &str,
    jpeg_bytes: &[u8],
    sender: &mut (impl SinkExt<Message> + Unpin),
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

    // Check for alerts
    {
        let mut tracker = state.alert_tracker.lock().await;
        if let Some(alert) =
            tracker.check_frame(camera_id, frame_no, &parsed_json, Some(jpeg_bytes.to_vec()))
        {
            let _ = state.alert_tx.send(alert);
        }
    }

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
            if cam.results.len() > 200 {
                cam.results.drain(..cam.results.len() - 200);
            }
        }
    }

    // Broadcast to SSE subscribers (tagged envelope: kind="result")
    let event_json =
        serde_json::to_string(&crate::state::SseEvent::Result(result.clone())).unwrap_or_default();
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
