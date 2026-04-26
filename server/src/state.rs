use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::config::Config;
use crate::monitor::MonitorProfile;
use crate::vlm::VlmClient;

/// A single inference result from analyzing a camera frame.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameResult {
    pub camera_id: String,
    pub frame_no: u64,
    pub time: String,
    pub infer_secs: f64,
    pub model: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_json: Option<serde_json::Value>,
}

/// Per-camera state tracked by the server.
#[derive(Debug)]
pub struct CameraState {
    pub camera_id: String,
    pub name: String,
    pub frame_no: u64,
    /// Most recent JPEG frame bytes (for snapshot / web UI preview).
    pub latest_frame: Option<Vec<u8>>,
    /// Rolling buffer of recent results (newest last).
    pub results: Vec<FrameResult>,
    pub running: bool,
}

impl CameraState {
    pub fn new(camera_id: String, name: String) -> Self {
        Self {
            camera_id,
            name,
            frame_no: 0,
            latest_frame: None,
            results: Vec::new(),
            running: false,
        }
    }
}

/// Shared application state, wrapped in Arc for handler access.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub cameras: Arc<RwLock<HashMap<String, CameraState>>>,
    pub vlm: Arc<VlmClient>,
    pub monitor_profiles: Arc<HashMap<String, MonitorProfile>>,
    /// Broadcast channel for SSE / live UI updates.
    pub events_tx: broadcast::Sender<String>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let vlm = Arc::new(VlmClient::new(&config.vlm));
        let (events_tx, _) = broadcast::channel(256);
        let monitor_profiles = Arc::new(crate::monitor::default_profiles());
        Self {
            config,
            cameras: Arc::new(RwLock::new(HashMap::new())),
            vlm,
            monitor_profiles,
            events_tx,
        }
    }
}
