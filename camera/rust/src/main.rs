//! Floor Monitor — Camera Client (Rust)
//!
//! Captures frames from a local webcam and streams them to the floor-monitor
//! server via WebSocket. Reads the same `camera.toml` config as the Python client.
//!
//! Note: RTSP support requires additional FFmpeg bindings (not included).
//! For RTSP cameras, use the Python client or add opencv/ffmpeg crate support.

use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;
use serde::Deserialize;
use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{error, info, warn};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
    camera: CameraConfig,
}

#[derive(Debug, Deserialize)]
struct ServerConfig {
    ws_url: String,
}

#[derive(Debug, Deserialize)]
struct CameraConfig {
    id: String,
    name: String,
    #[serde(default)]
    source_type: String,
    #[serde(default)]
    device_index: u32,
    #[serde(default = "default_interval")]
    interval: f64,
    #[serde(default = "default_max_dim")]
    max_dimension: u32,
    #[serde(default = "default_quality")]
    jpeg_quality: u8,
    /// Capabilities advertised on registration (e.g. "ptz", "patrol").
    /// The server uses these to decide which cameras can receive movement
    /// commands. Leave empty for fixed cameras with no PTZ hardware.
    #[serde(default)]
    capabilities: Vec<String>,
}

fn default_interval() -> f64 {
    2.0
}
fn default_max_dim() -> u32 {
    768
}
fn default_quality() -> u8 {
    85
}

fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}

/// Handle a `command` message from the server: log it and ack it.
/// Mirrors the Python client — we don't drive real motor hardware here,
/// so PTZ/patrol commands are acknowledged but not acted upon.
async fn handle_command(write: &mut WsWrite, camera_id: &str, data: &serde_json::Value) {
    let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let params = data
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    info!("Received command: action={} params={}", action, params);

    let (success, message) = match action {
        "ptz" => {
            let direction = params
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            info!(
                "PTZ command: {} (no PTZ hardware on this client)",
                direction
            );
            (
                true,
                format!("PTZ {} acknowledged (no PTZ hardware)", direction),
            )
        }
        "patrol" => {
            info!("Patrol command (no PTZ hardware on this client)");
            (true, "Patrol acknowledged (no PTZ hardware)".to_string())
        }
        other => {
            warn!("Unknown command action: {}", other);
            (false, format!("Unknown action: {}", other))
        }
    };

    let ack = serde_json::json!({
        "type": "command_ack",
        "camera_id": camera_id,
        "action": action,
        "success": success,
        "message": message,
    });
    if let Err(e) = write.send(Message::Text(ack.to_string().into())).await {
        warn!("Failed to send command_ack: {}", e);
    }
}

/// Drain any pending command messages without blocking the frame loop.
/// Called after a result arrives to handle commands that the server
/// queued between cycles. Mirrors the Python client's 10ms recv loop.
async fn drain_pending_commands(read: &mut WsRead, write: &mut WsWrite, camera_id: &str) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_millis(10), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                    if data.get("type").and_then(|t| t.as_str()) == Some("command") {
                        handle_command(write, camera_id, &data).await;
                    }
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                warn!("WebSocket error during drain: {}", e);
                return false;
            }
            Ok(None) => {
                info!("Server closed connection during drain");
                return false;
            }
            Err(_) => {
                // Timeout: no more pending messages.
                return true;
            }
        }
    }
}

/// Capture a frame from the local camera using nokhwa, encode as JPEG.
fn capture_frame_jpeg(
    camera: &mut nokhwa::Camera,
    max_dim: u32,
    quality: u8,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let frame = camera.frame()?;
    let decoded = frame.decode_image::<nokhwa::pixel_format::RgbFormat>()?;

    // Resize if needed
    let img = if decoded.width() > max_dim || decoded.height() > max_dim {
        image::DynamicImage::ImageRgb8(decoded).resize(
            max_dim,
            max_dim,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image::DynamicImage::ImageRgb8(decoded)
    };

    // Encode as JPEG
    let rgb = img.to_rgb8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = JpegEncoder::new_with_quality(&mut buf, quality);
    encoder.write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(buf.into_inner())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "floor_monitor_camera=info".into()),
        )
        .init();

    let config_path = std::env::args().nth(1).unwrap_or_else(|| {
        // Look for camera.toml in parent directory (camera/) or current dir
        if Path::new("../camera.toml").exists() {
            "../camera.toml".to_string()
        } else {
            "camera.toml".to_string()
        }
    });

    let config = load_config(Path::new(&config_path))?;
    info!("Camera: {} ({})", config.camera.name, config.camera.id);

    if config.camera.source_type == "rtsp" {
        error!("RTSP sources are not supported in the Rust client. Use the Python client.");
        error!("The Rust client supports local USB/built-in cameras only.");
        std::process::exit(1);
    }

    // Open local camera
    let index = nokhwa::utils::CameraIndex::Index(config.camera.device_index);
    let requested = nokhwa::utils::RequestedFormat::new::<nokhwa::pixel_format::RgbFormat>(
        nokhwa::utils::RequestedFormatType::AbsoluteHighestFrameRate,
    );
    let mut camera = nokhwa::Camera::new(index, requested)?;
    camera.open_stream()?;
    info!(
        "Camera stream opened (index={})",
        config.camera.device_index
    );

    let interval = Duration::from_secs_f64(config.camera.interval);

    // Connection loop with auto-reconnect
    loop {
        info!("Connecting to {} ...", config.server.ws_url);
        match connect_async(&config.server.ws_url).await {
            Ok((ws_stream, _)) => {
                info!("Connected to server");
                let (mut write, mut read) = ws_stream.split();

                // Register
                let register = serde_json::json!({
                    "type": "register",
                    "camera_id": config.camera.id,
                    "name": config.camera.name,
                    "capabilities": config.camera.capabilities,
                });
                if let Err(e) = write.send(Message::Text(register.to_string().into())).await {
                    warn!("Failed to send register: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }

                // Wait for ack
                if let Some(Ok(msg)) = read.next().await {
                    info!("Server: {}", msg);
                }

                // Frame loop
                let mut frame_no: u64 = 0;
                loop {
                    let t0 = Instant::now();

                    match capture_frame_jpeg(
                        &mut camera,
                        config.camera.max_dimension,
                        config.camera.jpeg_quality,
                    ) {
                        Ok(jpeg) => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                            let msg = serde_json::json!({
                                "type": "frame",
                                "camera_id": config.camera.id,
                                "jpeg_b64": b64,
                            });
                            if let Err(e) = write.send(Message::Text(msg.to_string().into())).await
                            {
                                warn!("Send failed: {} — reconnecting", e);
                                break;
                            }
                            frame_no += 1;

                            // Wait for the inference result, dispatching any
                            // command messages that arrive in the meantime.
                            // Total budget is 120s; commands don't reset it.
                            let deadline = Instant::now() + Duration::from_secs(120);
                            let mut connection_alive = true;
                            loop {
                                let remaining = deadline.saturating_duration_since(Instant::now());
                                if remaining.is_zero() {
                                    warn!("Inference timeout — continuing");
                                    break;
                                }
                                match tokio::time::timeout(remaining, read.next()).await {
                                    Ok(Some(Ok(Message::Text(text)))) => {
                                        let Ok(data) =
                                            serde_json::from_str::<serde_json::Value>(&text)
                                        else {
                                            continue;
                                        };
                                        match data.get("type").and_then(|t| t.as_str()) {
                                            Some("result") => {
                                                info!(
                                                    "Frame {}: infer={:.2}s — {}",
                                                    frame_no,
                                                    data.get("infer_secs")
                                                        .and_then(|v| v.as_f64())
                                                        .unwrap_or(0.0),
                                                    data.get("text")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                        .chars()
                                                        .take(80)
                                                        .collect::<String>()
                                                );
                                                break;
                                            }
                                            Some("command") => {
                                                handle_command(
                                                    &mut write,
                                                    &config.camera.id,
                                                    &data,
                                                )
                                                .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                    Ok(Some(Ok(_))) => {}
                                    Ok(Some(Err(e))) => {
                                        warn!("WebSocket error: {} — reconnecting", e);
                                        connection_alive = false;
                                        break;
                                    }
                                    Ok(None) => {
                                        info!("Server closed connection");
                                        connection_alive = false;
                                        break;
                                    }
                                    Err(_) => {
                                        warn!("Inference timeout — continuing");
                                        break;
                                    }
                                }
                            }
                            if !connection_alive {
                                break;
                            }

                            // Drain any commands queued behind the result.
                            if !drain_pending_commands(&mut read, &mut write, &config.camera.id)
                                .await
                            {
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("Frame capture failed: {}", e);
                        }
                    }

                    // Sleep remaining interval
                    let elapsed = t0.elapsed();
                    if elapsed < interval {
                        tokio::time::sleep(interval - elapsed).await;
                    }
                }
            }
            Err(e) => {
                warn!("Connection failed: {} — retrying in 5s", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
