use base64::Engine;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

use crate::config::VlmConfig;

/// VLM client using OpenAI-compatible API.
/// Supports both the Chat Completions format (image_url/text) and the
/// newer Responses format (input_image/input_text). Tries the newer
/// format first; auto-falls back to the legacy format on 400 errors.
pub struct VlmClient {
    api_url: String,
    api_key: Option<String>,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    http: reqwest::Client,
    /// When true, use legacy "image_url"/"text" content types.
    /// Starts false (use newer format); flips on first 400 error.
    use_legacy_format: AtomicBool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

impl VlmClient {
    pub fn new(config: &VlmConfig) -> Self {
        info!("VLM client: model={} url={}", config.model, config.api_url);
        Self {
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            http: reqwest::Client::new(),
            use_legacy_format: AtomicBool::new(false),
        }
    }

    /// Run inference on a JPEG image with the given prompt.
    /// Returns (response_text, elapsed_seconds).
    pub async fn infer(
        &self,
        jpeg_bytes: &[u8],
        prompt: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        let img_b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_bytes);
        let data_url = format!("data:image/jpeg;base64,{}", img_b64);

        // Try current format; on 400 error, flip and retry with the other format
        let use_legacy = self.use_legacy_format.load(Ordering::Relaxed);
        let payload = self.build_payload(&data_url, prompt, use_legacy);

        let start = std::time::Instant::now();
        let result = self.send_request(&payload).await;

        match result {
            Ok(text) => {
                let elapsed = start.elapsed().as_secs_f64();
                info!(
                    "VLM inference done in {:.2}s: {}",
                    elapsed,
                    &text[..text.len().min(80)]
                );
                Ok((text, elapsed))
            }
            Err(e) => {
                let err_msg = e.to_string();
                // If we got a 400 with "Invalid value: 'image_url'" or "'input_image'",
                // flip the format flag and retry once
                if err_msg.contains("400")
                    && (err_msg.contains("image_url")
                        || err_msg.contains("input_image")
                        || err_msg.contains("input_text"))
                {
                    let new_legacy = !use_legacy;
                    self.use_legacy_format.store(new_legacy, Ordering::Relaxed);
                    info!("VLM format auto-switch: legacy={}. Retrying...", new_legacy);
                    let payload = self.build_payload(&data_url, prompt, new_legacy);
                    let text = self.send_request(&payload).await?;
                    let elapsed = start.elapsed().as_secs_f64();
                    info!(
                        "VLM inference done in {:.2}s (after format switch): {}",
                        elapsed,
                        &text[..text.len().min(80)]
                    );
                    Ok((text, elapsed))
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Run inference on a JPEG image, prepending recent scene-description
    /// context to the question. Each entry in `context` is one observation
    /// line; ordering is preserved (caller passes oldest-first).
    pub async fn infer_with_context(
        &self,
        jpeg_bytes: &[u8],
        context: &[String],
        question: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        let prompt = if context.is_empty() {
            question.to_string()
        } else {
            format!(
                "Recent observations from this camera (oldest first, most recent last):\n{}\n\nThe attached image is the current frame. Use the observations above as short-term memory; do not invent details not visible in the image or recorded above.\n\nQuestion: {}",
                context.join("\n"),
                question
            )
        };
        self.infer(jpeg_bytes, &prompt).await
    }

    /// Text-only inference using a tiny placeholder image.
    #[allow(dead_code)]
    pub async fn infer_text_only(
        &self,
        prompt: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        let placeholder = minimal_jpeg();
        self.infer(&placeholder, prompt).await
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Build the JSON payload. Two formats:
    /// - Newer (Responses API): input_image + input_text
    /// - Legacy (Chat Completions): image_url + text
    fn build_payload(&self, data_url: &str, prompt: &str, use_legacy: bool) -> serde_json::Value {
        let content = if use_legacy {
            // Legacy Chat Completions format
            serde_json::json!([
                {
                    "type": "image_url",
                    "image_url": { "url": data_url }
                },
                {
                    "type": "text",
                    "text": prompt
                }
            ])
        } else {
            // Newer Responses API format (GPT-5.x)
            serde_json::json!([
                {
                    "type": "input_image",
                    "image_url": data_url
                },
                {
                    "type": "input_text",
                    "text": prompt
                }
            ])
        };

        let mut payload = serde_json::json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": content
            }],
            "max_tokens": self.max_tokens,
        });

        if let Some(temp) = self.temperature {
            payload["temperature"] = serde_json::json!(temp);
        }

        payload
    }

    /// Send request and parse response.
    async fn send_request(
        &self,
        payload: &serde_json::Value,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut req = self
            .http
            .post(&self.api_url)
            .json(payload)
            .timeout(std::time::Duration::from_secs(120));

        if let Some(ref key) = self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(300).collect();
            warn!("VLM API error {}: {}", status, truncated);
            return Err(format!("VLM API returned {status}: {truncated}").into());
        }

        let raw_text = resp.text().await.unwrap_or_default();
        let body: ChatResponse = match serde_json::from_str(&raw_text) {
            Ok(b) => b,
            Err(e) => {
                let truncated: String = raw_text.chars().take(200).collect();
                warn!("VLM returned invalid JSON: {} — body: {}", e, truncated);
                return Err(format!("VLM returned invalid JSON: {}", e).into());
            }
        };

        Ok(body
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default())
    }
}

/// Minimal valid 1x1 JPEG for text-only VLM calls.
#[allow(dead_code)]
fn minimal_jpeg() -> Vec<u8> {
    vec![
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
        0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B,
        0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
        0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31,
        0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF,
        0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00,
        0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
        0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05,
        0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
        0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08,
        0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0xFF, 0xDA,
        0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B, 0x94, 0x11, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xFF, 0xD9,
    ]
}

impl std::fmt::Debug for VlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VlmClient")
            .field("api_url", &self.api_url)
            .field("model", &self.model)
            .field(
                "use_legacy_format",
                &self.use_legacy_format.load(Ordering::Relaxed),
            )
            .finish()
    }
}
