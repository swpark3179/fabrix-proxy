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

/// UUID 직매치 → alias 완전일치 → 대소문자 무시. **폴백하지 않습니다.**
///
/// 폴백을 이 함수 안에 두면 `/v1/models/{id}` 가 모르는 id 에 200 을 돌려주는 거짓말이
/// 됩니다 — 클라이언트는 그 id 가 있다고 믿고 계속 씁니다. 폴백이 필요한 자리
/// (`model` 을 아예 안 보낸 요청)는 [`default_model`] 을 따로 부릅니다.
pub fn find_model<'a>(models: &'a [ResolvedModel], requested: &str) -> Option<&'a ResolvedModel> {
    let req = requested.trim();
    if req.is_empty() {
        return None;
    }
    models
        .iter()
        .find(|m| m.model_id == req)
        .or_else(|| models.iter().find(|m| m.alias == req))
        .or_else(|| models.iter().find(|m| m.alias.eq_ignore_ascii_case(req)))
}

/// `model` 을 아예 안 보낸 요청에 쓸 모델 — 설정의 기본 alias, 없으면 목록의 첫 모델.
pub fn default_model<'a>(
    models: &'a [ResolvedModel],
    default_alias: &str,
) -> Option<&'a ResolvedModel> {
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

/// 사내가 `temperature` 에 허용하는 상한. 스펙 문서가 "0에서 1 사이" 라고 못박습니다.
///
/// OpenAI 는 0–2 라, 그 범위를 기본값으로 보내는 클라이언트가 많습니다. 그래서 검증은
/// 0–2 로 통과시키고(거절하면 그 클라이언트들이 전부 400 을 받습니다) **나갈 값만**
/// 여기로 줄입니다. 조용히 줄이지는 않습니다 — 로그 ③ 칸 꼬리에 적습니다.
pub const FABRIX_MAX_TEMPERATURE: f64 = 1.0;

/// ⚠️ 반복 페널티와 top-k 의 철자가 **문서와 샘플에서 다릅니다.**
///
/// - 스펙 문서의 `llmConfig Properties` 목록: `repetion_penalty` · `tok_k`
/// - 벤더가 준 **실행되는 샘플 코드**: `repetition_penalty` · `top_k`
///
/// 어느 쪽이 서버가 실제로 읽는 키인지 확인되지 않아 **양쪽 다** 보냅니다. 사내는
/// `llmConfig` 의 모르는 키를 무시합니다 — 지금까지 문서 철자만 보내면서도 앱이 동작한
/// 것이 그 증거입니다(엄격히 거절한다면 이미 깨져 있어야 합니다).
///
/// 실서버에서 어느 쪽인지 확정되면 해당 필드 두 줄만 지우면 됩니다.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// 샘플 코드의 철자.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    /// 문서의 철자(오타로 보이지만 서버가 기대하는 키일 수 있습니다).
    #[serde(rename = "repetion_penalty", skip_serializing_if = "Option::is_none")]
    pub repetion_penalty: Option<f64>,
    /// 샘플 코드의 철자.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// 문서의 철자.
    #[serde(rename = "tok_k", skip_serializing_if = "Option::is_none")]
    pub tok_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_new_tokens: Option<u32>,
}

impl LlmConfig {
    pub fn from_request(req: &ChatRequest) -> Option<Self> {
        // 두 철자에 같은 값을 싣습니다 — 서버가 어느 쪽을 읽든 전달되게.
        let penalty = req.frequency_penalty;
        let top_k = req.top_k;
        let cfg = Self {
            temperature: req.temperature.map(clamp_temperature),
            top_p: req.top_p,
            repetition_penalty: penalty,
            repetion_penalty: penalty,
            top_k,
            tok_k: top_k,
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

/// OpenAI 범위(0–2)로 받은 값을 사내 범위(0–1)로 줄입니다.
pub fn clamp_temperature(requested: f64) -> f64 {
    requested.min(FABRIX_MAX_TEMPERATURE)
}

/// 클램프가 실제로 일어났는가 — 로그 한 줄을 붙일지 정하는 데 씁니다.
pub fn temperature_was_clamped(requested: Option<f64>) -> Option<(f64, f64)> {
    let requested = requested?;
    let sent = clamp_temperature(requested);
    (sent < requested).then_some((requested, sent))
}

/// `contents` 배열에서 한 원소가 누구의 발화인지. **위치가 롤입니다.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speaker {
    User,
    Assistant,
}

/// 대화가 assistant 발화로 시작할 때 짝을 맞추려고 앞에 넣는 user 턴.
///
/// `chat.rs` 가 "시스템/규약만 있고 사용자 턴이 없는 라운드" 에 쓰는 것과 같은 문구입니다 —
/// 같은 목적(사내가 거절하지 않을 최소한의 한 줄)이라 굳이 다르게 부를 이유가 없습니다.
pub const CONTINUE_TURN: &str = "(continue)";

/// OpenAI `messages` → FabriX `systemPrompt` + `contents`.
///
/// **`contents` 는 턴 배열입니다** — 원소 하나가 한 턴이고, 배열 위치가 롤을 나타냅니다
/// (짝수 = user, 홀수 = assistant). 근거는 벤더 샘플입니다:
///
/// ```text
/// "contents": ["안녕하세요?", "네 안녕하세요", "내 이름은 LCY인데 너 이름은 뭐니?"]
/// #              user           assistant        user
/// ```
///
/// 예전에는 멀티턴을 `["User: …\n\nAssistant: …"]` 한 덩어리로 접었습니다. "FabriX 엔 롤
/// 구조가 없다" 고 본 것인데, 롤 *라벨* 이 없을 뿐 배열이 턴을 담습니다. 한 덩어리로 보내면
/// 사내 모델의 chat template 이 제대로 걸리지 않고, 모델은 "대화 중" 이 아니라 "대화록을
/// 읽는 중" 으로 인식합니다 — 프롬프트 기반 툴콜은 모델이 자기 턴의 시작을 알아야 잘
/// 동작하므로 도구 준수율에 직접 영향을 줍니다.
///
/// 교대는 **구조적으로 보장**합니다:
/// 1. 연속 동일 롤은 하나로 병합합니다. 안 하면 원소가 하나 밀려 그 뒤 전체의 롤이 뒤집힙니다.
/// 2. 첫 턴이 assistant 면 앞에 [`CONTINUE_TURN`] 을 넣습니다.
///
/// 도구를 쓰는 대화에서 조심할 것이 둘 있습니다.
///
/// 1. 도구 호출만 있는 assistant 턴은 `content` 가 `null` 이라 본문이 빕니다. 공백이라고
///    버리면 모델은 다음 턴에 **자기가 호출한 적 없는 결과**를 보게 되고, 같은 도구를 다시
///    부르는 루프에 빠집니다. 그래서 공백 판정을 역할별 분기 안으로 내렸습니다.
/// 2. `role: "tool"` 결과는 **user 턴**으로 들어갑니다. 사내엔 tool 롤이 없고, 결과를
///    모델에게 돌려주는 쪽은 우리(=user)입니다. 프롬프트 기반 툴콜의 표준 모양이기도 합니다.
pub fn fold_messages(messages: &[crate::openai::Message]) -> (Option<String>, Vec<String>) {
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<(Speaker, String)> = Vec::new();

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
                    turns.push((Speaker::Assistant, parts.join("\n")));
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
                turns.push((Speaker::User, format!("{head}:\n{body}")));
            }
            role => {
                let text = m.text();
                if !text.trim().is_empty() {
                    match role {
                        "user" => turns.push((Speaker::User, text)),
                        // 모르는 롤은 user 턴에 머리말을 붙여 넣습니다 — 사내에 실을 자리가
                        // user/assistant 둘뿐이라, 버리는 것보다 누가 말했는지 적는 편이 낫습니다.
                        other => turns.push((Speaker::User, format!("{other}: {text}"))),
                    }
                }
            }
        }
    }

    let system_prompt = if system.is_empty() { None } else { Some(system.join("\n\n")) };
    (system_prompt, alternating(turns))
}

/// 턴 목록을 교대가 보장된 `contents` 배열로 만듭니다.
fn alternating(turns: Vec<(Speaker, String)>) -> Vec<String> {
    if turns.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<(Speaker, String)> = Vec::with_capacity(turns.len());
    for (speaker, text) in turns {
        match out.last_mut() {
            // 연속 동일 롤 병합 — 이게 교대 보장의 핵심입니다.
            Some((last, body)) if *last == speaker => {
                body.push_str("\n\n");
                body.push_str(&text);
            }
            _ => out.push((speaker, text)),
        }
    }

    let mut contents: Vec<String> = Vec::with_capacity(out.len() + 1);
    // 첫 원소는 언제나 user 자리입니다.
    if out[0].0 == Speaker::Assistant {
        contents.push(CONTINUE_TURN.to_string());
    }
    contents.extend(out.into_iter().map(|(_, text)| text));
    contents
}

/// `contents` 의 마지막 원소가 user 자리인가.
///
/// index 0 이 user 이므로 길이가 홀수면 마지막이 user 입니다. 꼬리 리마인더를 붙일 자리를
/// 고르는 데 씁니다 — assistant 자리에 지시문을 넣으면 **모델 자신의 발화**가 됩니다.
pub fn last_is_user_turn(contents: &[String]) -> bool {
    contents.len() % 2 == 1
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

    // ── 토큰 수 후보 필드들 ──
    //
    // 스펙에 없어서 **오늘은 항상 `None`** 입니다. 방어적으로 받아 두는 이유: 사내가
    // 토큰 수를 주기 시작하면 다른 코드를 고치지 않고 `usage` 가 추정치에서 실측으로
    // 넘어갑니다(`proxy::usage::build`). 받는 값이 없을 때 비용은 0 입니다.
    #[serde(default, alias = "inputTokens", alias = "prompt_tokens", alias = "promptTokens")]
    pub input_tokens: Option<u32>,
    #[serde(
        default,
        alias = "outputTokens",
        alias = "completion_tokens",
        alias = "completionTokens"
    )]
    pub output_tokens: Option<u32>,
    /// 중첩 `usage {prompt_tokens, completion_tokens}` 모양도 받습니다.
    #[serde(default)]
    pub usage: Option<UpstreamUsage>,
}

/// 사내가 OpenAI 처럼 중첩 `usage` 를 줄 경우의 모양.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpstreamUsage {
    #[serde(default, alias = "promptTokens", alias = "inputTokens", alias = "input_tokens")]
    pub prompt_tokens: Option<u32>,
    #[serde(
        default,
        alias = "completionTokens",
        alias = "outputTokens",
        alias = "output_tokens"
    )]
    pub completion_tokens: Option<u32>,
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

    /// 사내가 준 토큰 수. 둘 다 있어야 씁니다 — 한쪽만 있으면 반쪽짜리 usage 가 되어
    /// 추정치보다 오해를 부릅니다.
    pub fn upstream_tokens(&self) -> Option<(u32, u32)> {
        let nested = self.usage.as_ref();
        let prompt = self.input_tokens.or_else(|| nested.and_then(|u| u.prompt_tokens))?;
        let completion = self.output_tokens.or_else(|| nested.and_then(|u| u.completion_tokens))?;
        Some((prompt, completion))
    }

    /// 응답이 "제대로 온 것"인지 — 답변이 비었을 때 502 를 낼지 200 을 낼지 가릅니다.
    ///
    /// 모델이 정말 빈 답을 줄 수도 있습니다(짧은 max_tokens, 필터 직전 등). 그걸
    /// 502 로 처리하면 사내 잘못이 아닌 것을 사내 오류로 보고하는 셈입니다. 성공
    /// 표지(`status`/`responseCode`/`finish_reason`)가 하나라도 있으면 빈 답변도
    /// 정상 응답으로 봅니다. 아무 표지도 없으면 애초에 우리가 못 알아본 본문이라
    /// 502 가 맞습니다.
    pub fn looks_successful(&self) -> bool {
        if self.looks_like_error() {
            return false;
        }
        let filled = |s: &Option<String>| s.as_deref().is_some_and(|v| !v.trim().is_empty());
        filled(&self.status) || filled(&self.response_code) || filled(&self.finish_reason)
    }

    /// 비스트리밍 답변 텍스트: `content` → `contentReferences[].answer`(결합).
    ///
    /// 스펙 문서상 답변은 `content` 입니다. `contentReferences` 는 "References used while
    /// generating the answer" 이고 `answer` 하위 필드는 문서에 없습니다 — 즉 이 폴백은
    /// **미확인**이고 죽은 코드일 수 있습니다. 그래도 남기는 이유: 살아 있다면(플러그인/RAG
    /// 응답이 정말 거기 답을 싣는다면) 지우는 순간 그 답변을 잃고, `content` 가 있을 때는
    /// 발동하지 않아 비용이 0 입니다. 실서버에서 확정되면 지우세요.
    ///
    /// `eventData` 폴백은 **지웠습니다** — 문서가 `Event Data` 라고만 하고, 내부 이벤트
    /// 문자열이 assistant 답변으로 나가면 안 됩니다.
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
        (!joined.is_empty()).then_some(joined)
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
/// `decide_finish` 만 호출합니다 — 종료 사유 판단이 한 자리에 모여 있게 하려고
/// 밖으로 내보내지 않습니다.
fn map_finish_reason(raw: Option<&str>, truncated: bool) -> Option<String> {
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

/// `map_finish_reason` 의 결과를 **와이어에 나가기 직전** OpenAI 열거값으로 접습니다.
///
/// `map_finish_reason` 이 모르는 값을 그대로 넘기는 것은 의도입니다 — 상위가 뭐라고
/// 했는지는 로그에 남아야 합니다. 다만 그 값이 응답에까지 나가면 `finish_reason` 을
/// 열거형으로 파싱하는 클라이언트가 깨집니다. 그래서 보존과 준수를 두 함수로 나누고,
/// 이 함수는 직렬화 경계에서만 씁니다.
///
/// 중단 계열(`abort`·`timeout`·`error`…)은 `stop` 이 아니라 `length` 로 접습니다 —
/// 끊긴 답변을 완성된 것처럼 부르면 안 됩니다.
fn clamp_finish_reason(mapped: Option<&str>) -> &'static str {
    match mapped.unwrap_or("stop").trim().to_ascii_lowercase().as_str() {
        "stop" => "stop",
        "length" => "length",
        "tool_calls" => "tool_calls",
        "content_filter" => "content_filter",
        "function_call" => "function_call",
        "abort" | "aborted" | "cancel" | "cancelled" | "canceled" | "error" | "timeout"
        | "incomplete" => "length",
        _ => "stop",
    }
}

/// 이 턴의 `finish_reason` 을 정하는 **단 하나의** 자리.
///
/// 예전에는 스트림 경로와 비스트림 경로가 각자 이 판단을 했습니다. 두 사본이 어긋나면
/// 같은 답변이 `stream` 여부에 따라 다른 종료 사유를 받습니다 — 클라이언트는 이 값으로
/// 에이전트 루프를 계속할지 정하므로, 어긋남이 곧 "한쪽에서만 루프가 끊김" 입니다.
///
/// 우선순위: **도구 호출 > 절단 > 상위가 준 사유 > stop**.
///
/// 도구 호출이 절단보다 앞서는 것은 의도입니다. 스캐너는 닫는 태그를 보고 이름 검증까지
/// 끝난 **완성된 호출만** 내보내므로, 뒤가 잘렸어도 이미 뽑은 호출은 실행할 수 있습니다.
/// 절단 사실은 로그 ③ 칸 꼬리에 남습니다.
///
/// 스트림이 중간에 끊긴 경우는 호출부가 `upstream` 자리에 `"error"` 를 넣어 알립니다 —
/// `clamp_finish_reason` 의 중단 계열 갈래가 그것을 `length` 로 접습니다. 별도 상수를
/// 두지 않는 이유: 상수와 clamp 표가 따로 있으면 둘이 어긋날 수 있습니다.
pub fn decide_finish(saw_call: bool, upstream: Option<&str>, truncated: bool) -> &'static str {
    if saw_call {
        return "tool_calls";
    }
    clamp_finish_reason(map_finish_reason(upstream, truncated).as_deref())
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Delta(String),
    Reasoning(String),
    Finish(String),
    Error(String),
    Done,
}

/// 바이트 스트림 → OpenAI 델타.
///
/// 줄 단위로만 잘라 쓰기 때문에 청크 경계에서 UTF-8 문자가 쪼개져도 안전합니다
/// (개행은 항상 단일 바이트라 멀티바이트 문자 중간에 걸리지 않습니다).
///
/// `content` 는 **증분**입니다. 벤더 샘플이 `result_message += ch_json['content']` 로
/// 이어 붙이는 것이 근거입니다 — 누적이면 `+=` 가 아니라 대입이어야 합니다. 예전에는
/// 누적/증분을 두 번째 프레임에서 자동 판별하고 재작성(`Reset`)까지 다뤘는데, 확인된
/// 지금은 그 갈래가 전부 죽은 코드였습니다.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    buf: Vec<u8>,
    acc: String,
    pub finish_reason: Option<String>,
    pub model_type: Option<String>,
    pub done: bool,
    /// 상위가 답변을 잘랐다고 알려 왔는지. `finish_reason: "length"` 로 옮깁니다.
    ///
    /// 플래그로 들고 있는 이유: 여기서 `Finish` 이벤트를 내면 뒤따라 오는 진짜
    /// 종료 프레임이 값을 덮어씁니다. 최종 판단은 소비자가 마지막에 합니다.
    pub truncated: bool,
    /// 사내가 토큰 수를 실어 보냈다면 그 값. 오늘은 항상 `None` 입니다
    /// (`FabrixChunk::upstream_tokens` 참고).
    pub upstream_tokens: Option<(u32, u32)>,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 지금까지 합쳐진 전체 답변.
    pub fn text(&self) -> &str {
        &self.acc
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

        // 표준 SSE 만 받습니다. 벤더 샘플이 `sseclient.SSEClient(response)` 를 쓰는 것이
        // 근거입니다 — `data:` 프레이밍이 확정입니다. 예전에는 `data:` 없이 개행 구분
        // JSON 을 흘리는 서버도 받아들였는데, 그러면 아무 텍스트 줄이나 프레임으로
        // 오인할 여지가 남습니다.
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            return; // `event:` · `id:` · `retry:` 등 다른 SSE 필드
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
        // 토큰 수는 보통 마지막 프레임에만 오지만, 어느 프레임에 실릴지 스펙에 없어
        // 나오는 대로 기억합니다(나중 값이 이깁니다).
        if let Some(tokens) = chunk.upstream_tokens() {
            self.upstream_tokens = Some(tokens);
        }
        // 텍스트 누적은 소비자(`proxy::turn::Turn`)가 합니다 — 디코더가 따로 모으면 두
        // 곳이 어긋납니다. `acc` 는 로그·테스트가 보는 전체 답변용으로만 씁니다.
        if let Some(reasoning) = chunk.reasoning_content.as_deref() {
            if !reasoning.is_empty() {
                out.push(StreamEvent::Reasoning(reasoning.to_string()));
            }
        }
        if let Some(content) = chunk.content.as_deref().filter(|c| !c.is_empty()) {
            self.acc.push_str(content);
            out.push(StreamEvent::Delta(content.to_string()));
        }
        if let Some(reason) = chunk.finish_reason.as_deref().filter(|r| !r.is_empty()) {
            self.finish_reason = Some(reason.to_string());
            out.push(StreamEvent::Finish(reason.to_string()));
        }
    }
}

/// 비스트림 응답 한 덩어리를 스트림과 **같은 이벤트 열**로 바꿉니다.
///
/// 이 함수가 있어서 비스트림 경로가 자기만의 조립 코드를 갖지 않습니다 — 두 경로가
/// 같은 상태 기계(`proxy::turn::Turn`)를 지나가는 것이 이 변환의 목적입니다.
/// 순서는 스트림 프레임과 같습니다: 추론 → 본문 → 종료.
///
/// `contentReferences`·`eventData` 폴백은 **여기서만** 일어납니다(스트림 프레임에는
/// 적용하지 않습니다). 실서버 와이어를 확인하면 줄어드는 것도 이 함수 하나입니다.
pub fn nonstream_events(chunk: &FabrixChunk) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    if let Some(reasoning) = chunk.reasoning_content.as_deref().filter(|s| !s.is_empty()) {
        out.push(StreamEvent::Reasoning(reasoning.to_string()));
    }
    if let Some(text) = chunk.answer_text().filter(|s| !s.is_empty()) {
        out.push(StreamEvent::Delta(text));
    }
    // 공백만 있는 사유로 이벤트를 만들지 않습니다 — `map_finish_reason` 이 어차피
    // 트림해서 버리므로 와이어 결과는 같고, 빈 이벤트만 사라집니다.
    if let Some(reason) = chunk.finish_reason.as_deref().filter(|r| !r.trim().is_empty()) {
        out.push(StreamEvent::Finish(reason.to_string()));
    }
    out
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

    /// 기계가 분기하는 값. `type` 은 상태 코드에서 유도되므로(`proxy::openai_type`)
    /// 우리 고유의 구분은 여기 담습니다 — 예전 `kind()` 가 `type` 자리에 넣던
    /// 비표준 값(`upstream_error` · `configuration_error`)이 이쪽으로 옮겨온 것입니다.
    pub fn code(&self) -> &'static str {
        match self {
            FabrixError::NotConfigured => "not_configured",
            FabrixError::Unreachable(_) => "upstream_unreachable",
            FabrixError::Quota(_) => "rate_limit_exceeded",
            FabrixError::Upstream { .. } => "upstream_error",
            FabrixError::BadPayload(_) => "upstream_bad_response",
        }
    }

    /// 응답 봉투 하나. 호출부가 `status`/`message`/`code` 를 따로 엮지 않게 모아 둡니다.
    pub fn envelope(&self) -> crate::openai::ErrorEnvelope {
        crate::openai::ErrorEnvelope::new(
            self.message(),
            super::openai_type(self.status()),
            Some(self.code().to_string()),
        )
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

pub fn classify_status(status: u16, body: &str) -> FabrixError {
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

    /// 카멜케이스 표기도 받습니다 — 문서상 비스트림이 카멜이고, `isStream=false` 인데
    /// SSE 를 흘리는 상위를 위해 디코더도 양쪽을 받습니다.
    #[test]
    fn parses_camel_case_field_names() {
        let mut d = StreamDecoder::new();
        let out = events(
            &mut d,
            "data: {\"content\":\"A\"}\ndata: {\"content\":\"B\",\"finishReason\":\"stop\"}\n",
        );
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

    /// `eventData` 는 **답변이 아닙니다** — 문서가 `Event Data` 라고만 합니다. 내부
    /// 이벤트 문자열이 assistant 답변으로 나가면 안 됩니다.
    #[test]
    fn nostream_does_not_treat_event_data_as_the_answer() {
        let raw = r#"{
            "content": null,
            "contentReferences": [],
            "eventData": "이벤트 데이터",
            "status": "SUCCESS"
        }"#;
        let chunk = parse_nostream(raw);
        assert_eq!(chunk.answer_text(), None);
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

    fn two_models() -> Vec<ResolvedModel> {
        build_aliases(&[
            FabrixModel {
                model_id: "uuid-1".into(),
                name: vec![LocalizedText {
                    language_code: Some("en".into()),
                    content: Some("Chat 4".into()),
                }],
                description: vec![],
            },
            FabrixModel {
                model_id: "uuid-2".into(),
                name: vec![LocalizedText {
                    language_code: Some("en".into()),
                    content: Some("Chat Lite".into()),
                }],
                description: vec![],
            },
        ])
    }

    /// 클라이언트가 열거형으로 파싱하는 값이라 규약 밖 값이 절대 나가면 안 됩니다.
    #[test]
    fn clamp_never_leaks_a_non_openai_value() {
        const LEGAL: &[&str] = &["stop", "length", "tool_calls", "content_filter", "function_call"];
        for raw in [
            Some("stop"),
            Some("length"),
            Some("tool_calls"),
            Some("content_filter"),
            Some("function_call"),
            Some("weird"),
            Some(""),
            Some("   "),
            Some("STOP"),
            Some("사내값"),
            None,
        ] {
            let out = clamp_finish_reason(raw);
            assert!(LEGAL.contains(&out), "{raw:?} → {out}");
        }
        // 열거값은 그대로 통과합니다.
        assert_eq!(clamp_finish_reason(Some("tool_calls")), "tool_calls");
        assert_eq!(clamp_finish_reason(Some("content_filter")), "content_filter");
        // 모르는 값과 없는 값은 stop.
        assert_eq!(clamp_finish_reason(Some("weird")), "stop");
        assert_eq!(clamp_finish_reason(None), "stop");
    }

    /// 중단은 `stop` 이 아닙니다 — 끊긴 답변을 완성된 것처럼 부르면 안 됩니다.
    #[test]
    fn abortish_values_clamp_to_length() {
        for raw in ["abort", "aborted", "cancelled", "canceled", "error", "timeout", "incomplete"] {
            assert_eq!(clamp_finish_reason(Some(raw)), "length", "{raw}");
        }
    }

    /// 빈 답변을 200 으로 볼지 502 로 볼지 가르는 판단.
    #[test]
    fn looks_successful_distinguishes_an_empty_answer_from_garbage() {
        let of = |raw: &str| {
            serde_json::from_str::<FabrixChunk>(raw).unwrap().looks_successful()
        };
        // 성공 표지가 있으면 답변이 비어도 성공입니다.
        assert!(of(r#"{"content":"","status":"SUCCESS"}"#));
        assert!(of(r#"{"content":null,"finishReason":"stop"}"#));
        assert!(of(r#"{"responseCode":"200"}"#));
        // 표지가 하나도 없으면 우리가 못 알아본 본문입니다.
        assert!(!of(r#"{}"#));
        assert!(!of(r#"{"content":""}"#));
        assert!(!of(r#"{"status":"   "}"#));
        // 오류 표지가 있으면 성공이 아닙니다.
        assert!(!of(r#"{"status":"ERROR","finishReason":"stop"}"#));
    }

    /// 다섯 변형 모두 합법 `type` + 우리 고유의 `code` 를 내야 합니다.
    #[test]
    fn every_error_variant_maps_to_a_legal_type_and_a_code() {
        let cases = [
            (FabrixError::NotConfigured, 503, "api_error", "not_configured"),
            (FabrixError::Unreachable("t".into()), 502, "api_error", "upstream_unreachable"),
            (FabrixError::Quota("q".into()), 429, "rate_limit_error", "rate_limit_exceeded"),
            (
                FabrixError::Upstream { status: 418, message: "teapot".into() },
                418,
                "invalid_request_error",
                "upstream_error",
            ),
            (FabrixError::BadPayload("p".into()), 502, "api_error", "upstream_bad_response"),
        ];
        for (err, status, kind, code) in cases {
            assert_eq!(err.status(), status, "{err:?}");
            assert_eq!(err.code(), code, "{err:?}");
            let env = err.envelope();
            assert_eq!(env.error.kind, kind, "{err:?}");
            assert_eq!(env.error.code.as_deref(), Some(code), "{err:?}");
            // 사람이 읽는 메시지는 한국어 그대로 남습니다.
            assert!(!env.error.message.is_empty());
        }
    }

    #[test]
    fn find_model_hits_by_uuid_alias_and_ignoring_case() {
        let models = two_models();
        assert_eq!(find_model(&models, "uuid-1").unwrap().alias, "fabrix-chat-4");
        assert_eq!(find_model(&models, "fabrix-chat-lite").unwrap().model_id, "uuid-2");
        assert_eq!(find_model(&models, "FABRIX-Chat-4").unwrap().model_id, "uuid-1");
        // 앞뒤 공백은 다듬습니다.
        assert_eq!(find_model(&models, "  fabrix-chat-4  ").unwrap().model_id, "uuid-1");
    }

    /// 이 함수의 존재 이유입니다 — 모르는 이름에 아무 모델도 돌려주지 않아야 합니다.
    #[test]
    fn find_model_never_falls_back() {
        let models = two_models();
        assert!(find_model(&models, "gpt-4o").is_none());
        assert!(find_model(&models, "").is_none());
        assert!(find_model(&models, "   ").is_none());
    }

    #[test]
    fn default_model_prefers_the_configured_alias_then_the_first() {
        let models = two_models();
        assert_eq!(default_model(&models, "fabrix-chat-lite").unwrap().model_id, "uuid-2");
        // 설정이 비었거나 목록에 없으면 첫 모델.
        assert_eq!(default_model(&models, "").unwrap().model_id, "uuid-1");
        assert_eq!(default_model(&models, "fabrix-nope").unwrap().model_id, "uuid-1");
        assert!(default_model(&[], "fabrix-chat-4").is_none());
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

    // ── contents 는 턴 배열 (위치가 롤) ──

    fn msg(json: &str) -> crate::openai::Message {
        serde_json::from_str(json).unwrap()
    }

    /// 벤더 샘플과 **같은 대화**로 못박습니다:
    /// `["안녕하세요?", "네 안녕하세요", "내 이름은 LCY인데 너 이름은 뭐니?"]`
    #[test]
    fn multi_turn_becomes_one_element_per_turn() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"안녕하세요?"}"#),
            msg(r#"{"role":"assistant","content":"네 안녕하세요"}"#),
            msg(r#"{"role":"user","content":"내 이름은 LCY인데 너 이름은 뭐니?"}"#),
        ];
        let (system, contents) = fold_messages(&msgs);
        assert!(system.is_none());
        assert_eq!(
            contents,
            vec![
                "안녕하세요?".to_string(),
                "네 안녕하세요".to_string(),
                "내 이름은 LCY인데 너 이름은 뭐니?".to_string(),
            ]
        );
        // 라벨을 붙이지 않습니다 — 위치가 롤입니다.
        assert!(!contents.iter().any(|c| c.starts_with("User:") || c.starts_with("Assistant:")));
    }

    /// 연속 동일 롤을 병합하지 않으면 원소가 하나 밀려 그 뒤 전체의 롤이 뒤집힙니다.
    /// 이 테스트가 교대 보장을 지킵니다.
    #[test]
    fn consecutive_same_role_messages_merge_to_keep_alternation() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"첫 질문"}"#),
            msg(r#"{"role":"user","content":"덧붙임"}"#),
            msg(r#"{"role":"assistant","content":"답 앞"}"#),
            msg(r#"{"role":"assistant","content":"답 뒤"}"#),
            msg(r#"{"role":"user","content":"마지막"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert_eq!(
            contents,
            vec![
                "첫 질문\n\n덧붙임".to_string(),
                "답 앞\n\n답 뒤".to_string(),
                "마지막".to_string(),
            ]
        );
        assert!(last_is_user_turn(&contents));
    }

    /// 대화가 assistant 로 시작하면 앞에 user 턴을 넣어 짝을 맞춥니다 — 안 하면 그
    /// assistant 발화가 user 자리에 앉습니다.
    #[test]
    fn a_conversation_starting_with_assistant_gets_a_continue_turn() {
        let msgs = vec![
            msg(r#"{"role":"assistant","content":"이어서 말하자면"}"#),
            msg(r#"{"role":"user","content":"계속해"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert_eq!(
            contents,
            vec![
                CONTINUE_TURN.to_string(),
                "이어서 말하자면".to_string(),
                "계속해".to_string(),
            ]
        );
    }

    #[test]
    fn a_single_user_turn_is_one_bare_element() {
        let (_, contents) = fold_messages(&[msg(r#"{"role":"user","content":"안녕"}"#)]);
        assert_eq!(contents, vec!["안녕".to_string()]);
        assert!(last_is_user_turn(&contents));
    }

    #[test]
    fn last_is_user_turn_tracks_the_alternation() {
        assert!(last_is_user_turn(&["u".into()]));
        assert!(!last_is_user_turn(&["u".into(), "a".into()]));
        assert!(last_is_user_turn(&["u".into(), "a".into(), "u".into()]));
        // 빈 배열에는 붙일 자리가 없습니다.
        assert!(!last_is_user_turn(&[]));
    }

    // ── 도구 대화 평탄화 ──

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
        // user / assistant(호출) / user(결과) — 도구 결과는 user 자리입니다.
        assert_eq!(contents.len(), 3, "{contents:?}");
        assert_eq!(contents[0], "페이지 만들어줘");
        let call = &contents[1];
        assert!(call.contains("<tool_call>"), "도구 호출이 사라졌습니다:\n{call}");
        assert!(call.contains("\"name\":\"write\""), "{call}");
        assert!(call.contains("call_a1"), "{call}");
        assert!(contents[2].contains("wrote 12 bytes"), "{}", contents[2]);
        assert!(last_is_user_turn(&contents));
    }

    /// 도구 결과 줄은 자기 머리말을 이미 갖고 있습니다. 롤 라벨을 또 붙이면
    /// `Tool: Tool result (id=…)` 가 됩니다.
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
        let result = &contents[2];
        assert!(!result.contains("Tool: Tool result"), "이중 라벨:\n{result}");
        // 이름은 호출을 낸 assistant 턴에서 끌어옵니다.
        assert!(result.contains("Tool result (id=c1, name=read)"), "{result}");
    }

    /// 병렬 호출의 결과는 assistant 턴 하나 뒤에 연달아 오므로 **한 user 원소로
    /// 병합**되어야 합니다 — 따로 두면 두 번째 결과가 assistant 자리에 앉습니다.
    #[test]
    fn parallel_call_results_merge_into_one_user_turn() {
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
        assert_eq!(contents.len(), 3, "결과가 교대를 깨뜨렸습니다: {contents:?}");
        let results = &contents[2];
        // 결과가 호출 순서와 다르게 와도 id 로 짝지어져야 합니다.
        assert!(results.contains("Tool result (id=c2, name=read)"), "{results}");
        assert!(results.contains("Tool result (id=c1, name=write)"), "{results}");
        assert!(last_is_user_turn(&contents));
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

    /// 모르는 롤은 user 자리에 머리말을 붙여 실립니다 — 사내에 자리가 user/assistant
    /// 둘뿐이라, 버리는 것보다 누가 말했는지 적는 편이 낫습니다. 앞의 user 턴과 연달아
    /// 있으므로 한 원소로 병합됩니다.
    #[test]
    fn unknown_roles_keep_their_label_inside_a_user_turn() {
        let msgs = vec![
            msg(r#"{"role":"user","content":"안녕"}"#),
            msg(r#"{"role":"critic","content":"별로"}"#),
        ];
        let (_, contents) = fold_messages(&msgs);
        assert_eq!(contents, vec!["안녕\n\ncritic: 별로".to_string()]);
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
        // 이 함수는 원문 보존용입니다 — 모르는 값을 그대로 넘깁니다.
        // 와이어로 나가기 전에 `clamp_finish_reason` 이 접습니다.
        assert_eq!(map_finish_reason(Some("weird"), false).as_deref(), Some("weird"));
        assert_eq!(map_finish_reason(Some("   "), false), None);
        assert_eq!(map_finish_reason(None, false), None);
    }

    // ── llmConfig ──

    fn llm_config(body: &str) -> Value {
        let req: crate::openai::ChatRequest = serde_json::from_str(body).unwrap();
        serde_json::to_value(LlmConfig::from_request(&req).unwrap()).unwrap()
    }

    /// 문서와 샘플의 철자가 달라 **양쪽 다** 보냅니다. 한쪽만 보내면 서버가 다른 쪽을
    /// 읽는 경우 값이 조용히 버려집니다 — 지금까지 그럴 수 있었습니다.
    #[test]
    fn llm_config_sends_both_spellings() {
        let cfg = llm_config(
            r#"{"messages":[{"role":"user","content":"hi"}],"frequency_penalty":1.04,"top_k":14}"#,
        );
        assert_eq!(cfg["repetition_penalty"], 1.04, "샘플 철자: {cfg}");
        assert_eq!(cfg["repetion_penalty"], 1.04, "문서 철자: {cfg}");
        assert_eq!(cfg["top_k"], 14, "샘플 철자: {cfg}");
        assert_eq!(cfg["tok_k"], 14, "문서 철자: {cfg}");
    }

    /// 안 보낸 값은 두 철자 모두 키가 없어야 합니다 — 빈 키를 보내면 서버가 기본값을
    /// 덮어쓸 수 있습니다.
    #[test]
    fn llm_config_omits_both_spellings_when_absent() {
        let cfg = llm_config(r#"{"messages":[{"role":"user","content":"hi"}],"temperature":0.4}"#);
        for key in ["repetition_penalty", "repetion_penalty", "top_k", "tok_k"] {
            assert!(cfg.get(key).is_none(), "{key} 가 실려 나갔습니다: {cfg}");
        }
        assert_eq!(cfg["temperature"], 0.4);
    }

    /// 문서가 사내 `temperature` 를 0–1 로 못박습니다. OpenAI 는 0–2 라 그 범위를
    /// 기본값으로 보내는 클라이언트가 많아, 거절하지 않고 줄여서 보냅니다.
    #[test]
    fn temperature_is_clamped_to_the_fabrix_ceiling() {
        let cfg = llm_config(r#"{"messages":[{"role":"user","content":"hi"}],"temperature":1.5}"#);
        assert_eq!(cfg["temperature"], 1.0);

        // 범위 안의 값은 그대로.
        let ok = llm_config(r#"{"messages":[{"role":"user","content":"hi"}],"temperature":0.4}"#);
        assert_eq!(ok["temperature"], 0.4);
    }

    /// 줄였을 때만 로그 한 줄이 붙어야 합니다.
    #[test]
    fn temperature_clamp_is_reported_only_when_it_happens() {
        assert_eq!(temperature_was_clamped(Some(1.5)), Some((1.5, 1.0)));
        assert_eq!(temperature_was_clamped(Some(2.0)), Some((2.0, 1.0)));
        assert_eq!(temperature_was_clamped(Some(1.0)), None);
        assert_eq!(temperature_was_clamped(Some(0.4)), None);
        assert_eq!(temperature_was_clamped(None), None);
    }

    /// 두 경로가 공유하는 유일한 종료 사유 판단. 표로 못박아 둡니다.
    #[test]
    fn decide_finish_puts_tool_calls_first() {
        // 도구 호출이 있으면 절단이든 상위 사유든 이깁니다 — 뽑은 호출은 실행 가능합니다.
        assert_eq!(decide_finish(true, Some("stop"), false), "tool_calls");
        assert_eq!(decide_finish(true, None, true), "tool_calls");
        assert_eq!(decide_finish(true, Some("weird"), true), "tool_calls");
        // 호출이 없으면 절단이 상위 사유를 이깁니다.
        assert_eq!(decide_finish(false, Some("stop"), true), "length");
        // 상위가 모르는 값을 줘도 와이어에는 열거값만 나갑니다.
        assert_eq!(decide_finish(false, Some("weird"), false), "stop");
        assert_eq!(decide_finish(false, Some("content_filter"), false), "content_filter");
        assert_eq!(decide_finish(false, None, false), "stop");
    }

    /// 스트림 중단은 호출부가 `"error"` 로 알리고, clamp 가 `length` 로 접습니다 —
    /// 끊긴 답변을 완성된 것처럼 `stop` 으로 부르면 안 됩니다.
    #[test]
    fn decide_finish_folds_a_midstream_break_to_length() {
        assert_eq!(decide_finish(false, Some("error"), false), "length");
        // 다만 이미 완성된 호출을 뽑았다면 그것이 우선입니다 — 사장시킬 이유가 없습니다.
        assert_eq!(decide_finish(true, Some("error"), false), "tool_calls");
    }

    /// 비스트림 본문도 스트림과 **같은 순서**의 이벤트가 되어야 합니다.
    #[test]
    fn nonstream_events_order_reasoning_then_content_then_finish() {
        let chunk = FabrixChunk {
            content: Some("답변".into()),
            reasoning_content: Some("생각".into()),
            finish_reason: Some("stop".into()),
            ..FabrixChunk::default()
        };
        assert_eq!(
            nonstream_events(&chunk),
            vec![
                StreamEvent::Reasoning("생각".into()),
                StreamEvent::Delta("답변".into()),
                StreamEvent::Finish("stop".into()),
            ]
        );
    }

    #[test]
    fn nonstream_events_skip_empty_fields() {
        let chunk = FabrixChunk {
            content: Some(String::new()),
            reasoning_content: Some(String::new()),
            finish_reason: Some("   ".into()),
            ..FabrixChunk::default()
        };
        // 빈 값으로 이벤트를 만들면 소비자가 빈 델타 청크를 내보냅니다.
        assert_eq!(nonstream_events(&chunk), Vec::new());
    }

    /// 추론만 있고 본문이 빈 응답 — 이번 버그의 실제 모양입니다.
    #[test]
    fn nonstream_events_carry_reasoning_only_answers() {
        let chunk = FabrixChunk {
            content: None,
            reasoning_content: Some("<tool_call>{\"name\":\"read\"}</tool_call>".into()),
            ..FabrixChunk::default()
        };
        assert_eq!(
            nonstream_events(&chunk),
            vec![StreamEvent::Reasoning("<tool_call>{\"name\":\"read\"}</tool_call>".into())]
        );
    }

    #[test]
    fn decoder_records_the_truncated_flag() {
        let mut d = StreamDecoder::new();
        d.push(b"data: {\"content\":\"ab\"}\n");
        assert!(!d.truncated);
        d.push(b"data: {\"content\":\"cd\",\"truncated\":true}\n");
        assert!(d.truncated);
    }

    /// `content` 는 증분입니다 — 벤더 샘플이 `result_message += …` 로 이어 붙입니다.
    /// 프레임이 접두사처럼 보여도 누적으로 오판하면 안 됩니다.
    #[test]
    fn content_frames_are_always_incremental() {
        let mut d = StreamDecoder::new();
        assert_eq!(
            d.push("data: {\"content\":\"안\"}\n".as_bytes()),
            vec![StreamEvent::Delta("안".into())]
        );
        assert_eq!(
            d.push("data: {\"content\":\"녕\"}\n".as_bytes()),
            vec![StreamEvent::Delta("녕".into())]
        );
        // "안" 다음에 "안녕" 이 와도 접두사 판별을 하지 않습니다 — 증분이니 그대로 잇습니다.
        assert_eq!(
            d.push("data: {\"content\":\"안녕\"}\n".as_bytes()),
            vec![StreamEvent::Delta("안녕".into())]
        );
        assert_eq!(d.text(), "안녕안녕");
    }

    /// `data:` 없는 줄은 프레임이 아닙니다 — 벤더가 표준 SSE(`sseclient`)를 씁니다.
    #[test]
    fn lines_without_the_data_prefix_are_ignored() {
        let mut d = StreamDecoder::new();
        assert_eq!(d.push("{\"content\":\"생 JSON\"}\n".as_bytes()), Vec::new());
        assert_eq!(d.push("event: message\n".as_bytes()), Vec::new());
        assert_eq!(d.push(": keep-alive 주석\n".as_bytes()), Vec::new());
        assert_eq!(d.text(), "");
        // 제대로 된 프레임만 통과합니다.
        assert_eq!(
            d.push("data: {\"content\":\"본문\"}\n".as_bytes()),
            vec![StreamEvent::Delta("본문".into())]
        );
    }

    // ── 디코더 + 스캐너 결합 (펌프가 실제로 하는 일) ──

    /// 목업이 `MOCK_CHUNK=3` 으로 흘리는 것과 같은 모양의 SSE 프레임을 만듭니다.
    /// 센티널 한가운데가 갈립니다: `.\n<` `too` `l_c` `all` `>\n{` …
    ///
    /// `key` 로 채널을 고릅니다 — `content` 또는 `reasoning_content`.
    fn sse_frames(body: &str, chunk: usize, key: &str) -> Vec<String> {
        body.chars()
            .collect::<Vec<char>>()
            .chunks(chunk)
            .map(|piece| {
                let piece: String = piece.iter().collect();
                format!("data: {}\n", serde_json::json!({ key: piece }))
            })
            .collect()
    }

    /// 프레임을 디코더에 먹이고 스캐너까지 태워, 펌프가 내보낼 텍스트와 도구 호출을
    /// 그대로 재현합니다.
    fn decode_and_scan(frames: &[String], names: &[&str]) -> (String, Vec<(u32, String, String)>) {
        use super::super::tools::{Channel, ScanOut, ToolCallScanner};
        let mut decoder = StreamDecoder::new();
        let mut scanner =
            ToolCallScanner::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>(), true);
        let mut text = String::new();
        let mut calls = Vec::new();

        let absorb = |out: ScanOut, text: &mut String, calls: &mut Vec<(u32, String, String)>| {
            text.push_str(&out.text);
            for c in out.calls {
                calls.push((c.index, c.name, c.arguments));
            }
        };

        let mut events: Vec<StreamEvent> =
            frames.iter().flat_map(|f| decoder.push(f.as_bytes())).collect();
        events.extend(decoder.finish());
        for event in events {
            match event {
                StreamEvent::Delta(d) => {
                    absorb(scanner.push_on(Channel::Content, &d), &mut text, &mut calls)
                }
                StreamEvent::Reasoning(r) => {
                    absorb(scanner.push_on(Channel::Reasoning, &r), &mut text, &mut calls)
                }
                _ => {}
            }
        }
        absorb(scanner.finish_on(Channel::Content), &mut text, &mut calls);
        absorb(scanner.finish_on(Channel::Reasoning), &mut text, &mut calls);
        (text, calls)
    }

    const TOOL_BODY: &str = "만들겠습니다.\n<tool_call>\n{\"name\":\"write\",\"arguments\":{\"filePath\":\"index.html\",\"content\":\"<!doctype html>\"}}\n</tool_call>";

    /// 프레임 경계가 어디로 떨어져도, **어느 채널로 와도** 같은 결과여야 합니다.
    /// 추론 채널 축이 이번에 추가된 것입니다 — 예전에는 누적/증분 축이 있었지만
    /// 실서버가 증분으로 확정되어 그 축은 사라졌습니다.
    #[test]
    fn tool_call_survives_frame_splitting_on_both_channels() {
        for key in ["content", "reasoning_content"] {
            for chunk in [1usize, 3, 7, 200] {
                let frames = sse_frames(TOOL_BODY, chunk, key);
                let (text, calls) = decode_and_scan(&frames, &["write", "read"]);
                let label = format!("key={key} chunk={chunk}");

                assert_eq!(calls.len(), 1, "{label} — 도구 호출 수");
                assert_eq!(calls[0].0, 0, "{label} — index");
                assert_eq!(calls[0].1, "write", "{label} — 이름");

                let args: Value = serde_json::from_str(&calls[0].2)
                    .unwrap_or_else(|e| panic!("{label} — arguments 가 JSON 이 아님: {e}"));
                assert_eq!(args["filePath"], "index.html", "{label}");
                assert_eq!(args["content"], "<!doctype html>", "{label}");

                assert!(text.contains("만들겠습니다."), "{label} — 앞 산문 유실");
                assert!(!text.contains("tool_call"), "{label} — 센티널이 본문에 남음: {text}");
            }
        }
    }

    #[test]
    fn ordinary_answer_is_untouched_by_the_scanner() {
        for key in ["content", "reasoning_content"] {
            let frames = sse_frames(ANSWER_SAMPLE, 3, key);
            let (text, calls) = decode_and_scan(&frames, &["write"]);
            assert!(calls.is_empty(), "key={key}");
            assert_eq!(text, ANSWER_SAMPLE, "key={key}");
        }
    }

    const ANSWER_SAMPLE: &str = "시연차는 입사 1년 차에 15일이 부여됩니다. 자세한 내용은 규정을 보세요.";
}
