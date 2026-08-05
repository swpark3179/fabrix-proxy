//! `POST /v1/chat/completions` — 프록시의 심장.
//!
//! 흐름: 받은 요청(OpenAI) → 변환해서 보낸 요청(FabriX) → 돌려준 응답.
//! 로그 창의 세 칸이 정확히 이 세 단계입니다.

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::Value;
use uuid::Uuid;

use crate::logstore::{self, Kind, LogEntry};
use crate::openai::{
    AssistantMessage, ChatChunk, ChatCompletion, ChatRequest, Choice, Delta, ErrorEnvelope,
    ToolCall, ToolCallDelta,
};
use crate::state::{self, Shared};

use super::fabrix::{
    default_model, extract_object, find_model, fold_messages, map_finish_reason, FabrixChunk, FabrixError,
    LlmConfig, MessagesRequest, ResolvedModel, StreamDecoder, StreamEvent, MESSAGES_PATH,
};
use super::models::ensure_models;
use super::tools::{self, ToolCallScanner};
use super::validate;
use super::{authorize, error_response, fabrix_headers_line, pretty};

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
    /// 검증을 통과한 요청에서 뽑아낸 실행 계획 — 무시한 필드와 버린 이미지 파트를
    /// 로그 ③ 칸까지 들고 갑니다.
    plan: validate::Plan,
}

/// 로그 꼬리에 붙일 도구 관련 한 줄.
///
/// "요청에 도구가 있었는데 프록시가 버렸다" 와 "도구는 전달됐는데 모델이 안 썼다" 를
/// 사용자가 구분할 수 있어야 합니다 — 지금까지는 둘 다 똑같이 조용히 실패했습니다.
fn tool_meta(declared: usize, emulated: bool, calls: u32, rejects: u32) -> Option<String> {
    if declared == 0 {
        return None; // 평범한 채팅에는 아무 것도 붙이지 않습니다.
    }
    if !emulated {
        return Some(format!("도구 {declared}개 선언 · 에뮬레이션 꺼짐 — 무시함"));
    }
    let mut line = match calls {
        0 => format!("도구 {declared}개 선언 · 호출 0건 — 모델이 규약을 따르지 않음"),
        n => format!("도구 {declared}개 선언 · 호출 {n}건"),
    };
    if rejects > 0 {
        line.push_str(&format!(" · 형식 오류 {rejects}건은 텍스트로 되돌림"));
    }
    Some(line)
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

pub async fn handle(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let started = Instant::now();
    let cfg = state.config();

    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    // 인바운드 Authorization: 키발급없이 허용 모드면 값과 무관하게 통과,
    // 토큰 사용 모드면 발행 토큰과 일치할 때만 통과합니다. (아래 authorize 에서 검사)
    let incoming: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let mut ctx = Ctx {
        started,
        stream: false,
        client: logstore::short_client(ua),
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
        plan: validate::Plan::default(),
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
            let msg = format!("요청 본문을 해석하지 못했습니다: {err}");
            state.record(ctx.entry(
                400,
                true,
                Some("잘못된 요청".into()),
                Some("잘못된 요청".into()),
                msg.clone(),
                "요청 파싱 실패".into(),
            ));
            return error_response(400, ErrorEnvelope::new(msg, "invalid_request_error", None));
        }
    };
    ctx.stream = req.is_stream();
    ctx.model_requested = req.model.clone();

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
        Ok((models, _)) => models,
        Err(err) => {
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
                        ErrorEnvelope::new(err.message(), err.kind(), None),
                    );
                }
            }
        }
    };

    let resolved = req
        .model
        .as_deref()
        .and_then(|requested| find_model(&models, requested))
        .or_else(|| default_model(&models, &cfg.default_model_alias));
    let Some(model) = resolved else {
        let msg = "사내 모델 목록이 비어 있어 요청을 보낼 수 없습니다.".to_string();
        state.record(ctx.entry(
            502,
            true,
            Some("모델 없음".into()),
            Some("모델 없음".into()),
            msg.clone(),
            "실패 · 모델 목록 없음".into(),
        ));
        return error_response(502, ErrorEnvelope::new(msg, "upstream_error", None));
    };
    ctx.model_alias = Some(model.alias.clone());
    ctx.model_id = Some(model.model_id.clone());
    ctx.model_label = Some(model.label.clone());

    // ── ② 변환해서 보낸 요청 ─────────────────────────────────
    let (mut system_prompt, mut contents) = fold_messages(&req.messages);

    // 도구 에뮬레이션. FabriX 에 도구 필드가 없어 규약을 systemPrompt 뒤에 붙이고,
    // 답변에서 <tool_call> 을 걷어내 tool_calls 로 돌려줍니다.
    let declared = req.declared_tools();
    let emulate = cfg.tool_emulation && req.wants_tools();
    let tool_names: HashSet<String> = if emulate {
        declared.iter().map(|f| f.name.trim().to_string()).collect()
    } else {
        HashSet::new()
    };
    if emulate {
        if let Some(block) = tools::render_system_block(&declared, &req.tool_mode()) {
            system_prompt = Some(match system_prompt {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }
    }
    ctx.tools_declared = declared.len();
    ctx.tools_emulated = emulate;
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
            return error_response(400, ErrorEnvelope::new(msg, "invalid_request_error", None));
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

    let Some(client) = state.fabrix_client() else {
        let err = FabrixError::NotConfigured;
        state.record(ctx.fail(&err));
        return error_response(err.status(), ErrorEnvelope::new(err.message(), err.kind(), None));
    };

    let res = match client.messages(&payload).await {
        Ok(res) => res,
        Err(err) => {
            state.record(ctx.fail(&err));
            return error_response(err.status(), ErrorEnvelope::new(err.message(), err.kind(), None));
        }
    };

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
    if payload.is_stream {
        stream_response(state, ctx, res, echo_model, tool_names)
    } else {
        collect_response(state, ctx, res, echo_model, tool_names).await
    }
}

fn sse_json<T: serde::Serialize>(value: &T) -> Event {
    Event::default().data(serde_json::to_string(value).unwrap_or_default())
}

/// 스캐너 출력 하나를 `chat.completion.chunk` 들로 바꿉니다 — 텍스트 먼저,
/// 그다음 도구 호출. 클라이언트가 보는 순서가 곧 이 순서입니다.
///
/// 펌프 본문과 꼬리 처리가 같은 경로를 쓰도록 함수로 뺐습니다
/// (`async_stream::stream!` 안에서는 `yield` 를 매크로로 감쌀 수 없습니다).
/// SSE 직렬화는 호출부가 하므로 이 함수는 순수하고 테스트할 수 있습니다.
fn scan_chunks(
    id: &str,
    created: i64,
    model: &str,
    out: tools::ScanOut,
    sent_role: &mut bool,
) -> Vec<ChatChunk> {
    let mut chunks = Vec::new();
    if !out.text.is_empty() {
        let delta = Delta {
            role: (!*sent_role).then_some("assistant"),
            content: Some(out.text),
            ..Delta::default()
        };
        *sent_role = true;
        chunks.push(ChatChunk::new(id, created, model, delta, None));
    }
    for call in out.calls {
        let delta = Delta {
            role: (!*sent_role).then_some("assistant"),
            tool_calls: Some(vec![ToolCallDelta::whole(
                call.index,
                &call.id,
                &call.name,
                &call.arguments,
            )]),
            ..Delta::default()
        };
        *sent_role = true;
        chunks.push(ChatChunk::new(id, created, model, delta, None));
    }
    chunks
}

fn scan_events(
    id: &str,
    created: i64,
    model: &str,
    out: tools::ScanOut,
    sent_role: &mut bool,
) -> Vec<Event> {
    scan_chunks(id, created, model, out, sent_role)
        .iter()
        .map(sse_json)
        .collect()
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
    first_token: Option<Duration>,
    frames: u32,
    finish: Option<String>,
    failure: Option<String>,
    /// FabriX 스트림을 끝까지 읽었는지. false 면 클라이언트가 먼저 끊은 것.
    drained: bool,
    tool_calls: u32,
    tool_rejects: u32,
}

impl Drop for StreamLog {
    fn drop(&mut self) {
        let text = self.decoder.text().to_string();
        let aborted = !self.drained;

        let mut meta: Vec<String> = Vec::new();
        if let Some(first) = self.first_token {
            meta.push(format!("첫 토큰 {:.1}s", first.as_secs_f64()));
        }
        meta.push(format!(
            "finish_reason: {}",
            self.finish.clone().unwrap_or_else(|| if aborted { "abort".into() } else { "stop".into() })
        ));
        meta.push(format!("SSE {}프레임", self.frames));
        meta.push(format!("{}자", text.chars().count()));
        meta.push("사내 응답에 토큰 수 없음".into());
        if let Some(line) = tool_meta(
            self.ctx.tools_declared,
            self.ctx.tools_emulated,
            self.tool_calls,
            self.tool_rejects,
        ) {
            meta.push(line);
        }

        let note = match (aborted, self.failure.is_some()) {
            (true, _) => Some("클라이언트가 연결을 끊음".to_string()),
            (false, true) => Some("스트리밍 중 끊김".to_string()),
            // 도구를 줬는데 모델이 한 번도 안 썼다 — 실패는 아니지만 눈에 띄어야
            // 합니다. Open Design 같은 클라이언트는 이 경우 조용히 빈손이 됩니다.
            (false, false) if self.ctx.tools_emulated && self.tool_calls == 0 => {
                Some("도구 미사용".to_string())
            }
            (false, false) => None,
        };

        // 받은 답변은 자르지 않고 통째로 담습니다 — 화면이 앞부분만 보여 주고
        // "전체보기" 로 펼치므로, 여기서 자르면 펼칠 뒤가 남지 않습니다.
        let body = match (&self.failure, text.is_empty()) {
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
) -> Response {
    let id = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = state::epoch_secs();

    let body = async_stream::stream! {
        // 이름이 비면 스캐너는 스스로 통과 모드가 됩니다.
        let mut scanner = ToolCallScanner::new(tool_names, true);
        // 제너레이터가 어떻게 끝나든(완주 · 취소) Drop 에서 로그가 남습니다.
        let mut log = StreamLog {
            state,
            ctx,
            decoder: StreamDecoder::new(),
            first_token: None,
            frames: 0,
            finish: None,
            failure: None,
            drained: false,
            tool_calls: 0,
            tool_rejects: 0,
        };

        let mut bytes = res.bytes_stream();
        let mut sent_role = false;

        'pump: loop {
            let Some(item) = bytes.next().await else { break };
            let chunk = match item {
                Ok(chunk) => chunk,
                Err(err) => {
                    log.failure = Some(format!("스트림이 끊겼습니다: {err}"));
                    break;
                }
            };
            log.frames += 1;

            for event in log.decoder.push(&chunk) {
                match event {
                    StreamEvent::Delta(text) => {
                        let started = log.ctx.started;
                        log.first_token.get_or_insert_with(|| started.elapsed());
                        let out = scanner.push(&text);
                        for event in scan_events(&id, created, &model, out, &mut sent_role) {
                            yield Ok::<Event, Infallible>(event);
                        }
                    }
                    StreamEvent::Reasoning(text) => {
                        let delta = Delta {
                            role: (!sent_role).then_some("assistant"),
                            reasoning_content: Some(text),
                            ..Delta::default()
                        };
                        sent_role = true;
                        yield Ok(sse_json(&ChatChunk::new(&id, created, &model, delta, None)));
                    }
                    StreamEvent::Reset => scanner.reset(),
                    StreamEvent::Finish(reason) => log.finish = Some(reason),
                    StreamEvent::Error(msg) => {
                        log.failure = Some(msg);
                        break 'pump;
                    }
                    StreamEvent::Done => break 'pump,
                }
            }
        }

        // 개행 없이 끝난 마지막 프레임. 예전에는 `_ => {}` 가 꼬리 Reasoning/Error 를
        // 통째로 삼켰습니다 — 마지막 도구 호출이 개행 없이 끝나면 그대로 유실됩니다.
        for event in log.decoder.finish() {
            match event {
                StreamEvent::Delta(text) => {
                    let started = log.ctx.started;
                    log.first_token.get_or_insert_with(|| started.elapsed());
                    let out = scanner.push(&text);
                    for event in scan_events(&id, created, &model, out, &mut sent_role) {
                        yield Ok(event);
                    }
                }
                StreamEvent::Reasoning(text) => {
                    let delta = Delta {
                        role: (!sent_role).then_some("assistant"),
                        reasoning_content: Some(text),
                        ..Delta::default()
                    };
                    sent_role = true;
                    yield Ok(sse_json(&ChatChunk::new(&id, created, &model, delta, None)));
                }
                StreamEvent::Reset => scanner.reset(),
                StreamEvent::Finish(reason) => log.finish = Some(reason),
                StreamEvent::Error(msg) => log.failure = Some(msg),
                StreamEvent::Done => {}
            }
        }
        // 스캐너가 붙들고 있던 미완성 꼬리를 흘려보냅니다 — 절대 버리지 않습니다.
        for event in scan_events(&id, created, &model, scanner.finish(), &mut sent_role) {
            yield Ok(event);
        }
        log.drained = true;
        log.tool_calls = scanner.call_count();
        log.tool_rejects = scanner.rejected;

        if let Some(msg) = log.failure.clone() {
            yield Ok(sse_json(&ErrorEnvelope::new(msg, "upstream_error", None)));
        } else {
            // 도구 호출이 하나라도 나왔으면 그 사실이 상위 사유보다 우선합니다 —
            // 클라이언트는 이 값으로 에이전트 루프를 계속할지 정합니다. 그다음이
            // 절단(length), 마지막이 상위가 준 사유입니다.
            let reason = if scanner.saw_call() {
                "tool_calls".to_string()
            } else {
                map_finish_reason(log.finish.as_deref(), log.decoder.truncated)
                    .unwrap_or_else(|| "stop".into())
            };
            yield Ok(sse_json(&ChatChunk::new(&id, created, &model, Delta::default(), Some(reason))));
        }
        yield Ok(Event::default().data("[DONE]"));
    };

    // keep-alive 주석은 붙이지 않습니다 — 로컬호스트에는 중간 프록시가 없고,
    // 일부 OpenAI 호환 클라이언트가 주석 프레임을 다루지 못합니다.
    Sse::new(body).into_response()
}

/// 비스트리밍 응답을 `chat.completion` 하나로 조립합니다.
async fn collect_response(
    state: Shared,
    ctx: Ctx,
    res: reqwest::Response,
    model: String,
    tool_names: HashSet<String>,
) -> Response {
    let raw = match res.text().await {
        Ok(text) => text,
        Err(err) => {
            let err = FabrixError::from(err);
            state.record(ctx.fail(&err));
            return error_response(err.status(), ErrorEnvelope::new(err.message(), err.kind(), None));
        }
    };

    let parsed = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => {
            let chunk = serde_json::from_value::<FabrixChunk>(extract_object(&value))
                .unwrap_or_default();
            if chunk.looks_like_error() {
                let err = FabrixError::Upstream { status: 502, message: chunk.error_text() };
                state.record(ctx.fail(&err));
                return error_response(
                    err.status(),
                    ErrorEnvelope::new(err.message(), err.kind(), None),
                );
            }
            // content 가 비어도 플러그인/RAG 답변이 contentReferences 등에 올 수 있어 폴백합니다.
            let content = chunk.answer_text().unwrap_or_default();
            let reasoning = chunk.reasoning_content.clone().filter(|s| !s.is_empty());
            // 답변이 하나도 없고 필터 차단 사유가 있으면 일반 파싱오류 대신 사유를 노출합니다.
            if content.is_empty() && reasoning.is_none() {
                if let Some(reason) = chunk.filter_message() {
                    let err = FabrixError::Upstream { status: 502, message: reason };
                    state.record(ctx.fail(&err));
                    return error_response(
                        err.status(),
                        ErrorEnvelope::new(err.message(), err.kind(), None),
                    );
                }
            }
            Parsed {
                content,
                reasoning,
                truncated: chunk.truncated == Some(true),
                finish: chunk.finish_reason,
                via_stream_decoder: false,
            }
        }
        // isStream=false 인데 SSE 를 흘려보내는 서버도 있어 한 번 더 시도합니다.
        Err(_) => {
            let mut decoder = StreamDecoder::new();
            decoder.push(raw.as_bytes());
            decoder.finish();
            Parsed {
                content: decoder.text().to_string(),
                reasoning: Some(decoder.reasoning().to_string()).filter(|s| !s.is_empty()),
                finish: decoder.finish_reason.clone(),
                truncated: decoder.truncated,
                via_stream_decoder: true,
            }
        }
    };

    // 스트리밍과 **같은 상태 기계**를 태웁니다 — 두 경로가 어긋날 수 없게.
    let scanned = if tool_names.is_empty() {
        tools::ScanOut::default()
    } else {
        tools::parse_all(&parsed.content, &tool_names)
    };
    let has_calls = !scanned.calls.is_empty();

    // 도구 호출만 있고 산문이 없는 답변은 정상입니다 — 빈 응답으로 502 내면 안 됩니다.
    if parsed.content.is_empty() && parsed.reasoning.is_none() && !has_calls {
        let err = FabrixError::BadPayload(format!("본문 앞부분: {}", logstore::preview(&raw, 200)));
        state.record(ctx.fail(&err));
        return error_response(err.status(), ErrorEnvelope::new(err.message(), err.kind(), None));
    }

    let reason = if has_calls {
        "tool_calls".to_string()
    } else {
        map_finish_reason(parsed.finish.as_deref(), parsed.truncated)
            .unwrap_or_else(|| "stop".into())
    };
    let completion = ChatCompletion {
        id: format!("chatcmpl-{}", Uuid::new_v4().simple()),
        object: "chat.completion",
        created: state::epoch_secs(),
        model: model.clone(),
        choices: vec![Choice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                // 도구 호출만 있는 턴은 `content: null` 이 규약입니다.
                content: if has_calls {
                    Some(scanned.text.clone()).filter(|s| !s.trim().is_empty())
                } else {
                    Some(parsed.content.clone())
                },
                reasoning_content: parsed.reasoning.clone(),
                tool_calls: has_calls
                    .then(|| scanned.calls.iter().map(ToolCall::from).collect()),
            },
            finish_reason: Some(reason.clone()),
        }],
        // FabriX 가 토큰 수를 주지 않으므로 추정치를 지어내지 않고 생략합니다.
        usage: None,
    };

    let mut meta = vec![
        format!("finish_reason: {reason}"),
        format!("{}자", parsed.content.chars().count()),
        "사내 응답에 토큰 수 없음".to_string(),
    ];
    if parsed.via_stream_decoder {
        meta.push("SSE 본문을 합쳐 해석".into());
    }
    if let Some(line) = tool_meta(
        ctx.tools_declared,
        ctx.tools_emulated,
        scanned.calls.len() as u32,
        0,
    ) {
        meta.push(line);
    }

    let note = (ctx.tools_emulated && !has_calls).then(|| "도구 미사용".to_string());

    state.record(ctx.entry(
        200,
        false,
        note,
        ctx.success_summary(),
        // 자르지 않은 전문 — 화면에서 앞부분만 보여 주고 "전체보기" 로 펼칩니다.
        // 스캐너가 걷어내기 **전** 원문이라 <tool_call> 유무를 눈으로 볼 수 있습니다.
        parsed.content,
        meta.join(" · "),
    ));

    Json(completion).into_response()
}

struct Parsed {
    content: String,
    reasoning: Option<String>,
    finish: Option<String>,
    truncated: bool,
    via_stream_decoder: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_meta_stays_silent_for_ordinary_chats() {
        assert_eq!(tool_meta(0, false, 0, 0), None);
        assert_eq!(tool_meta(0, true, 0, 0), None);
    }

    /// 이 두 경우를 구분하는 것이 이 줄의 존재 이유입니다. 지금까지는 도구를 버린
    /// 것과 모델이 안 쓴 것이 사용자에게 똑같이 보였습니다.
    #[test]
    fn tool_meta_separates_dropped_from_unused() {
        assert_eq!(
            tool_meta(12, false, 0, 0).unwrap(),
            "도구 12개 선언 · 에뮬레이션 꺼짐 — 무시함"
        );
        assert_eq!(
            tool_meta(12, true, 0, 0).unwrap(),
            "도구 12개 선언 · 호출 0건 — 모델이 규약을 따르지 않음"
        );
        assert_eq!(tool_meta(12, true, 2, 0).unwrap(), "도구 12개 선언 · 호출 2건");
    }

    #[test]
    fn tool_meta_reports_rejected_blocks() {
        let line = tool_meta(3, true, 1, 2).unwrap();
        assert!(line.contains("호출 1건"), "{line}");
        assert!(line.contains("형식 오류 2건"), "{line}");
    }

    // ── 클라이언트가 실제로 보는 청크 ──

    fn scan(out: tools::ScanOut, sent_role: &mut bool) -> Vec<Value> {
        scan_chunks("chatcmpl-x", 1, "fabrix-chat-4", out, sent_role)
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
        let a = scan(tools::ScanOut { text: "안".into(), calls: vec![] }, &mut sent_role);
        let b = scan(tools::ScanOut { text: "녕".into(), calls: vec![] }, &mut sent_role);
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
        let out = tools::ScanOut {
            text: String::new(),
            calls: vec![call(0, "write", r#"{"filePath":"a.html"}"#)],
        };
        let chunks = scan(out, &mut sent_role);
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

    #[test]
    fn text_is_emitted_before_tool_calls() {
        let mut sent_role = false;
        let out = tools::ScanOut {
            text: "만들겠습니다.".into(),
            calls: vec![call(0, "write", "{}"), call(1, "read", "{}")],
        };
        let chunks = scan(out, &mut sent_role);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0]["choices"][0]["delta"]["content"], "만들겠습니다.");
        assert_eq!(chunks[1]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(chunks[2]["choices"][0]["delta"]["tool_calls"][0]["index"], 1);
        // role 은 맨 앞 청크에만.
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        assert!(chunks[1]["choices"][0]["delta"].get("role").is_none());
    }

    #[test]
    fn empty_scan_emits_nothing() {
        let mut sent_role = false;
        assert!(scan(tools::ScanOut::default(), &mut sent_role).is_empty());
        assert!(!sent_role, "빈 출력이 role 을 소비하면 안 됩니다");
    }
}
