//! Unit tests for monitor parsing and profile logic.

#[test]
fn test_parse_vlm_json_valid() {
    let input = r#"{"activity": "坐在桌前写作业", "num_children": 1, "risk_level": "none", "risk_reason": ""}"#;
    let result = floor_monitor_server::monitor::parse_vlm_json(input);
    assert!(result.is_some());
    let v = result.unwrap();
    assert_eq!(v["activity"], "坐在桌前写作业");
    assert_eq!(v["num_children"], 1);
    assert_eq!(v["risk_level"], "none");
}

#[test]
fn test_parse_vlm_json_with_junk() {
    let input = r#"Here is the result: {"activity": "玩积木", "num_children": 2, "risk_level": "none", "risk_reason": ""} that's it"#;
    let result = floor_monitor_server::monitor::parse_vlm_json(input);
    assert!(result.is_some());
    let v = result.unwrap();
    assert_eq!(v["activity"], "玩积木");
    assert_eq!(v["num_children"], 2);
}

#[test]
fn test_parse_vlm_json_empty() {
    assert!(floor_monitor_server::monitor::parse_vlm_json("").is_none());
}

#[test]
fn test_parse_vlm_json_no_json() {
    assert!(floor_monitor_server::monitor::parse_vlm_json("no json here").is_none());
}

#[test]
fn test_parse_vlm_json_invalid_json() {
    assert!(floor_monitor_server::monitor::parse_vlm_json("{bad json}").is_none());
}

#[test]
fn test_extract_risk_level() {
    let parsed = serde_json::json!({"risk_level": "high", "activity": "test"});
    assert_eq!(
        floor_monitor_server::monitor::extract_risk_level(&parsed),
        "high"
    );
}

#[test]
fn test_extract_risk_level_missing() {
    let parsed = serde_json::json!({"activity": "test"});
    assert_eq!(
        floor_monitor_server::monitor::extract_risk_level(&parsed),
        "none"
    );
}

#[test]
fn test_load_profiles_from_directory() {
    // Try loading from the real profiles/ directory
    let dir = std::path::Path::new("profiles");
    let profiles = if dir.is_dir() {
        floor_monitor_server::monitor::load_profiles(dir)
    } else {
        // Fallback for running from repo root
        let alt = std::path::Path::new("server/profiles");
        floor_monitor_server::monitor::load_profiles(alt)
    };
    assert!(profiles.contains_key("kid"));
    assert!(profiles.contains_key("office"));
    assert!(profiles.contains_key("retail"));
    assert!(profiles.contains_key("security"));
}

#[test]
fn test_fallback_default_profiles() {
    // Loading from a nonexistent directory should return built-in defaults
    let profiles =
        floor_monitor_server::monitor::load_profiles(std::path::Path::new("nonexistent_dir"));
    assert!(!profiles.is_empty());
    assert!(profiles.contains_key("kid"));
}

#[test]
fn test_profile_prompts_non_empty() {
    let dir = std::path::Path::new("profiles");
    let profiles = if dir.is_dir() {
        floor_monitor_server::monitor::load_profiles(dir)
    } else {
        floor_monitor_server::monitor::load_profiles(std::path::Path::new("server/profiles"))
    };
    for (id, profile) in &profiles {
        assert!(
            !profile.prompt.is_empty(),
            "Profile {} has empty prompt",
            id
        );
        assert!(
            !profile.summary_intro.is_empty(),
            "Profile {} has empty summary_intro",
            id
        );
        assert!(
            !profile.danger_categories.is_empty(),
            "Profile {} has no danger categories",
            id
        );
    }
}

#[test]
fn test_config_load_example() {
    // The example config should parse successfully
    let content = std::fs::read_to_string("config.toml.example")
        .or_else(|_| std::fs::read_to_string("server/config.toml.example"))
        .expect("Cannot find config.toml.example");
    let config: Result<floor_monitor_server::config::Config, _> = toml::from_str(&content);
    assert!(
        config.is_ok(),
        "config.toml.example should parse: {:?}",
        config.err()
    );
    let cfg = config.unwrap();
    assert_eq!(cfg.server.port, 3456);
    assert!(!cfg.vlm.model.is_empty());
}

#[test]
fn test_config_openai_format_detection() {
    // OpenAI-style URL
    let cfg = floor_monitor_server::config::VlmConfig {
        api_url: "http://localhost:8000/v1/chat/completions".to_string(),
        api_key: None,
        model: "test".to_string(),
        max_tokens: 100,
        temperature: None,
    };
    let client = floor_monitor_server::vlm::VlmClient::new(&cfg);
    assert_eq!(client.model_name(), "test");
}

#[test]
fn test_config_vlm_client_creation() {
    let cfg = floor_monitor_server::config::VlmConfig {
        api_url: "http://localhost:8000/v1/chat/completions".to_string(),
        api_key: Some("test-key".to_string()),
        model: "Qwen/Qwen2.5-VL-3B-Instruct".to_string(),
        max_tokens: 200,
        temperature: None,
    };
    let client = floor_monitor_server::vlm::VlmClient::new(&cfg);
    assert_eq!(client.model_name(), "Qwen/Qwen2.5-VL-3B-Instruct");
}

// --- Alert tracker tests ---

#[test]
fn test_alert_tracker_fires_after_consecutive() {
    let config = floor_monitor_server::config::MonitorConfig {
        default_profile: "kid".to_string(),
        summary_window_min: 30,
        alert_consecutive: 2,
        alert_cooldown_sec: 0.0, // no cooldown for test
        context_window_frames: 30,
    };
    let mut tracker = floor_monitor_server::alert::AlertTracker::new(&config);
    let high_json = Some(
        serde_json::json!({"risk_level": "high", "risk_reason": "intruder", "activity": "test"}),
    );

    // First high-risk frame: no alert yet
    assert!(tracker.check_frame("cam1", 1, &high_json, None).is_none());
    // Second: should fire
    assert!(tracker.check_frame("cam1", 2, &high_json, None).is_some());
}

#[test]
fn test_alert_tracker_resets_on_none() {
    let config = floor_monitor_server::config::MonitorConfig {
        default_profile: "kid".to_string(),
        summary_window_min: 30,
        alert_consecutive: 2,
        alert_cooldown_sec: 0.0,
        context_window_frames: 30,
    };
    let mut tracker = floor_monitor_server::alert::AlertTracker::new(&config);
    let high =
        Some(serde_json::json!({"risk_level": "high", "risk_reason": "test", "activity": "x"}));
    let none = Some(serde_json::json!({"risk_level": "none", "activity": "normal"}));

    // One high, then none resets
    assert!(tracker.check_frame("cam1", 1, &high, None).is_none());
    assert!(tracker.check_frame("cam1", 2, &none, None).is_none());
    // Need 2 consecutive again
    assert!(tracker.check_frame("cam1", 3, &high, None).is_none());
    assert!(tracker.check_frame("cam1", 4, &high, None).is_some());
}

#[test]
fn test_alert_tracker_cooldown() {
    let config = floor_monitor_server::config::MonitorConfig {
        default_profile: "kid".to_string(),
        summary_window_min: 30,
        alert_consecutive: 1,
        alert_cooldown_sec: 9999.0, // very long cooldown
        context_window_frames: 30,
    };
    let mut tracker = floor_monitor_server::alert::AlertTracker::new(&config);
    let high =
        Some(serde_json::json!({"risk_level": "high", "risk_reason": "test", "activity": "x"}));

    // First fires
    assert!(tracker.check_frame("cam1", 1, &high, None).is_some());
    // Second blocked by cooldown
    assert!(tracker.check_frame("cam1", 2, &high, None).is_none());
}

// --- Intent classification tests ---

#[test]
fn test_keyword_classify_help() {
    let intent = floor_monitor_server::llm::classify_keywords("/help");
    assert!(matches!(intent, floor_monitor_server::llm::Intent::Help));
}

#[test]
fn test_keyword_classify_snapshot() {
    let intent = floor_monitor_server::llm::classify_keywords("/snapshot");
    assert!(matches!(
        intent,
        floor_monitor_server::llm::Intent::Snapshot
    ));
}

#[test]
fn test_keyword_classify_patrol() {
    let intent = floor_monitor_server::llm::classify_keywords("patrol");
    assert!(matches!(intent, floor_monitor_server::llm::Intent::Patrol));
}

#[test]
fn test_keyword_classify_ptz() {
    let intent = floor_monitor_server::llm::classify_keywords("pan left please");
    match intent {
        floor_monitor_server::llm::Intent::PtzControl { direction } => {
            assert_eq!(direction, "pan_left");
        }
        _ => panic!("Expected PtzControl"),
    }
}

#[test]
fn test_keyword_classify_visual_question() {
    let intent = floor_monitor_server::llm::classify_keywords("how many people are there?");
    assert!(matches!(
        intent,
        floor_monitor_server::llm::Intent::VisualQuestion { .. }
    ));
}

#[test]
fn test_parse_intent_valid_json() {
    let raw = r#"{"intent":"snapshot"}"#;
    let intent = floor_monitor_server::llm::parse_intent(raw).unwrap();
    assert!(matches!(
        intent,
        floor_monitor_server::llm::Intent::Snapshot
    ));
}

#[test]
fn test_parse_intent_with_junk() {
    let raw = r#"Sure! Here's the result: {"intent":"patrol"} hope that helps"#;
    let intent = floor_monitor_server::llm::parse_intent(raw).unwrap();
    assert!(matches!(intent, floor_monitor_server::llm::Intent::Patrol));
}

// --- Summary storage / SSE envelope tests ---

#[tokio::test]
async fn test_push_summary_caps_at_max() {
    let toml_str = r#"
[server]
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
"#;
    let config: floor_monitor_server::config::Config = toml::from_str(toml_str).unwrap();
    let (state, _rx) = floor_monitor_server::state::AppState::new(config);

    for i in 0..(floor_monitor_server::state::MAX_SUMMARIES + 5) {
        let entry = floor_monitor_server::state::SummaryEntry {
            time: format!("2026-04-26 12:{:02}", i),
            window_min: 30,
            text: format!("summary {}", i),
        };
        floor_monitor_server::state::push_summary(&state, entry).await;
    }

    let buf = state.summaries.read().await;
    assert_eq!(buf.len(), floor_monitor_server::state::MAX_SUMMARIES);
    // Oldest entries should have been dropped; newest preserved at the back.
    assert_eq!(
        buf.back().unwrap().text,
        format!("summary {}", floor_monitor_server::state::MAX_SUMMARIES + 4)
    );
}

#[test]
fn test_sse_event_serializes_with_kind_tag() {
    let result = floor_monitor_server::state::FrameResult {
        camera_id: "cam1".to_string(),
        frame_no: 7,
        time: "12:00:00".to_string(),
        infer_secs: 1.0,
        model: "test".to_string(),
        text: "hello".to_string(),
        parsed_json: None,
    };
    let json =
        serde_json::to_string(&floor_monitor_server::state::SseEvent::Result(result)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["kind"], "result");
    assert_eq!(v["frame_no"], 7);
    assert_eq!(v["text"], "hello");

    let summary = floor_monitor_server::state::SummaryEntry {
        time: "2026-04-26 12:00".to_string(),
        window_min: 30,
        text: "all quiet".to_string(),
    };
    let json =
        serde_json::to_string(&floor_monitor_server::state::SseEvent::Summary(summary)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["kind"], "summary");
    assert_eq!(v["window_min"], 30);
    assert_eq!(v["text"], "all quiet");
}

// --- Context digest tests ---

fn make_result(time: &str, text: &str) -> floor_monitor_server::state::FrameResult {
    floor_monitor_server::state::FrameResult {
        camera_id: "cam1".to_string(),
        frame_no: 0,
        time: time.to_string(),
        infer_secs: 0.0,
        model: "test".to_string(),
        text: text.to_string(),
        parsed_json: None,
    }
}

#[test]
fn test_recent_context_digest_takes_last_n_in_order() {
    let mut cam =
        floor_monitor_server::state::CameraState::new("cam1".to_string(), "Test".to_string());
    cam.results = (1..=10)
        .map(|i| make_result(&format!("12:00:{:02}", i), &format!("frame {}", i)))
        .collect();

    let digest = cam.recent_context_digest(3);
    assert_eq!(digest.len(), 3);
    assert_eq!(digest[0], "12:00:08 frame 8");
    assert_eq!(digest[1], "12:00:09 frame 9");
    assert_eq!(digest[2], "12:00:10 frame 10");
}

#[test]
fn test_recent_context_digest_handles_short_history() {
    let mut cam =
        floor_monitor_server::state::CameraState::new("cam1".to_string(), "Test".to_string());
    cam.results = vec![make_result("12:00:01", "only frame")];

    let digest = cam.recent_context_digest(30);
    assert_eq!(digest, vec!["12:00:01 only frame".to_string()]);
}

#[test]
fn test_recent_context_digest_zero_or_empty() {
    let cam = floor_monitor_server::state::CameraState::new("cam1".to_string(), "Test".to_string());
    assert!(cam.recent_context_digest(30).is_empty());

    let mut cam2 =
        floor_monitor_server::state::CameraState::new("cam2".to_string(), "Test".to_string());
    cam2.results = vec![make_result("12:00:01", "x")];
    assert!(cam2.recent_context_digest(0).is_empty());
}

#[test]
fn test_config_context_window_frames_default() {
    let toml_str = r#"
[server]
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
"#;
    let config: floor_monitor_server::config::Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.monitor.context_window_frames, 30);
}

#[test]
fn test_config_context_window_frames_override() {
    let toml_str = r#"
[server]
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"

[monitor]
context_window_frames = 5
"#;
    let config: floor_monitor_server::config::Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.monitor.context_window_frames, 5);
}

#[test]
fn test_config_with_asr_and_llm() {
    let toml_str = r#"
[server]
host = "0.0.0.0"
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
model = "test"

[asr]
api_url = "http://localhost:8080/v1/audio/transcriptions"
model = "whisper-1"

[llm]
api_url = "http://localhost:8000/v1/chat/completions"
model = "qwen2.5:3b"
"#;
    let config: floor_monitor_server::config::Config = toml::from_str(toml_str).unwrap();
    assert!(config.asr.api_url.is_some());
    assert!(config.llm.api_url.is_some());
    assert_eq!(config.asr.model, "whisper-1");
    assert_eq!(config.llm.model, "qwen2.5:3b");
}

#[test]
fn test_config_without_asr_and_llm() {
    let toml_str = r#"
[server]
port = 3456

[vlm]
api_url = "http://localhost:8000/v1/chat/completions"
"#;
    let config: floor_monitor_server::config::Config = toml::from_str(toml_str).unwrap();
    assert!(config.asr.api_url.is_none());
    assert!(config.llm.api_url.is_none());
}
