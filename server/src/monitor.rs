use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A monitor profile defines how the VLM analyzes frames for a specific domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorProfile {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub summary_intro: String,
    pub danger_categories: Vec<String>,
}

/// Build the default set of monitor profiles (Kid, Office, Retail, Home Security).
pub fn default_profiles() -> HashMap<String, MonitorProfile> {
    let mut m = HashMap::new();

    m.insert(
        "kid".to_string(),
        MonitorProfile {
            id: "kid".to_string(),
            name: "Kid Monitor".to_string(),
            prompt: KID_PROMPT.to_string(),
            summary_intro: "You are a careful parenting assistant. Below is a summary of activity \
                in the children's room. Write 2-4 sentences: what the child was mostly doing, \
                and anything worth noting. Do not list entries or repeat timestamps."
                .to_string(),
            danger_categories: vec![
                "roughhousing".into(),
                "climbing furniture".into(),
                "near window".into(),
                "playing with outlets/wires".into(),
                "playing with sharp objects".into(),
                "playing with fire/lighter".into(),
                "choking hazard".into(),
                "lying motionless or crying".into(),
                "stranger in room".into(),
                "falling posture".into(),
            ],
        },
    );

    m.insert(
        "office".to_string(),
        MonitorProfile {
            id: "office".to_string(),
            name: "Office Monitor".to_string(),
            prompt: OFFICE_PROMPT.to_string(),
            summary_intro: "You are an office operations assistant. Below is a summary of office \
                activity. Write 2-4 sentences covering: work atmosphere, main activities, \
                people flow, and any anomalies. Do not list entries or repeat timestamps."
                .to_string(),
            danger_categories: vec![
                "person injured or collapsed".into(),
                "violent argument or conflict".into(),
                "open fire or smoke".into(),
                "unauthorized person".into(),
                "equipment collapse or leak".into(),
                "suspicious vandalism".into(),
            ],
        },
    );

    m.insert(
        "retail".to_string(),
        MonitorProfile {
            id: "retail".to_string(),
            name: "Retail Store Monitor".to_string(),
            prompt: RETAIL_PROMPT.to_string(),
            summary_intro: "You are a retail operations assistant. Below is a summary of store \
                activity. Write 2-4 sentences covering: customer flow, staff service, \
                cleanliness, and any issues needing attention. Do not list entries."
                .to_string(),
            danger_categories: vec![
                "customer unattended".into(),
                "staff ignoring customer".into(),
                "counter visibly dirty".into(),
                "trash accumulation".into(),
                "customer conflict".into(),
                "person injured".into(),
                "unauthorized person".into(),
                "open fire or smoke".into(),
            ],
        },
    );

    m.insert(
        "security".to_string(),
        MonitorProfile {
            id: "security".to_string(),
            name: "Home Security".to_string(),
            prompt: SECURITY_PROMPT.to_string(),
            summary_intro: "You are a home security assistant. Below is a summary of home \
                monitoring activity. Write 2-4 sentences: whether the home was quiet, any \
                people entering/leaving, and any anomalies. Do not list entries."
                .to_string(),
            danger_categories: vec![
                "intruder".into(),
                "activity at unusual hours".into(),
                "door/window forced open".into(),
                "open fire or smoke".into(),
                "glass or object broken".into(),
                "person collapsed".into(),
                "pet in distress".into(),
            ],
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

// ---------------------------------------------------------------------------
// Prompt constants (English)
// ---------------------------------------------------------------------------

const KID_PROMPT: &str = "\
You are a children's room safety monitor. Look at this image and output ONLY a single line of valid JSON. \
No explanation, no code fences.\n\
\n\
Fields:\n\
- activity: string — describe in 3-10 words what the child is doing. Do NOT copy this instruction text.\n\
- num_children: integer — number of children visible, 0 if none.\n\
- risk_level: must be one of \"none\" / \"low\" / \"medium\" / \"high\".\n\
- risk_reason: string — empty string \"\" when risk_level is none; otherwise pick one from the list:\n\
  roughhousing | climbing furniture | near window | playing with outlets/wires | \
playing with sharp objects | playing with fire/lighter | choking hazard (small objects in mouth) | \
lying motionless or crying persistently | stranger in room | falling posture\n\
\n\
Rules (strict):\n\
- Normal activities (homework, reading, tablet, playing with toys, resting, empty room) -> risk_level must be \"none\".\n\
- Only items from the list above, actively happening, qualify as \"high\".\n\
- Early signs of listed behaviors -> \"medium\". \"low\" is rarely used.\n\
\n\
Example format (do NOT copy content):\n\
{\"activity\": \"sitting at desk doing homework\", \"num_children\": 1, \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const OFFICE_PROMPT: &str = "\
You are an office safety monitor. Look at this image and output ONLY a single line of valid JSON. \
No explanation, no code fences.\n\
\n\
Fields:\n\
- activity: string — describe the current office state in 3-15 words. Do NOT copy this instruction text.\n\
- num_people: integer — number of people visible, 0 if none.\n\
- focus_state: one of \"focused\" / \"meeting\" / \"casual\" / \"idle\" / \"unknown\".\n\
- risk_level: one of \"none\" / \"low\" / \"medium\" / \"high\".\n\
- risk_reason: string — empty when risk_level is none; otherwise pick from:\n\
  person injured or collapsed | violent argument or physical conflict | open fire or smoke | \
unauthorized person | equipment collapse or water leak | suspicious vandalism\n\
\n\
Rules (strict):\n\
- Normal work, meetings, phone calls, walking, short breaks -> risk_level must be \"none\".\n\
- Only listed items actively happening qualify as \"high\".\n\
\n\
Example format:\n\
{\"activity\": \"team meeting around table\", \"num_people\": 4, \"focus_state\": \"meeting\", \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const RETAIL_PROMPT: &str = "\
You are a retail store operations monitor. Look at this image and output ONLY a single line of valid JSON. \
No explanation, no code fences.\n\
\n\
Fields:\n\
- activity: string — describe the current store scene in 3-15 words. Do NOT copy this instruction text.\n\
- num_customers: integer — visible customers, 0 if none.\n\
- num_staff: integer — visible staff, 0 if none.\n\
- staff_engagement: one of \"active\" / \"passive\" / \"none\" / \"n/a\".\n\
- cleanliness: one of \"good\" / \"fair\" / \"poor\".\n\
- risk_level: one of \"none\" / \"low\" / \"medium\" / \"high\".\n\
- risk_reason: string — empty when risk_level is none; otherwise pick from:\n\
  customer unattended | staff ignoring customer (on phone) | counter visibly dirty | \
trash accumulation | customer conflict | person injured | unauthorized person | open fire or smoke\n\
\n\
Rules (strict):\n\
- Normal service, checkout, stocking, empty store, cleaning -> risk_level must be \"none\".\n\
- Only listed items actively happening qualify as \"high\".\n\
\n\
Example format:\n\
{\"activity\": \"staff helping customer with product\", \"num_customers\": 1, \"num_staff\": 1, \
\"staff_engagement\": \"active\", \"cleanliness\": \"good\", \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const SECURITY_PROMPT: &str = "\
You are a home security monitor. Look at this image and output ONLY a single line of valid JSON. \
No explanation, no code fences.\n\
\n\
Fields:\n\
- activity: string — describe the current scene in 3-15 words. Do NOT copy this instruction text.\n\
- num_people: integer — visible people, 0 if none.\n\
- num_pets: integer — visible pets, 0 if none.\n\
- risk_level: one of \"none\" / \"low\" / \"medium\" / \"high\".\n\
- risk_reason: string — empty when risk_level is none; otherwise pick from:\n\
  intruder | activity at unusual hours | door/window forced open | open fire or smoke | \
glass or object broken | person collapsed | pet in distress\n\
\n\
Rules (strict):\n\
- Empty room, family members in normal activity, pets resting -> risk_level must be \"none\".\n\
- Only listed items actively happening qualify as \"high\".\n\
\n\
Example format:\n\
{\"activity\": \"living room quiet and empty\", \"num_people\": 0, \"num_pets\": 1, \"risk_level\": \"none\", \"risk_reason\": \"\"}";
