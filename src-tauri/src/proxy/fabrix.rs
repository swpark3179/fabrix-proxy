//! FabriX 스키마와 그 위의 얇은 HTTP 클라이언트.
//!
//! 스펙 문서에 SSE 프레임 형식이 없어서 파싱은 전부 방어적으로 갑니다 —
//! camelCase/snake_case 양쪽, `data:` 접두 유무 양쪽, 누적/증분 양쪽.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openai::ChatRequest;

pub const MODELS_PATH: &str = "/openapi/chat/v1/models";
pub const MESSAGES_PATH: &str = "/openapi/chat/v1/messages";

/// 목업의 502 행이 30s 인 것과 맞춥니다. 스트리밍에는 적용하지 않습니다
/// (긴 응답이 정상적으로 30초를 넘길 수 있으므로 청크 간 read_timeout 으로 대신).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(90);

// ─────────────────────────── 모델 목록 ───────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct LocalizedText {
    #[serde(default, alias = "languageCode")]
    pub language_code: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FabrixModel {
    #[serde(alias = "modelId", alias = "id")]
    pub model_id: String,
    #[serde(default)]
    pub name: Vec<LocalizedText>,
    #[serde(default)]
    pub description: Vec<LocalizedText>,
}

/// alias 를 부여한 모델. `/v1/models` 응답과 모델 해석에 모두 쓰입니다.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedModel {
    /// 클라이언트에 노출하는 이름 (예: `fabrix-chat-4`).
    pub alias: String,
    /// FabriX 가 기대하는 UUID.
    pub model_id: String,
    /// 사람이 읽는 이름 (예: `챗 4`).
    pub label: String,
    pub description: Option<String>,
}

fn pick(list: &[LocalizedText], lang: &str) -> Option<String> {
    list.iter()
        .find(|t| t.language_code.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(lang)))
        .and_then(|t| t.content.clone())
        .filter(|s| !s.trim().is_empty())
}

fn first_content(list: &[LocalizedText]) -> Option<String> {
    list.iter().find_map(|t| t.content.clone()).filter(|s| !s.trim().is_empty())
}

/// ASCII 슬러그. 한글만 있는 이름은 빈 문자열이 되어 UUID 기반 alias 로 넘어갑니다.
fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true; // 선행 '-' 방지
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(40).collect()
}

/// 목업의 `fabrix-chat-4` / `fabrix-chat-lite` 형태를 만듭니다.
///
/// 이름이 한글뿐이면 슬러그가 비므로 UUID 앞 8자리로 대체합니다 — 클라이언트가
/// 모델 이름을 하드코딩해도 서버 순서에 흔들리지 않게 하기 위함입니다.
pub fn build_aliases(models: &[FabrixModel]) -> Vec<ResolvedModel> {
    let mut used: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(models.len());

    for m in models {
        let en = pick(&m.name, "en");
        let ko = pick(&m.name, "ko");
        let any = first_content(&m.name);

        let label = ko.clone().or_else(|| en.clone()).or_else(|| any.clone()).unwrap_or_else(|| m.model_id.clone());

        let slug_src = en.or(any).unwrap_or_default();
        let mut slug = slugify(&slug_src);
        if slug.is_empty() {
            slug = m.model_id.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        }
        if slug.is_empty() {
            slug = "model".into();
        }

        let mut alias = format!("fabrix-{slug}");
        let mut n = 2;
        while used.contains(&alias) {
            alias = format!("fabrix-{slug}-{n}");
            n += 1;
        }
        used.insert(alias.clone());

        out.push(ResolvedModel {
            alias,
            model_id: m.model_id.clone(),
            label,
            description: pick(&m.description, "ko").or_else(|| first_content(&m.description)),
        });
    }
    out
}

/// UUID 직매치 → alias 완전일치 → 대소문자 무시 → 기본 모델 폴백.
pub fn resolve_model<'a>(
    models: &'a [ResolvedModel],
    requested: Option<&str>,
    default_alias: &str,
) -> Option<&'a ResolvedModel> {
    if let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(hit) = models.iter().find(|m| m.model_id == req) {
            return Some(hit);
        }
        if let Some(hit) = models.iter().find(|m| m.alias == req) {
            return Some(hit);
        }
        if let Some(hit) = models.iter().find(|m| m.alias.eq_ignore_ascii_case(req)) {
            return Some(hit);
        }
    }
    models
        .iter()
        .find(|m| m.alias == default_alias)
        .or_else(|| models.first())
}

// ─────────────────────────── 요청 번역 ───────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesRequest {
    pub model_ids: Vec<String>,
    pub contents: Vec<String>,
    pub is_stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_config: Option<LlmConfig>,
}

/// ⚠️ `repetion_penalty` 와 `tok_k` 는 **스펙 문서의 철자 그대로**입니다.
/// 오타로 보이지만 서버가 기대하는 키일 가능성이 높아 그대로 보냅니다.
/// 실서버 검증에서 다르면 이 두 `rename` 만 고치면 됩니다.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(rename = "repetion_penalty", skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    #[serde(rename = "tok_k", skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_new_tokens: Option<u32>,
}

impl LlmConfig {
    pub fn from_request(req: &ChatRequest) -> Option<Self> {
        let cfg = Self {
            temperature: req.temperature,
            top_p: req.top_p,
            repetition_penalty: req.frequency_penalty,
            top_k: req.top_k,
            seed: req.seed,
            max_new_tokens: req.max_new_tokens(),
        };
        let empty = cfg.temperature.is_none()
            && cfg.top_p.is_none()
            && cfg.repetition_penalty.is_none()
            && cfg.top_k.is_none()
            && cfg.seed.is_none()
            && cfg.max_new_tokens.is_none();
        if empty {
            None
        } else {
            Some(cfg)
        }
    }
}

/// OpenAI `messages` → FabriX `systemPrompt` + `contents`.
///
/// FabriX 에는 롤 구조가 없어 멀티턴은 한 덩어리 트랜스크립트로 평탄화됩니다.
/// 손실적이지만 대안이 없고, 로그 ② 칸에 변환 결과가 그대로 보이므로 사용자가
/// 무엇이 어떻게 접혔는지 확인할 수 있습니다.
///
/// 도구를 쓰는 대화에서 조심할 것이 둘 있습니다.
///
/// 1. 도구 호출만 있는 assistant 턴은 `content` 가 `null` 이라 본문이 빕니다. 공백
///    이라고 버리면 모델은 다음 턴에 **자기가 호출한 적 없는 결과**를 보게 되고,
///    같은 도구를 다시 부르는 루프에 빠집니다. 그래서 공백 판정을 역할별 분기
///    안으로 내렸습니다.
/// 2. 라벨을 `Option` 으로 둔 이유는 도구 결과 줄이 자기 머리말을 이미 갖고 있어서
///    입니다. 그대로 롤 라벨을 붙이면 `Tool: Tool result (id=…)` 가 됩니다.
pub fn fold_messages(messages: &[crate::openai::Message]) -> (Option<String>, Vec<String>) {
    let mut system: Vec<String> = Vec::new();
    // 라벨이 `None` 이면 본문에 이미 머리말이 들어 있다는 뜻입니다.
    let mut turns: Vec<(Option<&'static str>, String)> = Vec::new();

    // 병렬 호출을 상관시키려면 결과 줄에 함수 이름이 필요한데, `role:"tool"` 메시지는
    // `tool_call_id` 만 갖고 이름은 그 호출을 낸 assistant 턴에 있습니다.
    let mut call_names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for m in messages {
        for c in m.tool_calls() {
            if !c.id.is_empty() {
                call_names.insert(c.id.as_str(), c.function.name.as_str());
            }
        }
    }

    for m in messages {
        match m.role.as_str() {
            "system" | "developer" => {
                let text = m.text();
                if !text.trim().is_empty() {
                    system.push(text);
                }
            }
            "assistant" => {
                let mut parts: Vec<String> = Vec::new();
                let text = m.text();
                if !text.trim().is_empty() {
                    parts.push(text);
                }
                for c in m.tool_calls() {
                    parts.push(super::tools::render_history_call(c));
                }
                if !parts.is_empty() {
                    turns.push((Some("Assistant"), parts.join("\n")));
                }
            }
            "tool" | "function" => {
                let body = m.tool_result_text();
                let name = m.name.as_deref().or_else(|| {
                    m.tool_call_id.as_deref().and_then(|id| call_names.get(id).copied())
                });
                let head = match (m.tool_call_id.as_deref(), name) {
                    (Some(id), Some(n)) => format!("Tool result (id={id}, name={n})"),
                    (Some(id), None) => format!("Tool result (id={id})"),
                    (None, Some(n)) => format!("Tool result (name={n})"),
                    (None, None) => "Tool result".to_string(),
                };
                // 본문이 비어도 남깁니다 — "불렀고 결과가 비었다"도 정보입니다.
                turns.push((None, format!("{head}:\n{body}")));
            }
            role => {
                let text = m.text();
                if !text.trim().is_empty() {
                    // 모르는 롤은 본문에 머리말을 직접 넣고 라벨은 비웁니다.
                    match role {
                        "user" => turns.push((Some("User"), text)),
                        other => turns.push((None, format!("{other}: {text}"))),
                    }
                }
            }
        }
    }

    let system_prompt = if system.is_empty() { None } else { Some(system.join("\n\n")) };

    let content = match turns.len() {
        0 => String::new(),
        1 if turns[0].0 == Some("User") => turns.remove(0).1,
        _ => turns
            .iter()
            .map(|(label, text)| match label {
                Some(l) => format!("{l}: {text}"),
                None => text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    };

    if content.is_empty() {
        (system_prompt, Vec::new())
    } else {
        (system_prompt, vec![content])
    }
}

// ─────────────────────────── 응답 파싱 ───────────────────────────

/// FabriX 응답 한 조각. 필드명은 스트리밍이면 snake_case, 아니면 camelCase 로
/// 온다고 문서에 적혀 있어 양쪽을 모두 받습니다.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FabrixChunk {
    #[serde(default, alias = "modelType")]
    pub model_type: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(default, alias = "finishReason")]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default, alias = "responseCode")]
    pub response_code: Option<String>,
    #[serde(default, alias = "eventStatus")]
    pub event_status: Option<String>,
    #[serde(default, alias = "eventData")]
    pub event_data: Option<String>,
    #[serde(default, alias = "reasoningContent")]
    pub reasoning_content: Option<String>,
    /// 플러그인/RAG 가 답한 경우 답변이 여기에 담깁니다(비스트림 응답).
    #[serde(default, alias = "contentReferences")]
    pub content_references: Vec<ContentReference>,
    /// 필터에 걸린 경우 차단 사유가 담깁니다.
    #[serde(default, alias = "filterBlockReason")]
    pub filter_block_reason: Option<FilterBlockReason>,
    /// 오류 응답에서 흔히 쓰이는 필드들 — 메시지 추출용.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, alias = "errorMessage")]
    pub error_message: Option<String>,
}

/// 플러그인/RAG 답변 한 건. 답변 텍스트만 쓰고 나머지(references 등)는 무시합니다.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContentReference {
    #[serde(default)]
    pub answer: Option<String>,
}

/// 필터 차단 사유. 사용자에게 노출할 메시지 추출용.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FilterBlockReason {
    #[serde(default)]
    pub ko: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, alias = "resultCode")]
    pub result_code: Option<String>,
}

impl FabrixChunk {
    pub fn looks_like_error(&self) -> bool {
        let bad = |s: &Option<String>| {
            s.as_deref().is_some_and(|v| {
                let v = v.to_ascii_uppercase();
                v.contains("ERROR") || v.contains("FAIL")
            })
        };
        bad(&self.status) || bad(&self.event_status)
    }

    pub fn error_text(&self) -> String {
        self.error_message
            .clone()
            .or_else(|| self.message.clone())
            .or_else(|| self.event_data.clone())
            .or_else(|| self.content.clone())
            .unwrap_or_else(|| "사내 서버가 오류를 반환했습니다".into())
    }

    /// 비스트리밍 답변 텍스트를 폴백 순서로 추출합니다.
    /// content → contentReferences[].answer(결합) → eventData.
    /// 순수 LLM 답변은 content 에, 플러그인/RAG 답변은 contentReferences 에 오기 때문입니다.
    pub fn answer_text(&self) -> Option<String> {
        if let Some(text) = self.content.as_deref().filter(|s| !s.is_empty()) {
            return Some(text.to_string());
        }
        let joined = self
            .content_references
            .iter()
            .filter_map(|r| r.answer.as_deref())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !joined.is_empty() {
            return Some(joined);
        }
        self.event_data
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    /// 필터 차단 사유 메시지를 추출합니다(모두 비어 있으면 None).
    pub fn filter_message(&self) -> Option<String> {
        let reason = self.filter_block_reason.as_ref()?;
        reason
            .message
            .as_deref()
            .or(reason.ko.as_deref())
            .or(reason.en.as_deref())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }
}

/// FabriX 의 종료 사유를 OpenAI `finish_reason` 으로 옮깁니다.
///
/// 절단이 최우선입니다. 예전에는 `truncated` 를 파싱만 하고 아무 데도 쓰지 않아,
/// 상한에 걸려 잘린 답변이 클라이언트에는 깔끔한 `"stop"` 으로 보였습니다 —
/// 도구 호출 인자 한가운데서 잘려도 마찬가지라, 파일이 반쪽만 써지고도 성공으로
/// 보입니다.
pub fn map_finish_reason(raw: Option<&str>, truncated: bool) -> Option<String> {
    if truncated {
        return Some("length".into());
    }
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        match raw.to_ascii_lowercase().as_str() {
            "length" | "max_tokens" | "max_new_tokens" | "truncated" => "length",
            "content_filter" | "filtered" | "blocked" => "content_filter",
            "stop" | "end_turn" | "eos" | "complete" | "completed" => "stop",
            // 모르는 값은 지어내지 않고 그대로 넘깁니다.
            _ => return Some(raw.to_string()),
        }
        .to_string(),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Delta(String),
    Reasoning(String),
    /// 누적 모드에서 상위가 답변을 **통째로 다시 쓴** 지점.
    ///
    /// 이때 `Delta` 로 전체 본문이 다시 흘러나오므로, 텍스트를 누적해 파싱하는
    /// 소비자(도구 호출 스캐너)는 상태를 버려야 합니다. 그러지 않으면 이미
    /// 내보낸 도구 호출을 두 번 냅니다.
    Reset,
    Finish(String),
    Error(String),
    Done,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DeltaMode {
    #[default]
    Unknown,
    /// 매 프레임이 지금까지의 **전체** 텍스트.
    Cumulative,
    /// 매 프레임이 **증분**.
    Incremental,
}

/// 바이트 스트림 → OpenAI 델타.
///
/// 줄 단위로만 잘라 쓰기 때문에 청크 경계에서 UTF-8 문자가 쪼개져도 안전합니다
/// (개행은 항상 단일 바이트라 멀티바이트 문자 중간에 걸리지 않습니다).
#[derive(Debug, Default)]
pub struct StreamDecoder {
    buf: Vec<u8>,
    acc: String,
    reasoning: String,
    mode: DeltaMode,
    pub finish_reason: Option<String>,
    pub model_type: Option<String>,
    pub done: bool,
    /// 상위가 답변을 잘랐다고 알려 왔는지. `finish_reason: "length"` 로 옮깁니다.
    ///
    /// 플래그로 들고 있는 이유: 여기서 `Finish` 이벤트를 내면 뒤따라 오는 진짜
    /// 종료 프레임이 값을 덮어씁니다. 최종 판단은 소비자가 마지막에 합니다.
    pub truncated: bool,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 지금까지 합쳐진 전체 답변.
    pub fn text(&self) -> &str {
        &self.acc
    }

    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<StreamEvent> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            self.handle_line(&line, &mut events);
        }
        events
    }

    /// 스트림이 끝났을 때 개행 없이 남은 마지막 조각을 처리합니다.
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            self.handle_line(&line, &mut events);
        }
        events
    }

    fn handle_line(&mut self, raw: &str, out: &mut Vec<StreamEvent>) {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with(':') {
            return; // 프레임 구분자 또는 SSE 주석
        }

        let payload = if let Some(rest) = line.strip_prefix("data:") {
            rest.trim()
        } else if line.starts_with("event:") || line.starts_with("id:") || line.starts_with("retry:") {
            return;
        } else {
            // `data:` 없이 개행 구분 JSON 을 흘리는 서버도 받아들입니다.
            line
        };

        if payload.is_empty() {
            return;
        }
        if payload == "[DONE]" {
            self.done = true;
            out.push(StreamEvent::Done);
            return;
        }

        match serde_json::from_str::<Value>(payload) {
            Ok(Value::Array(items)) => {
                for item in items {
                    self.handle_value(item, out);
                }
            }
            Ok(value) => self.handle_value(value, out),
            // JSON 이 아닌 줄은 무시합니다 (프록시 배너, 빈 keep-alive 등).
            Err(_) => {}
        }
    }

    fn handle_value(&mut self, value: Value, out: &mut Vec<StreamEvent>) {
        let Ok(chunk) = serde_json::from_value::<FabrixChunk>(value) else {
            return;
        };

        if chunk.looks_like_error() {
            out.push(StreamEvent::Error(chunk.error_text()));
            return;
        }
        if let Some(mt) = chunk.model_type.clone() {
            self.model_type.get_or_insert(mt);
        }
        if chunk.truncated == Some(true) {
            self.truncated = true;
        }
        if let Some(reasoning) = chunk.reasoning_content.as_deref() {
            if !reasoning.is_empty() {
                self.reasoning.push_str(reasoning);
                out.push(StreamEvent::Reasoning(reasoning.to_string()));
            }
        }
        if let Some(content) = chunk.content.as_deref() {
            match self.absorb(content) {
                Absorbed::Append(delta) => out.push(StreamEvent::Delta(delta)),
                Absorbed::Rewrite(whole) => {
                    out.push(StreamEvent::Reset);
                    out.push(StreamEvent::Delta(whole));
                }
                Absorbed::Nothing => {}
            }
        }
        if let Some(reason) = chunk.finish_reason.as_deref().filter(|r| !r.is_empty()) {
            self.finish_reason = Some(reason.to_string());
            out.push(StreamEvent::Finish(reason.to_string()));
        }
    }

    /// 누적/증분 판별. 두 번째 프레임에서 모드를 확정하고 이후로는 고정합니다 —
    /// 매 프레임 접두사 검사를 하면 "안" 다음에 증분 "안녕"이 왔을 때 오판합니다.
    fn absorb(&mut self, content: &str) -> Absorbed {
        if content.is_empty() {
            return Absorbed::Nothing;
        }
        if self.acc.is_empty() {
            self.acc.push_str(content);
            return Absorbed::Append(content.to_string());
        }

        match self.mode {
            DeltaMode::Unknown => {
                if content == self.acc {
                    self.mode = DeltaMode::Cumulative;
                    Absorbed::Nothing
                } else if content.len() > self.acc.len() && content.starts_with(self.acc.as_str()) {
                    self.mode = DeltaMode::Cumulative;
                    let delta = content[self.acc.len()..].to_string();
                    self.acc = content.to_string();
                    Absorbed::Append(delta)
                } else {
                    self.mode = DeltaMode::Incremental;
                    self.acc.push_str(content);
                    Absorbed::Append(content.to_string())
                }
            }
            DeltaMode::Cumulative => {
                if content == self.acc {
                    Absorbed::Nothing
                } else if content.len() > self.acc.len() && content.starts_with(self.acc.as_str()) {
                    let delta = content[self.acc.len()..].to_string();
                    self.acc = content.to_string();
                    Absorbed::Append(delta)
                } else {
                    // 서버가 답변을 다시 쓴 경우 — 통째로 교체합니다. 이건 증분이
                    // 아니라 재작성이므로 소비자에게 그 사실을 알려야 합니다.
                    self.acc = content.to_string();
                    Absorbed::Rewrite(content.to_string())
                }
            }
            DeltaMode::Incremental => {
                self.acc.push_str(content);
                Absorbed::Append(content.to_string())
            }
        }
    }
}

/// `absorb` 의 결과. 추가인지 재작성인지 구분해야 소비자가 상태를 언제 버릴지 압니다.
enum Absorbed {
    Nothing,
    Append(String),
    Rewrite(String),
}

// ─────────────────────────── 오류 ───────────────────────────

#[derive(Debug, Clone)]
pub enum FabrixError {
    NotConfigured,
    /// 연결 실패 · 타임아웃 → 502 `사내 응답 없음`
    Unreachable(String),
    /// FabriX 429 → `사내 쿼터 초과`
    Quota(String),
    Upstream { status: u16, message: String },
    BadPayload(String),
}

impl FabrixError {
    pub fn status(&self) -> u16 {
        match self {
            FabrixError::NotConfigured => 503,
            FabrixError::Unreachable(_) => 502,
            FabrixError::Quota(_) => 429,
            FabrixError::Upstream { status, .. } => *status,
            FabrixError::BadPayload(_) => 502,
        }
    }

    /// 로그 목록 두 번째 줄에 뜨는 짧은 설명 — 목업 문구와 일치시킵니다.
    pub fn note(&self) -> String {
        match self {
            FabrixError::NotConfigured => "사내 연결 설정이 필요합니다".into(),
            FabrixError::Unreachable(_) => "사내 응답 없음".into(),
            FabrixError::Quota(_) => "사내 쿼터 초과".into(),
            FabrixError::Upstream { status, .. } => format!("사내 오류 {status}"),
            FabrixError::BadPayload(_) => "응답을 해석하지 못했습니다".into(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            FabrixError::NotConfigured => {
                "사내 연결 정보가 설정되지 않았습니다. 트레이 메뉴 → 창 열기에서 설정하세요.".into()
            }
            FabrixError::Unreachable(detail) => format!("사내 AI 서버에 연결하지 못했습니다: {detail}"),
            FabrixError::Quota(detail) => format!("사내 쿼터를 초과했습니다: {detail}"),
            FabrixError::Upstream { status, message } => format!("사내 서버 오류 {status}: {message}"),
            FabrixError::BadPayload(detail) => format!("사내 응답을 해석하지 못했습니다: {detail}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            FabrixError::Quota(_) => "rate_limit_error",
            FabrixError::NotConfigured => "configuration_error",
            _ => "upstream_error",
        }
    }
}

impl From<reqwest::Error> for FabrixError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            FabrixError::Unreachable("응답 시간 초과".into())
        } else if err.is_connect() {
            FabrixError::Unreachable("연결할 수 없습니다".into())
        } else if err.is_decode() {
            FabrixError::BadPayload(err.to_string())
        } else {
            FabrixError::Unreachable(err.to_string())
        }
    }
}

// ─────────────────────────── 클라이언트 ───────────────────────────

pub struct FabrixClient {
    pub http: reqwest::Client,
    pub base: String,
    pub client_key: String,
    pub token: String,
}

impl FabrixClient {
    pub fn models_url(&self) -> String {
        format!("{}{MODELS_PATH}", self.base)
    }

    pub fn messages_url(&self) -> String {
        format!("{}{MESSAGES_PATH}", self.base)
    }

    fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        self.http
            .request(method, url)
            // 스펙: Content-Type 은 application/json 으로 고정.
            .header("Content-Type", "application/json")
            .header("x-fabrix-client", &self.client_key)
            .header("x-openapi-token", &self.token)
    }

    pub async fn list_models(&self) -> Result<Vec<FabrixModel>, FabrixError> {
        let res = self
            .request(reqwest::Method::GET, self.models_url())
            .header("Accept", "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;

        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(classify_status(status.as_u16(), &body));
        }

        let value: Value = serde_json::from_str(&body)
            .map_err(|e| FabrixError::BadPayload(format!("{e} — 본문 앞부분: {}", head(&body))))?;
        let array = extract_array(&value)
            .ok_or_else(|| FabrixError::BadPayload(format!("모델 배열을 찾지 못했습니다: {}", head(&body))))?;

        let mut models = Vec::new();
        for item in array {
            match serde_json::from_value::<FabrixModel>(item.clone()) {
                Ok(m) => models.push(m),
                // modelId 없는 항목은 조용히 건너뜁니다.
                Err(_) => continue,
            }
        }
        if models.is_empty() {
            return Err(FabrixError::BadPayload("모델 목록이 비어 있습니다".into()));
        }
        Ok(models)
    }

    /// 스트리밍이면 `timeout` 을 걸지 않습니다 — 긴 답변이 정상적으로 30초를
    /// 넘길 수 있고, 청크가 끊기는 것은 클라이언트의 read_timeout 이 잡습니다.
    pub async fn messages(&self, body: &MessagesRequest) -> Result<reqwest::Response, FabrixError> {
        let mut req = self
            .request(reqwest::Method::POST, self.messages_url())
            .header("Accept", if body.is_stream { "text/event-stream" } else { "application/json" })
            .json(body);

        if !body.is_stream {
            req = req.timeout(REQUEST_TIMEOUT);
        }

        let res = req.send().await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(classify_status(status.as_u16(), &text));
        }
        Ok(res)
    }
}

pub fn build_http_client(insecure: bool) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(concat!("fabrix-proxy/", env!("CARGO_PKG_VERSION")));

    let builder = if insecure { builder.danger_accept_invalid_certs(true) } else { builder };

    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

fn classify_status(status: u16, body: &str) -> FabrixError {
    let message = extract_message(body).unwrap_or_else(|| head(body));
    match status {
        429 => FabrixError::Quota(message),
        502 | 503 | 504 => FabrixError::Unreachable(message),
        other => FabrixError::Upstream { status: other, message },
    }
}

/// 오류 본문에서 사람이 읽을 메시지를 뽑아냅니다.
fn extract_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    for key in ["message", "errorMessage", "error_message", "detail", "eventData"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(s.to_string());
            }
        }
    }
    if let Some(err) = value.get("error") {
        if let Some(s) = err.as_str() {
            return Some(s.to_string());
        }
        if let Some(s) = err.get("message").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn head(body: &str) -> String {
    crate::logstore::preview(body, 200)
}

/// FabriX 응답 봉투 모양을 모르므로 흔한 자리를 순서대로 뒤집니다.
pub fn extract_array(value: &Value) -> Option<&Vec<Value>> {
    if let Some(a) = value.as_array() {
        return Some(a);
    }
    const KEYS: [&str; 7] = ["data", "result", "results", "models", "list", "items", "content"];
    for key in KEYS {
        if let Some(a) = value.get(key).and_then(Value::as_array) {
            return Some(a);
        }
    }
    for outer in ["data", "result", "response"] {
        if let Some(inner) = value.get(outer) {
            for key in KEYS {
                if let Some(a) = inner.get(key).and_then(Value::as_array) {
                    return Some(a);
                }
            }
        }
    }
    None
}

/// 비스트리밍 응답에서 실제 답변 객체를 골라냅니다.
pub fn extract_object(value: &Value) -> Value {
    if let Some(items) = value.as_array() {
        return items.first().cloned().unwrap_or(Value::Null);
    }
    for key in ["data", "result", "response"] {
        if let Some(inner) = value.get(key) {
            if let Some(items) = inner.as_array() {
                return items.first().cloned().unwrap_or(Value::Null);
            }
            if inner.is_object() && (inner.get("content").is_some() || inner.get("finishReason").is_some()) {
                return inner.clone();
            }
        }
    }
    value.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(decoder: &mut StreamDecoder, raw: &str) -> Vec<StreamEvent> {
        decoder.push(raw.as_bytes())
    }

    #[test]
    fn parses_sse_with_data_prefix_and_snake_case() {
        let mut d = StreamDecoder::new();
        let out = events(
            &mut d,
            "data: {\"content\":\"안녕\",\"model_type\":\"llama\"}\n\ndata: {\"content\":\"하세요\"}\n\n",
        );
        assert_eq!(
            out,
            vec![StreamEvent::Delta("안녕".into()), StreamEvent::Delta("하세요".into())]
        );
        assert_eq!(d.text(), "안녕하세요");
    }

    #[test]
    fn parses_bare_json_lines_and_camel_case() {
        let mut d = StreamDecoder::new();
        let out = events(&mut d, "{\"content\":\"A\"}\n{\"content\":\"B\",\"finishReason\":\"stop\"}\n");
        assert_eq!(
            out,
            vec![
                StreamEvent::Delta("A".into()),
                StreamEvent::Delta("B".into()),
                StreamEvent::Finish("stop".into())
            ]
        );
    }

    #[test]
    fn cumulative_frames_emit_only_the_new_suffix() {
        let mut d = StreamDecoder::new();
        let mut all = events(&mut d, "data: {\"content\":\"안녕\"}\n");
        all.extend(events(&mut d, "data: {\"content\":\"안녕하세\"}\n"));
        all.extend(events(&mut d, "data: {\"content\":\"안녕하세요\"}\n"));
        assert_eq!(
            all,
            vec![
                StreamEvent::Delta("안녕".into()),
                StreamEvent::Delta("하세".into()),
                StreamEvent::Delta("요".into())
            ]
        );
        assert_eq!(d.text(), "안녕하세요");
    }

    #[test]
    fn multibyte_split_across_chunk_boundaries_is_safe() {
        let mut d = StreamDecoder::new();
        let frame = "data: {\"content\":\"한글\"}\n".as_bytes();
        let (a, b) = frame.split_at(20); // '한' 의 3바이트 중 2바이트만 넘긴 지점
        assert!(d.push(a).is_empty());
        assert_eq!(d.push(b), vec![StreamEvent::Delta("한글".into())]);
    }

    #[test]
    fn done_sentinel_and_comments() {
        let mut d = StreamDecoder::new();
        let out = events(&mut d, ": keep-alive\n\ndata: [DONE]\n\n");
        assert_eq!(out, vec![StreamEvent::Done]);
        assert!(d.done);
    }

    #[test]
    fn error_status_surfaces_as_error_event() {
        let mut d = StreamDecoder::new();
        let out = events(&mut d, "data: {\"status\":\"ERROR\",\"eventData\":\"쿼터 초과\"}\n");
        assert_eq!(out, vec![StreamEvent::Error("쿼터 초과".into())]);
    }

    /// 비스트림 응답 본문(raw JSON)을 실제 파싱 경로대로 FabrixChunk 로 만듭니다.
    fn parse_nostream(raw: &str) -> FabrixChunk {
        let value: Value = serde_json::from_str(raw).unwrap();
        serde_json::from_value::<FabrixChunk>(extract_object(&value)).unwrap()
    }

    #[test]
    fn nostream_pure_llm_reads_content() {
        // 순수 LLM 답변: content 에 답변, contentReferences.answer 는 빈 문자열.
        let raw = r#"{
            "modelType": "FabriX",
            "content": "소프트웨어의 역사는\n\n컴퓨터의 발전과 함께 시작되었습니다.",
            "reasoningContent": null,
            "contentReferences": [{ "plugin": "RAG", "answer": "", "references": [] }],
            "finishReason": null,
            "filterBlockReason": { "ko": null, "en": null, "message": null, "result_code": "FR-200" },
            "status": "SUCCESS",
            "responseCode": "R20000",
            "plugins": ["LLM"],
            "eventStatus": "CHUNK",
            "eventData": ""
        }"#;
        let chunk = parse_nostream(raw);
        assert!(!chunk.looks_like_error());
        assert_eq!(chunk.answer_text().as_deref(), Some("소프트웨어의 역사는\n\n컴퓨터의 발전과 함께 시작되었습니다."));
        assert_eq!(chunk.filter_message(), None);
    }

    #[test]
    fn nostream_plugin_answer_falls_back_to_content_references() {
        // 플러그인/RAG 답변: content 는 null, 답변이 contentReferences[].answer 에 온다.
        let raw = r#"{
            "modelType": "FabriX",
            "content": null,
            "reasoningContent": null,
            "contentReferences": [
                { "plugin": "RAG", "answer": "첫 번째 근거 답변", "references": [] },
                { "plugin": "RAG", "answer": "두 번째 근거 답변", "references": [] }
            ],
            "finishReason": null,
            "status": "SUCCESS",
            "responseCode": "R20000",
            "eventStatus": "CHUNK",
            "eventData": ""
        }"#;
        let chunk = parse_nostream(raw);
        assert_eq!(chunk.content, None);
        assert_eq!(chunk.answer_text().as_deref(), Some("첫 번째 근거 답변\n두 번째 근거 답변"));
        assert_eq!(chunk.filter_message(), None);
    }

    #[test]
    fn nostream_falls_back_to_event_data() {
        // content 도 contentReferences 도 없고 eventData 에만 답이 있는 경우.
        let raw = r#"{
            "content": null,
            "contentReferences": [],
            "eventData": "이벤트 데이터 답변",
            "status": "SUCCESS"
        }"#;
        let chunk = parse_nostream(raw);
        assert_eq!(chunk.answer_text().as_deref(), Some("이벤트 데이터 답변"));
    }

    #[test]
    fn nostream_filter_block_surfaces_reason() {
        // 필터 차단: 답변은 비고 filterBlockReason 에 사유가 담긴다.
        let raw = r#"{
            "content": null,
            "reasoningContent": null,
            "contentReferences": [],
            "finishReason": null,
            "filterBlockReason": {
                "ko": "부적절한 표현이 감지되었습니다.",
                "en": null,
                "message": null,
                "result_code": "FR-403"
            },
            "status": "SUCCESS",
            "responseCode": "R20000",
            "eventStatus": "CHUNK",
            "eventData": ""
        }"#;
        let chunk = parse_nostream(raw);
        assert_eq!(chunk.answer_text(), None);
        assert_eq!(chunk.filter_message().as_deref(), Some("부적절한 표현이 감지되었습니다."));
    }

    #[test]
    fn nostream_extra_fields_do_not_break_streaming_frame_parsing() {
        // 새로 추가한 필드들이 스트리밍 프레임(추가 필드 없음) 파싱을 깨지 않는지 확인.
        let mut d = StreamDecoder::new();
        let out = events(&mut d, "data: {\"content\":\"조각\",\"model_type\":\"llama\"}\n");
        assert_eq!(out, vec![StreamEvent::Delta("조각".into())]);
    }

    #[test]
    fn aliases_prefer_english_names_and_fall_back_to_uuid() {
        let models = vec![
            FabrixModel {
                model_id: "0196f1fc-2858-70a9-a232-74dbddb971d0".into(),
                name: vec![
                    LocalizedText { language_code: Some("ko".into()), content: Some("챗 4".into()) },
                    LocalizedText { language_code: Some("en".into()), content: Some("Chat 4".into()) },
                ],
                description: vec![],
            },
            FabrixModel {
                model_id: "01970a3b-91d4-7c8e-1111-222233334444".into(),
                name: vec![LocalizedText { language_code: Some("ko".into()), content: Some("라이트".into()) }],
                description: vec![],
            },
        ];
        let resolved = build_aliases(&models);
        assert_eq!(resolved[0].alias, "fabrix-chat-4");
        assert_eq!(resolved[0].label, "챗 4");
        // 한글만 있는 이름 → UUID 앞 8자리 (하이픈 제외)
        assert_eq!(resolved[1].alias, "fabrix-01970a3b");
        assert_eq!(resolved[1].label, "라이트");
    }

    #[test]
    fn unknown_model_falls_back_to_default() {
        let models = build_aliases(&[FabrixModel {
            model_id: "uuid-1".into(),
            name: vec![LocalizedText { language_code: Some("en".into()), content: Some("Chat 4".into()) }],
            description: vec![],
        }]);
        let hit = resolve_model(&models, Some("gpt-4o"), "fabrix-chat-4").unwrap();
        assert_eq!(hit.model_id, "uuid-1");
        // UUID 를 직접 보내도 통합니다.
        assert_eq!(resolve_model(&models, Some("uuid-1"), "").unwrap().alias, "fabrix-chat-4");
    }

    #[test]
    fn system_messages_split_into_system_prompt() {
        use crate::openai::{Content, Message};
        let msgs = vec![
            Message { role: "system".into(), content: Some(Content::Text("너는 사내 규정 도우미다.".into())), ..Default::default() },
            Message { role: "user".into(), content: Some(Content::Text("연차 이월 규정 알려줘".into())), ..Default::default() },
        ];
        let (system, contents) = fold_messages(&msgs);
        assert_eq!(system.as_deref(), Some("너는 사내 규정 도우미다."));
        assert_eq!(contents, vec!["연차 이월 규정 알려줘".to_string()]);
    }

    #[test]
    fn multi_turn_flattens_into_a_labelled_transcript() {
        use crate::openai::{Content, Message};
        let msgs = vec![
            Message { role: "user".into(), content: Some(Content::Text("안녕".into())), ..Default::default() },
            Message { role: "assistant".into(), content: Some(Content::Text("네".into())), ..Default::default() },
            Message { role: "user".into(), content: Some(Content::Text("규정은?".into())), ..Default::default() },
        ];
        let (system, contents) = fold_messages(&msgs);
        assert!(system.is_none());
        assert_eq!(contents, vec!["User: 안녕\n\nAssistant: 네\n\nUser: 규정은?".to_string()]);
    }

    // ── 도구 대화 평탄화 ──

    fn msg(json: &str) -> crate::openai::Message {
        serde_json::from_str(json).unwrap()
    }

    /// 회귀 방지: 예전에는 `content: null` 이라 공백 드롭에 걸려 이 턴이 통째로
    /// 사라졌고, 모델은 자기가 부른 적 없는 결과를 보게 됐습니다.
    #[test]
    fn assistant_tool_call_turn_is_not_dropped() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"페이지 만들어줘"}"#),
            msg(
                r#"{"role":"assistant","content":null,"tool_calls":[
                     {"id":"call_a1","type":"function",
                      "function":{"name":"write","arguments":"{\"filePath\":\"a.html\"}"}}]}"#,
            ),
            msg(r#"{"role":"tool","tool_call_id":"call_a1","content":"wrote 12 bytes"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        let t = &contents[0];
        assert!(t.contains("<tool_call>"), "도구 호출이 사라졌습니다:\n{t}");
        assert!(t.contains("\"name\":\"write\""), "{t}");
        assert!(t.contains("call_a1"), "{t}");
        assert!(t.contains("wrote 12 bytes"), "{t}");
    }

    #[test]
    fn tool_result_is_not_double_labelled() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"go"}"#),
            msg(
                r#"{"role":"assistant","content":null,"tool_calls":[
                     {"id":"c1","function":{"name":"read","arguments":"{}"}}]}"#,
            ),
            msg(r#"{"role":"tool","tool_call_id":"c1","content":"body"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert!(!contents[0].contains("Tool: Tool result"), "이중 라벨:\n{}", contents[0]);
        // 이름은 호출을 낸 assistant 턴에서 끌어옵니다.
        assert!(contents[0].contains("Tool result (id=c1, name=read)"), "{}", contents[0]);
    }

    #[test]
    fn parallel_calls_correlate_by_id() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"go"}"#),
            msg(
                r#"{"role":"assistant","content":null,"tool_calls":[
                     {"id":"c1","function":{"name":"write","arguments":"{}"}},
                     {"id":"c2","function":{"name":"read","arguments":"{}"}}]}"#,
            ),
            msg(r#"{"role":"tool","tool_call_id":"c2","content":"두번째"}"#),
            msg(r#"{"role":"tool","tool_call_id":"c1","content":"첫번째"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        let t = &contents[0];
        // 결과가 호출 순서와 다르게 와도 id 로 짝지어져야 합니다.
        assert!(t.contains("Tool result (id=c2, name=read)"), "{t}");
        assert!(t.contains("Tool result (id=c1, name=write)"), "{t}");
    }

    /// 회귀 방지: AI SDK v5 는 결과를 파트 배열로 보냅니다. `text` 파트가 없어
    /// `text()` 가 비고, 예전이라면 여기서 결과가 통째로 사라졌습니다.
    #[test]
    fn ai_sdk_tool_result_parts_survive_the_fold() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"go"}"#),
            msg(
                r#"{"role":"tool","tool_call_id":"c1",
                    "content":[{"type":"tool-result","output":{"ok":true}}]}"#,
            ),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert!(contents[0].contains("tool-result"), "{}", contents[0]);
        assert!(contents[0].contains("\"ok\":true"), "{}", contents[0]);
    }

    #[test]
    fn empty_tool_result_still_records_the_call() {
        let msgs = vec![msg(r#"{"role":"tool","tool_call_id":"c1","content":""}"#)];
        let (_, contents) = fold_messages(&msgs);
        // "불렀는데 결과가 비었다"도 정보라 줄은 남아야 합니다.
        assert_eq!(contents.len(), 1);
        assert!(contents[0].contains("Tool result (id=c1)"), "{}", contents[0]);
    }

    #[test]
    fn legacy_function_role_uses_its_own_name() {
        let msgs = vec![msg(r#"{"role":"function","name":"calc","content":"84"}"#)];
        let (_, contents) = fold_messages(&msgs);
        assert!(contents[0].contains("Tool result (name=calc)"), "{}", contents[0]);
    }

    #[test]
    fn unknown_roles_keep_their_label() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"안녕"}"#),
            msg(r#"{"role":"critic","content":"별로"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert_eq!(contents[0], "User: 안녕\n\ncritic: 별로");
    }

    // ── 종료 사유와 절단 ──

    #[test]
    fn truncation_outranks_every_other_finish_reason() {
        // 회귀 방지: `truncated` 를 파싱만 하고 안 쓰던 시절에는 상한에 걸려 잘린
        // 답변이 클라이언트에 깔끔한 "stop" 으로 보였습니다.
        assert_eq!(map_finish_reason(Some("stop"), true).as_deref(), Some("length"));
        assert_eq!(map_finish_reason(None, true).as_deref(), Some("length"));
    }

    #[test]
    fn finish_reason_maps_known_synonyms() {
        assert_eq!(map_finish_reason(Some("max_tokens"), false).as_deref(), Some("length"));
        assert_eq!(map_finish_reason(Some("MAX_NEW_TOKENS"), false).as_deref(), Some("length"));
        assert_eq!(map_finish_reason(Some("filtered"), false).as_deref(), Some("content_filter"));
        assert_eq!(map_finish_reason(Some("end_turn"), false).as_deref(), Some("stop"));
    }

    #[test]
    fn finish_reason_passes_unknown_values_through() {
        // 모르는 값을 "stop" 으로 지어내면 진짜 상태를 감추게 됩니다.
        assert_eq!(map_finish_reason(Some("weird"), false).as_deref(), Some("weird"));
        assert_eq!(map_finish_reason(Some("   "), false), None);
        assert_eq!(map_finish_reason(None, false), None);
    }

    #[test]
    fn decoder_records_the_truncated_flag() {
        let mut d = StreamDecoder::new();
        d.push(b"data: {\"content\":\"ab\"}\n");
        assert!(!d.truncated);
        d.push(b"data: {\"content\":\"cd\",\"truncated\":true}\n");
        assert!(d.truncated);
    }

    /// 누적 모드에서 상위가 답변을 다시 쓰면 전체 본문이 델타로 재방출됩니다.
    /// 그 사실을 알려 주지 않으면 텍스트를 누적해 파싱하는 소비자가 이미 처리한
    /// 것을 두 번 처리합니다.
    #[test]
    fn cumulative_rewrite_emits_reset_before_the_replacement() {
        let mut d = StreamDecoder::new();
        assert_eq!(d.push("data: {\"content\":\"안녕\"}\n".as_bytes()), vec![StreamEvent::Delta("안녕".into())]);
        // 두 번째 프레임이 접두사라 누적 모드로 확정됩니다.
        assert_eq!(
            d.push("data: {\"content\":\"안녕하세요\"}\n".as_bytes()),
            vec![StreamEvent::Delta("하세요".into())]
        );
        // 접두사가 아닌 본문 → 재작성.
        assert_eq!(
            d.push("data: {\"content\":\"전혀 다른 답\"}\n".as_bytes()),
            vec![StreamEvent::Reset, StreamEvent::Delta("전혀 다른 답".into())]
        );
        assert_eq!(d.text(), "전혀 다른 답");
    }

    #[test]
    fn incremental_mode_never_emits_reset() {
        let mut d = StreamDecoder::new();
        d.push("data: {\"content\":\"안\"}\n".as_bytes());
        let events = d.push("data: {\"content\":\"녕\"}\n".as_bytes());
        assert_eq!(events, vec![StreamEvent::Delta("녕".into())]);
        assert_eq!(d.text(), "안녕");
    }

    #[test]
    fn single_user_turn_stays_bare() {
        // 도구 분기를 넣어도 가장 흔한 단일 턴은 라벨 없이 그대로 나가야 합니다.
        let msgs = vec![msg(r#"{"role":"user","content":"안녕"}"#)];
        let (_, contents) = fold_messages(&msgs);
        assert_eq!(contents, vec!["안녕".to_string()]);
    }
}
