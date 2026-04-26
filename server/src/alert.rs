use std::collections::HashMap;
use std::time::Instant;

use crate::config::MonitorConfig;

/// An alert event fired when consecutive high-risk frames exceed the threshold.
#[derive(Debug, Clone)]
pub struct AlertEvent {
    pub camera_id: String,
    pub risk_level: String,
    pub risk_reason: String,
    pub activity: String,
    pub frame_no: u64,
    pub jpeg: Option<Vec<u8>>,
}

/// Tracks consecutive high-risk frames per camera and fires alerts.
pub struct AlertTracker {
    consecutive: HashMap<String, u32>,
    last_alert: HashMap<String, Instant>,
    threshold: u32,
    cooldown_secs: f64,
}

impl AlertTracker {
    pub fn new(config: &MonitorConfig) -> Self {
        Self {
            consecutive: HashMap::new(),
            last_alert: HashMap::new(),
            threshold: config.alert_consecutive,
            cooldown_secs: config.alert_cooldown_sec,
        }
    }

    /// Check a frame's parsed JSON for alert conditions.
    /// Returns an `AlertEvent` if alert should fire.
    pub fn check_frame(
        &mut self,
        camera_id: &str,
        frame_no: u64,
        parsed_json: &Option<serde_json::Value>,
        jpeg: Option<Vec<u8>>,
    ) -> Option<AlertEvent> {
        let (risk_level, risk_reason, activity) = match parsed_json {
            Some(v) => {
                let rl = v
                    .get("risk_level")
                    .and_then(|r| r.as_str())
                    .unwrap_or("none")
                    .to_lowercase();
                let rr = v
                    .get("risk_reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                let act = v
                    .get("activity")
                    .and_then(|a| a.as_str())
                    .unwrap_or("")
                    .to_string();
                (rl, rr, act)
            }
            None => return None,
        };

        if risk_level != "high" && risk_level != "medium" {
            // Reset streak
            self.consecutive.remove(camera_id);
            return None;
        }

        let count = self.consecutive.entry(camera_id.to_string()).or_insert(0);
        *count += 1;

        if *count < self.threshold {
            return None;
        }

        // Check cooldown
        if let Some(last) = self.last_alert.get(camera_id) {
            if last.elapsed().as_secs_f64() < self.cooldown_secs {
                return None;
            }
        }

        // Fire alert
        self.last_alert
            .insert(camera_id.to_string(), Instant::now());
        *count = 0; // Reset after firing

        Some(AlertEvent {
            camera_id: camera_id.to_string(),
            risk_level,
            risk_reason,
            activity,
            frame_no,
            jpeg,
        })
    }
}
