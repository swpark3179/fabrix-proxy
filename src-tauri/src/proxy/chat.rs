//! `POST /v1/chat/completions` — 프록시의 심장.
//!
//! 흐름: 받은 요청(OpenAI) → 변환해서 보낸 요청(FabriX) → 돌려준 응답.
//! 로그 창의 세 칸이 정확히 이 세 단계입니다.

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::Value;
use uuid::Uuid;

use crate::logstore::{self, Kind, LogEntry, RawBuf, RawWire};
use crate::openai::{
    AssistantMessage, ChatChunk, ChatCompletion, ChatRequest, Choice, Delta, ErrorEnvelope,
    ToolCall, ToolCallDelta,
};
use crate::state::{self, Shared};

use super::fabrix::{
    self, default_model, extract_object, find_model, fold_messages, nonstream_events, FabrixChunk,
    FabrixError, LlmConfig, MessagesRequest, ResolvedModel, StreamDecoder, MESSAGES_PATH,
};
use super::models::ensure_models;
use super::tools;
use super::turn::{Emit, Piece, ToolStats, Turn};
use super::usage;
use super::validate;
use super::{
    authorize, error_response, fabrix_headers_line, openai_type, pretty, read_body,
    CHAT_BODY_LIMIT,
};

/// 로그 한 건을 조립하는 데 필요한 것들. 스트리밍 제너레이터 안으로 통째로
/// 옮겨가므로 소유 값만 담습니다.
struct Ctx {
    started: Instant,
    stream: bool,
    client: Option<String>,
    model_requested: Option<String>,
    model_alias: Option<String>,
    model_id: Option<String>,
    model_label: Option<String>,
    req_openai: String,
    req_fabrix: String,
    req_fabrix_headers: String,
    fabrix_url: String,
    /// 클라이언트가 선언한 도구 수. 0 이면 도구를 쓰지 않는 평범한 채팅입니다.
    tools_declared: usize,
    /// 그 도구를 규약으로 접어 실제로 보냈는가(설정으로 끌 수 있습니다).
    tools_emulated: bool,
    /// 클라이언트가 `tool_choice: "none"` 으로 스스로 껐는가.
    ///
    /// 프록시 설정으로 끈 것과 구분해야 합니다 — 예전에는 둘 다 로그에
    /// "에뮬레이션 꺼짐" 으로 찍혀, 사용자가 자기 요청을 의심할 수 없었습니다.
    tools_choice_none: bool,
    /// 검증을 통과한 요청에서 뽑아낸 실행 계획 — 무시한 필드와 버린 이미지 파트를
    /// 로그 ③ 칸까지 들고 갑니다.
    plan: validate::Plan,
    /// `model` 을 아예 안 보내 기본 모델로 처리했는가. 없는 이름을 보낸 것과 구분해야
    /// 하므로(그건 이제 404 입니다) 따로 들고 있습니다.
    model_defaulted: bool,
    /// 클라이언트가 보낸 `temperature` 원값. 사내 상한(0–1)으로 줄였는지 로그에 적는 데
    /// 씁니다 — 나간 값은 `req_fabrix` 에 있으니 여기엔 **요청 원값**을 둡니다.
    temperature_requested: Option<f64>,
    /// 실제로 사내에 보낸 프롬프트 전문(systemPrompt + contents).
    ///
    /// 클라이언트의 `messages` 가 아니라 이걸로 토큰을 추정합니다 — 모델이 실제로 본
    /// 글이 이것이고, 주입된 도구 규약까지 포함해야 값이 맞습니다.
    prompt_text: String,
    /// 사내가 준 바이트 그대로. **설정과 무관하게 언제나 담습니다.**
    ///
    /// 로그 ③ 칸은 이미 **가공된** 답변입니다(`<think>` 를 갈라내고 `<tool_call>` 을
    /// 걷어낸 뒤). 그래서 "0자" 가 나왔을 때 모델이 아무 말도 안 한 것인지, 우리가
    /// 프레임을 못 읽은 것인지 구분할 방법이 없었습니다. 이 두 버퍼가 그 자리입니다.
    ///
    /// 이쪽만 토글에서 뺀 이유: ③ 칸의 "사내 원문 보기" 는 사용자가 답변을 의심하는
    /// **바로 그때** 눌리는 버튼입니다. 그 순간 설정이 꺼져 있었다면 되살릴 방법이
    /// 없고, 재현되지 않는 한 번짜리 응답이 특히 그렇습니다.
    raw_upstream: RawBuf,
    /// 클라이언트로 나간 본문 그대로. 이쪽은 `rawWireLog` 토글이 제어합니다.
    raw_client: RawBuf,
}

/// 로그 꼬리에 붙일 도구 관련 한 줄.
///
/// "요청에 도구가 있었는데 프록시가 버렸다" 와 "도구는 전달됐는데 모델이 안 썼다" 를
/// 사용자가 구분할 수 있어야 합니다 — 지금까지는 둘 다 똑같이 조용히 실패했습니다.
fn tool_meta(
    declared: usize,
    emulated: bool,
    choice_none: bool,
    stats: &ToolStats,
) -> Option<String> {
    if declared == 0 {
        return None; // 평범한 채팅에는 아무 것도 붙이지 않습니다.
    }
    if !emulated {
        // 누가 껐는지가 다음 행동을 가릅니다 — 설정을 볼지, 요청을 볼지.
        return Some(if choice_none {
            format!("도구 {declared}개 선언 · 클라이언트가 tool_choice: none 으로 껐음")
        } else {
            format!("도구 {declared}개 선언 · 에뮬레이션 꺼짐 — 무시함")
        });
    }
    let mut line = match stats.calls {
        0 => format!("도구 {declared}개 선언 · 호출 0건 — 모델이 규약을 따르지 않음"),
        n => format!("도구 {declared}개 선언 · 호출 {n}건"),
    };
    // 이 숫자가 "추론 단계마다 stop" 수정이 실제로 물었는지를 말해 줍니다.
    if stats.in_reasoning > 0 {
        line.push_str(&format!(" · 그중 {}건은 추론 채널에서", stats.in_reasoning));
    }
    if stats.rejected > 0 {
        line.push_str(&format!(" · 형식 오류 {}건은 텍스트로 되돌림", stats.rejected));
    }
    Some(line)
}

/// 로그 꼬리에 붙일 추론 채널 한 줄.
///
/// 도구와 무관하게 일어나므로(`<think>` 분리는 평범한 채팅에서도 합니다) `tool_meta`
/// 밖에 둡니다. 추론이 없으면 아무 것도 붙이지 않아 기존 로그가 그대로 유지됩니다.
fn reasoning_meta(turn: &Turn) -> Option<String> {
    if turn.reasoning().is_empty() {
        return None;
    }
    let stats = turn.tool_stats();
    let mut line = format!("추론 {}자", turn.reasoning().chars().count());
    if stats.think_blocks > 0 {
        line.push_str(&format!(" · <think> {}개 분리", stats.think_blocks));
    }
    if stats.think_unclosed {
        line.push_str(" · <think> 가 닫히지 않음");
    }
    Some(line)
}

/// 로그 ③ 칸 본문. 추론이 있으면 두 칸으로 나눠 담습니다.
///
/// 스캐너가 걷어내기 **전** 원문이라 `<tool_call>` 유무를 눈으로 볼 수 있습니다.
/// 추론을 빼놓으면 호출이 추론 채널에서 나온 경우에 ③ 칸이 산문만 보여 주어,
/// "모델이 규약을 안 지켰다"와 구별할 방법이 없습니다 — 이번 버그가 그렇게 오래
/// 숨어 있었던 이유가 정확히 이것입니다.
fn log_body(turn: &Turn) -> String {
    let content = turn.raw_content();
    let reasoning = turn.raw_reasoning();
    if reasoning.is_empty() {
        return content.to_string();
    }
    format!("[추론]\n{reasoning}\n\n[답변]\n{content}")
}

/// 본문이 상한을 넘어 `Ctx` 를 만들기도 전에 끝난 호출의 로그 한 건.
///
/// 지금까지 이 실패는 axum 이 핸들러 밖에서 처리해 **로그에 흔적이 없었습니다**.
/// 사용자에게는 원인 없는 실패였습니다.
fn oversize_entry(
    started: Instant,
    client: Option<String>,
    cfg: &crate::config::Config,
    envelope: &ErrorEnvelope,
) -> LogEntry {
    LogEntry {
        id: Uuid::new_v4().to_string(),
        ts: state::now_hm(),
        ts_full: state::now_iso(),
        kind: Kind::Chat,
        method: Kind::Chat.method(),
        path: Kind::Chat.path().into(),
        status: 413,
        latency_ms: started.elapsed().as_millis() as u64,
        stream: false,
        cached: false,
        model_requested: None,
        model_alias: None,
        model_id: None,
        model_label: None,
        client,
        note: Some("본문이 너무 큼".into()),
        summary: Some("본문이 너무 큼".into()),
        is_error: true,
        req_openai: "(본문이 상한을 넘어 읽지 않았습니다)".into(),
        req_fabrix: "(사내 호출을 하지 않았습니다)".into(),
        req_fabrix_headers: fabrix_headers_line(cfg),
        fabrix_url: format!("{}{MESSAGES_PATH}", cfg.normalized_base_url()),
        resp_body: envelope.error.message.clone(),
        resp_meta: "거부 · HTTP 413".into(),
        // 사내 호출을 하지 않았고 응답도 우리가 지어낸 봉투뿐이라 원문이 없습니다.
        raw: RawWire::default(),
    }
}

/// 로그 꼬리에 붙일 "반영하지 못한 것" 줄들.
///
/// 조용히 버리는 것과 버렸다고 말하는 것의 차이가 이 함수의 존재 이유입니다 —
/// `tool_meta` 가 "도구를 버렸다"와 "모델이 안 썼다"를 가르는 것과 같은 이유입니다.
fn plan_meta(ctx: &Ctx) -> Vec<String> {
    let plan = &ctx.plan;
    let mut out = Vec::new();
    if ctx.model_defaulted {
        out.push("model 미지정 → 기본 모델".to_string());
    }
    if !plan.ignored.is_empty() {
        out.push(format!("무시된 파라미터: {}", plan.ignored.join(" · ")));
    }
    if plan.images_dropped > 0 {
        out.push(format!(
            "이미지 파트 {}개는 사내 채팅 API 가 받지 못해 버렸습니다",
            plan.images_dropped
        ));
    }
    if !plan.unknown.is_empty() {
        out.push(format!("스펙에 없는 키: {}", plan.unknown.join(" · ")));
    }
    // OpenAI 는 temperature 0–2, 사내는 0–1 입니다. 줄여서 보냈으면 그렇게 말합니다 —
    // 조용히 다른 값을 보내는 것이 이 줄이 막으려는 것입니다.
    if let Some((requested, sent)) = fabrix::temperature_was_clamped(ctx.temperature_requested) {
        out.push(format!("temperature {requested} → {sent} (사내 상한)"));
    }
    out
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    fn entry(
        &self,
        status: u16,
        is_error: bool,
        note: Option<String>,
        summary: Option<String>,
        resp_body: String,
        resp_meta: String,
    ) -> LogEntry {
        LogEntry {
            id: Uuid::new_v4().to_string(),
            ts: state::now_hm(),
            ts_full: state::now_iso(),
            kind: Kind::Chat,
            method: Kind::Chat.method(),
            path: Kind::Chat.path().into(),
            status,
            latency_ms: self.started.elapsed().as_millis() as u64,
            stream: self.stream,
            cached: false,
            model_requested: self.model_requested.clone(),
            model_alias: self.model_alias.clone(),
            model_id: self.model_id.clone(),
            model_label: self.model_label.clone(),
            client: self.client.clone(),
            note,
            summary,
            is_error,
            req_openai: self.req_openai.clone(),
            req_fabrix: self.req_fabrix.clone(),
            req_fabrix_headers: self.req_fabrix_headers.clone(),
            fabrix_url: self.fabrix_url.clone(),
            resp_body,
            resp_meta,
            raw: RawWire {
                upstream_captured: self.raw_upstream.enabled(),
                client_captured: self.raw_client.enabled(),
                upstream: self.raw_upstream.text(),
                client: self.raw_client.text(),
            },
        }
    }

    fn fail(&self, err: &FabrixError) -> LogEntry {
        self.entry(
            err.status(),
            true,
            Some(err.note()),
            Some(err.note()),
            err.message(),
            format!("실패 · HTTP {}", err.status()),
        )
    }

    /// 메인 창 "최근 호출" 오른쪽 칸: `gpt-4o → 챗 4`.
    fn success_summary(&self) -> Option<String> {
        match (&self.model_requested, &self.model_label) {
            (Some(req), Some(label)) if req != label => Some(format!("{req} → {label}")),
            (_, Some(label)) => Some(label.clone()),
            (Some(req), None) => Some(req.clone()),
            _ => None,
        }
    }
}

pub async fn handle(State(state): State<Shared>, headers: HeaderMap, body: Body) -> Response {
    let started = Instant::now();
    let cfg = state.config();

    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let client = logstore::short_client(ua);

    // 본문 상한을 우리가 겁니다 — 레이어에 맡기면 초과가 로그에 남지 않습니다.
    let body = match read_body(&headers, body, CHAT_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(envelope) => {
            state.record(oversize_entry(started, client, &cfg, &envelope));
            return error_response(413, envelope);
        }
    };

    // 인바운드 Authorization: 키발급없이 허용 모드면 값과 무관하게 통과,
    // 토큰 사용 모드면 발행 토큰과 일치할 때만 통과합니다. (아래 authorize 에서 검사)
    let incoming: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let mut ctx = Ctx {
        started,
        stream: false,
        client,
        model_requested: None,
        model_alias: None,
        model_id: None,
        model_label: None,
        req_openai: if incoming.is_null() {
            logstore::preview(&String::from_utf8_lossy(&body), 2000)
        } else {
            pretty(&incoming)
        },
        req_fabrix: "(요청을 변환하기 전에 실패했습니다)".into(),
        req_fabrix_headers: fabrix_headers_line(&cfg),
        fabrix_url: format!("{}{MESSAGES_PATH}", cfg.normalized_base_url()),
        tools_declared: 0,
        tools_emulated: false,
        tools_choice_none: false,
        plan: validate::Plan::default(),
        model_defaulted: false,
        temperature_requested: None,
        prompt_text: String::new(),
        // 사내가 준 쪽은 토글을 보지 않습니다 — ③ 칸의 원문 보기가 늘 동작해야 합니다.
        raw_upstream: RawBuf::new(true),
        raw_client: RawBuf::new(cfg.raw_wire_log),
    };

    // ── 토큰 검증 ───────────────────────────────────────────
    // 토큰 사용 모드에서 인바운드 토큰이 발행 토큰과 다르면 사내 호출 전에 거부합니다.
    if let Err((status, envelope)) = authorize(&cfg, &headers) {
        state.record(ctx.entry(
            status,
            true,
            Some("토큰 거부".into()),
            Some("토큰 거부".into()),
            envelope.error.message.clone(),
            format!("거부 · HTTP {status}"),
        ));
        return error_response(status, envelope);
    }

    // ── ① 받은 요청 ─────────────────────────────────────────
    let req: ChatRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => {
            // 응답에는 위치만 알려 줍니다. serde 의 원문에는 파싱하다 만 **요청 본문
            // 조각**이 섞여 나올 수 있어, 그대로 되돌려주면 클라이언트 로그로 흘러갑니다.
            // 진단에 필요한 전문은 로그 ③ 칸에만 담습니다.
            let msg = format!(
                "요청 본문을 JSON 으로 해석하지 못했습니다 (line {}, column {}).",
                err.line(),
                err.column()
            );
            state.record(ctx.entry(
                400,
                true,
                Some("잘못된 요청".into()),
                Some("잘못된 요청".into()),
                format!("{msg}\n\n{err}"),
                "요청 파싱 실패".into(),
            ));
            return error_response(
                400,
                ErrorEnvelope::new(msg, openai_type(400), Some("invalid_json".into())),
            );
        }
    };
    ctx.stream = req.is_stream();
    ctx.model_requested = req.model.clone();
    ctx.temperature_requested = req.temperature;

    // ── 요청 검증 ───────────────────────────────────────────
    // 규약 위반은 **사내 호출 전에** 걸러냅니다. 잘못된 요청이 사내 쿼터를 쓰거나
    // 사내 서버에 도달할 이유가 없습니다.
    let plan = match validate::plan(&req, &incoming) {
        Ok(plan) => plan,
        Err(invalid) => {
            state.record(ctx.entry(
                invalid.status,
                true,
                Some(invalid.note()),
                Some(invalid.note()),
                invalid.message.clone(),
                format!("요청 검증 실패 · HTTP {}", invalid.status),
            ));
            return error_response(invalid.status, invalid.envelope());
        }
    };
    ctx.plan = plan.clone();

    // ── 모델 해석 ───────────────────────────────────────────
    let models = match ensure_models(&state).await {
        Ok(loaded) => loaded.models,
        // 목록 조회의 응답 원문은 흘려보냅니다 — 이 로그 한 건의 ④ 칸은 채팅 호출의
        // 원문 자리이고, 목록 조회는 자기 로그(`/v1/models`)에 이미 남습니다.
        Err((err, _)) => {
            // 목록 조회가 실패해도 클라이언트가 UUID 를 직접 보냈다면 진행합니다.
            match req.model.as_deref().filter(|m| Uuid::parse_str(m).is_ok()) {
                Some(uuid) => vec![ResolvedModel {
                    alias: uuid.to_string(),
                    model_id: uuid.to_string(),
                    label: uuid.to_string(),
                    description: None,
                }],
                None => {
                    state.record(ctx.fail(&err));
                    return error_response(
                        err.status(),
                        err.envelope(),
                    );
                }
            }
        }
    };

    // 요청한 이름이 실제로 있는가. **폴백하지 않습니다** — 예전에는 없는 이름을
    // 조용히 기본 모델로 바꿔 주어, 오타가 성공처럼 보였습니다. 그 조용한 실패가
    // 규약 위반보다 나쁩니다.
    let requested = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let resolved = match requested {
        Some(name) => find_model(&models, name),
        // `model` 을 아예 안 보낸 요청에는 설정의 기본 모델을 씁니다.
        None => default_model(&models, &cfg.default_model_alias),
    };

    let Some(model) = resolved else {
        // 404 문구는 `/v1/models/{id}` 와 **같은 함수**를 씁니다 — 두 경로에서 다른 말을
        // 하면 사용자가 원인을 두 번 찾습니다.
        let (status, envelope) = match requested {
            Some(name) => (404, super::models::not_found_envelope(name)),
            None => (
                502,
                ErrorEnvelope::new(
                    "사내 모델 목록이 비어 있어 요청을 보낼 수 없습니다.",
                    openai_type(502),
                    Some("upstream_bad_response".into()),
                ),
            ),
        };
        state.record(ctx.entry(
            status,
            true,
            Some("모델 없음".into()),
            Some("모델 없음".into()),
            envelope.error.message.clone(),
            format!("실패 · HTTP {status} · 모델 없음"),
        ));
        return error_response(status, envelope);
    };
    if requested.is_none() {
        ctx.model_defaulted = true;
    }
    ctx.model_alias = Some(model.alias.clone());
    ctx.model_id = Some(model.model_id.clone());
    ctx.model_label = Some(model.label.clone());

    // ── ② 변환해서 보낸 요청 ─────────────────────────────────
    let (mut system_prompt, mut contents) = fold_messages(&req.messages);

    // 도구 에뮬레이션. FabriX 에 도구 필드가 없어 규약을 systemPrompt 뒤에 붙이고,
    // 답변에서 <tool_call> 을 걷어내 tool_calls 로 돌려줍니다.
    let declared = req.declared_tools();
    let tool_mode = req.tool_mode();
    let emulate = cfg.tool_emulation && req.wants_tools();
    let tool_names: HashSet<String> = if emulate {
        declared.iter().map(|f| f.name.trim().to_string()).collect()
    } else {
        HashSet::new()
    };
    if emulate {
        if let Some(block) = tools::render_system_block(&declared, &tool_mode) {
            system_prompt = Some(match system_prompt {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }
    }
    ctx.tools_declared = declared.len();
    ctx.tools_emulated = emulate;
    ctx.tools_choice_none = tool_mode == crate::openai::ToolChoiceMode::None;
    drop(declared);

    if contents.is_empty() {
        if system_prompt.is_some() {
            // 시스템/규약만 있고 사용자 턴이 없는 라운드(도구 강제 등). FabriX 는
            // contents 가 비면 거절하므로 최소 한 줄을 채웁니다.
            contents.push("(continue)".to_string());
        } else {
            let msg = "messages 에 보낼 내용이 없습니다.".to_string();
            state.record(ctx.entry(
                400,
                true,
                Some("빈 messages".into()),
                Some("빈 messages".into()),
                msg.clone(),
                "요청 검증 실패".into(),
            ));
            return error_response(
                400,
                ErrorEnvelope::new(msg, openai_type(400), Some("invalid_value".into()))
                    .with_param("messages"),
            );
        }
    }

    // 규약 형식을 **모델이 마지막으로 읽는 자리**에 한 번 더 못박습니다. 여기서
    // 붙이므로 로그 ② 칸과 토큰 추정(`ctx.prompt_text`)에 자동으로 포함됩니다.
    if emulate {
        if let Some(reminder) = tools::render_tail_reminder(&tool_mode) {
            // 리마인더는 **user 자리**에 있어야 합니다. assistant 자리에 넣으면 지시문이
            // 모델 자신의 발화가 되어, 모델은 자기가 이미 그렇게 말했다고 읽습니다.
            if fabrix::last_is_user_turn(&contents) {
                let last = contents.last_mut().expect("길이가 홀수면 원소가 있습니다");
                last.push_str("\n\n");
                last.push_str(&reminder);
            } else {
                contents.push(reminder);
            }
        }
    }

    let payload = MessagesRequest {
        model_ids: vec![model.model_id.clone()],
        contents,
        is_stream: req.is_stream(),
        system_prompt,
        llm_config: LlmConfig::from_request(&req),
    };
    ctx.req_fabrix = serde_json::to_string_pretty(&payload).unwrap_or_default();
    // 토큰 추정의 입력 — 클라이언트가 보낸 messages 가 아니라 **모델이 실제로 본 글**
    // 입니다(주입된 도구 규약 포함).
    ctx.prompt_text = payload
        .system_prompt
        .iter()
        .map(String::as_str)
        .chain(payload.contents.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");

    let Some(client) = state.fabrix_client() else {
        let err = FabrixError::NotConfigured;
        state.record(ctx.fail(&err));
        return error_response(err.status(), err.envelope());
    };

    let res = match client.messages(&payload).await {
        Ok(res) => res,
        Err((err, raw)) => {
            // 거절 응답의 전문입니다. 오류 메시지에는 앞 200자만 들어가므로, 사내가
            // **왜** 거절했는지는 여기에만 남습니다.
            ctx.raw_upstream.push_str(&raw);
            state.record(ctx.fail(&err));
            return error_response(err.status(), err.envelope());
        }
    };
    // 상태 줄과 헤더를 본문 앞에 붙입니다 — ④ 칸이 그 자체로 HTTP 기록이 되도록.
    ctx.raw_upstream.push_str(&fabrix::response_head(&res));
    ctx.raw_upstream.push_str("\n");

    // ── ③ 돌려준 응답 ───────────────────────────────────────
    //
    // OpenAI 는 요청받은 `model` 문자열을 **그대로** 되돌려줍니다. 우리는 해석된
    // alias 를 넣고 있었는데, 그러면 폴백이 일어난 순간 값이 달라집니다. 이걸
    // 모델 없음으로 읽고 연결 자체를 실패로 처리하는 클라이언트가 있습니다
    // (Open Design 의 연결 테스트가 그렇습니다). 진단 정보는 로그의
    // model_alias/model_id/model_label 에 그대로 남으므로 잃는 것이 없습니다.
    let echo_model = req
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| model.alias.clone());

    // 지문은 **실제로 나간** modelId 로 만듭니다 — echo 되는 이름이 아니라 답한 모델이
    // 무엇인지가 이 필드의 뜻입니다.
    let fingerprint = Some(super::system_fingerprint(&payload.model_ids[0]));

    if payload.is_stream {
        stream_response(state, ctx, res, echo_model, tool_names, fingerprint, plan.include_usage)
    } else {
        collect_response(state, ctx, res, echo_model, tool_names, fingerprint).await
    }
}

/// 청크 하나를 SSE `data:` 에 실을 문자열로. 원문 기록과 실제 전송이 **같은 문자열**을
/// 쓰도록 직렬화를 여기 한 곳에만 둡니다 — 두 번 만들면 로그가 거짓말할 수 있습니다.
fn sse_data<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// `Emit` 하나를 `chat.completion.chunk` 들로 바꿉니다. 순서는 `Emit` 의 순서
/// 그대로 — 클라이언트가 보는 순서가 곧 이 순서입니다.
///
/// 함수로 빼 둔 이유: `async_stream::stream!` 안에서는 `yield` 를 함수로 감쌀 수
/// 없으므로 청크 **조립**만 밖으로 내고 방출은 제너레이터가 합니다. 덕분에 이
/// 함수는 순수하고 HTTP 없이 테스트할 수 있습니다.
fn emit_chunks(
    id: &str,
    created: i64,
    model: &str,
    emit: Emit,
    sent_role: &mut bool,
) -> Vec<ChatChunk> {
    let mut chunks = Vec::new();
    for piece in emit.0 {
        let delta = match piece {
            Piece::Content(text) => Delta {
                role: (!*sent_role).then_some("assistant"),
                content: Some(text),
                ..Delta::default()
            },
            Piece::Reasoning(text) => Delta {
                role: (!*sent_role).then_some("assistant"),
                reasoning_content: Some(text),
                ..Delta::default()
            },
            Piece::Call(call) => Delta {
                role: (!*sent_role).then_some("assistant"),
                tool_calls: Some(vec![ToolCallDelta::whole(
                    call.index,
                    &call.id,
                    &call.name,
                    &call.arguments,
                )]),
                ..Delta::default()
            },
        };
        *sent_role = true;
        chunks.push(ChatChunk::new(id, created, model, delta, None));
    }
    chunks
}

/// 스트리밍 한 건의 로그를 `Drop` 에서 남깁니다.
///
/// 클라이언트가 도중에 연결을 끊으면 제너레이터가 통째로 버려지므로, 마지막
/// 줄에 `record()` 를 두면 그 호출은 로그에 아예 남지 않습니다. "무슨 일이
/// 있었는지 본다"가 이 앱의 셋 중 하나라 그건 곤란합니다. `Drop` 에 두면
/// 정상 종료와 취소가 같은 경로를 지납니다.
struct StreamLog {
    state: Shared,
    ctx: Ctx,
    decoder: StreamDecoder,
    /// 이 턴의 조립 상태. 답변·추론·도구 호출·종료 사유를 모두 이것이 압니다.
    turn: Turn,
    first_token: Option<Duration>,
    frames: u32,
    /// FabriX 스트림을 끝까지 읽었는지. false 면 클라이언트가 먼저 끊은 것.
    drained: bool,
    /// 실제로 클라이언트에 내보낸 `finish_reason`. 상위가 준 원문과 다를 수
    /// 있고, 그 차이가 로그에 보여야 합니다.
    emitted_finish: Option<&'static str>,
    /// 이 스트림에서 계산한 usage. 로그 꼬리 한 줄에 씁니다.
    counted: Option<usage::Counted>,
}

impl StreamLog {
    /// 클라이언트로 나갈 SSE 프레임 하나 — 원문을 기록하고 이벤트로 만듭니다.
    ///
    /// 모든 `yield` 가 이 함수를 지나므로 로그 ④ 칸의 아래쪽은 클라이언트가 실제로 읽은
    /// 바이트와 같습니다. `data:` 접두와 빈 줄까지 적는 이유: 프레임 경계가 보여야
    /// "청크 하나에 뭐가 실렸나" 를 눈으로 셀 수 있습니다.
    fn out(&mut self, data: String) -> Event {
        self.ctx.raw_client.push_str("data: ");
        self.ctx.raw_client.push_str(&data);
        self.ctx.raw_client.push_str("\n\n");
        Event::default().data(data)
    }
}

impl Drop for StreamLog {
    fn drop(&mut self) {
        let aborted = !self.drained;
        let failure = self.turn.failure().map(str::to_string);
        let stats = self.turn.tool_stats();

        let mut meta: Vec<String> = Vec::new();
        if let Some(first) = self.first_token {
            meta.push(format!("첫 토큰 {:.1}s", first.as_secs_f64()));
        }
        let emitted = self
            .emitted_finish
            .unwrap_or(if aborted { "(없음 · 클라이언트가 끊음)" } else { "stop" });
        meta.push(match self.turn.upstream_finish() {
            // 상위가 준 값이 그대로 안 나갔으면 원문을 함께 적습니다 — 준수와 진단을
            // 동시에 만족시키는 자리입니다.
            Some(raw) if raw != emitted => format!("finish_reason: {emitted} (사내: {raw})"),
            _ => format!("finish_reason: {emitted}"),
        });
        meta.push(format!("SSE {}프레임", self.frames));
        meta.push(format!("{}자", self.turn.raw_content().chars().count()));
        // "모델이 아무 말도 안 했다" 와 "우리가 못 알아들었다" 는 다음 행동이 정반대입니다.
        if self.decoder.undecodable > 0 {
            meta.push(format!("해석하지 못한 프레임 {}개", self.decoder.undecodable));
        }
        meta.push(match &self.counted {
            Some(counted) => usage::meta_line(counted),
            // include_usage 를 안 켠 스트림은 usage 를 계산하지 않습니다 — 규약대로
            // 청크를 안 보내므로 지어낸 숫자를 로그에만 남길 이유가 없습니다.
            None => "usage 미계산 (stream_options.include_usage 를 켜면 계산합니다)".to_string(),
        });
        if let Some(line) = reasoning_meta(&self.turn) {
            meta.push(line);
        }
        if let Some(line) = tool_meta(
            self.ctx.tools_declared,
            self.ctx.tools_emulated,
            self.ctx.tools_choice_none,
            &stats,
        ) {
            meta.push(line);
        }
        meta.extend(plan_meta(&self.ctx));

        let note = match (aborted, failure.is_some()) {
            (true, _) => Some("클라이언트가 연결을 끊음".to_string()),
            (false, true) => Some("스트리밍 중 끊김".to_string()),
            // 도구를 줬는데 모델이 한 번도 안 썼다 — 실패는 아니지만 눈에 띄어야
            // 합니다. Open Design 같은 클라이언트는 이 경우 조용히 빈손이 됩니다.
            (false, false) if self.ctx.tools_emulated && stats.calls == 0 => {
                Some("도구 미사용".to_string())
            }
            (false, false) => None,
        };

        // 받은 답변은 자르지 않고 통째로 담습니다 — 화면이 앞부분만 보여 주고
        // "전체보기" 로 펼치므로, 여기서 자르면 펼칠 뒤가 남지 않습니다.
        let text = log_body(&self.turn);
        let body = match (&failure, text.is_empty()) {
            (Some(msg), true) => msg.clone(),
            (Some(msg), false) => format!("{text}\n\n[중단] {msg}"),
            (None, true) if aborted => "(클라이언트가 먼저 끊어 받은 내용이 없습니다)".into(),
            (None, true) => "(빈 응답)".into(),
            (None, false) => text,
        };

        let failed = note.is_some();
        self.state.record(self.ctx.entry(
            // 헤더는 이미 200 으로 나갔으므로 있는 그대로 기록하고,
            // 끊긴 사실은 note 로 표시합니다.
            200,
            failed,
            note.clone(),
            if failed { note } else { self.ctx.success_summary() },
            body,
            meta.join(" · "),
        ));
    }
}

/// 디코더가 낸 이벤트 묶음을 `Turn` 에 먹이고 내보낼 청크들을 만듭니다.
///
/// 두 번째 반환값은 상위가 스트림을 끝냈는가(`Done`/`Error`) — 펌프의 종료 조건입니다.
/// 청크 조립을 함수로 빼서 펌프와 디코더 꼬리가 **같은 코드**를 지나게 합니다.
fn drive(
    log: &mut StreamLog,
    events: Vec<super::fabrix::StreamEvent>,
    id: &str,
    created: i64,
    model: &str,
    sent_role: &mut bool,
) -> (Vec<ChatChunk>, bool) {
    use super::fabrix::StreamEvent;

    let mut chunks = Vec::new();
    let mut ended = false;
    for event in events {
        // 첫 토큰은 본문뿐 아니라 **추론**으로도 시작할 수 있습니다. 예전에는 Delta
        // 만 셌기 때문에 추론부터 흘리는 모델은 첫 토큰 지연이 로그에 안 남았습니다.
        if matches!(event, StreamEvent::Delta(_) | StreamEvent::Reasoning(_)) {
            let started = log.ctx.started;
            log.first_token.get_or_insert_with(|| started.elapsed());
        }
        ended |= matches!(event, StreamEvent::Done | StreamEvent::Error(_));
        let emit = log.turn.push(event);
        chunks.extend(emit_chunks(id, created, model, emit, sent_role));
        if ended {
            break;
        }
    }
    (chunks, ended)
}

/// FabriX 스트림을 OpenAI `chat.completion.chunk` SSE 로 옮겨 흘립니다.
///
/// 로그는 스트림이 완전히 끝난 뒤에 남깁니다 — 첫 토큰 지연과 프레임 수를
/// 그때서야 알 수 있기 때문입니다.
fn stream_response(
    state: Shared,
    ctx: Ctx,
    res: reqwest::Response,
    model: String,
    tool_names: HashSet<String>,
    fingerprint: Option<String>,
    include_usage: bool,
) -> Response {
    let id = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = state::epoch_secs();

    let body = async_stream::stream! {
        // 제너레이터가 어떻게 끝나든(완주 · 취소) Drop 에서 로그가 남습니다.
        let mut log = StreamLog {
            state,
            ctx,
            decoder: StreamDecoder::new(),
            // 이름이 비면 스캐너는 스스로 통과 모드가 됩니다.
            turn: Turn::new(tool_names, true),
            first_token: None,
            frames: 0,
            drained: false,
            emitted_finish: None,
            counted: None,
        };

        let mut bytes = res.bytes_stream();

        // OpenAI 처럼 롤만 담은 청크를 **맨 앞에** 하나 흘립니다. 이후 청크에는 role 을
        // 넣지 않으므로 `sent_role` 은 참으로 시작합니다.
        let opening = sse_data(&ChatChunk::opening(&id, created, &model, fingerprint.clone()));
        yield Ok::<Event, Infallible>(log.out(opening));
        let mut sent_role = true;

        // 펌프와 디코더 꼬리가 **같은 함수**를 지납니다. 예전에는 두 곳에 똑같은
        // match 가 있어, 한쪽만 고치면 개행 없이 끝난 마지막 프레임에서 조용히
        // 다르게 동작했습니다.
        let mut ended = false;
        while !ended {
            let events = match bytes.next().await {
                Some(Ok(chunk)) => {
                    log.frames += 1;
                    // 해석하기 **전에** 남깁니다 — 해석에 실패한 프레임이야말로 원문이
                    // 필요한 프레임입니다.
                    log.ctx.raw_upstream.push(&chunk);
                    log.decoder.push(&chunk)
                }
                Some(Err(err)) => {
                    log.turn.mark_failure(format!("스트림이 끊겼습니다: {err}"));
                    break;
                }
                None => break,
            };
            let (chunks, saw_end) = drive(&mut log, events, &id, created, &model, &mut sent_role);
            for chunk in chunks {
                let data = sse_data(&chunk);
                yield Ok::<Event, Infallible>(log.out(data));
            }
            ended = saw_end;
        }

        // 개행 없이 끝난 마지막 프레임.
        let tail = log.decoder.finish();
        let (chunks, _) = drive(&mut log, tail, &id, created, &model, &mut sent_role);
        for chunk in chunks {
            let data = sse_data(&chunk);
            yield Ok(log.out(data));
        }
        // 분리기와 두 채널 버퍼가 붙들고 있던 꼬리를 흘려보냅니다 — 절대 버리지 않습니다.
        // `finish_reason` 을 읽기 전에 반드시 여기를 지나야 합니다: 마지막 도구 호출이
        // 닫는 태그를 만나 완성되는 자리가 여기입니다.
        let closing = log.turn.finish();
        for chunk in emit_chunks(&id, created, &model, closing, &mut sent_role) {
            let data = sse_data(&chunk);
            yield Ok(log.out(data));
        }
        if log.decoder.truncated {
            log.turn.mark_truncated();
        }
        log.drained = true;

        if let Some(msg) = log.turn.failure().map(str::to_string) {
            let data = sse_data(&ErrorEnvelope::new(
                msg, openai_type(502), Some("upstream_error".into()),
            ));
            yield Ok(log.out(data));
        }
        // 종료 사유는 실패든 아니든 **한 함수**가 정합니다. 오류 프레임 뒤에도 finish
        // 청크를 넣습니다 — 없으면 종료 사유를 기다리는 클라이언트가 [DONE] 을 받고도
        // 스트림을 미완으로 남깁니다.
        let reason = log.turn.finish_reason();
        log.emitted_finish = Some(reason);
        let data = sse_data(
            &ChatChunk::new(&id, created, &model, Delta::default(), Some(reason.into()))
                .with_fingerprint(fingerprint.clone()),
        );
        yield Ok(log.out(data));

        // 규약 순서: finish 청크 → usage 청크(choices: []) → [DONE].
        // 클라이언트가 include_usage 로 **명시적으로 옵트인**했을 때만 보냅니다.
        if include_usage {
            let counted = usage::build(
                log.decoder.upstream_tokens,
                &log.ctx.prompt_text,
                &log.turn.completion_text(),
            );
            let data = sse_data(&ChatChunk::usage_only(&id, created, &model, counted.usage.clone()));
            yield Ok(log.out(data));
            log.counted = Some(counted);
        }
        yield Ok(log.out("[DONE]".to_string()));
    };

    // keep-alive 주석은 붙이지 않습니다 — 로컬호스트에는 중간 프록시가 없고,
    // 일부 OpenAI 호환 클라이언트가 주석 프레임을 다루지 못합니다.
    let mut response = Sse::new(body).into_response();
    if include_usage {
        // 헤더는 첫 바이트와 함께 나가므로 스트림이 끝나기 전에 정해야 합니다.
        // 사내가 토큰 수를 주기 시작하면 이 값이 `upstream` 이 되어야 하지만, 그건
        // 마지막 프레임을 읽어야 알 수 있어 여기서는 추정으로 표기합니다 — 실제 출처는
        // 로그 ③ 칸 꼬리가 정확히 말합니다.
        response.headers_mut().insert(
            "x-fabrix-usage",
            HeaderValue::from_static(usage::Source::Estimated.header_value()),
        );
    }
    response
}

/// 비스트리밍 응답을 `chat.completion` 하나로 조립합니다.
async fn collect_response(
    state: Shared,
    mut ctx: Ctx,
    res: reqwest::Response,
    model: String,
    tool_names: HashSet<String>,
    fingerprint: Option<String>,
) -> Response {
    let raw = match res.text().await {
        Ok(text) => text,
        Err(err) => {
            let err = FabrixError::from(err);
            state.record(ctx.fail(&err));
            return error_response(err.status(), err.envelope());
        }
    };
    // 해석하기 전에 남깁니다 — 아래 갈래 중 어디로 빠지든 원문은 로그에 남습니다.
    ctx.raw_upstream.push_str(&raw);

    // 스트리밍과 **같은 상태 기계**를 태웁니다 — 두 경로가 어긋날 수 없게.
    let mut turn = Turn::new(tool_names, true);

    // `Turn` 이 알 수 없는 것들 — 상위 응답 봉투에만 있는 표지입니다.
    let looks_successful;
    let mut filter_reason = None;
    let truncated;
    let upstream_tokens;
    let mut via_stream_decoder = false;
    let mut undecodable = 0;

    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => {
            let chunk = serde_json::from_value::<FabrixChunk>(extract_object(&value))
                .unwrap_or_default();
            if chunk.looks_like_error() {
                let err = FabrixError::Upstream { status: 502, message: chunk.error_text() };
                state.record(ctx.fail(&err));
                return error_response(err.status(), err.envelope());
            }
            looks_successful = chunk.looks_successful();
            filter_reason = chunk.filter_message();
            truncated = chunk.truncated == Some(true);
            upstream_tokens = chunk.upstream_tokens();
            // content → contentReferences → eventData 폴백은 이 변환 안에 있습니다.
            for event in nonstream_events(&chunk) {
                turn.feed(event);
            }
        }
        // isStream=false 인데 SSE 를 흘려보내는 서버도 있어 한 번 더 시도합니다.
        Err(_) => {
            via_stream_decoder = true;
            let mut decoder = StreamDecoder::new();
            let mut events = decoder.push(raw.as_bytes());
            events.extend(decoder.finish());
            for event in events {
                turn.feed(event);
            }
            // 스트림 디코더까지 태워 프레임을 읽어 냈다면 응답 자체는 성립했습니다.
            looks_successful = decoder.finish_reason.is_some();
            truncated = decoder.truncated;
            upstream_tokens = decoder.upstream_tokens;
            undecodable = decoder.undecodable;
        }
    }
    if truncated {
        turn.mark_truncated();
    }
    // 붙들려 있던 꼬리를 흘려보냅니다. `is_empty()`·`finish_reason()` 은 이 **뒤에야**
    // 읽을 수 있습니다 — 마지막 도구 호출이 여기서 완성될 수 있습니다.
    turn.finish();

    // 도구 호출만 있고 산문이 없는 답변은 정상입니다 — 빈 응답으로 502 내면 안 됩니다.
    //
    // 답변이 정말 비었을 때도 상위 응답에 성공 표지가 있으면 200 + `content: ""` 입니다.
    // 모델이 짧은 max_tokens 등으로 빈 답을 줄 수 있고, 그걸 502 로 부르면 사내 잘못이
    // 아닌 것을 사내 오류로 보고하는 셈입니다. 아무 표지도 없으면 애초에 우리가 못
    // 알아본 본문이라 502 가 맞습니다.
    if turn.is_empty() {
        // 답변이 하나도 없고 필터 차단 사유가 있으면 일반 파싱오류 대신 사유를 노출합니다.
        if let Some(reason) = filter_reason {
            let err = FabrixError::Upstream { status: 502, message: reason };
            state.record(ctx.fail(&err));
            return error_response(err.status(), err.envelope());
        }
        if !looks_successful {
            let err =
                FabrixError::BadPayload(format!("본문 앞부분: {}", logstore::preview(&raw, 200)));
            state.record(ctx.fail(&err));
            return error_response(err.status(), err.envelope());
        }
    }

    let reason = turn.finish_reason();
    // 비스트림 응답에는 `usage` 를 **항상** 채웁니다 — 규약이 요구하는 필드입니다.
    // 사내가 실측을 주지 않으므로 추정치이고, 추정임은 헤더·로그·README 가 말합니다.
    let counted = usage::build(upstream_tokens, &ctx.prompt_text, &turn.completion_text());

    let calls = turn.calls();
    let completion = ChatCompletion {
        id: format!("chatcmpl-{}", Uuid::new_v4().simple()),
        object: "chat.completion",
        created: state::epoch_secs(),
        model: model.clone(),
        choices: vec![Choice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                content: turn.assistant_content(),
                reasoning_content: turn.reasoning_field(),
                tool_calls: (!calls.is_empty())
                    .then(|| calls.iter().map(ToolCall::from).collect()),
            },
            finish_reason: Some(reason.to_string()),
            logprobs: None,
        }],
        usage: Some(counted.usage.clone()),
        system_fingerprint: fingerprint,
    };

    let stats = turn.tool_stats();
    let mut meta = vec![
        match turn.upstream_finish() {
            Some(raw) if raw != reason => format!("finish_reason: {reason} (사내: {raw})"),
            _ => format!("finish_reason: {reason}"),
        },
        format!("{}자", turn.raw_content().chars().count()),
        usage::meta_line(&counted),
    ];
    if via_stream_decoder {
        meta.push("SSE 본문을 합쳐 해석".into());
    }
    if undecodable > 0 {
        meta.push(format!("해석하지 못한 프레임 {undecodable}개"));
    }
    if let Some(line) = reasoning_meta(&turn) {
        meta.push(line);
    }
    if let Some(line) =
        tool_meta(ctx.tools_declared, ctx.tools_emulated, ctx.tools_choice_none, &stats)
    {
        meta.push(line);
    }
    meta.extend(plan_meta(&ctx));

    let note = (ctx.tools_emulated && stats.calls == 0).then(|| "도구 미사용".to_string());

    // 클라이언트가 받는 바이트 그대로. `Json` 도 `serde_json` 으로 직렬화하므로 같은
    // 문자열이고, 여기서 한 번 더 만드는 비용은 요청당 한 번뿐입니다.
    ctx.raw_client.push_str(&serde_json::to_string(&completion).unwrap_or_default());

    state.record(ctx.entry(
        200,
        false,
        note,
        ctx.success_summary(),
        // 자르지 않은 전문 — 화면에서 앞부분만 보여 주고 "전체보기" 로 펼칩니다.
        log_body(&turn),
        meta.join(" · "),
    ));

    let mut response = Json(completion).into_response();
    response
        .headers_mut()
        .insert("x-fabrix-usage", HeaderValue::from_static(counted.source.header_value()));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(calls: u32, in_reasoning: u32, rejected: u32) -> ToolStats {
        ToolStats { calls, in_reasoning, rejected, ..ToolStats::default() }
    }

    #[test]
    fn tool_meta_stays_silent_for_ordinary_chats() {
        assert_eq!(tool_meta(0, false, false, &stats(0, 0, 0)), None);
        assert_eq!(tool_meta(0, true, false, &stats(0, 0, 0)), None);
    }

    /// 이 두 경우를 구분하는 것이 이 줄의 존재 이유입니다. 지금까지는 도구를 버린
    /// 것과 모델이 안 쓴 것이 사용자에게 똑같이 보였습니다.
    #[test]
    fn tool_meta_separates_dropped_from_unused() {
        assert_eq!(
            tool_meta(12, false, false, &stats(0, 0, 0)).unwrap(),
            "도구 12개 선언 · 에뮬레이션 꺼짐 — 무시함"
        );
        assert_eq!(
            tool_meta(12, true, false, &stats(0, 0, 0)).unwrap(),
            "도구 12개 선언 · 호출 0건 — 모델이 규약을 따르지 않음"
        );
        assert_eq!(
            tool_meta(12, true, false, &stats(2, 0, 0)).unwrap(),
            "도구 12개 선언 · 호출 2건"
        );
    }

    /// 프록시 설정으로 끈 것과 클라이언트가 끈 것은 다음 행동이 다릅니다 —
    /// 설정을 볼지, 요청을 볼지.
    #[test]
    fn tool_meta_names_who_turned_tools_off() {
        assert_eq!(
            tool_meta(4, false, true, &stats(0, 0, 0)).unwrap(),
            "도구 4개 선언 · 클라이언트가 tool_choice: none 으로 껐음"
        );
    }

    #[test]
    fn tool_meta_reports_rejected_blocks() {
        let line = tool_meta(3, true, false, &stats(1, 0, 2)).unwrap();
        assert!(line.contains("호출 1건"), "{line}");
        assert!(line.contains("형식 오류 2건"), "{line}");
    }

    /// 이 한 줄이 "추론 단계마다 stop" 수정이 실제로 물었는지를 말해 줍니다.
    #[test]
    fn tool_meta_names_the_reasoning_channel() {
        let line = tool_meta(3, true, false, &stats(3, 2, 0)).unwrap();
        assert!(line.contains("호출 3건"), "{line}");
        assert!(line.contains("그중 2건은 추론 채널에서"), "{line}");
        // 추론에서 안 나왔으면 붙지 않습니다.
        let quiet = tool_meta(3, true, false, &stats(3, 0, 0)).unwrap();
        assert!(!quiet.contains("추론"), "{quiet}");
    }

    fn ctx_with(plan: validate::Plan, model_defaulted: bool) -> Ctx {
        Ctx {
            started: Instant::now(),
            stream: false,
            client: None,
            model_requested: None,
            model_alias: None,
            model_id: None,
            model_label: None,
            req_openai: String::new(),
            req_fabrix: String::new(),
            req_fabrix_headers: String::new(),
            fabrix_url: String::new(),
            tools_declared: 0,
            tools_emulated: false,
            tools_choice_none: false,
            plan,
            model_defaulted,
            temperature_requested: None,
            prompt_text: String::new(),
            raw_upstream: RawBuf::new(false),
            raw_client: RawBuf::new(false),
        }
    }

    #[test]
    fn plan_meta_is_silent_for_a_plain_request() {
        assert!(plan_meta(&ctx_with(validate::Plan::default(), false)).is_empty());
    }

    #[test]
    fn plan_meta_names_what_was_not_honored() {
        let plan = validate::Plan {
            include_usage: false,
            images_dropped: 2,
            ignored: vec!["stop", "presence_penalty"],
            unknown: vec!["래빗홀".into()],
        };
        let lines = plan_meta(&ctx_with(plan, true));
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "model 미지정 → 기본 모델");
        assert_eq!(lines[1], "무시된 파라미터: stop · presence_penalty");
        assert!(lines[2].contains("이미지 파트 2개"), "{}", lines[2]);
        assert!(lines[3].contains("래빗홀"), "{}", lines[3]);
    }

    // ── 로그 ③ 칸 ──

    /// 추론 채널에서 호출이 나왔을 때 그 원문을 못 보면 "모델이 규약을 안 지켰다"와
    /// 구별할 방법이 없습니다. 이번 버그가 오래 숨어 있던 이유가 정확히 이것입니다.
    #[test]
    fn log_body_labels_both_channels_only_when_reasoning_exists() {
        let mut plain = Turn::new(HashSet::new(), true);
        plain.push(super::super::fabrix::StreamEvent::Delta("답변만".into()));
        plain.finish();
        assert_eq!(log_body(&plain), "답변만", "추론이 없으면 머리말을 붙이지 않습니다");

        let mut both = Turn::new(HashSet::new(), true);
        both.push(super::super::fabrix::StreamEvent::Reasoning(
            "<tool_call>{\"name\":\"read\"}</tool_call>".into(),
        ));
        both.push(super::super::fabrix::StreamEvent::Delta("답변".into()));
        both.finish();
        let body = log_body(&both);
        assert!(body.starts_with("[추론]\n"), "{body}");
        assert!(body.contains("[답변]\n답변"), "{body}");
        // 걷어내기 전 원문이라 센티널이 눈에 보입니다.
        assert!(body.contains("<tool_call>"), "{body}");
    }

    #[test]
    fn reasoning_meta_is_silent_without_reasoning() {
        let mut t = Turn::new(HashSet::new(), true);
        t.push(super::super::fabrix::StreamEvent::Delta("답변".into()));
        t.finish();
        assert_eq!(reasoning_meta(&t), None);
    }

    #[test]
    fn reasoning_meta_reports_think_splitting() {
        let mut t = Turn::new(HashSet::new(), true);
        t.push(super::super::fabrix::StreamEvent::Delta("<think>가나다</think>답".into()));
        t.finish();
        let line = reasoning_meta(&t).unwrap();
        assert!(line.contains("추론 3자"), "{line}");
        assert!(line.contains("<think> 1개 분리"), "{line}");

        let mut unclosed = Turn::new(HashSet::new(), true);
        unclosed.push(super::super::fabrix::StreamEvent::Delta("<think>끊김".into()));
        unclosed.finish();
        assert!(reasoning_meta(&unclosed).unwrap().contains("닫히지 않음"), );
    }

    // ── 클라이언트가 실제로 보는 청크 ──

    fn emit(pieces: Vec<Piece>, sent_role: &mut bool) -> Vec<Value> {
        emit_chunks("chatcmpl-x", 1, "fabrix-chat-4", Emit(pieces), sent_role)
            .iter()
            .map(|c| serde_json::to_value(c).unwrap())
            .collect()
    }

    fn call(index: u32, name: &str, arguments: &str) -> tools::ScannedCall {
        tools::ScannedCall {
            index,
            id: format!("call_{name}"),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn text_chunk_carries_the_role_only_once() {
        let mut sent_role = false;
        let a = emit(vec![Piece::Content("안".into())], &mut sent_role);
        let b = emit(vec![Piece::Content("녕".into())], &mut sent_role);
        assert_eq!(a[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(a[0]["choices"][0]["delta"]["content"], "안");
        // 두 번째부터는 role 키 자체가 없어야 합니다.
        assert!(b[0]["choices"][0]["delta"].get("role").is_none());
        assert_eq!(b[0]["choices"][0]["delta"]["content"], "녕");
    }

    /// `@ai-sdk/openai-compatible` 은 같은 index 의 첫 조각에 id 와 function.name 이
    /// 없으면 InvalidResponseDataError 를 던집니다. 우리는 완성된 호출만 내보내므로
    /// 조각이 하나뿐이고, 그 하나에 전부 들어 있어야 합니다.
    #[test]
    fn tool_call_chunk_is_self_contained() {
        let mut sent_role = true;
        let chunks = emit(
            vec![Piece::Call(call(0, "write", r#"{"filePath":"a.html"}"#))],
            &mut sent_role,
        );
        assert_eq!(chunks.len(), 1);
        let tc = &chunks[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["index"], 0);
        assert_eq!(tc["id"], "call_write");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "write");
        assert_eq!(tc["function"]["arguments"], r#"{"filePath":"a.html"}"#);
        // 도구 청크에는 content 키가 없어야 합니다 (null 이 아니라 아예 없음).
        assert!(chunks[0]["choices"][0]["delta"].get("content").is_none());
        assert_eq!(chunks[0]["object"], "chat.completion.chunk");
    }

    /// 추론 조각은 `reasoning_content` 로만 나가야 합니다 — `content` 에 섞이면
    /// 사고 과정이 답변에 새어 나옵니다.
    #[test]
    fn reasoning_piece_becomes_a_reasoning_content_delta() {
        let mut sent_role = true;
        let chunks = emit(vec![Piece::Reasoning("생각".into())], &mut sent_role);
        assert_eq!(chunks.len(), 1);
        let delta = &chunks[0]["choices"][0]["delta"];
        assert_eq!(delta["reasoning_content"], "생각");
        assert!(delta.get("content").is_none());
        assert!(delta.get("tool_calls").is_none());
    }

    /// `Emit` 의 순서가 곧 클라이언트가 보는 순서입니다.
    #[test]
    fn pieces_keep_their_order_within_one_emit() {
        let mut sent_role = false;
        let chunks = emit(
            vec![
                Piece::Content("만들겠습니다.".into()),
                Piece::Call(call(0, "write", "{}")),
                Piece::Call(call(1, "read", "{}")),
            ],
            &mut sent_role,
        );
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0]["choices"][0]["delta"]["content"], "만들겠습니다.");
        assert_eq!(chunks[1]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(chunks[2]["choices"][0]["delta"]["tool_calls"][0]["index"], 1);
        // role 은 맨 앞 청크에만.
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert!(chunks[1]["choices"][0]["delta"].get("role").is_none());
    }

    #[test]
    fn empty_emit_produces_nothing() {
        let mut sent_role = false;
        assert!(emit(Vec::new(), &mut sent_role).is_empty());
        assert!(!sent_role, "빈 출력이 role 을 소비하면 안 됩니다");
    }
}
