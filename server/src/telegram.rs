use reqwest::multipart;
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::llm;
use crate::state::AppState;

const API_BASE: &str = "https://api.telegram.org/bot";
const POLL_TIMEOUT: u64 = 25;

/// Telegram notifier — send text messages and photos to one or more chats.
#[derive(Debug, Clone)]
pub struct TelegramNotifier {
    bot_token: String,
    /// All chat IDs to send messages to and accept incoming messages from.
    chat_ids: Vec<i64>,
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
    message: Option<TgMessage>,
}

#[derive(Deserialize)]
struct TgMessage {
    chat: Chat,
    text: Option<String>,
    voice: Option<Voice>,
}

#[derive(Deserialize)]
struct Chat {
    id: i64,
}

#[derive(Deserialize)]
struct Voice {
    file_id: String,
}

#[derive(Deserialize)]
struct GetFileResult {
    #[allow(dead_code)]
    ok: bool,
    result: FileInfo,
}

#[derive(Deserialize)]
struct FileInfo {
    file_path: Option<String>,
}

impl TelegramNotifier {
    pub fn from_config(config: &crate::config::TelegramConfig) -> Option<Self> {
        let token = config.bot_token.as_deref()?.trim();
        if token.is_empty() {
            return None;
        }

        // Merge chat_id (singular) and chat_ids (list) into one deduplicated list
        let mut ids: Vec<i64> = Vec::new();
        if let Some(ref single) = config.chat_id {
            if let Ok(id) = single.trim().parse::<i64>() {
                ids.push(id);
            }
        }
        for s in &config.chat_ids {
            if let Ok(id) = s.trim().parse::<i64>() {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        if ids.is_empty() {
            return None;
        }

        info!(
            "Telegram notifier enabled for {} chat(s): {:?}",
            ids.len(),
            ids
        );
        Some(Self {
            bot_token: token.to_string(),
            chat_ids: ids,
            http: reqwest::Client::new(),
        })
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", API_BASE, self.bot_token, method)
    }

    /// Send a text message to all configured chats.
    pub async fn send(&self, text: &str) -> bool {
        let mut any_ok = false;
        for chat_id in &self.chat_ids {
            let payload = serde_json::json!({
                "chat_id": chat_id,
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
                Ok(resp) => {
                    if resp
                        .json::<TelegramApiResult>()
                        .await
                        .map(|r| r.ok)
                        .unwrap_or(false)
                    {
                        any_ok = true;
                    }
                }
                Err(e) => {
                    warn!("Telegram send to {} failed: {}", chat_id, e);
                }
            }
        }
        any_ok
    }

    /// Send a photo to all configured chats.
    pub async fn send_photo(&self, jpeg_bytes: Vec<u8>, caption: &str) -> bool {
        let mut any_ok = false;
        let cap: String = caption.chars().take(1024).collect();
        for chat_id in &self.chat_ids {
            let part = multipart::Part::bytes(jpeg_bytes.clone())
                .file_name("frame.jpg")
                .mime_str("image/jpeg")
                .unwrap();
            let mut form = multipart::Form::new()
                .text("chat_id", chat_id.to_string())
                .part("photo", part);
            if !cap.is_empty() {
                form = form
                    .text("caption", cap.clone())
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
                Ok(resp) => {
                    if resp
                        .json::<TelegramApiResult>()
                        .await
                        .map(|r| r.ok)
                        .unwrap_or(false)
                    {
                        any_ok = true;
                    }
                }
                Err(e) => {
                    warn!("Telegram send_photo to {} failed: {}", chat_id, e);
                }
            }
        }
        any_ok
    }

    /// Check if a chat ID is in the allowed list.
    pub fn is_allowed_chat(&self, chat_id: i64) -> bool {
        self.chat_ids.contains(&chat_id)
    }

    /// Download a file from Telegram by file_id (e.g. voice messages).
    pub async fn download_file(
        &self,
        file_id: &str,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        // Step 1: getFile to get file_path
        let resp = self
            .http
            .get(self.api_url("getFile"))
            .query(&[("file_id", file_id)])
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        let result: GetFileResult = resp.json().await?;
        let file_path = result
            .result
            .file_path
            .ok_or("Telegram getFile returned no file_path")?;

        // Step 2: download the file
        let file_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.bot_token, file_path
        );
        let resp = self
            .http
            .get(&file_url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(format!("File download failed: {}", resp.status()).into());
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

const HELP_TEXT: &str = "\
🤖 *Floor Monitor Bot*\n\
Ask me about the camera feed, or request a summary.\n\n\
*Commands:*\n\
• Free-form visual question (text or voice, ZH or EN)\n\
• `summarize past 15 min` — summarize recent activity\n\
• `/snapshot` — current frame photo\n\
• `/patrol` — sweep camera across the room\n\
• `pan left` / `pan right` — move camera\n\
• `/help` · `/status`\n\n\
Voice messages are also supported — just send a voice note!";

/// Start the Telegram long-polling listener as a background task.
pub fn start_listener(state: Arc<AppState>, notifier: Arc<TelegramNotifier>) {
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
                            if !notifier.is_allowed_chat(msg.chat.id) {
                                continue;
                            }

                            // Handle voice messages
                            if let Some(voice) = msg.voice {
                                handle_voice(&state, &notifier, &voice.file_id).await;
                                continue;
                            }

                            // Handle text messages
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

/// Handle a voice message: download OGG → ASR transcribe → handle as text.
async fn handle_voice(state: &AppState, notifier: &TelegramNotifier, file_id: &str) {
    let asr = match &state.asr {
        Some(asr) => asr,
        None => {
            notifier
                .send("🎙️ Voice messages require ASR to be configured. Add `[asr]` to config.toml.")
                .await;
            return;
        }
    };

    notifier.send("🎙️ Transcribing voice message...").await;

    // Download OGG from Telegram
    let ogg_bytes = match notifier.download_file(file_id).await {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to download voice file: {}", e);
            notifier
                .send(&format!("Failed to download voice: {}", e))
                .await;
            return;
        }
    };

    // Transcribe
    match asr.transcribe(&ogg_bytes, "voice.ogg").await {
        Ok(text) => {
            info!("Voice transcribed: {}", &text[..text.len().min(80)]);
            notifier
                .send(&format!("🎙️ _{}_", &text[..text.len().min(200)]))
                .await;
            handle_message(state, notifier, &text).await;
        }
        Err(e) => {
            warn!("ASR failed: {}", e);
            notifier.send(&format!("Transcription failed: {}", e)).await;
        }
    }
}

/// Handle a text message: classify intent (via LLM or keywords), dispatch.
async fn handle_message(state: &AppState, notifier: &TelegramNotifier, text: &str) {
    // Classify intent via the required LLM. Falls back to keyword matching
    // if the LLM call errors at request time (network blip, provider down).
    let intent = match state.llm.classify(text).await {
        Ok(i) => {
            info!("LLM intent: {:?}", i);
            i
        }
        Err(e) => {
            warn!("LLM classify failed, falling back to keywords: {}", e);
            llm::classify_keywords(text)
        }
    };

    // Dispatch
    match intent {
        llm::Intent::Help => {
            notifier.send(HELP_TEXT).await;
        }
        llm::Intent::Status => {
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
        }
        llm::Intent::Snapshot => {
            snapshot_reply(state, notifier).await;
        }
        llm::Intent::VisualQuestion { question } => {
            notifier.send("🔍 Analyzing current frame...").await;
            let answer = ask_visual(state, &question).await;
            notifier.send(&format!("👁️ {}", answer)).await;
        }
        llm::Intent::HistorySummary { minutes } => {
            notifier
                .send(&format!("⏳ Summarizing past {} minutes...", minutes))
                .await;
            let summary = build_history_summary(state, minutes).await;
            notifier
                .send(&format!("📊 *Past {} min*\n\n{}", minutes, summary))
                .await;
        }
        llm::Intent::Patrol => {
            match crate::ws::send_command_to_any_camera(
                state,
                "patrol",
                serde_json::json!({"sweep": true}),
            )
            .await
            {
                Ok(cam_id) => {
                    notifier
                        .send(&format!("🔄 Patrol command sent to camera `{}`", cam_id))
                        .await;
                }
                Err(e) => {
                    notifier.send(&format!("❌ {}", e)).await;
                }
            }
        }
        llm::Intent::PtzControl { direction } => {
            match crate::ws::send_command_to_any_camera(
                state,
                "ptz",
                serde_json::json!({"direction": direction}),
            )
            .await
            {
                Ok(cam_id) => {
                    notifier
                        .send(&format!(
                            "🎯 PTZ `{}` sent to camera `{}`",
                            direction, cam_id
                        ))
                        .await;
                }
                Err(e) => {
                    notifier.send(&format!("❌ {}", e)).await;
                }
            }
        }
    }
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
    let n = state.config.monitor.context_window_frames as usize;
    let cameras = state.cameras.read().await;
    let snapshot = cameras
        .values()
        .find(|c| c.latest_frame.is_some())
        .map(|c| (c.latest_frame.clone().unwrap(), c.recent_context_digest(n)));
    drop(cameras);

    let Some((jpeg, context)) = snapshot else {
        return "The camera has no live frame, so I cannot chat right now.".to_string();
    };

    match state
        .vlm
        .infer_with_context(&jpeg, &context, question)
        .await
    {
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

/// Build a history summary from recent frame results using VLM.
async fn build_history_summary(state: &AppState, minutes: u32) -> String {
    let cameras = state.cameras.read().await;
    let cutoff = chrono::Local::now() - chrono::Duration::minutes(i64::from(minutes));
    let cutoff_str = cutoff.format("%H:%M:%S").to_string();

    let mut entries: Vec<String> = Vec::new();
    for cam in cameras.values() {
        for r in cam.results.iter().rev().take(100) {
            if r.time >= cutoff_str {
                entries.push(format!("{} [{}] {}", r.time, r.camera_id, r.text));
            }
        }
    }
    drop(cameras);

    if entries.is_empty() {
        return format!("No activity recorded in the past {} minutes.", minutes);
    }

    // Build compact digest and summarize with VLM
    entries.reverse();
    let digest = entries.join("\n");
    let prompt = format!(
        "Below are camera observation records from the past {} minutes.\
         Write a concise summary (under 200 words) focusing on main activities and anomalies.\
         If nothing notable happened, state that briefly.\n\nRecords:\n{}\n\nSummary:",
        minutes, digest
    );

    match state.llm.complete(&prompt).await {
        Ok((text, _)) => text,
        Err(e) => format!("Summary generation failed: {}", e),
    }
}
