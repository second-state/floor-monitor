//! WebSocket command handling.
//!
//! - [`dispatch`] is the pure-async core: maps `(action, params)` to
//!   `(success, message)` by routing through the `Ptz` trait. Tests call
//!   this directly with a [`crate::ptz::fake::FakePtz`] and never touch
//!   a real WebSocket.
//! - [`build_ack`] formats the JSON shape the server expects.
//! - [`handle_command`] is the thin glue that pulls fields off the inbound
//!   `command` frame, calls `dispatch`, builds the ack, and writes it back.

use crate::config::PatrolConfig;
use crate::ptz::patrol::{start_patrol, PatrolHandle};
use crate::ptz::{self, Ptz};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::SinkExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type WsWrite = SplitSink<WsStream, Message>;
pub type WsRead = SplitStream<WsStream>;

/// Per-connection command-handling state. The `patrol_slot` lives across
/// frame loops so a new patrol can cancel the previous one and disconnect
/// can take + cancel any in-flight patrol before reconnect.
pub struct CommandCtx<'a> {
    pub ptz: Arc<dyn Ptz>,
    pub patrol_slot: &'a mut Option<PatrolHandle>,
    pub patrol_cfg: &'a PatrolConfig,
}

/// Build the `command_ack` JSON envelope. Wire format must match what
/// the server's `CameraMessage::CommandAck` deserializer expects.
pub fn build_ack(camera_id: &str, action: &str, success: bool, message: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "command_ack",
        "camera_id": camera_id,
        "action": action,
        "success": success,
        "message": message,
    })
}

/// Pure dispatch: route an action+params pair through the `Ptz` trait
/// and return the `(success, message)` tuple that goes into the ack.
///
/// The `patrol` arm cancels any in-flight patrol, spawns a new one, and
/// acks `patrol_started` immediately. The new task runs to completion (or
/// cancellation) on the tokio runtime; the WebSocket loop is never blocked
/// by patrol's multi-second sweep.
pub async fn dispatch(
    ctx: &mut CommandCtx<'_>,
    action: &str,
    params: &serde_json::Value,
) -> (bool, String) {
    info!("dispatch: action={} params={}", action, params);
    match action {
        "ptz" => match ptz::execute_ptz(&ctx.ptz, params).await {
            Ok(msg) => (true, msg),
            Err(e) => (false, e.to_string()),
        },
        "patrol" => {
            // Cancel any previous patrol so a new request always wins.
            if let Some(prev) = ctx.patrol_slot.take() {
                prev.cancel().await;
            }
            // Patrol requires a ptz that can pan.
            if !ctx.ptz.capabilities().pan {
                return (false, "patrol unsupported (no pan)".to_string());
            }
            *ctx.patrol_slot = Some(start_patrol(ctx.ptz.clone(), ctx.patrol_cfg.clone()));
            (true, "patrol_started".to_string())
        }
        other => (false, format!("Unknown action: {}", other)),
    }
}

/// Handle a `command` message from the server: dispatch via the `Ptz`
/// trait, then send a `command_ack` back over the WebSocket.
pub async fn handle_command(
    write: &mut WsWrite,
    camera_id: &str,
    ctx: &mut CommandCtx<'_>,
    data: &serde_json::Value,
) {
    let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let params = data
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let (success, message) = dispatch(ctx, action, &params).await;
    let ack = build_ack(camera_id, action, success, &message);
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
    ctx: &mut CommandCtx<'_>,
) -> bool {
    use futures_util::StreamExt;
    loop {
        match tokio::time::timeout(Duration::from_millis(10), read.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                    if data.get("type").and_then(|t| t.as_str()) == Some("command") {
                        handle_command(write, camera_id, ctx, &data).await;
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
