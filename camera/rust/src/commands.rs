//! WebSocket command handling — receives `{"type":"command",...}` frames
//! from the server and replies with `{"type":"command_ack",...}`.
//!
//! In commit 1 of this PR, this module preserves the existing stub
//! behavior: PTZ/patrol arms just log + ack with no real motor action.
//! A later commit replaces the stub arms with a `Ptz`-trait dispatch.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::SinkExt;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsWrite = SplitSink<WsStream, Message>;
pub type WsRead = SplitStream<WsStream>;

/// Handle a `command` message from the server: log it and ack it.
/// Mirrors the Python client — we don't drive real motor hardware here yet,
/// so PTZ/patrol commands are acknowledged but not acted upon.
pub async fn handle_command(write: &mut WsWrite, camera_id: &str, data: &serde_json::Value) {
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
pub async fn drain_pending_commands(
    read: &mut WsRead,
    write: &mut WsWrite,
    camera_id: &str,
) -> bool {
    use futures_util::StreamExt;
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
