use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::alert::{AlertEvent, AlertTracker};
use crate::asr::AsrClient;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::monitor::MonitorProfile;
use crate::telegram::TelegramNotifier;
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
pub struct CameraState {
    pub camera_id: String,
    pub name: String,
    pub frame_no: u64,
    /// Most recent JPEG frame bytes (for snapshot / web UI preview).
    pub latest_frame: Option<Vec<u8>>,
    /// Rolling buffer of recent results (newest last).
    pub results: Vec<FrameResult>,
    pub running: bool,
    /// Channel for sending commands to this camera via WebSocket.
    pub cmd_tx: Option<mpsc::UnboundedSender<String>>,
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
            cmd_tx: None,
        }
    }
}

impl std::fmt::Debug for CameraState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CameraState")
            .field("camera_id", &self.camera_id)
            .field("name", &self.name)
            .field("frame_no", &self.frame_no)
            .field("running", &self.running)
            .field("has_cmd_tx", &self.cmd_tx.is_some())
            .finish()
    }
}

/// Shared application state, wrapped in Arc for handler access.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub cameras: Arc<RwLock<HashMap<String, CameraState>>>,
    pub vlm: Arc<VlmClient>,
    pub asr: Option<Arc<AsrClient>>,
    pub llm: Option<Arc<LlmClient>>,
    pub monitor_profiles: Arc<HashMap<String, MonitorProfile>>,
    /// Broadcast channel for SSE / live UI updates.
    pub events_tx: broadcast::Sender<String>,
    /// Channel for alert events (fired by AlertTracker → consumed by Telegram notifier).
    pub alert_tx: mpsc::UnboundedSender<AlertEvent>,
    pub alert_tracker: Arc<Mutex<AlertTracker>>,
    pub notifier: Option<Arc<TelegramNotifier>>,
}

impl AppState {
    pub fn new(config: Config) -> (Self, mpsc::UnboundedReceiver<AlertEvent>) {
        let vlm = Arc::new(VlmClient::new(&config.vlm));
        let asr = AsrClient::new(&config.asr).map(Arc::new);
        let llm = LlmClient::new(&config.llm).map(Arc::new);
        let (events_tx, _) = broadcast::channel(256);
        let (alert_tx, alert_rx) = mpsc::unbounded_channel();
        let monitor_profiles = Arc::new(crate::monitor::default_profiles());
        let alert_tracker = Arc::new(Mutex::new(AlertTracker::new(&config.monitor)));
        let notifier = TelegramNotifier::from_config(&config.telegram).map(Arc::new);

        let state = Self {
            config,
            cameras: Arc::new(RwLock::new(HashMap::new())),
            vlm,
            asr,
            llm,
            monitor_profiles,
            events_tx,
            alert_tx,
            alert_tracker,
            notifier,
        };
        (state, alert_rx)
    }
}
