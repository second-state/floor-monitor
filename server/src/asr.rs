use tracing::{info, warn};

use crate::config::AsrConfig;

/// Client for Whisper-compatible ASR (speech-to-text) API.
pub struct AsrClient {
    api_url: String,
    api_key: Option<String>,
    model: String,
    http: reqwest::Client,
}

impl AsrClient {
    /// Returns `None` if ASR is not configured (no api_url).
    pub fn new(config: &AsrConfig) -> Option<Self> {
        let url = config.api_url.as_deref()?.trim();
        if url.is_empty() {
            return None;
        }
        info!("ASR client: model={} url={}", config.model, url);
        Some(Self {
            api_url: url.to_string(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            http: reqwest::Client::new(),
        })
    }

    /// Transcribe audio bytes (OGG, WAV, etc.) to text.
    /// Converts to 16kHz mono WAV via ffmpeg before sending.
    pub async fn transcribe(
        &self,
        audio_bytes: &[u8],
        filename: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // Write to temp file
        let tmp_dir = std::env::temp_dir();
        let id = uuid::Uuid::new_v4();
        let input_path = tmp_dir.join(format!("fm-asr-{}-{}", id, filename));
        tokio::fs::write(&input_path, audio_bytes).await?;

        // Convert to WAV via ffmpeg
        let wav_path = tmp_dir.join(format!("fm-asr-{}.wav", id));
        let wav_data =
            match convert_to_wav(input_path.to_str().unwrap(), wav_path.to_str().unwrap()).await {
                Ok(data) => {
                    // Clean up input file
                    let _ = tokio::fs::remove_file(&input_path).await;
                    data
                }
                Err(e) => {
                    warn!("ffmpeg conversion failed, sending original: {}", e);
                    let _ = tokio::fs::remove_file(&input_path).await;
                    audio_bytes.to_vec()
                }
            };

        // Call ASR API with retries
        let max_retries = 3u64;
        let mut last_error = String::new();
        let mime = if wav_data.starts_with(b"RIFF") {
            "audio/wav"
        } else {
            "audio/ogg"
        };
        let send_filename = if mime == "audio/wav" {
            "audio.wav"
        } else {
            filename
        };

        for attempt in 1..=max_retries {
            let file_part = reqwest::multipart::Part::bytes(wav_data.clone())
                .file_name(send_filename.to_string())
                .mime_str(mime)
                .map_err(|e| format!("MIME error: {}", e))?;

            let form = reqwest::multipart::Form::new()
                .part("file", file_part)
                .text("model", self.model.clone());

            let mut req = self.http.post(&self.api_url).multipart(form);
            if let Some(ref key) = self.api_key {
                if !key.is_empty() {
                    req = req.bearer_auth(key);
                }
            }

            let resp = match req.timeout(std::time::Duration::from_secs(30)).send().await {
                Ok(r) => r,
                Err(err) => {
                    last_error = format!("HTTP error: {}", err);
                    warn!(
                        attempt,
                        max_retries, error = %err, "ASR request failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
                    continue;
                }
            };

            if resp.status().is_server_error() {
                let status = resp.status();
                last_error = format!("ASR API returned {}", status);
                warn!(attempt, max_retries, %status, "ASR server error, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
                continue;
            }

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("ASR API returned {}: {}", status, body).into());
            }

            let json: serde_json::Value = resp.json().await?;
            let text = json["text"].as_str().unwrap_or("").trim().to_string();

            if text.is_empty() {
                return Err(
                    "ASR returned empty transcription — audio may be silent or too short".into(),
                );
            }

            info!("ASR transcription: {}", &text[..text.len().min(80)]);
            return Ok(text);
        }

        Err(format!("ASR failed after {} attempts: {}", max_retries, last_error).into())
    }
}

/// Convert audio to 16kHz mono WAV using ffmpeg.
async fn convert_to_wav(
    input_path: &str,
    output_path: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let output = tokio::process::Command::new("ffmpeg")
        .args([
            "-i",
            input_path,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-y",
            output_path,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = tokio::fs::remove_file(output_path).await;
        return Err(format!("ffmpeg failed: {}", &stderr[..stderr.len().min(200)]).into());
    }

    let data = tokio::fs::read(output_path).await?;
    let _ = tokio::fs::remove_file(output_path).await;
    Ok(data)
}

impl std::fmt::Debug for AsrClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsrClient")
            .field("api_url", &self.api_url)
            .field("model", &self.model)
            .finish()
    }
}
