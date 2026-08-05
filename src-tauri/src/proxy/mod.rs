//! 로컬 OpenAI 호환 엔드포인트.
//!
//! `POST /v1/chat/completions` · `GET /v1/models` · `GET /v1/models/{id}` ·
//! `POST /v1/images/generations` · `POST /v1/images/edits`, 그리고 `/v1` 을 빼먹은
//! 별칭들. 오류는 전부 OpenAI 봉투(`type`·`code`·`param`)를 타고,
//! `type` 은 상태 코드에서 유도합니다([`openai_type`]).

pub mod b64;
pub mod chat;
pub mod fabrix;
pub mod image_backend;
pub mod images;
pub mod models;
pub mod tools;
pub mod usage;
pub mod validate;

use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
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

    // 인자가 아니라 **실제로 바인드된** 포트를 씁니다. 앱은 언제나 구체적인 포트를
    // 주므로 두 값이 같지만, 포트 `0`(OS 자동 할당)을 넘기면 이 값만이 진짜입니다 —
    // 통합 테스트가 고정 포트를 다투지 않고 서버를 띄울 수 있게 하기 위함입니다.
    let bound = listener
        .local_addr()
        .map_err(|err| format!("포트를 확인하지 못했습니다: {err}"))?
        .port();

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

    *state.server.lock().unwrap() = Some(ServerHandle { port: bound, shutdown: tx });
    Ok(bound)
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
        // axum 0.8 의 경로 파라미터 문법은 `{id}` 입니다 — 0.7 의 `:id` 를 쓰면 리터럴
        // 경로가 되어 조용히 아무것도 맞지 않습니다.
        .route("/v1/models/{id}", get(models::retrieve))
        .route("/v1/chat/completions", post(chat::handle))
        .route("/v1/images/generations", post(images::generations))
        .route("/v1/images/edits", post(images::edits))
        // Base URL 에 `/v1` 을 빼먹고 넣는 클라이언트도 받아 줍니다.
        .route("/models", get(models::handle))
        .route("/models/{id}", get(models::retrieve))
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
        // `x-fabrix-*` 는 브라우저 클라이언트가 읽어야 뜻이 있습니다. CORS 는 기본적으로
        // 안전 목록 밖 응답 헤더를 스크립트에서 가립니다.
        .layer(CorsLayer::permissive().expose_headers([
            axum::http::HeaderName::from_static("x-fabrix-usage"),
            axum::http::HeaderName::from_static("x-fabrix-image-stub"),
        ]))
        .with_state(state)
}

async fn health() -> Response {
    Json(serde_json::json!({ "status": "ok" })).into_response()
}

async fn not_found() -> Response {
    error_response(
        404,
        ErrorEnvelope::new(
            "이 프록시는 /v1/chat/completions · /v1/models · /v1/models/{id} · \
             /v1/images/generations · /v1/images/edits 를 제공합니다.",
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

/// 백엔드 구성 지문. FabriX 는 이런 값을 주지 않으므로 **재현 가능한** 값을 만듭니다 —
/// 프록시 버전과 실제로 나간 modelId 를 FNV-1a 로 접습니다. 같은 구성이면 앱을 다시
/// 켜도 같은 값이고, 모델이나 프록시가 바뀌면 값이 바뀝니다. 그게 이 필드의 뜻입니다.
///
/// `DefaultHasher` 를 쓰지 않는 이유: 출력이 Rust 릴리스 간 안정하다는 보장이 없어,
/// 컴파일러를 올리면 지문이 통째로 달라집니다. FNV-1a 는 10줄이라 직접 씁니다.
pub fn system_fingerprint(model_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in env!("CARGO_PKG_VERSION").bytes().chain(b"/".iter().copied()).chain(model_id.bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    // 하위 48비트만 씁니다 — OpenAI 의 `fp_…` 와 비슷한 길이로 맞추기 위한 것이고,
    // 지문은 충돌 저항이 아니라 "구성이 같으면 같은 값" 만 만족하면 됩니다.
    format!("fp_{:012x}", hash & 0xffff_ffff_ffff)
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

/// 상한을 넘은 본문을 **버리면서 끝까지 읽어 줄** 최대 크기 (상한의 4배).
///
/// 여기까지는 읽어 주고 413 봉투를 돌려줍니다. 초과를 발견한 순간 연결을 닫으면
/// 클라이언트는 아직 본문을 쓰던 중이라 `socket hang up` 만 보고, **정작 우리가 준비한
/// 설명을 읽지 못합니다** — 그 설명이 이 코드의 존재 이유인데 말이죠. 그렇다고 무한히
/// 받아 줄 수도 없으니 선을 긋습니다. 이 선을 넘는 요청은 어차피 도와줄 방법이 없습니다.
const DRAIN_MULTIPLE: usize = 4;

/// 요청 본문을 상한까지 읽습니다. 초과면 OpenAI 봉투를 돌려줄 수 있도록 `Err` 입니다.
///
/// `DefaultBodyLimit` 레이어에 맡기지 않는 이유: 그러면 초과가 핸들러에 **들어오기
/// 전에** axum 의 평문 413 으로 끝나 로그에 한 줄도 남지 않습니다. 우리가 읽으면
/// 봉투도 주고 로그도 남깁니다.
pub async fn read_body(
    headers: &HeaderMap,
    body: axum::body::Body,
    limit: usize,
) -> Result<axum::body::Bytes, ErrorEnvelope> {
    let drain_cap = limit.saturating_mul(DRAIN_MULTIPLE);

    // 선언된 길이가 배수 상한마저 넘으면 한 바이트도 받지 않고 거절합니다.
    let declared = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > drain_cap) {
        return Err(too_large(limit));
    }

    let mut stream = body.into_data_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut total = 0usize;
    let mut over = false;

    while let Some(chunk) = stream.next().await {
        // 전송이 끊긴 것은 상한 초과와 다릅니다 — 이 경우는 본문이 없는 것으로 넘기고
        // 파싱 단계가 400 을 내게 합니다.
        let Ok(chunk) = chunk else { break };
        total = total.saturating_add(chunk.len());
        if total > limit {
            // 넘은 순간부터 모으지 않고 버리기만 합니다 — 상한의 4배까지 받아 주는 것은
            // 클라이언트가 응답을 읽을 수 있게 하려는 것뿐이고, 그 내용은 쓰지 않습니다.
            over = true;
            buf = Vec::new();
        } else {
            buf.extend_from_slice(&chunk);
        }
        if total > drain_cap {
            break;
        }
    }

    if over {
        return Err(too_large(limit));
    }
    Ok(axum::body::Bytes::from(buf))
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
    /// 같은 구성이면 같은 값, 모델이 다르면 다른 값 — 그게 이 필드의 뜻입니다.
    #[test]
    fn fingerprint_is_stable_and_model_sensitive() {
        let a = system_fingerprint("0196f1fc-2858-70a9-a232-74dbddb971d0");
        assert_eq!(a, system_fingerprint("0196f1fc-2858-70a9-a232-74dbddb971d0"));
        assert_ne!(a, system_fingerprint("01970a3b-91d4-7c8e-9a11-2f3c4d5e6f70"));
        assert!(a.starts_with("fp_"), "{a}");
        assert_eq!(a.len(), 3 + 12);
        assert!(a[3..].chars().all(|c| c.is_ascii_hexdigit()), "{a}");
    }

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

    /// 터무니없이 큰 본문은 한 바이트도 받지 않고 거절합니다 — 어차피 응답을 읽게
    /// 해 줄 수 없는 크기입니다.
    #[tokio::test]
    async fn read_body_rejects_absurd_declared_length_without_reading() {
        let mut headers = HeaderMap::new();
        let absurd = CHAT_BODY_LIMIT * DRAIN_MULTIPLE + 1;
        headers.insert("content-length", absurd.to_string().parse().unwrap());
        let err = read_body(&headers, axum::body::Body::empty(), CHAT_BODY_LIMIT)
            .await
            .expect_err("413 이 나야 합니다");
        assert_eq!(err.error.code.as_deref(), Some("request_too_large"));
    }

    /// 상한을 조금 넘은 본문은 **끝까지 받아 준 뒤** 413 을 돌려줍니다. 초과를 발견한
    /// 순간 닫으면 클라이언트가 응답 대신 broken pipe 를 봅니다.
    #[tokio::test]
    async fn read_body_drains_a_slightly_oversized_body_then_rejects() {
        let err = read_body(&HeaderMap::new(), axum::body::Body::from(vec![0u8; 20]), 8)
            .await
            .expect_err("413 이 나야 합니다");
        assert_eq!(err.error.code.as_deref(), Some("request_too_large"));
    }

    #[tokio::test]
    async fn read_body_accepts_a_body_at_the_limit() {
        let ok = read_body(&HeaderMap::new(), axum::body::Body::from(vec![0u8; 8]), 8).await;
        assert_eq!(ok.unwrap().len(), 8);
        let ok = read_body(&HeaderMap::new(), axum::body::Body::empty(), 8).await;
        assert_eq!(ok.unwrap().len(), 0);
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
