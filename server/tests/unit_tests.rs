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
fn test_default_profiles_exist() {
    let profiles = floor_monitor_server::monitor::default_profiles();
    assert!(profiles.contains_key("kid"));
    assert!(profiles.contains_key("office"));
    assert!(profiles.contains_key("retail"));
    assert!(profiles.contains_key("security"));
}

#[test]
fn test_profile_prompts_non_empty() {
    let profiles = floor_monitor_server::monitor::default_profiles();
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
        temperature: 0.1,
    };
    let client = floor_monitor_server::vlm::VlmClient::new(&cfg);
    assert_eq!(client.model_name(), "test");
}

#[test]
fn test_config_ollama_format_detection() {
    // Ollama-style URL
    let cfg = floor_monitor_server::config::VlmConfig {
        api_url: "http://localhost:11434/api/generate".to_string(),
        api_key: None,
        model: "qwen2.5-vl:3b".to_string(),
        max_tokens: 200,
        temperature: 0.1,
    };
    let client = floor_monitor_server::vlm::VlmClient::new(&cfg);
    assert_eq!(client.model_name(), "qwen2.5-vl:3b");
}
