use reqwest::multipart;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::state::AppState;

const API_BASE: &str = "https://api.telegram.org/bot";
const POLL_TIMEOUT: u64 = 25;

/// Telegram notifier — send text messages and photos.
#[derive(Debug, Clone)]
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct TelegramApiResult {
    ok: bool,
}

#[derive(Deserialize)]
struct GetUpdatesResult {
    #[allow(dead_code)]
    ok: bool,
    result: Vec<Update>,
}

#[derive(Deserialize)]
struct Update {
    update_id: i64,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    chat: Chat,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
}

impl TelegramNotifier {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        Self {
            bot_token,
            chat_id,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_config(config: &crate::config::TelegramConfig) -> Option<Self> {
        let token = config.bot_token.as_deref()?.trim();
        let chat = config.chat_id.as_deref()?.trim();
        if token.is_empty() || chat.is_empty() {
            return None;
        }
        Some(Self::new(token.to_string(), chat.to_string()))
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", API_BASE, self.bot_token, method)
    }

    pub async fn send(&self, text: &str) -> bool {
        let payload = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        match self
            .http
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => resp
                .json::<TelegramApiResult>()
                .await
                .map(|r| r.ok)
                .unwrap_or(false),
            Err(e) => {
                warn!("Telegram send failed: {}", e);
                false
            }
        }
    }

    pub async fn send_photo(&self, jpeg_bytes: Vec<u8>, caption: &str) -> bool {
        let part = multipart::Part::bytes(jpeg_bytes)
            .file_name("frame.jpg")
            .mime_str("image/jpeg")
            .unwrap();
        let mut form = multipart::Form::new()
            .text("chat_id", self.chat_id.clone())
            .part("photo", part);
        if !caption.is_empty() {
            form = form
                .text("caption", caption[..caption.len().min(1024)].to_string())
                .text("parse_mode", "Markdown");
        }
        match self
            .http
            .post(self.api_url("sendPhoto"))
            .multipart(form)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
        {
            Ok(resp) => resp
                .json::<TelegramApiResult>()
                .await
                .map(|r| r.ok)
                .unwrap_or(false),
            Err(e) => {
                warn!("Telegram send_photo failed: {}", e);
                false
            }
        }
    }

    pub fn chat_id_i64(&self) -> i64 {
        self.chat_id.parse().unwrap_or(0)
    }
}

const HELP_TEXT: &str = "\
🤖 *Floor Monitor Bot*\n\
Ask me about the camera feed, or request a summary.\n\n\
*Commands:*\n\
• Free-form visual question (ZH or EN)\n\
• `过去 15 分钟怎么样？` — summarize recent activity\n\
• `截图` or `/snapshot` — current frame photo\n\
• `/help` · `/status`";

/// Start the Telegram long-polling listener as a background task.
pub fn start_listener(state: Arc<AppState>, notifier: TelegramNotifier) {
    tokio::spawn(async move {
        info!("Telegram listener started");
        let mut offset: Option<i64> = None;

        // Prime offset to skip backlog
        if let Ok(updates) = get_updates(&notifier, Some(-1), 1).await {
            if let Some(last) = updates.last() {
                offset = Some(last.update_id + 1);
                info!("Primed Telegram offset at {}", offset.unwrap());
            }
        }

        loop {
            match get_updates(&notifier, offset, POLL_TIMEOUT).await {
                Ok(updates) => {
                    for upd in updates {
                        offset = Some(upd.update_id + 1);
                        if let Some(msg) = upd.message {
                            if msg.chat.id != notifier.chat_id_i64() {
                                continue;
                            }
                            if let Some(text) = msg.text {
                                let text = text.trim().to_string();
                                if text.is_empty() {
                                    continue;
                                }
                                handle_message(&state, &notifier, &text).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("getUpdates failed: {}, retrying in 5s", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    });
}

async fn get_updates(
    notifier: &TelegramNotifier,
    offset: Option<i64>,
    timeout: u64,
) -> Result<Vec<Update>, reqwest::Error> {
    let mut params = serde_json::json!({"timeout": timeout});
    if let Some(off) = offset {
        params["offset"] = serde_json::json!(off);
    }
    let resp = notifier
        .http
        .post(notifier.api_url("getUpdates"))
        .json(&params)
        .timeout(std::time::Duration::from_secs(timeout + 10))
        .send()
        .await?;
    let result: GetUpdatesResult = resp.json().await?;
    Ok(result.result)
}

async fn handle_message(state: &AppState, notifier: &TelegramNotifier, text: &str) {
    let low = text.to_lowercase();

    // Fast slash-command path
    if matches!(low.as_str(), "/help" | "help" | "/start") {
        notifier.send(HELP_TEXT).await;
        return;
    }
    if low == "/status" {
        let cameras = state.cameras.read().await;
        let active = cameras.values().filter(|c| c.running).count();
        let total_frames: u64 = cameras.values().map(|c| c.frame_no).sum();
        let status = format!(
            "📟 Cameras: {} active | Frames: {} | Model: {}",
            active,
            total_frames,
            state.vlm.model_name()
        );
        notifier.send(&status).await;
        return;
    }
    if matches!(low.as_str(), "/snapshot" | "截图" | "发图" | "snapshot") {
        snapshot_reply(state, notifier).await;
        return;
    }

    // Default: visual question on latest frame
    notifier.send("🔍 Analyzing current frame...").await;
    let answer = ask_visual(state, text).await;
    notifier.send(&format!("👁️ {}", answer)).await;
}

async fn snapshot_reply(state: &AppState, notifier: &TelegramNotifier) {
    let cameras = state.cameras.read().await;
    let frame = cameras.values().find_map(|c| c.latest_frame.clone());
    drop(cameras);

    match frame {
        Some(jpeg) => {
            notifier.send_photo(jpeg, "📷 Current frame").await;
        }
        None => {
            notifier
                .send("📷 No frame available (no camera connected?)")
                .await;
        }
    }
}

async fn ask_visual(state: &AppState, question: &str) -> String {
    let cameras = state.cameras.read().await;
    let frame = cameras.values().find_map(|c| c.latest_frame.clone());
    drop(cameras);

    let Some(jpeg) = frame else {
        return "No frame available (no camera connected).".to_string();
    };

    match state.vlm.infer(&jpeg, question).await {
        Ok((text, _)) => {
            if text.is_empty() {
                "(Empty response from model)".to_string()
            } else {
                text
            }
        }
        Err(e) => format!("Inference error: {}", e),
    }
}
