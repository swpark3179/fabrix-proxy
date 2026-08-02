//! 로컬 OpenAI 호환 엔드포인트. 노출 경로는 딱 둘 —
//! `POST /v1/chat/completions` 와 `GET /v1/models`.

pub mod chat;
pub mod fabrix;
pub mod models;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::cors::CorsLayer;

use crate::config::Config;
use crate::openai::ErrorEnvelope;
use crate::state::{ServerHandle, Shared};

/// 리스너를 **먼저** 바인드해서 포트 오류를 즉시 표면화한 뒤에 spawn 합니다.
pub async fn start(state: Shared, port: u16) -> Result<u16, String> {
    if state.is_running() {
        return Err("프록시가 이미 실행 중입니다".into());
    }

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|err| format!("{port} 포트를 열지 못했습니다: {err}"))?;

    let (tx, rx) = oneshot::channel::<()>();
    let router = router(state.clone());

    tauri::async_runtime::spawn(async move {
        let served = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
        if let Err(err) = served {
            eprintln!("[proxy] 서버가 비정상 종료했습니다: {err}");
        }
    });

    *state.server.lock().unwrap() = Some(ServerHandle { port, shutdown: tx });
    Ok(port)
}

pub fn stop(state: &Shared) {
    let handle = state.server.lock().unwrap().take();
    if let Some(handle) = handle {
        // 수신자가 이미 사라졌으면 서버도 이미 죽은 것이므로 결과는 무시합니다.
        let _ = handle.shutdown.send(());
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/v1/models", get(models::handle))
        .route("/v1/chat/completions", post(chat::handle))
        // Base URL 에 `/v1` 을 빼먹고 넣는 클라이언트도 받아 줍니다.
        .route("/models", get(models::handle))
        .route("/chat/completions", post(chat::handle))
        .route("/health", get(health))
        .fallback(not_found)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn health() -> Response {
    Json(serde_json::json!({ "status": "ok" })).into_response()
}

async fn not_found() -> Response {
    error_response(
        404,
        ErrorEnvelope::new(
            "이 프록시는 /v1/chat/completions 와 /v1/models 만 제공합니다.",
            "invalid_request_error",
            Some("unknown_endpoint".into()),
        ),
    )
}

pub fn error_response(status: u16, envelope: ErrorEnvelope) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, Json(envelope)).into_response()
}

/// 로그 ② 칸 상단에 붙는 마스킹된 헤더 줄.
pub fn fabrix_headers_line(cfg: &Config) -> String {
    format!(
        "x-fabrix-client {} · x-openapi-token {}",
        Config::mask(&cfg.fabrix_client),
        Config::mask(&cfg.openapi_token)
    )
}

pub fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}
