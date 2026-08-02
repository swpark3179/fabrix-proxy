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
