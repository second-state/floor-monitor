use axum::{
    extract::State,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;
use tera::Context;

use crate::state::AppState;

/// Render a Tera template, returning 500 on error.
fn render(
    tera: &tera::Tera,
    template: &str,
    context: &Context,
) -> Result<Html<String>, (StatusCode, String)> {
    tera.render(template, context).map(Html).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
    })
}

/// GET / — redirect to /dashboard
pub async fn index() -> Response {
    axum::response::Redirect::to("/dashboard").into_response()
}

/// GET /dashboard — main monitoring dashboard
pub async fn dashboard(
    State(state): State<Arc<AppState>>,
) -> Result<Html<String>, (StatusCode, String)> {
    let tera = load_templates()?;
    let mut ctx = Context::new();

    let cameras = state.cameras.read().await;
    let camera_list: Vec<serde_json::Value> = cameras
        .values()
        .map(|c| {
            serde_json::json!({
                "camera_id": c.camera_id,
                "name": c.name,
                "running": c.running,
                "frame_no": c.frame_no,
                "capabilities": c.capabilities,
            })
        })
        .collect();

    let all_results: Vec<crate::state::FrameResult> = cameras
        .values()
        .flat_map(|c| c.results.iter().rev().take(50).cloned())
        .collect();
    drop(cameras);

    // Recent summaries (newest first for display)
    let summaries_buf = state.summaries.read().await;
    let summaries: Vec<crate::state::SummaryEntry> = summaries_buf.iter().rev().cloned().collect();
    drop(summaries_buf);

    let profiles: Vec<serde_json::Value> = state
        .monitor_profiles
        .iter()
        .map(|(id, p)| {
            serde_json::json!({
                "id": id,
                "name": p.name,
            })
        })
        .collect();

    ctx.insert("cameras", &camera_list);
    ctx.insert("results", &all_results);
    ctx.insert("summaries", &summaries);
    ctx.insert("profiles", &profiles);
    ctx.insert("active_profile", &state.config.monitor.default_profile);
    ctx.insert("model", state.vlm.model_name());
    ctx.insert("static_version", &state.static_version);
    ctx.insert(
        "ws_url",
        &format!(
            "ws://{}:{}/ws",
            state.config.server.host, state.config.server.port
        ),
    );

    render(&tera, "dashboard.html", &ctx)
}

/// GET /api/cameras — JSON list of connected cameras
pub async fn api_cameras(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cameras = state.cameras.read().await;
    let list: Vec<serde_json::Value> = cameras
        .values()
        .map(|c| {
            serde_json::json!({
                "camera_id": c.camera_id,
                "name": c.name,
                "running": c.running,
                "frame_no": c.frame_no,
                "capabilities": c.capabilities,
            })
        })
        .collect();
    axum::Json(list)
}

/// GET /api/results — JSON list of recent results from all cameras
pub async fn api_results(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let cameras = state.cameras.read().await;
    let results: Vec<crate::state::FrameResult> = cameras
        .values()
        .flat_map(|c| c.results.iter().rev().take(50).cloned())
        .collect();
    axum::Json(serde_json::json!(results))
}

/// GET /api/summaries — JSON list of recent periodic summaries (newest first)
pub async fn api_summaries(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let buf = state.summaries.read().await;
    let summaries: Vec<crate::state::SummaryEntry> = buf.iter().rev().cloned().collect();
    axum::Json(summaries)
}

/// GET /api/snapshot/:camera_id — latest JPEG frame for a camera.
/// Sends `Content-Disposition: attachment` so browsers always treat this
/// as a download. Without it, clicking "Download Snapshot" can navigate
/// the dashboard tab to the JPEG and tear down the SSE connection.
pub async fn api_snapshot(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(camera_id): axum::extract::Path<String>,
) -> Response {
    let cameras = state.cameras.read().await;
    if let Some(cam) = cameras.get(&camera_id) {
        if let Some(ref jpeg) = cam.latest_frame {
            let filename = format!("{}.jpg", camera_id);
            let disposition = format!("attachment; filename=\"{}\"", filename);
            return (
                StatusCode::OK,
                [
                    ("content-type", "image/jpeg"),
                    ("content-disposition", disposition.as_str()),
                ],
                jpeg.clone(),
            )
                .into_response();
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

/// GET /api/events — Server-Sent Events stream for live updates.
///
/// Cloudflare/proxy compatibility:
/// - 15s keep-alive comments avoid Cloudflare's ~100s idle-connection timeout.
/// - `Cache-Control: no-cache, no-transform` prevents edge caching and
///   compression rewrites that would buffer the stream.
/// - `X-Accel-Buffering: no` disables nginx response buffering.
pub async fn api_events(State(state): State<Arc<AppState>>) -> Response {
    let mut rx = state.events_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(data) => {
                    yield Ok::<_, std::convert::Infallible>(
                        axum::response::sse::Event::default().data(data)
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("SSE client lagged by {} messages", n);
                    continue;
                }
                Err(_) => break,
            }
        }
    };

    let keep_alive = axum::response::sse::KeepAlive::new()
        .interval(std::time::Duration::from_secs(15))
        .text("heartbeat");

    let mut response = axum::response::Sse::new(stream)
        .keep_alive(keep_alive)
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

// ---------------------------------------------------------------------------
// POST API endpoints for dashboard commands
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AskRequest {
    pub question: String,
}

/// POST /api/ask — ask a visual question about the current frame.
/// Injects the last N scene descriptions (config: `context_window_frames`)
/// as short-term memory so users can ask about recent activity.
pub async fn api_ask(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<AskRequest>,
) -> impl IntoResponse {
    let n = state.config.monitor.context_window_frames as usize;
    let cameras = state.cameras.read().await;
    let snapshot = cameras
        .values()
        .find(|c| c.latest_frame.is_some())
        .map(|c| (c.latest_frame.clone().unwrap(), c.recent_context_digest(n)));
    drop(cameras);

    let Some((jpeg, context)) = snapshot else {
        return axum::Json(serde_json::json!({
            "error": "The camera has no live frame, so I cannot chat right now."
        }));
    };

    match state
        .vlm
        .infer_with_context(&jpeg, &context, &body.question)
        .await
    {
        Ok((text, secs)) => axum::Json(serde_json::json!({"answer": text, "infer_secs": secs})),
        Err(e) => axum::Json(serde_json::json!({"error": format!("Inference error: {}", e)})),
    }
}

#[derive(Deserialize)]
pub struct CommandRequest {
    pub action: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub camera_id: Option<String>,
}

/// POST /api/command — send a command (ptz, patrol) to a camera
pub async fn api_command(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<CommandRequest>,
) -> impl IntoResponse {
    let result = if let Some(ref cam_id) = body.camera_id {
        crate::ws::send_camera_command(&state, cam_id, &body.action, body.params.clone())
            .await
            .map(|_| cam_id.clone())
    } else {
        crate::ws::send_command_to_any_camera(&state, &body.action, body.params.clone()).await
    };

    match result {
        Ok(cam_id) => axum::Json(serde_json::json!({"ok": true, "camera_id": cam_id})),
        Err(e) => axum::Json(serde_json::json!({"ok": false, "error": e})),
    }
}

/// Load Tera templates from the templates/ directory.
fn load_templates() -> Result<tera::Tera, (StatusCode, String)> {
    // Resolve relative to the server binary's directory or CWD
    let template_dir = if std::path::Path::new("templates").exists() {
        "templates/**/*".to_string()
    } else if std::path::Path::new("server/templates").exists() {
        "server/templates/**/*".to_string()
    } else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot find templates/ directory".to_string(),
        ));
    };
    tera::Tera::new(&template_dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template error: {}", e),
        )
    })
}
