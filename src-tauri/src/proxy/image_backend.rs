//! 이미지 백엔드 — FLUX(생성) + gemma(인식) 을 사내 FabriX 위에서 호출합니다.
//!
//! 두 기능 모두 업스트림은 **`POST {base}/openapi/chat/v1/messages-with-models`** 하나이며
//! (chat 의 `/messages` 와 다름), 요청은 `modelIds = [텍스트 모델, 이미지 모델]` 두 개를 함께 보냅니다.
//!
//! - 생성(t2i): `application/x-www-form-urlencoded`, `isStream=False`, `messageConfig={width,height}`.
//!   응답 JSON 의 `actions[0].answer`(base64) 를 디코드해 이미지 바이트를 얻습니다.
//! - 인식(i2t): `multipart/form-data`(파일 파트 `files`), `isStream=True`. SSE 를 누적해 설명 텍스트를 얻습니다.
//!
//! 연결(base URL · `x-fabrix-client` · `x-openapi-token`)은 chat 과 동일하게 재사용합니다.
//! 편집은 gemma 인식 → 설명 + 편집 지시 합성 → FLUX 재생성(describe-then-regenerate)으로 동작합니다.

use std::time::Duration;

use crate::config::Config;
use crate::proxy::fabrix::{classify_status, FabrixError, StreamDecoder};

/// 이미지 생성·인식 공용 업스트림 경로.
pub const MESSAGES_WITH_MODELS_PATH: &str = "/openapi/chat/v1/messages-with-models";

/// 비스트림(생성) 요청의 전체 타임아웃. 생성은 chat 보다 오래 걸릴 수 있습니다.
pub const IMAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// 요청 size 가 없거나 파싱 실패 시 기본 해상도.
pub const DEFAULT_SIZE: (u32, u32) = (1024, 1024);
/// `n` 상한 — 작업/메모리 폭주 방지 (스펙상 다중 생성은 미지원이므로 사실상 1).
pub const MAX_N: u32 = 4;

/// gemma 에게 참조 이미지를 재현 가능한 수준으로 묘사하도록 요청하는 기본 지시.
pub const DESCRIBE_INSTRUCTION: &str =
    "이 이미지를 다른 이미지 생성 모델이 재현할 수 있을 만큼 자세히 묘사해줘. \
     주요 객체, 색상, 구도, 스타일, 배경, 질감을 빠짐없이 포함하고, 텍스트로만 답해줘.";

// ─────────────────────────── 오류 ───────────────────────────

/// 이미지 파이프라인 오류. 전송/업스트림 분류는 `FabrixError` 를 재사용합니다.
#[derive(Debug, Clone)]
pub enum ImageError {
    /// 잘못된 요청(prompt/이미지 누락, data URL·MIME 오류 등). 400.
    BadRequest(String),
    /// 전송/업스트림 — FabriX 분류 재사용.
    Backend(FabrixError),
}

impl ImageError {
    pub fn status(&self) -> u16 {
        match self {
            ImageError::BadRequest(_) => 400,
            ImageError::Backend(inner) => match inner {
                // 값비싼 이미지 백엔드의 일시 장애는 소비자가 재시도해야 유용하므로,
                // chat 의 502 대신 503(재시도 집합 포함)으로 매핑합니다.
                FabrixError::Unreachable(_) => 503,
                other => other.status(),
            },
        }
    }

    pub fn note(&self) -> String {
        match self {
            ImageError::BadRequest(_) => "잘못된 요청".into(),
            ImageError::Backend(inner) => inner.note(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            ImageError::BadRequest(m) => m.clone(),
            ImageError::Backend(inner) => inner.message(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ImageError::BadRequest(_) => "invalid_request_error",
            ImageError::Backend(inner) => inner.kind(),
        }
    }
}

// ─────────────────────────── 모델 · 사이즈 resolve ───────────────────────────

fn require_model(value: &str) -> Result<String, ImageError> {
    let v = value.trim();
    if v.is_empty() {
        Err(ImageError::Backend(FabrixError::NotConfigured))
    } else {
        Ok(v.to_string())
    }
}

/// 이미지 호출에 함께 보내는 텍스트(베이스 LLM) 모델 — 설정 고정값.
pub fn resolve_text_model(cfg: &Config) -> Result<String, ImageError> {
    require_model(&cfg.image_text_model)
}

/// FLUX(생성) 모델 — 설정 화면에서 고른 고정값.
pub fn resolve_image_model(cfg: &Config) -> Result<String, ImageError> {
    require_model(&cfg.image_model)
}

/// gemma(인식) 모델 — 설정 화면에서 고른 고정값.
pub fn resolve_vision_model(cfg: &Config) -> Result<String, ImageError> {
    require_model(&cfg.vision_model)
}

/// 요청 size 를 그대로 쓰되, 없으면 기본값. (업스트림이 width/height 를 직접 받으므로 스냅 불필요)
pub fn size_or_default(requested: Option<(u32, u32)>) -> (u32, u32) {
    requested.unwrap_or(DEFAULT_SIZE)
}

/// gemma 설명 + 사용자 편집 지시를 FLUX 프롬프트로 결합합니다. (순수 · 테스트 대상)
pub fn compose_edit_prompt(description: &str, instruction: &str) -> String {
    let description = description.trim();
    let instruction = instruction.trim();
    if description.is_empty() {
        return instruction.to_string();
    }
    format!(
        "원본 이미지 설명:\n{description}\n\n요청된 편집:\n{instruction}\n\n\
         위 설명을 바탕으로, 편집 지시를 반영한 이미지를 생성하세요."
    )
}

/// 매직바이트로 mime 을 판정합니다 (PNG / JPEG / WebP). 그 외는 PNG 로 간주합니다.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 3 && bytes[0..3] == [0xFF, 0xD8, 0xFF] {
        "image/jpeg"
    } else if bytes.len() >= 8
        && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        "image/png"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/png"
    }
}

fn ext_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// 이미지 호출에 함께 보내는 기본 llmConfig(JSON 문자열).
/// ⚠️ 이 엔드포인트는 chat 의 오타(`repetion_penalty`/`tok_k`)가 아니라 **정상 철자**를 씁니다.
fn default_llm_config() -> String {
    serde_json::json!({
        "max_new_tokens": 1024,
        "top_k": 14,
        "top_p": 0.94,
        "temperature": 0.4,
        "repetition_penalty": 1.04
    })
    .to_string()
}

/// 1×1 투명 PNG. `image_stub_mode` 에서만 반환하며, 실제 성공과 혼동되지 않도록
/// 호출부가 `[stub]` 로 표기합니다. (CRC 를 손으로 맞추지 않도록 base64 로 임베드)
pub fn placeholder_png() -> Vec<u8> {
    const B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    crate::proxy::b64::decode(B64).unwrap_or_default()
}

/// 이미지용 HTTP 클라이언트. 생성이 무응답으로 오래 걸릴 수 있어 read(비활동) 타임아웃을 길게 잡습니다.
pub fn build_image_http_client(insecure: bool) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(300))
        .user_agent(concat!("fabrix-proxy/", env!("CARGO_PKG_VERSION")));
    let builder = if insecure { builder.danger_accept_invalid_certs(true) } else { builder };
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

// ─────────────────────────── 클라이언트 ───────────────────────────

/// FabriX 연결(base URL · 헤더)을 재사용하는 이미지 백엔드 클라이언트.
///
/// 이 구조체와 `state.image_client()` 생성자 하나가 "이미지가 FabriX 게이트웨이를 경유한다"는
/// 결정을 격리하는 seam 입니다 — 나중에 별도 이미지 서비스로 바뀌어도 여기와 설정 필드만 손보면 됩니다.
pub struct ImageClient {
    pub http: reqwest::Client,
    pub base: String,
    pub client_key: String,
    pub token: String,
}

impl ImageClient {
    fn url(&self) -> String {
        format!("{}{MESSAGES_WITH_MODELS_PATH}", self.base)
    }

    /// FLUX 2.0 — 텍스트→이미지. form-urlencoded · isStream=false · messageConfig{width,height}.
    /// 응답의 `actions[0].answer`(base64) 를 디코드해 이미지 바이트를 돌려줍니다.
    pub async fn generate(
        &self,
        text_model: &str,
        gen_model: &str,
        prompt: &str,
        size: (u32, u32),
    ) -> Result<Vec<u8>, ImageError> {
        let message_config = serde_json::json!({ "width": size.0, "height": size.1 }).to_string();
        // modelIds 는 [텍스트 모델, 이미지 모델] 두 개를 반복 키로 보냅니다.
        let params: Vec<(&str, String)> = vec![
            ("modelIds", text_model.to_string()),
            ("modelIds", gen_model.to_string()),
            ("contents", prompt.to_string()),
            ("llmConfig", default_llm_config()),
            ("isStream", "False".to_string()),
            ("messageConfig", message_config),
        ];

        let res = self
            .http
            .post(self.url())
            .header("x-fabrix-client", &self.client_key)
            .header("x-openapi-token", &self.token)
            .header("Accept", "application/json")
            .timeout(IMAGE_REQUEST_TIMEOUT)
            .form(&params)
            .send()
            .await
            .map_err(|e| ImageError::Backend(FabrixError::from(e)))?;

        let status = res.status();
        let body = res.text().await.map_err(|e| ImageError::Backend(FabrixError::from(e)))?;
        if !status.is_success() {
            return Err(ImageError::Backend(classify_status(status.as_u16(), &body)));
        }

        let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
            ImageError::Backend(FabrixError::BadPayload(format!(
                "이미지 생성 응답 JSON 파싱 실패: {e} — 본문 앞부분: {}",
                crate::logstore::preview(&body, 200)
            )))
        })?;

        let answer = value
            .get("actions")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.get("answer"))
            .and_then(|s| s.as_str());
        let Some(answer) = answer else {
            return Err(ImageError::Backend(FabrixError::BadPayload(format!(
                "이미지 생성 응답에서 actions[0].answer 를 찾지 못했습니다 — 본문 앞부분: {}",
                crate::logstore::preview(&body, 200)
            ))));
        };

        // data URL 접두(`data:...;base64,`)가 붙어 있으면 payload 만 취합니다.
        let payload = answer.split_once(',').map(|(_, b)| b).unwrap_or(answer);
        crate::proxy::b64::decode(payload).map_err(|e| {
            ImageError::Backend(FabrixError::BadPayload(format!("이미지 base64 디코드 실패: {e}")))
        })
    }

    /// gemma 4 — 참조 이미지→설명 텍스트. multipart(파일 파트 `files`) · isStream=true.
    /// SSE 를 누적해 설명 텍스트를 돌려줍니다.
    pub async fn understand(
        &self,
        text_model: &str,
        vision_model: &str,
        image: &[u8],
        mime: &str,
        instruction: &str,
    ) -> Result<String, ImageError> {
        let part = reqwest::multipart::Part::bytes(image.to_vec())
            .file_name(format!("image.{}", ext_for_mime(mime)))
            .mime_str(mime)
            .map_err(|e| ImageError::BadRequest(format!("이미지 MIME 오류: {e}")))?;

        // modelIds 는 [텍스트 모델, 인식 모델] 두 개를 같은 이름의 파트로 보냅니다.
        let form = reqwest::multipart::Form::new()
            .text("modelIds", text_model.to_string())
            .text("modelIds", vision_model.to_string())
            .text("contents", instruction.to_string())
            .text("llmConfig", default_llm_config())
            .text("isStream", "True")
            .part("files", part);

        let res = self
            .http
            .post(self.url())
            .header("x-fabrix-client", &self.client_key)
            .header("x-openapi-token", &self.token)
            .header("Accept", "text/event-stream")
            .multipart(form)
            .send()
            .await
            .map_err(|e| ImageError::Backend(FabrixError::from(e)))?;

        let status = res.status();
        let body = res.text().await.map_err(|e| ImageError::Backend(FabrixError::from(e)))?;
        if !status.is_success() {
            return Err(ImageError::Backend(classify_status(status.as_u16(), &body)));
        }

        // FabriX SSE 를 방어적으로 누적합니다(chat 과 동일한 디코더 재사용).
        let mut decoder = StreamDecoder::new();
        decoder.push(body.as_bytes());
        decoder.finish();
        let text = decoder.text().trim().to_string();
        if text.is_empty() {
            return Err(ImageError::Backend(FabrixError::BadPayload(format!(
                "이미지 분석 응답이 비었습니다 — 본문 앞부분: {}",
                crate::logstore::preview(&body, 200)
            ))));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn image_error_status_mapping() {
        assert_eq!(ImageError::BadRequest("x".into()).status(), 400);
        assert_eq!(ImageError::Backend(FabrixError::Quota("q".into())).status(), 429);
        // Unreachable 은 chat 의 502 가 아니라 503 으로 오버라이드(재시도 유도).
        assert_eq!(ImageError::Backend(FabrixError::Unreachable("u".into())).status(), 503);
        assert_eq!(
            ImageError::Backend(FabrixError::Upstream { status: 418, message: "t".into() }).status(),
            418
        );
        assert_eq!(ImageError::Backend(FabrixError::NotConfigured).status(), 503);
    }

    #[test]
    fn image_error_kind() {
        assert_eq!(ImageError::BadRequest("x".into()).kind(), "invalid_request_error");
        assert_eq!(ImageError::Backend(FabrixError::Quota("q".into())).kind(), "rate_limit_error");
    }

    #[test]
    fn resolve_models_require_config() {
        let mut cfg = Config::default();
        assert!(resolve_text_model(&cfg).is_err());
        assert!(resolve_image_model(&cfg).is_err());
        assert!(resolve_vision_model(&cfg).is_err());
        cfg.image_text_model = "text-1".into();
        cfg.image_model = "flux-2".into();
        cfg.vision_model = "gemma-4".into();
        assert_eq!(resolve_text_model(&cfg).unwrap(), "text-1");
        assert_eq!(resolve_image_model(&cfg).unwrap(), "flux-2");
        assert_eq!(resolve_vision_model(&cfg).unwrap(), "gemma-4");
    }

    #[test]
    fn size_or_default_falls_back() {
        assert_eq!(size_or_default(None), DEFAULT_SIZE);
        assert_eq!(size_or_default(Some((1792, 1024))), (1792, 1024));
    }

    #[test]
    fn compose_edit_prompt_includes_both() {
        let p = compose_edit_prompt("빨간 자전거", "밤 풍경으로 바꿔줘");
        assert!(p.contains("빨간 자전거"));
        assert!(p.contains("밤 풍경으로 바꿔줘"));
        assert_eq!(compose_edit_prompt("", "밤 풍경으로"), "밤 풍경으로");
    }

    #[test]
    fn sniff_and_ext() {
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0x00]), "image/jpeg");
        assert_eq!(
            sniff_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            "image/png"
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_mime(&webp), "image/webp");
        assert_eq!(sniff_mime(&[0, 1, 2, 3]), "image/png");
        assert_eq!(ext_for_mime("image/jpeg"), "jpg");
        assert_eq!(ext_for_mime("image/webp"), "webp");
        assert_eq!(ext_for_mime("image/png"), "png");
        assert_eq!(ext_for_mime("application/octet-stream"), "png");
    }

    #[test]
    fn placeholder_is_valid_png() {
        let png = placeholder_png();
        assert!(!png.is_empty());
        assert_eq!(sniff_mime(&png), "image/png");
    }

    #[test]
    fn default_llm_config_uses_correct_spelling() {
        let c = default_llm_config();
        assert!(c.contains("repetition_penalty"));
        assert!(c.contains("top_k"));
        // chat 의 오타 키가 섞이지 않아야 합니다.
        assert!(!c.contains("repetion_penalty"));
        assert!(!c.contains("tok_k"));
    }
}

/// 실제 HTTP 왕복 검증 — 작은 로컬 서버를 띄워 요청 구성(멀티파트/폼)과 응답 파싱을 확인합니다.
/// (전체 Tauri 앱을 띄우지 않고도 messages-with-models 배선을 검증하기 위함.)
#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::routing::post;
    use axum::Router;

    const PNG_B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    async fn spawn(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn client(base: String) -> ImageClient {
        ImageClient { http: reqwest::Client::new(), base, client_key: "k".into(), token: "t".into() }
    }

    async fn gen_handler(
        State(seen): State<Arc<Mutex<String>>>,
        body: String,
    ) -> axum::Json<serde_json::Value> {
        *seen.lock().unwrap() = body;
        axum::Json(serde_json::json!({
            "status": "SUCCESS",
            "response_code": "R20000",
            "actions": [{ "answer": PNG_B64 }]
        }))
    }

    #[tokio::test]
    async fn generate_sends_two_model_ids_and_decodes_answer() {
        let seen: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let router = Router::new()
            .route(MESSAGES_WITH_MODELS_PATH, post(gen_handler))
            .with_state(seen.clone());
        let base = spawn(router).await;

        let bytes = client(base)
            .generate("text-1", "flux-2", "강아지 그려줘", (64, 48))
            .await
            .unwrap();
        assert_eq!(sniff_mime(&bytes), "image/png");

        let body = seen.lock().unwrap().clone();
        // modelIds = [텍스트, 이미지] 두 개가 반복 키로 나가야 합니다.
        assert!(body.contains("modelIds=text-1"), "body={body}");
        assert!(body.contains("modelIds=flux-2"), "body={body}");
        assert!(body.contains("isStream=False"), "body={body}");
        assert!(body.contains("width") && body.contains("64"), "body={body}");
    }

    async fn understand_handler() -> axum::response::Response {
        axum::response::Response::builder()
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(axum::body::Body::from(
                "data: {\"event_status\":\"CHUNK\",\"content\":\"파란 하늘 \"}\n\n\
                 data: {\"event_status\":\"CHUNK\",\"content\":\"아래 강아지\"}\n\n\
                 data: [DONE]\n\n",
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn understand_accumulates_sse_content() {
        let router = Router::new().route(MESSAGES_WITH_MODELS_PATH, post(understand_handler));
        let base = spawn(router).await;

        let desc = client(base)
            .understand("text-1", "gemma-4", &[0x89, 0x50, 0x4E, 0x47], "image/png", "묘사해줘")
            .await
            .unwrap();
        assert!(desc.contains("파란 하늘"), "desc={desc}");
        assert!(desc.contains("아래 강아지"), "desc={desc}");
    }
}
