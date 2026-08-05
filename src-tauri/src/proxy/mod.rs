//! 로컬 OpenAI 호환 엔드포인트. 노출 경로는 딱 둘 —
//! `POST /v1/chat/completions` 와 `GET /v1/models`.

pub mod b64;
pub mod chat;
pub mod fabrix;
pub mod image_backend;
pub mod images;
pub mod models;
pub mod tools;
pub mod validate;

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

/// 이미지 요청 본문 상한. i2i 의 base64 data URL 은 axum 기본값(2MiB)을 쉽게 넘깁니다.
pub const IMAGE_BODY_LIMIT: usize = 25 * 1024 * 1024;

/// 채팅 요청 본문 상한.
///
/// axum 기본값은 2MiB 인데, 에이전트 클라이언트는 도구 스키마 여러 벌에 더해 읽은
/// 파일 내용을 `role:"tool"` 결과로 되먹이기 때문에 긴 세션에서 이를 넘길 수 있습니다.
pub const CHAT_BODY_LIMIT: usize = 16 * 1024 * 1024;

fn router(state: Shared) -> Router {
    Router::new()
        // 본문 상한은 `DefaultBodyLimit` 레이어가 아니라 **핸들러 안에서** 겁니다
        // (`read_body`). 레이어에 맡기면 초과가 핸들러에 들어오기 전에 axum 의 평문
        // 413 으로 끝나 로그 창에 아무 흔적도 남지 않습니다 — 사용자 입장에서는
        // 원인 없는 실패였습니다.
        .route("/v1/models", get(models::handle))
        .route("/v1/chat/completions", post(chat::handle))
        .route("/v1/images/generations", post(images::generations))
        .route("/v1/images/edits", post(images::edits))
        // Base URL 에 `/v1` 을 빼먹고 넣는 클라이언트도 받아 줍니다.
        .route("/models", get(models::handle))
        .route("/chat/completions", post(chat::handle))
        .route("/images/generations", post(images::generations))
        .route("/images/edits", post(images::edits))
        .route("/health", get(health))
        .layer(DefaultBodyLimit::disable())
        // 아는 경로에 잘못된 메서드가 오면 axum 기본값은 **본문 없는 405** 입니다.
        // `.fallback` 은 모르는 경로만 잡으므로 따로 답니다. 라우트를 다 등록한 뒤에
        // 불러야 그 시점의 라우트들에 적용됩니다.
        .method_not_allowed_fallback(method_not_allowed)
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
            openai_type(404),
            Some("unknown_endpoint".into()),
        ),
    )
}

/// 상태 코드에서 OpenAI 오류 `type` 을 유도합니다.
///
/// 예전에는 `type` 에 우리가 지은 값(`upstream_error` · `configuration_error`)이
/// 들어갔습니다. `error.type` 으로 분기하는 클라이언트는 그걸 모르는 값으로 봅니다.
/// 상태 코드에서 기계적으로 유도하면 **언제나 합법값**이 나오고, 우리 고유의 구분은
/// `code` 로 옮기면 하나도 잃지 않습니다.
pub fn openai_type(status: u16) -> &'static str {
    match status {
        401 => "authentication_error",
        403 => "permission_error",
        429 => "rate_limit_error",
        s if s >= 500 => "api_error",
        _ => "invalid_request_error",
    }
}

fn method_not_allowed_envelope() -> ErrorEnvelope {
    ErrorEnvelope::new(
        "이 경로가 받지 않는 메서드입니다. 채팅·이미지는 POST, 모델 목록은 GET 입니다.",
        openai_type(405),
        Some("method_not_allowed".into()),
    )
}

async fn method_not_allowed() -> Response {
    error_response(405, method_not_allowed_envelope())
}

/// 요청 본문을 상한까지 읽습니다. 초과면 OpenAI 봉투를 돌려줄 수 있도록 `Err` 입니다.
///
/// `DefaultBodyLimit` 레이어에 맡기지 않는 이유: 그러면 초과가 핸들러에 **들어오기
/// 전에** axum 의 평문 413 으로 끝나 로그에 한 줄도 남지 않습니다. 우리가 읽으면
/// 봉투도 주고 로그도 남깁니다.
///
/// `content-length` 를 먼저 보는 이유는 한 바이트도 받지 않고 거절할 수 있기 때문입니다.
/// 그 헤더가 없는(chunked) 요청은 `to_bytes` 의 상한이 잡습니다 — 상한을 두 겹으로
/// 겁니다.
pub async fn read_body(
    headers: &HeaderMap,
    body: axum::body::Body,
    limit: usize,
) -> Result<axum::body::Bytes, ErrorEnvelope> {
    let declared = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > limit) {
        return Err(too_large(limit));
    }
    axum::body::to_bytes(body, limit).await.map_err(|_| too_large(limit))
}

fn too_large(limit: usize) -> ErrorEnvelope {
    ErrorEnvelope::new(
        format!(
            "요청 본문이 상한({} MiB)을 넘었습니다. 에이전트 세션이 길어지면 도구 결과가 \
             쌓여 이 값을 넘길 수 있습니다 — 대화를 새로 시작하거나 첨부를 줄이세요.",
            limit / (1024 * 1024)
        ),
        openai_type(413),
        Some("request_too_large".into()),
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
            // 401 은 `authentication_error` 입니다 — 예전에는 `invalid_request_error`
            // 였는데, 인증 실패와 잘못된 파라미터를 구분해 재시도/재인증을 결정하는
            // 클라이언트가 그 둘을 가릴 수 없었습니다.
            ErrorEnvelope::new(
                "토큰이 일치하지 않습니다. 발행된 토큰을 API 키로 입력하세요.",
                openai_type(401),
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

    /// OpenAI 가 정의한 `type` 값만 나가야 합니다 — 예전에는 우리가 지은
    /// `upstream_error` · `configuration_error` 가 이 자리에 들어갔습니다.
    #[test]
    fn openai_type_is_always_a_legal_openai_type() {
        const LEGAL: &[&str] = &[
            "invalid_request_error",
            "authentication_error",
            "permission_error",
            "rate_limit_error",
            "api_error",
        ];
        for status in [400u16, 401, 403, 404, 405, 413, 415, 422, 429, 500, 502, 503, 504] {
            let kind = openai_type(status);
            assert!(LEGAL.contains(&kind), "{status} → {kind}");
        }
        assert_eq!(openai_type(400), "invalid_request_error");
        assert_eq!(openai_type(404), "invalid_request_error");
        assert_eq!(openai_type(405), "invalid_request_error");
        assert_eq!(openai_type(413), "invalid_request_error");
        assert_eq!(openai_type(401), "authentication_error");
        assert_eq!(openai_type(403), "permission_error");
        assert_eq!(openai_type(429), "rate_limit_error");
        assert_eq!(openai_type(502), "api_error");
        assert_eq!(openai_type(503), "api_error");
    }

    /// OpenAI 는 없는 값을 `null` 로 내보냅니다. 키 자체가 빠지면 `error.param` 을
    /// 무조건 읽는 클라이언트가 죽습니다.
    #[test]
    fn error_envelope_serializes_param_and_code_as_null() {
        let json =
            serde_json::to_value(ErrorEnvelope::new("나쁨", "invalid_request_error", None)).unwrap();
        assert!(json["error"].as_object().unwrap().contains_key("param"));
        assert!(json["error"].as_object().unwrap().contains_key("code"));
        assert!(json["error"]["param"].is_null());
        assert!(json["error"]["code"].is_null());
    }

    #[test]
    fn token_rejection_is_an_authentication_error() {
        let cfg = Config { token_mode: true, issued_token: "sk-abc".into(), ..Config::default() };
        let (status, env) = authorize(&cfg, &HeaderMap::new()).unwrap_err();
        assert_eq!(status, 401);
        assert_eq!(env.error.kind, "authentication_error");
        assert_eq!(env.error.code.as_deref(), Some("invalid_api_key"));
    }

    /// 예전에는 axum 이 **본문 없는** 405 를 냈습니다 — 봉투로 분기하는 클라이언트에는
    /// 원인 없는 실패였습니다.
    #[test]
    fn method_not_allowed_is_an_envelope() {
        let env = method_not_allowed_envelope();
        assert_eq!(env.error.kind, "invalid_request_error");
        assert_eq!(env.error.code.as_deref(), Some("method_not_allowed"));
        assert!(env.error.message.contains("POST"));
    }

    /// 예전에는 axum 이 핸들러 밖에서 **평문** 413 을 냈습니다.
    #[test]
    fn oversized_body_is_an_envelope_that_names_the_limit() {
        let env = too_large(CHAT_BODY_LIMIT);
        assert_eq!(env.error.kind, "invalid_request_error");
        assert_eq!(env.error.code.as_deref(), Some("request_too_large"));
        assert!(env.error.message.contains("16 MiB"), "{}", env.error.message);
        assert!(too_large(IMAGE_BODY_LIMIT).error.message.contains("25 MiB"));
    }

    #[tokio::test]
    async fn read_body_rejects_by_declared_content_length_without_reading() {
        let mut headers = HeaderMap::new();
        headers.insert("content-length", (CHAT_BODY_LIMIT + 1).to_string().parse().unwrap());
        // 본문은 비어 있지만 선언된 길이만으로 거절해야 합니다 — 한 바이트도 받지 않습니다.
        let err = read_body(&headers, axum::body::Body::empty(), CHAT_BODY_LIMIT)
            .await
            .expect_err("413 이 나야 합니다");
        assert_eq!(err.error.code.as_deref(), Some("request_too_large"));
    }

    /// `content-length` 가 없는(chunked) 요청도 상한이 잡아야 합니다 — 상한 두 겹의 두 번째.
    #[tokio::test]
    async fn read_body_rejects_oversized_chunked_body() {
        let err = read_body(&HeaderMap::new(), axum::body::Body::from(vec![0u8; 64]), 8)
            .await
            .expect_err("413 이 나야 합니다");
        assert_eq!(err.error.code.as_deref(), Some("request_too_large"));

        let ok = read_body(&HeaderMap::new(), axum::body::Body::from(vec![0u8; 8]), 8).await;
        assert_eq!(ok.unwrap().len(), 8);
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
