//! 프록시가 **노출하는** OpenAI 호환 스키마.

use serde::{Deserialize, Serialize};

// ─────────────────────────────── 요청 ───────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 신형 SDK 가 `max_tokens` 대신 보내는 이름.
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    /// OpenAI 표준에는 없지만 여러 호환 클라이언트가 보냅니다.
    #[serde(default)]
    pub top_k: Option<u32>,
}

impl ChatRequest {
    pub fn is_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn max_new_tokens(&self) -> Option<u32> {
        self.max_tokens.or(self.max_completion_tokens)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: Option<Content>,
}

impl Message {
    /// 멀티모달 파트 배열이면 text 파트만 이어붙입니다 (이미지는 FabriX 가
    /// 받지 못하므로 버립니다).
    pub fn text(&self) -> String {
        match &self.content {
            None => String::new(),
            Some(Content::Text(t)) => t.clone(),
            Some(Content::Parts(parts)) => parts
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContentPart {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

// ─────────────────────────────── 응답 ───────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ModelList {
    pub object: &'static str,
    pub data: Vec<ModelCard>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCard {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub owned_by: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletion {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssistantMessage {
    pub role: &'static str,
    pub content: String,
    /// o1 계열 클라이언트가 읽는 필드. FabriX 의 `reasoning_content` 를 실어 보냅니다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ── 스트리밍 청크 ──

#[derive(Debug, Clone, Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: &'static str,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatChunk {
    pub fn new(id: &str, created: i64, model: &str, delta: Delta, finish: Option<String>) -> Self {
        Self {
            id: id.to_string(),
            object: "chat.completion.chunk",
            created,
            model: model.to_string(),
            choices: vec![ChunkChoice { index: 0, delta, finish_reason: finish }],
        }
    }
}

// ── 오류 봉투 ──

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub message: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ErrorEnvelope {
    pub fn new(message: impl Into<String>, kind: &'static str, code: Option<String>) -> Self {
        Self { error: ErrorBody { message: message.into(), kind, code } }
    }
}

// ─────────────────────────────── 이미지 ───────────────────────────────
//
// OpenAI Images API 호환. 소비자(Open Design 등)는 `/v1/images/generations`(t2i) 와
// `/v1/images/edits`(i2i) 를 호출합니다. edits 는 표준 multipart 가 아니라 **application/json**
// 으로, 참조 이미지를 `images[].image_url` 의 base64 data URL 로 보냅니다.

#[derive(Debug, Clone, Deserialize)]
pub struct ImageGenerationRequest {
    #[serde(default)]
    pub prompt: Option<String>,
    /// 로깅용. 실제 사용 모델은 설정(config)의 고정값입니다.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub n: Option<u32>,
    /// `"1024x1024"` 등. `"auto"`/빈값이면 기본 해상도.
    #[serde(default)]
    pub size: Option<String>,
    /// 수용하되 무시합니다 — 항상 `b64_json` 으로 돌려줍니다.
    #[serde(default)]
    pub response_format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageEditRequest {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub response_format: Option<String>,
    /// 참조 이미지. 스펙상 data URL 배열이지만 **첫 장만** 사용합니다.
    #[serde(default)]
    pub images: Vec<ImageRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageRef {
    #[serde(default)]
    pub image_url: Option<ImageUrl>,
}

/// 스펙 예시는 `"data:..."` 문자열이지만, `{"url":"data:..."}` 객체형도 함께 받습니다
/// (`Content` 의 untagged 수용과 같은 방어적 처리).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ImageUrl {
    Url(String),
    Object {
        #[serde(default)]
        url: Option<String>,
    },
}

impl ImageUrl {
    /// 안에 든 data URL 문자열을 꺼냅니다.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ImageUrl::Url(s) => Some(s.as_str()),
            ImageUrl::Object { url } => url.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImagesResponse {
    pub created: i64,
    pub data: Vec<ImageDatum>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ImageDatum {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b64_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
}

/// `"WxH"` → `(W, H)`. `"auto"`/빈값/형식오류 → `None` (호출부가 기본 해상도를 정합니다).
/// 구분자는 `x` · `X` · `×` 를 모두 허용합니다.
pub fn parse_size(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("auto") {
        return None;
    }
    let (w, h) = s
        .split_once('x')
        .or_else(|| s.split_once('X'))
        .or_else(|| s.split_once('×'))?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// `"data:image/png;base64,AAAA"` → `Some(("image/png", "AAAA"))`.
///
/// data URL 이 아니거나 base64 인코딩이 아니면 `None` 입니다 (퍼센트 인코딩 등은 미지원).
/// 순수 문자열 분해만 하며, base64 디코딩은 `proxy::b64` 가 맡습니다.
pub fn split_data_url(s: &str) -> Option<(&str, &str)> {
    let rest = s.trim().strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let (meta, payload) = rest.split_at(comma);
    let payload = &payload[1..]; // 앞의 ',' 제거
    if !meta.to_ascii_lowercase().contains("base64") {
        return None;
    }
    let mime = meta.split(';').next().unwrap_or("");
    let mime = if mime.is_empty() { "application/octet-stream" } else { mime };
    Some((mime, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_basic() {
        assert_eq!(parse_size("1024x1024"), Some((1024, 1024)));
        assert_eq!(parse_size("1792x1024"), Some((1792, 1024)));
        assert_eq!(parse_size(" 512 x 512 "), Some((512, 512)));
        assert_eq!(parse_size("768X768"), Some((768, 768)));
        assert_eq!(parse_size("1024×1024"), Some((1024, 1024)));
    }

    #[test]
    fn parse_size_rejects() {
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("auto"), None);
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("1024"), None);
        assert_eq!(parse_size("0x100"), None);
    }

    #[test]
    fn split_data_url_ok() {
        assert_eq!(split_data_url("data:image/png;base64,AAAA"), Some(("image/png", "AAAA")));
        assert_eq!(split_data_url("data:image/jpeg;base64,/9j/"), Some(("image/jpeg", "/9j/")));
        // mime 생략된 형태.
        assert_eq!(split_data_url("data:;base64,AAAA"), Some(("application/octet-stream", "AAAA")));
    }

    #[test]
    fn split_data_url_rejects_non_base64_and_non_data() {
        assert_eq!(split_data_url("https://x/y.png"), None);
        assert_eq!(split_data_url("data:text/plain,hello"), None); // base64 아님
    }

    #[test]
    fn image_url_untagged_accepts_string_and_object() {
        let s: ImageRef = serde_json::from_str(r#"{"image_url":"data:image/png;base64,AAAA"}"#).unwrap();
        assert_eq!(s.image_url.unwrap().as_str(), Some("data:image/png;base64,AAAA"));
        let o: ImageRef = serde_json::from_str(r#"{"image_url":{"url":"data:image/png;base64,BBBB"}}"#).unwrap();
        assert_eq!(o.image_url.unwrap().as_str(), Some("data:image/png;base64,BBBB"));
    }

    #[test]
    fn generation_request_ignores_unknown_fields() {
        let r: ImageGenerationRequest = serde_json::from_str(
            r#"{"prompt":"hi","model":"flux-2","n":1,"size":"1024x1024","quality":"hd","foo":1}"#,
        )
        .unwrap();
        assert_eq!(r.prompt.as_deref(), Some("hi"));
        assert_eq!(r.size.as_deref(), Some("1024x1024"));
    }

    #[test]
    fn images_response_serializes_only_b64() {
        let resp = ImagesResponse {
            created: 1,
            data: vec![ImageDatum { b64_json: Some("QQ==".into()), ..Default::default() }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""b64_json":"QQ==""#));
        assert!(!json.contains("\"url\""));
        assert!(!json.contains("revised_prompt"));
    }
}
