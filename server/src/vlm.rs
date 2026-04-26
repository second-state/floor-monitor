use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::VlmConfig;

/// API format auto-detected from the URL path.
#[derive(Debug, Clone, PartialEq)]
enum ApiFormat {
    /// OpenAI-compatible /v1/chat/completions (works with vLLM, OpenAI, etc.)
    OpenAI,
    /// Ollama native /api/generate
    Ollama,
}

/// Generic VLM client that supports both OpenAI-compatible and Ollama APIs.
pub struct VlmClient {
    api_url: String,
    api_key: Option<String>,
    model: String,
    max_tokens: u32,
    temperature: f32,
    format: ApiFormat,
    http: reqwest::Client,
}

// --- Ollama request/response ---

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    images: Vec<String>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    num_predict: u32,
    temperature: f32,
}

#[derive(Deserialize)]
struct OllamaResponse {
    response: Option<String>,
}

// --- OpenAI-compatible request/response ---

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Serialize)]
struct OpenAIMessage {
    role: String,
    content: Vec<OpenAIContent>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum OpenAIContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Deserialize)]
struct OpenAIChoice {
    message: OpenAIRespMessage,
}

#[derive(Deserialize)]
struct OpenAIRespMessage {
    content: String,
}

impl VlmClient {
    pub fn new(config: &VlmConfig) -> Self {
        let format = if config.api_url.contains("/v1/") {
            ApiFormat::OpenAI
        } else {
            ApiFormat::Ollama
        };
        info!(
            "VLM client: model={} format={:?} url={}",
            config.model, format, config.api_url
        );

        Self {
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            format,
            http: reqwest::Client::new(),
        }
    }

    /// Run inference on a JPEG image with the given prompt.
    /// Returns (response_text, elapsed_seconds).
    pub async fn infer(
        &self,
        jpeg_bytes: &[u8],
        prompt: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        match self.format {
            ApiFormat::OpenAI => self.infer_openai(jpeg_bytes, prompt).await,
            ApiFormat::Ollama => self.infer_ollama(jpeg_bytes, prompt).await,
        }
    }

    /// Text-only inference using a tiny placeholder image.
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

    // --- OpenAI-compatible inference ---

    async fn infer_openai(
        &self,
        jpeg_bytes: &[u8],
        prompt: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        let img_b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_bytes);
        let data_url = format!("data:image/jpeg;base64,{}", img_b64);

        let payload = OpenAIRequest {
            model: self.model.clone(),
            messages: vec![OpenAIMessage {
                role: "user".to_string(),
                content: vec![
                    OpenAIContent::ImageUrl {
                        image_url: ImageUrl { url: data_url },
                    },
                    OpenAIContent::Text {
                        text: prompt.to_string(),
                    },
                ],
            }],
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        };

        let start = std::time::Instant::now();
        let mut req = self
            .http
            .post(&self.api_url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(120));

        if let Some(ref key) = self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "OpenAI API error {}: {}",
                status,
                &body[..body.len().min(300)]
            );
            return Err(format!("VLM API returned {status}").into());
        }

        let body: OpenAIResponse = resp.json().await?;
        let elapsed = start.elapsed().as_secs_f64();
        let text = body
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default();

        info!(
            "VLM inference (OpenAI) done in {:.2}s: {}",
            elapsed,
            &text[..text.len().min(80)]
        );
        Ok((text, elapsed))
    }

    // --- Ollama native inference ---

    async fn infer_ollama(
        &self,
        jpeg_bytes: &[u8],
        prompt: &str,
    ) -> Result<(String, f64), Box<dyn std::error::Error + Send + Sync>> {
        let img_b64 = base64::engine::general_purpose::STANDARD.encode(jpeg_bytes);
        let payload = OllamaRequest {
            model: self.model.clone(),
            prompt: prompt.to_string(),
            images: vec![img_b64],
            stream: false,
            options: OllamaOptions {
                num_predict: self.max_tokens,
                temperature: self.temperature,
            },
        };

        let start = std::time::Instant::now();
        let resp = self
            .http
            .post(&self.api_url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(
                "Ollama API error {}: {}",
                status,
                &body[..body.len().min(200)]
            );
            return Err(format!("VLM API returned {status}").into());
        }

        let body: OllamaResponse = resp.json().await?;
        let elapsed = start.elapsed().as_secs_f64();
        let text = body.response.unwrap_or_default().trim().to_string();
        info!(
            "VLM inference (Ollama) done in {:.2}s: {}",
            elapsed,
            &text[..text.len().min(80)]
        );
        Ok((text, elapsed))
    }
}

/// Minimal valid 1x1 JPEG for text-only VLM calls.
fn minimal_jpeg() -> Vec<u8> {
    // A known-good minimal JPEG (1x1 grey pixel)
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
            .field("format", &self.format)
            .finish()
    }
}
