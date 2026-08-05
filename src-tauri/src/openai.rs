//! 프록시가 **노출하는** OpenAI 호환 스키마.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// 도구 목록. FabriX 에는 대응 필드가 없어 `proxy::tools` 가 규약으로 접어
    /// `systemPrompt` 에 싣습니다.
    ///
    /// `Vec` + `default` 가 아니라 `Option<Vec<_>>` 인 이유: `default` 는 키가
    /// **없을 때**만 채워 주고 명시적 `"tools": null` 은 여전히 타입 오류라
    /// 요청 전체가 400 이 됩니다. null 을 보내는 클라이언트가 실제로 있습니다.
    #[serde(default)]
    pub tools: Option<Vec<Tool>>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// `tools` 이전 세대의 이름. 같은 것으로 취급합니다.
    #[serde(default)]
    pub functions: Option<Vec<FunctionDef>>,

    // ── 규약에는 있으나 사내 API 에 대응이 없는 필드들 ──
    //
    // 예전에는 이들을 아예 받지 않아 **조용히 사라졌습니다**. 클라이언트는 반영됐다고
    // 믿었고, 로그 ① 칸에도 흔적이 남지 않았습니다. 받아 두는 이유는 두 가지입니다 —
    // (1) 범위 검증을 해서 규약 위반은 400 으로 걸러내고, (2) 반영하지 못하는 것은
    // 로그에 "무시했다"고 적기 위해서입니다. `proxy::validate` 가 그 두 일을 합니다.
    //
    // 타입이 느슨한 것은 의도입니다. `deny_unknown_fields` 를 켜거나 타입을 좁히면
    // SDK 가 필드를 하나 붙일 때마다 요청 전체가 400 이 되어 클라이언트가 다음
    // 릴리스에서 깨집니다.
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub stop: Option<Stop>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    /// 토크나이저가 없어 해석할 수 없습니다 — 로그 표기용.
    #[serde(default)]
    pub logit_bias: Option<Value>,
    /// 스펙은 문자열이지만 객체를 보내는 클라이언트가 있어 `Value` 로 받습니다.
    #[serde(default)]
    pub user: Option<Value>,
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
    /// 구형 클라이언트는 bool 이 아니라 개수(정수)를 보냅니다.
    #[serde(default)]
    pub logprobs: Option<LogProbsFlag>,
    #[serde(default)]
    pub top_logprobs: Option<u32>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub store: Option<Value>,
    #[serde(default)]
    pub service_tier: Option<Value>,
}

/// `stop` — 문자열 하나 또는 최대 4개의 배열. 모르는 모양이 와도 파싱은 성공해야 합니다.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Stop {
    One(String),
    Many(Vec<String>),
    Other(Value),
}

impl Stop {
    /// 실제로 쓸 수 있는 시퀀스들 — 공백만 있는 항목은 버립니다.
    pub fn list(&self) -> Vec<String> {
        match self {
            Stop::One(s) => vec![s.clone()],
            Stop::Many(v) => v.clone(),
            Stop::Other(_) => Vec::new(),
        }
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect()
    }

    /// 규약 상한(4개) 검사용 — 빈 항목을 걸러내기 **전** 개수입니다.
    pub fn raw_len(&self) -> usize {
        match self {
            Stop::One(_) => 1,
            Stop::Many(v) => v.len(),
            Stop::Other(_) => 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseFormat {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub json_schema: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LogProbsFlag {
    Flag(bool),
    Count(u32),
    Other(Value),
}

impl LogProbsFlag {
    pub fn wants(&self) -> bool {
        match self {
            LogProbsFlag::Flag(b) => *b,
            LogProbsFlag::Count(n) => *n > 0,
            LogProbsFlag::Other(_) => false,
        }
    }
}

impl ChatRequest {
    pub fn is_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn max_new_tokens(&self) -> Option<u32> {
        self.max_tokens.or(self.max_completion_tokens)
    }

    /// `tools` 와 구형 `functions` 를 합친 실제 도구 목록. 이름이 빈 항목은 버립니다.
    pub fn declared_tools(&self) -> Vec<&FunctionDef> {
        self.tools
            .iter()
            .flatten()
            .filter_map(|t| t.function.as_ref())
            .chain(self.functions.iter().flatten())
            .filter(|f| !f.name.trim().is_empty())
            .collect()
    }

    pub fn tool_mode(&self) -> ToolChoiceMode {
        self.tool_choice.as_ref().map(ToolChoice::mode).unwrap_or(ToolChoiceMode::Auto)
    }

    /// 이 요청에 도구 에뮬레이션을 걸어야 하는가.
    pub fn wants_tools(&self) -> bool {
        !self.declared_tools().is_empty() && self.tool_mode() != ToolChoiceMode::None
    }

    /// 스트림 꼬리에 usage 청크를 넣어야 하는가. 클라이언트가 명시적으로 옵트인했을
    /// 때만 참입니다 — 규약이 그렇게 정해 두었습니다.
    pub fn wants_usage_chunk(&self) -> bool {
        self.stream_options.as_ref().and_then(|o| o.include_usage) == Some(true)
    }
}

// ── 도구 정의 ──
//
// FabriX 스키마에 도구 필드가 없어 **패스스루가 불가능**합니다. 여기 타입들은
// 받아서 프롬프트 규약으로 접기 위한 것이고, 응답 쪽(`ToolCall`/`ToolCallDelta`)은
// 모델이 뱉은 센티널을 다시 OpenAI 모양으로 조립해 돌려주기 위한 것입니다.

#[derive(Debug, Clone, Deserialize)]
pub struct Tool {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub function: Option<FunctionDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionDef {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema. 프록시는 해석하지 않고 규약에 그대로 실어 보냅니다.
    #[serde(default)]
    pub parameters: Option<Value>,
    /// 신형 SDK 가 보내는 필드 — 받아 두되 쓰지 않습니다.
    #[serde(default)]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "function_kind")]
    pub kind: String,
    pub function: ToolCallFunction,
}

fn function_kind() -> String {
    "function".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ToolCallFunction {
    #[serde(default)]
    pub name: String,
    /// OpenAI 규약상 **JSON 문자열**입니다(객체가 아님). 객체로 보내는 클라이언트가
    /// 있어 양쪽을 받고 언제나 문자열로 정규화합니다.
    #[serde(default, deserialize_with = "de_arguments")]
    pub arguments: String,
}

fn de_arguments<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        Other(Value),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Text(s) => s,
        Raw::Other(Value::Null) => "{}".into(),
        Raw::Other(v) => v.to_string(),
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// `"auto"` · `"none"` · `"required"`
    Mode(String),
    Named {
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        function: Option<NamedFunction>,
    },
    /// 모르는 모양이 와도 요청 전체를 400 내지 않기 위한 포괄 변형.
    Other(Value),
}

#[derive(Debug, Clone, Deserialize)]
pub struct NamedFunction {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoiceMode {
    Auto,
    None,
    Required,
    Function(String),
}

impl ToolChoice {
    pub fn mode(&self) -> ToolChoiceMode {
        match self {
            ToolChoice::Mode(s) => match s.trim().to_ascii_lowercase().as_str() {
                "none" => ToolChoiceMode::None,
                "required" | "any" => ToolChoiceMode::Required,
                _ => ToolChoiceMode::Auto,
            },
            ToolChoice::Named { function: Some(f), .. } if !f.name.trim().is_empty() => {
                ToolChoiceMode::Function(f.name.trim().to_string())
            }
            _ => ToolChoiceMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Message {
    #[serde(default)]
    pub role: String,
    /// 실제 에이전트 요청에서는 **`null` 이 정상**입니다 — 도구 호출만 있는 assistant 턴.
    #[serde(default)]
    pub content: Option<Content>,
    /// 구형 `role:"function"` 이 도구 이름을 여기에 싣습니다.
    #[serde(default)]
    pub name: Option<String>,
    /// `tools` 와 같은 이유로 `Option<Vec<_>>` — 명시적 `null` 을 보내는 클라이언트가 있습니다.
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.tool_calls.as_deref().unwrap_or(&[])
    }

    /// 멀티모달 파트 배열이면 text 파트만 이어붙입니다 (이미지는 FabriX 가
    /// 받지 못하므로 버립니다).
    pub fn text(&self) -> String {
        match &self.content {
            None => String::new(),
            Some(Content::Text(t)) => t.clone(),
            Some(Content::Parts(parts)) => parts
                .iter()
                .filter_map(part_text)
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// 사내가 받지 못해 **버려지는** 이미지 파트의 개수.
    ///
    /// `text()` 가 조용히 걸러내던 것을 세기만 합니다. 프롬프트는 한 글자도 바꾸지
    /// 않습니다 — 자리표시자를 끼워 넣으면 기존 모든 요청의 프롬프트가 달라집니다.
    /// 세는 이유는 로그에 "몇 개를 버렸다"를 적기 위해서입니다.
    pub fn image_parts(&self) -> usize {
        let Some(Content::Parts(parts)) = &self.content else {
            return 0;
        };
        parts.iter().filter(|p| is_image_part(p)).count()
    }

    /// 본문에 쓸 만한 텍스트가 있는가 (`fold_messages` 의 공백 드롭과 같은 기준).
    pub fn has_text(&self) -> bool {
        !self.text().trim().is_empty()
    }

    /// `role:"tool"` 결과 본문.
    ///
    /// AI SDK v5 는 도구 결과를 `[{"type":"tool-result","output":{…}}]` 처럼 보냅니다.
    /// `Content::Parts` 는 untagged 라 이런 배열도 그대로 매치되는데 `text` 파트가
    /// 없어 `text()` 가 빈 문자열을 돌려줍니다. 그대로 두면 `fold_messages` 의 공백
    /// 드롭에 걸려 **도구 결과가 통째로 사라집니다**. 그래서 원문 JSON 으로 폴백합니다.
    pub fn tool_result_text(&self) -> String {
        let text = self.text();
        if !text.trim().is_empty() {
            return text;
        }
        match &self.content {
            Some(Content::Parts(parts)) => parts
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

/// 파트 하나가 이미지인가. 모양이 여럿이라 넓게 봅니다 —
/// OpenAI 는 `{"type":"image_url",…}`, Responses 계열은 `input_image`,
/// AI SDK v5 는 `{"type":"file","mediaType":"image/png"}` 를 씁니다.
fn is_image_part(part: &Value) -> bool {
    let Value::Object(map) = part else {
        return false;
    };
    let kind = map.get("type").and_then(Value::as_str).unwrap_or("");
    if matches!(kind, "image_url" | "input_image" | "image") {
        return true;
    }
    if map.contains_key("image_url") {
        return true;
    }
    map.get("mediaType")
        .or_else(|| map.get("media_type"))
        .and_then(Value::as_str)
        .is_some_and(|m| m.starts_with("image/"))
}

/// 파트 하나에서 텍스트를 꺼냅니다. 문자열 파트(`["a","b"]`)도 받아 줍니다.
fn part_text(part: &Value) -> Option<String> {
    match part {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => map.get("text").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// 파트를 `ContentPart` 구조체가 아니라 **원문 `Value`** 로 들고 있는 이유:
/// 도구 결과 파트처럼 `text` 가 없는 모양을 잃지 않기 위함입니다
/// (`Message::tool_result_text` 참고). 구조체 + `#[serde(flatten)]` 조합은
/// untagged 안에서 미묘하게 깨질 수 있어 피했습니다.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Value>),
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
    /// 도구 호출만 있는 턴에서는 **`null` 이 정상**입니다. 그래서 `skip` 하지 않고
    /// 명시적으로 null 을 내보냅니다 — `content === null` 로 분기하는 클라이언트가 있습니다.
    pub content: Option<String>,
    /// o1 계열 클라이언트가 읽는 필드. FabriX 의 `reasoning_content` 를 실어 보냅니다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// 스트리밍 도구 호출 조각. OpenAI 는 `index` 로 같은 호출의 조각들을 잇습니다.
///
/// 우리는 닫는 태그를 보고 이름 검증까지 끝난 **완성된 호출만** 내보내므로 조각이
/// 언제나 하나입니다. 그래서 첫(=유일한) 조각에 `id` 와 `function.name` 이 모두
/// 들어갑니다 — `@ai-sdk/openai-compatible` 은 해당 index 의 첫 조각에 이 둘이
/// 없으면 `InvalidResponseDataError` 를 던집니다.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

impl ToolCallDelta {
    pub fn whole(index: u32, id: &str, name: &str, arguments: &str) -> Self {
        Self {
            index,
            id: Some(id.to_string()),
            kind: Some("function"),
            function: Some(FunctionDelta {
                name: Some(name.to_string()),
                arguments: Some(arguments.to_string()),
            }),
        }
    }
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

    // ── 도구(tool) 스키마 ──

    #[test]
    fn opencode_shaped_request_parses() {
        // OpenCode(@ai-sdk/openai-compatible)가 두 번째 라운드에 보내는 모양.
        let req: ChatRequest = serde_json::from_str(
            r#"{
              "model": "fabrix-chat-4",
              "stream": true,
              "messages": [
                {"role":"system","content":"you are a coding agent"},
                {"role":"user","content":"make a page"},
                {"role":"assistant","content":null,"tool_calls":[
                  {"id":"call_a1","type":"function",
                   "function":{"name":"write","arguments":"{\"filePath\":\"a.html\"}"}}]},
                {"role":"tool","tool_call_id":"call_a1","content":"wrote 12 bytes"}
              ],
              "tools": [
                {"type":"function","function":{
                  "name":"write","description":"Write a file",
                  "parameters":{"type":"object","properties":{"filePath":{"type":"string"}}}}}
              ],
              "tool_choice": "auto",
              "parallel_tool_calls": true
            }"#,
        )
        .unwrap();

        assert!(req.is_stream());
        assert_eq!(req.declared_tools().len(), 1);
        assert_eq!(req.declared_tools()[0].name, "write");
        assert!(req.declared_tools()[0].parameters.is_some());
        assert_eq!(req.tool_mode(), ToolChoiceMode::Auto);
        assert!(req.wants_tools());
        assert_eq!(req.parallel_tool_calls, Some(true));

        let assistant = &req.messages[2];
        assert_eq!(assistant.tool_calls().len(), 1);
        assert_eq!(assistant.tool_calls()[0].id, "call_a1");
        assert_eq!(assistant.tool_calls()[0].function.name, "write");
        // content 가 null 이라 text() 는 비지만 tool_calls 는 살아 있어야 합니다.
        assert!(assistant.text().is_empty());

        let tool = &req.messages[3];
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_a1"));
        assert_eq!(tool.tool_result_text(), "wrote 12 bytes");
    }

    // ── 규약 필드 수용 ──

    #[test]
    fn stop_accepts_string_array_and_garbage() {
        let of = |raw: &str| serde_json::from_str::<Stop>(raw).unwrap();
        assert_eq!(of(r#""\n\n""#).list(), vec!["\n\n"]);
        assert_eq!(of(r#"["a","b"]"#).list(), vec!["a", "b"]);
        // 빈 항목은 버리지만 상한 검사용 개수에는 남습니다.
        let many = of(r#"["a","","b"]"#);
        assert_eq!(many.list(), vec!["a", "b"]);
        assert_eq!(many.raw_len(), 3);
        // 모르는 모양이 와도 요청 전체가 400 이 되면 안 됩니다.
        assert!(of("{}").list().is_empty());
        assert_eq!(of("{}").raw_len(), 0);
    }

    #[test]
    fn logprobs_accepts_bool_and_count() {
        let of = |raw: &str| serde_json::from_str::<LogProbsFlag>(raw).unwrap();
        assert!(!of("false").wants());
        assert!(of("true").wants());
        assert!(of("3").wants());
        assert!(!of("0").wants());
        assert!(!of(r#""nope""#).wants());
    }

    #[test]
    fn stream_options_and_response_format_parse() {
        let req: ChatRequest = serde_json::from_str(
            r#"{"messages":[],"stream":true,"stream_options":{"include_usage":true},
                "response_format":{"type":"json_schema","json_schema":{"name":"x"}}}"#,
        )
        .unwrap();
        assert!(req.wants_usage_chunk());
        assert_eq!(req.response_format.as_ref().unwrap().kind.as_deref(), Some("json_schema"));
        assert!(req.response_format.as_ref().unwrap().json_schema.is_some());

        // 키가 없으면 usage 청크를 넣지 않습니다.
        let bare: ChatRequest = serde_json::from_str(r#"{"messages":[]}"#).unwrap();
        assert!(!bare.wants_usage_chunk());
        // include_usage: false 도 명시적 거절입니다.
        let off: ChatRequest =
            serde_json::from_str(r#"{"messages":[],"stream_options":{"include_usage":false}}"#)
                .unwrap();
        assert!(!off.wants_usage_chunk());
    }

    #[test]
    fn image_parts_counts_every_shape() {
        let count = |content: &str| -> usize {
            let m: Message =
                serde_json::from_str(&format!(r#"{{"role":"user","content":{content}}}"#)).unwrap();
            m.image_parts()
        };
        assert_eq!(count(r#"[{"type":"image_url","image_url":{"url":"data:…"}}]"#), 1);
        assert_eq!(count(r#"[{"type":"input_image","image_url":"data:…"}]"#), 1);
        assert_eq!(count(r#"[{"type":"file","mediaType":"image/png","data":"AAAA"}]"#), 1);
        assert_eq!(count(r#"[{"type":"file","mediaType":"application/pdf"}]"#), 0);
        assert_eq!(count(r#"[{"type":"text","text":"안녕"}]"#), 0);
        assert_eq!(count(r#""그냥 문자열""#), 0);
        assert_eq!(
            count(r#"[{"type":"text","text":"이거 뭐야"},{"type":"image_url","image_url":{}}]"#),
            1
        );
    }

    #[test]
    fn has_text_follows_the_fold_whitespace_rule() {
        let m = |content: &str| -> Message {
            serde_json::from_str(&format!(r#"{{"role":"user","content":{content}}}"#)).unwrap()
        };
        assert!(m(r#""안녕""#).has_text());
        assert!(!m(r#""   ""#).has_text());
        assert!(!m(r#"null"#).has_text());
        assert!(!m(r#"[{"type":"image_url","image_url":{}}]"#).has_text());
        assert!(m(r#"[{"type":"text","text":"안녕"},{"type":"image_url","image_url":{}}]"#).has_text());
    }

    /// 새 필드를 얹어도 실제 에이전트 요청이 그대로 파싱돼야 합니다.
    #[test]
    fn new_fields_do_not_break_a_real_request() {
        let req: ChatRequest = serde_json::from_str(
            r#"{"model":"fabrix-chat-4","stream":true,
                "messages":[{"role":"user","content":"hi"}],
                "n":1,"stop":null,"stream_options":null,"user":"kim","logit_bias":null,
                "presence_penalty":0.0,"logprobs":false,"metadata":{"a":1},"store":false,
                "service_tier":"auto","무슨키":"몰라도 통과"}"#,
        )
        .unwrap();
        assert_eq!(req.n, Some(1));
        assert!(req.stop.is_none());
        assert!(!req.wants_usage_chunk());
        assert_eq!(req.user.as_ref().unwrap().as_str(), Some("kim"));
        assert!(!req.logprobs.as_ref().unwrap().wants());
    }

    #[test]
    fn explicit_nulls_do_not_break_parsing() {
        // `Vec` + `#[serde(default)]` 였다면 여기서 요청 전체가 400 이 됩니다.
        let req: ChatRequest = serde_json::from_str(
            r#"{"model":"m","tools":null,"tool_choice":null,
                "messages":[{"role":"user","content":"hi","tool_calls":null}]}"#,
        )
        .unwrap();
        assert!(req.declared_tools().is_empty());
        assert!(!req.wants_tools());
        assert!(req.messages[0].tool_calls().is_empty());
    }

    #[test]
    fn legacy_functions_are_treated_as_tools() {
        let req: ChatRequest = serde_json::from_str(
            r#"{"functions":[{"name":"calc","description":"do math"}],"messages":[]}"#,
        )
        .unwrap();
        assert_eq!(req.declared_tools().len(), 1);
        assert_eq!(req.declared_tools()[0].name, "calc");
        assert!(req.wants_tools());
    }

    #[test]
    fn nameless_tools_are_ignored() {
        let req: ChatRequest =
            serde_json::from_str(r#"{"tools":[{"type":"function","function":{"name":"  "}}]}"#)
                .unwrap();
        assert!(req.declared_tools().is_empty());
        assert!(!req.wants_tools());
    }

    #[test]
    fn arguments_accepts_string_and_object() {
        let s: ToolCall = serde_json::from_str(
            r#"{"id":"c1","type":"function","function":{"name":"w","arguments":"{\"a\":1}"}}"#,
        )
        .unwrap();
        let o: ToolCall = serde_json::from_str(
            r#"{"id":"c1","type":"function","function":{"name":"w","arguments":{"a":1}}}"#,
        )
        .unwrap();
        assert_eq!(s.function.arguments, r#"{"a":1}"#);
        assert_eq!(o.function.arguments, r#"{"a":1}"#);

        let n: ToolCall = serde_json::from_str(
            r#"{"id":"c1","function":{"name":"w","arguments":null}}"#,
        )
        .unwrap();
        assert_eq!(n.function.arguments, "{}");
        // `type` 이 없으면 "function" 으로 채웁니다.
        assert_eq!(n.kind, "function");
    }

    #[test]
    fn tool_choice_untagged_covers_every_shape() {
        let mode = |raw: &str| {
            serde_json::from_str::<ToolChoice>(raw).unwrap().mode()
        };
        assert_eq!(mode(r#""auto""#), ToolChoiceMode::Auto);
        assert_eq!(mode(r#""none""#), ToolChoiceMode::None);
        assert_eq!(mode(r#""required""#), ToolChoiceMode::Required);
        assert_eq!(mode(r#""ANY""#), ToolChoiceMode::Required);
        assert_eq!(
            mode(r#"{"type":"function","function":{"name":"write"}}"#),
            ToolChoiceMode::Function("write".into())
        );
        // 모르는 모양이 와도 파싱은 성공하고 auto 로 떨어져야 합니다.
        assert_eq!(mode("12345"), ToolChoiceMode::Auto);
    }

    #[test]
    fn tool_choice_none_disables_emulation() {
        let req: ChatRequest = serde_json::from_str(
            r#"{"tools":[{"type":"function","function":{"name":"write"}}],"tool_choice":"none"}"#,
        )
        .unwrap();
        assert_eq!(req.declared_tools().len(), 1);
        assert!(!req.wants_tools());
    }

    #[test]
    fn ai_sdk_tool_result_parts_fall_back_to_raw_json() {
        // text 파트가 없어 text() 는 비지만, 원문은 살아 있어야 합니다.
        let m: Message = serde_json::from_str(
            r#"{"role":"tool","tool_call_id":"c1",
                "content":[{"type":"tool-result","output":{"ok":true}}]}"#,
        )
        .unwrap();
        assert!(m.text().is_empty());
        let body = m.tool_result_text();
        assert!(body.contains("tool-result"), "got {body}");
        assert!(body.contains("\"ok\":true"), "got {body}");
    }

    #[test]
    fn text_parts_still_win_over_raw_json() {
        let m: Message = serde_json::from_str(
            r#"{"role":"user","content":[{"type":"text","text":"안녕"},
                                        {"type":"image_url","image_url":{"url":"data:…"}}]}"#,
        )
        .unwrap();
        // 이미지 파트는 그대로 버립니다 (기존 동작 유지).
        assert_eq!(m.text(), "안녕");
        assert_eq!(m.tool_result_text(), "안녕");
    }

    #[test]
    fn assistant_message_serializes_null_content_with_tool_calls() {
        let msg = AssistantMessage {
            role: "assistant",
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: ToolCallFunction { name: "write".into(), arguments: "{}".into() },
            }]),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""content":null"#), "got {json}");
        assert!(json.contains(r#""tool_calls""#), "got {json}");
        assert!(!json.contains("reasoning_content"));
    }

    #[test]
    fn tool_call_delta_matches_the_openai_wire_shape() {
        let d = ToolCallDelta::whole(0, "call_x", "read", r#"{"filePath":"a"}"#);
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(
            json,
            r#"{"index":0,"id":"call_x","type":"function","function":{"name":"read","arguments":"{\"filePath\":\"a\"}"}}"#
        );
    }

    #[test]
    fn delta_without_tool_calls_omits_the_key() {
        let json = serde_json::to_string(&Delta {
            content: Some("hi".into()),
            ..Delta::default()
        })
        .unwrap();
        assert_eq!(json, r#"{"content":"hi"}"#);
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
