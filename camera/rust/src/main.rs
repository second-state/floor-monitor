//! Floor Monitor — Camera Client (Rust) binary.
//!
//! Captures frames from a local webcam via `nokhwa` and streams them to the
//! floor-monitor server via WebSocket. Reads the same `camera.toml` config
//! as the Python client.
//!
//! All protocol-side code (config parsing, WebSocket command handling) lives
//! in the library half (`floor_monitor_camera::*`). Only the webcam capture
//! loop stays here.
//!
//! Note: RTSP support requires additional FFmpeg bindings (not included).
//! For RTSP cameras, use the Python client.

use base64::Engine;
use floor_monitor_camera::commands::{drain_pending_commands, handle_command, CommandCtx};
use floor_monitor_camera::config::{load_config, Config};
use floor_monitor_camera::ptz::{
    self, detect::resolve_advertised_capabilities, patrol::PatrolHandle, Ptz,
};
use futures_util::{SinkExt, StreamExt};
use image::codecs::jpeg::JpegEncoder;
use image::ImageEncoder;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{error, info, warn};

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

    let config: Config = load_config(Path::new(&config_path))?;
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

    // PTZ controller. On Linux with detected pan/tilt controls this is
    // V4l2CtlPtz; everywhere else it's NoopPtz (acks but doesn't move).
    // The Ptz impl is chosen entirely by detection + `[ptz] enabled`;
    // [camera] capabilities only renames the advertised wire list.
    let ptz: Arc<dyn Ptz> = ptz::build(&config.ptz, &config.camera).await;
    let advertised_caps =
        resolve_advertised_capabilities(&config.camera.capabilities, ptz.capabilities());
    info!("PTZ caps advertised: {:?}", advertised_caps);

    // Patrol task slot. Lives across reconnects so we can cancel any
    // in-flight patrol when the WebSocket drops.
    let mut patrol_slot: Option<PatrolHandle> = None;

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
                    "capabilities": advertised_caps,
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
                                                let mut ctx = CommandCtx {
                                                    ptz: ptz.clone(),
                                                    patrol_slot: &mut patrol_slot,
                                                    patrol_cfg: &config.ptz.patrol,
                                                };
                                                handle_command(
                                                    &mut write,
                                                    &config.camera.id,
                                                    &mut ctx,
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
                            let mut ctx = CommandCtx {
                                ptz: ptz.clone(),
                                patrol_slot: &mut patrol_slot,
                                patrol_cfg: &config.ptz.patrol,
                            };
                            if !drain_pending_commands(
                                &mut read,
                                &mut write,
                                &config.camera.id,
                                &mut ctx,
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

        // Cancel any in-flight patrol before reconnect — the dashboard
        // sees us as offline, so an orphan patrol moving the camera
        // silently would be surprising.
        if let Some(p) = patrol_slot.take() {
            info!("Cancelling in-flight patrol due to disconnect");
            p.cancel().await;
        }
    }
}
