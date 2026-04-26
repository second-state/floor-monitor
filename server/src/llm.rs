use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::LlmConfig;

/// Intent classification result from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "intent")]
pub enum Intent {
    #[serde(rename = "visual_question")]
    VisualQuestion { question: String },
    #[serde(rename = "history_summary")]
    HistorySummary {
        #[serde(default = "default_minutes")]
        minutes: u32,
    },
    #[serde(rename = "snapshot")]
    Snapshot,
    #[serde(rename = "patrol")]
    Patrol,
    #[serde(rename = "ptz_control")]
    PtzControl { direction: String },
    #[serde(rename = "help")]
    Help,
    #[serde(rename = "status")]
    Status,
}

fn default_minutes() -> u32 {
    15
}

/// Client for OpenAI-compatible LLM used for intent classification.
pub struct LlmClient {
    api_url: String,
    api_key: Option<String>,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    http: reqwest::Client,
}

const SYSTEM_PROMPT: &str = r#"You are a dispatcher for a camera monitoring bot. Given a user message, output ONLY a single line of compact JSON (no explanation, no code fences).

Possible intents:
- {"intent":"visual_question","question":"<the user's question about the current camera image>"}
  Use when the user asks about what they see, counts, descriptions, or any question about the camera feed.
- {"intent":"history_summary","minutes":<N>}
  Use when the user asks about past activity ("past 15 minutes", "last hour", "what happened recently"). Estimate minutes: "half hour"=30, "hour"=60, "recently"=15.
- {"intent":"snapshot"}
  Use when the user wants a photo/screenshot of the current view.
- {"intent":"patrol"}
  Use when the user wants the camera to sweep/scan the room left-to-right.
- {"intent":"ptz_control","direction":"<pan_left|pan_right|tilt_up|tilt_down>"}
  Use when the user wants to move the camera in a specific direction.
- {"intent":"help"}
  Use for greetings, help requests, or "how to use" questions.
- {"intent":"status"}
  Use when the user asks about system status.

JSON:"#;

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
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

impl LlmClient {
    /// Returns `None` if LLM is not configured (no api_url).
    pub fn new(config: &LlmConfig) -> Option<Self> {
        let url = config.api_url.as_deref()?.trim();
        if url.is_empty() {
            return None;
        }
        info!("LLM client: model={} url={}", config.model, url);
        Some(Self {
            api_url: url.to_string(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            http: reqwest::Client::new(),
        })
    }

    /// Classify a user message into an Intent.
    pub async fn classify(
        &self,
        user_text: &str,
    ) -> Result<Intent, Box<dyn std::error::Error + Send + Sync>> {
        let payload = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_text.to_string(),
                },
            ],
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        };

        let mut req = self
            .http
            .post(&self.api_url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(30));
        if let Some(ref key) = self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(200).collect();
            return Err(format!("LLM API returned {}: {}", status, truncated).into());
        }

        let raw_text = resp.text().await.unwrap_or_default();
        let body: ChatResponse = match serde_json::from_str(&raw_text) {
            Ok(b) => b,
            Err(e) => {
                let truncated: String = raw_text.chars().take(200).collect();
                warn!("LLM returned invalid JSON: {} — body: {}", e, truncated);
                return Err(format!("LLM returned invalid JSON: {}", e).into());
            }
        };
        let raw = body
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_default();

        parse_intent(&raw)
    }
}

/// Parse LLM output into an Intent. Tolerant of surrounding text.
pub fn parse_intent(raw: &str) -> Result<Intent, Box<dyn std::error::Error + Send + Sync>> {
    let text = raw.trim();

    // Strip code fences if present
    let text = if text.starts_with("```") {
        text.lines()
            .filter(|l| !l.starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text.to_string()
    };

    // Try direct parse
    if let Ok(intent) = serde_json::from_str::<Intent>(text.trim()) {
        return Ok(intent);
    }

    // Find first { ... } block
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if start < end {
                if let Ok(intent) = serde_json::from_str::<Intent>(&text[start..=end]) {
                    return Ok(intent);
                }
            }
        }
    }

    // Fallback: treat as visual question
    warn!(
        "Could not parse LLM intent, defaulting to visual_question: {}",
        &raw[..raw.len().min(100)]
    );
    Ok(Intent::VisualQuestion {
        question: raw.to_string(),
    })
}

/// Simple keyword-based intent detection (fallback when LLM is not configured).
pub fn classify_keywords(text: &str) -> Intent {
    let low = text.to_lowercase();

    if matches!(low.as_str(), "/help" | "help" | "/start") {
        return Intent::Help;
    }
    if low == "/status" {
        return Intent::Status;
    }
    if matches!(low.as_str(), "/snapshot" | "/photo" | "snapshot" | "photo") {
        return Intent::Snapshot;
    }
    if matches!(low.as_str(), "/patrol" | "patrol" | "sweep") {
        return Intent::Patrol;
    }
    // PTZ keywords
    if low.contains("pan left") || low.contains("turn left") || low.contains("look left") {
        return Intent::PtzControl {
            direction: "pan_left".to_string(),
        };
    }
    if low.contains("pan right") || low.contains("turn right") || low.contains("look right") {
        return Intent::PtzControl {
            direction: "pan_right".to_string(),
        };
    }
    if low.contains("tilt up") || low.contains("look up") {
        return Intent::PtzControl {
            direction: "tilt_up".to_string(),
        };
    }
    if low.contains("tilt down") || low.contains("look down") {
        return Intent::PtzControl {
            direction: "tilt_down".to_string(),
        };
    }
    // History keywords
    if low.contains("past")
        || low.contains("recent")
        || low.contains("summary")
        || low.contains("last")
    {
        let minutes = extract_minutes(&low);
        return Intent::HistorySummary { minutes };
    }

    // Default: visual question
    Intent::VisualQuestion {
        question: text.to_string(),
    }
}

fn extract_minutes(text: &str) -> u32 {
    if text.contains("half hour") || text.contains("30") {
        return 30;
    }
    if text.contains("1 hour") || text.contains("60") {
        return 60;
    }
    // Try to find a number
    for word in text.split_whitespace() {
        if let Ok(n) = word
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
        {
            if n > 0 && n <= 720 {
                return n;
            }
        }
    }
    15 // default
}

impl std::fmt::Debug for LlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmClient")
            .field("api_url", &self.api_url)
            .field("model", &self.model)
            .finish()
    }
}
