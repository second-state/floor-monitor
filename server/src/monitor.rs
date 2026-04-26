use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

/// A monitor profile defines how the VLM analyzes frames for a specific domain.
/// Loaded from TOML files in the profiles/ directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorProfile {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub summary_intro: String,
    pub danger_categories: Vec<String>,
}

/// Load all monitor profiles from TOML files in the given directory.
/// Falls back to built-in defaults if the directory doesn't exist or is empty.
pub fn load_profiles(dir: &Path) -> HashMap<String, MonitorProfile> {
    let mut profiles = HashMap::new();

    if dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                    continue;
                }
                match load_profile_file(&path) {
                    Ok(profile) => {
                        info!("Loaded profile '{}' from {}", profile.id, path.display());
                        profiles.insert(profile.id.clone(), profile);
                    }
                    Err(e) => {
                        warn!("Failed to load profile {}: {}", path.display(), e);
                    }
                }
            }
        }
    }

    if profiles.is_empty() {
        info!("No external profiles found, using built-in defaults");
        profiles = default_profiles();
    }

    profiles
}

/// Load a single profile from a TOML file.
fn load_profile_file(path: &Path) -> Result<MonitorProfile, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let profile: MonitorProfile = toml::from_str(&content)?;
    if profile.id.is_empty() {
        return Err("Profile id is empty".into());
    }
    if profile.prompt.is_empty() {
        return Err("Profile prompt is empty".into());
    }
    Ok(profile)
}

/// Built-in default profiles (used when profiles/ directory is missing or empty).
pub fn default_profiles() -> HashMap<String, MonitorProfile> {
    let mut m = HashMap::new();

    m.insert(
        "kid".to_string(),
        MonitorProfile {
            id: "kid".to_string(),
            name: "Kid Monitor".to_string(),
            prompt: "You are a children's room safety monitor. Look at this image and output ONLY a single line of valid JSON. No explanation, no code fences.\n\nFields:\n- activity: string (3-10 words)\n- num_children: integer\n- risk_level: \"none\" / \"low\" / \"medium\" / \"high\"\n- risk_reason: string (empty when none)\n\nExample: {\"activity\": \"playing with blocks\", \"num_children\": 1, \"risk_level\": \"none\", \"risk_reason\": \"\"}".to_string(),
            summary_intro: "You are a careful parenting assistant. Summarize the children's room activity in 2-4 sentences.".to_string(),
            danger_categories: vec!["roughhousing".into(), "climbing furniture".into(), "near window".into(), "playing with sharp objects".into()],
        },
    );

    m.insert(
        "security".to_string(),
        MonitorProfile {
            id: "security".to_string(),
            name: "Home Security".to_string(),
            prompt: "You are a home security monitor. Look at this image and output ONLY a single line of valid JSON. No explanation, no code fences.\n\nFields:\n- activity: string (3-15 words)\n- num_people: integer\n- num_pets: integer\n- risk_level: \"none\" / \"low\" / \"medium\" / \"high\"\n- risk_reason: string (empty when none)\n\nExample: {\"activity\": \"living room quiet\", \"num_people\": 0, \"num_pets\": 0, \"risk_level\": \"none\", \"risk_reason\": \"\"}".to_string(),
            summary_intro: "You are a home security assistant. Summarize activity in 2-4 sentences.".to_string(),
            danger_categories: vec!["intruder".into(), "open fire or smoke".into(), "person collapsed".into()],
        },
    );

    m
}

/// Try to parse structured JSON from VLM output text.
pub fn parse_vlm_json(text: &str) -> Option<serde_json::Value> {
    if text.is_empty() {
        return None;
    }
    // Try direct parse
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if v.is_object() {
            return Some(v);
        }
    }
    // Find first { ... } block
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if start < end {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) {
                    if v.is_object() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Extract risk_level from parsed VLM JSON. Used by alert logic and tests.
#[allow(dead_code)]
pub fn extract_risk_level(parsed: &serde_json::Value) -> String {
    parsed
        .get("risk_level")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_lowercase()
}
