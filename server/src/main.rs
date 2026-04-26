use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use tracing::info;

use floor_monitor_server::config::Config;
use floor_monitor_server::state::AppState;
use floor_monitor_server::{routes, telegram, ws};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "floor_monitor_server=info,tower_http=info".into()),
        )
        .init();

    // Load configuration
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let config = Config::load(Path::new(&config_path)).unwrap_or_else(|e| {
        eprintln!("Failed to load {}: {}", config_path, e);
        eprintln!("Copy config.toml.example to config.toml and edit it.");
        std::process::exit(1);
    });

    let addr = SocketAddr::new(
        config.server.host.parse().expect("Invalid host address"),
        config.server.port,
    );

    let (state, mut alert_rx) = AppState::new(config.clone());
    let state = Arc::new(state);

    // Spawn alert consumer — sends Telegram notifications for high-risk detections
    if let Some(ref notifier) = state.notifier {
        let n = notifier.clone();
        tokio::spawn(async move {
            while let Some(alert) = alert_rx.recv().await {
                let text = format!(
                    "⚠️ *Alert*\n*Camera*: {}\n*Risk*: {}\n*Reason*: {}\n*Activity*: {}\n*Frame*: {}",
                    alert.camera_id, alert.risk_level, alert.risk_reason, alert.activity, alert.frame_no
                );
                if let Some(jpeg) = alert.jpeg {
                    if !n.send_photo(jpeg, &text).await {
                        n.send(&text).await;
                    }
                } else {
                    n.send(&text).await;
                }
            }
        });

        // Spawn summary scheduler
        let summary_min = config.monitor.summary_window_min;
        if summary_min > 0 {
            let s = state.clone();
            let n2 = notifier.clone();
            let profile_id = config.monitor.default_profile.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                    u64::from(summary_min) * 60,
                ));
                interval.tick().await; // skip first immediate tick
                loop {
                    interval.tick().await;
                    info!("Summary scheduler firing ({}min window)", summary_min);

                    let summary_intro = s
                        .monitor_profiles
                        .get(&profile_id)
                        .map(|p| p.summary_intro.as_str())
                        .unwrap_or("Summarize the recent activity.");

                    // Collect recent results
                    let cameras = s.cameras.read().await;
                    let mut entries: Vec<String> = Vec::new();
                    for cam in cameras.values() {
                        for r in cam.results.iter().rev().take(80) {
                            entries.push(format!("{} [{}] {}", r.time, r.camera_id, r.text));
                        }
                    }
                    drop(cameras);

                    if entries.is_empty() {
                        continue;
                    }

                    entries.reverse();
                    let digest = entries.join("\n");
                    let prompt = format!("{}\n\n{}", summary_intro, digest);

                    match s.vlm.infer_text_only(&prompt).await {
                        Ok((text, _)) => {
                            let msg =
                                format!("🕒 *{} min activity summary*\n\n{}", summary_min, text);
                            n2.send(&msg).await;
                        }
                        Err(e) => {
                            info!("Summary generation failed: {}", e);
                        }
                    }
                }
            });
        }

        // Start Telegram listener
        info!("Telegram bot enabled");
        let startup_msg = format!(
            "🟢 *Floor Monitor Server started*\nAddress: http://{}:{}\nModel: {}",
            config.server.host, config.server.port, config.vlm.model
        );
        let n = notifier.clone();
        tokio::spawn(async move {
            n.send(&startup_msg).await;
        });
        telegram::start_listener(state.clone(), notifier.clone());
    }

    // Build routes
    let app = Router::new()
        .route("/", get(routes::index))
        .route("/dashboard", get(routes::dashboard))
        .route("/ws", get(ws::ws_handler))
        .route("/api/cameras", get(routes::api_cameras))
        .route("/api/results", get(routes::api_results))
        .route("/api/snapshot/{camera_id}", get(routes::api_snapshot))
        .route("/api/events", get(routes::api_events))
        .route("/api/ask", axum::routing::post(routes::api_ask))
        .route("/api/command", axum::routing::post(routes::api_command))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    info!("Floor Monitor Server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
