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

mod ptz;
use ptz::{Axis, Dir, Ptz};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWrite = SplitSink<WsStream, Message>;
type WsRead = SplitStream<WsStream>;

#[derive(Debug, Deserialize)]
struct Config {
    server: ServerConfig,
    camera: CameraConfig,
    #[serde(default)]
    ptz: ptz::PtzConfig,
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

/// Bundles the live controller with patrol parameters, threaded through the loop.
struct PtzRuntime {
    ptz: Box<dyn Ptz>,
    patrol_steps: u32,
    patrol_dwell: Duration,
}

/// Blocking left/right sweep per the design spec: pan_left N, dwell,
/// pan_right 2N, dwell, pan_left N — the dwell falls between groups, not
/// between individual steps (kept in sync with the Python V4L2 patrol).
async fn run_patrol(rt: &mut PtzRuntime) -> Result<(), String> {
    let n = rt.patrol_steps;
    let dwell = rt.patrol_dwell;
    for _ in 0..n {
        rt.ptz.step(Axis::Pan, Dir::Neg)?;
    }
    tokio::time::sleep(dwell).await;
    for _ in 0..n.saturating_mul(2) {
        rt.ptz.step(Axis::Pan, Dir::Pos)?;
    }
    tokio::time::sleep(dwell).await;
    for _ in 0..n {
        rt.ptz.step(Axis::Pan, Dir::Neg)?;
    }
    Ok(())
}

/// Handle a `command` message: drive PTZ/zoom/patrol hardware, then ack.
async fn handle_command(
    write: &mut WsWrite,
    camera_id: &str,
    data: &serde_json::Value,
    rt: &mut PtzRuntime,
) {
    let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let params = data
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    info!("Received command: action={} params={}", action, params);

    let direction = params
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (success, message) = match action {
        "ptz" | "zoom" => match ptz::parse_direction(direction) {
            Some((axis, dir)) => match rt.ptz.step(axis, dir) {
                Ok(()) => (true, format!("{action} {direction} completed")),
                Err(e) => (false, format!("{action} {direction} failed: {e}")),
            },
            None => (false, format!("unknown direction: {direction}")),
        },
        "patrol" => match run_patrol(rt).await {
            Ok(()) => (true, "Patrol completed".to_string()),
            Err(e) => (false, format!("Patrol failed: {e}")),
        },
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
async fn drain_pending_commands(
    read: &mut WsRead,
    write: &mut WsWrite,
    camera_id: &str,
    rt: &mut PtzRuntime,
) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_millis(10), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                    if data.get("type").and_then(|t| t.as_str()) == Some("command") {
                        handle_command(write, camera_id, &data, rt).await;
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

    // Resolve the V4L2 device and detect PTZ controls once at startup.
    let ptz_device = config
        .ptz
        .device
        .clone()
        .unwrap_or_else(|| format!("/dev/video{}", config.camera.device_index));
    let detected = ptz::detect_controls(&ptz::V4l2CtlRunner, &ptz_device);
    let detected_caps = ptz::capabilities_from_controls(&detected);
    if detected.is_empty() {
        info!("PTZ: no V4L2 controls on {ptz_device} (or v4l2-ctl unavailable)");
    } else {
        info!("PTZ: detected {detected_caps:?} on {ptz_device}");
    }
    let capabilities = ptz::resolve_capabilities(&config.camera.capabilities, &detected_caps);
    let mut ptz_runtime = PtzRuntime {
        ptz: ptz::build_ptz(&config.ptz, &ptz_device, detected),
        // Floor at 1 like the Python client's max(1, patrol_steps), so a
        // configured 0 still sweeps instead of being a silent no-op.
        patrol_steps: config.ptz.patrol_steps.max(1),
        patrol_dwell: Duration::from_secs_f64(config.ptz.patrol_dwell_sec.max(0.0)),
    };

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
                    "capabilities": capabilities,
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
                                                    &mut ptz_runtime,
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
                            if !drain_pending_commands(
                                &mut read,
                                &mut write,
                                &config.camera.id,
                                &mut ptz_runtime,
                            )
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
