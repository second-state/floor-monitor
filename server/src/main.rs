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

    let state = Arc::new(AppState::new(config.clone()));

    // Start Telegram listener if configured
    if let Some(notifier) = telegram::TelegramNotifier::from_config(&config.telegram) {
        info!("Telegram bot enabled");
        let startup_msg = format!(
            "🟢 *Floor Monitor Server started*\nAddress: http://{}:{}\nModel: {}",
            config.server.host, config.server.port, config.vlm.model
        );
        let n = notifier.clone();
        tokio::spawn(async move {
            n.send(&startup_msg).await;
        });
        telegram::start_listener(state.clone(), notifier);
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
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state);

    info!("Floor Monitor Server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
