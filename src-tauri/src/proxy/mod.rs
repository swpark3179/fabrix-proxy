//! 로컬 OpenAI 호환 엔드포인트. 노출 경로는 딱 둘 —
//! `POST /v1/chat/completions` 와 `GET /v1/models`.

pub mod b64;
pub mod chat;
pub mod fabrix;
pub mod image_backend;
pub mod images;
pub mod models;

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderMap, StatusCode};
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

/// 이미지 요청 본문 상한. i2i 의 base64 data URL 은 axum 기본값(2MiB)을 쉽게 넘기므로
/// 이미지 라우트에만 올려 줍니다 (chat 은 기본값 유지).
const IMAGE_BODY_LIMIT: usize = 25 * 1024 * 1024;

fn router(state: Shared) -> Router {
    Router::new()
        .route("/v1/models", get(models::handle))
        .route("/v1/chat/completions", post(chat::handle))
        .route(
            "/v1/images/generations",
            post(images::generations).layer(DefaultBodyLimit::max(IMAGE_BODY_LIMIT)),
        )
        .route(
            "/v1/images/edits",
            post(images::edits).layer(DefaultBodyLimit::max(IMAGE_BODY_LIMIT)),
        )
        // Base URL 에 `/v1` 을 빼먹고 넣는 클라이언트도 받아 줍니다.
        .route("/models", get(models::handle))
        .route("/chat/completions", post(chat::handle))
        .route(
            "/images/generations",
            post(images::generations).layer(DefaultBodyLimit::max(IMAGE_BODY_LIMIT)),
        )
        .route(
            "/images/edits",
            post(images::edits).layer(DefaultBodyLimit::max(IMAGE_BODY_LIMIT)),
        )
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
            "이 프록시는 /v1/chat/completions · /v1/models · /v1/images/generations · /v1/images/edits 를 제공합니다.",
            "invalid_request_error",
            Some("unknown_endpoint".into()),
        ),
    )
}

pub fn error_response(status: u16, envelope: ErrorEnvelope) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, Json(envelope)).into_response()
}

/// 인바운드 토큰을 검사합니다. 토큰 사용 모드일 때만 실제로 검증하고,
/// 키발급없이 허용 모드면 항상 통과시킵니다(기존 동작 유지).
///
/// 통과하면 `Ok(())`, 거부면 OpenAI 표준 `invalid_api_key` 401 을 돌려줍니다.
pub fn authorize(cfg: &Config, headers: &HeaderMap) -> Result<(), (u16, ErrorEnvelope)> {
    if !cfg.token_mode {
        return Ok(());
    }

    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer ").unwrap_or(v).trim());

    let expected = cfg.issued_token.trim();
    match presented {
        Some(tok) if !expected.is_empty() && tok == expected => Ok(()),
        _ => Err((
            401,
            ErrorEnvelope::new(
                "토큰이 일치하지 않습니다. 발행된 토큰을 API 키로 입력하세요.",
                "invalid_request_error",
                Some("invalid_api_key".into()),
            ),
        )),
    }
}

/// 로그 ① 칸의 `Authorization:` 줄 — 모드에 따라 문구가 달라집니다.
pub fn inbound_auth_line(cfg: &Config, headers: &HeaderMap) -> &'static str {
    let has = headers.contains_key("authorization");
    if cfg.token_mode {
        if has {
            "Bearer ●●●●(검증됨)"
        } else {
            "(없음 · 토큰 모드에서는 거부)"
        }
    } else if has {
        "Bearer ●●●●(값 무시)"
    } else {
        "(없음 · 무시)"
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{generate_token, Config};

    fn with_auth(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", value.parse().unwrap());
        h
    }

    #[test]
    fn open_mode_passes_any_token() {
        let cfg = Config { token_mode: false, ..Config::default() };
        assert!(authorize(&cfg, &with_auth("Bearer whatever")).is_ok());
        assert!(authorize(&cfg, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn token_mode_accepts_matching_bearer() {
        let cfg = Config { token_mode: true, issued_token: "sk-abc123".into(), ..Config::default() };
        assert!(authorize(&cfg, &with_auth("Bearer sk-abc123")).is_ok());
    }

    #[test]
    fn token_mode_rejects_mismatch_missing_and_empty_issued() {
        let cfg = Config { token_mode: true, issued_token: "sk-abc123".into(), ..Config::default() };
        // 틀린 토큰
        assert_eq!(authorize(&cfg, &with_auth("Bearer sk-wrong")).unwrap_err().0, 401);
        // 헤더 없음
        assert_eq!(authorize(&cfg, &HeaderMap::new()).unwrap_err().0, 401);
        // 발행 토큰이 비어 있으면 어떤 값도 통과하지 못합니다 (빈 토큰 우회 방지).
        let empty = Config { token_mode: true, issued_token: String::new(), ..Config::default() };
        assert_eq!(authorize(&empty, &with_auth("Bearer ")).unwrap_err().0, 401);
    }

    #[test]
    fn generated_token_has_openai_shape() {
        let t = generate_token();
        assert!(t.starts_with("sk-"));
        assert_eq!(t.len(), 3 + 48);
        assert!(t[3..].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(generate_token(), generate_token());
    }
}
