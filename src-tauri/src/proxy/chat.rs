//! `POST /v1/chat/completions` — 프록시의 심장.
//!
//! 흐름: 받은 요청(OpenAI) → 변환해서 보낸 요청(FabriX) → 돌려준 응답.
//! 로그 창의 세 칸이 정확히 이 세 단계입니다.

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
};
use crate::state::{self, Shared};

use super::fabrix::{
    extract_object, fold_messages, resolve_model, FabrixChunk, FabrixError, LlmConfig,
    MessagesRequest, ResolvedModel, StreamDecoder, StreamEvent, MESSAGES_PATH,
};
use super::models::ensure_models;
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
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    fn entry(
        &self,
        status: u16,
        is_error: bool,
        note: Option<String>,
        summary: Option<String>,
        resp_preview: String,
        resp_meta: String,
    ) -> LogEntry {
        LogEntry {
            id: Uuid::new_v4().to_string(),
            ts: state::now_hm(),
            ts_full: state::now_iso(),
            kind: Kind::Chat,
            method: Kind::Chat.method(),
            path: Kind::Chat.path(),
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
            resp_preview,
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

    let Some(model) = resolve_model(&models, req.model.as_deref(), &cfg.default_model_alias) else {
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
    let (system_prompt, contents) = fold_messages(&req.messages);
    if contents.is_empty() {
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
    let alias = model.alias.clone();
    if payload.is_stream {
        stream_response(state, ctx, res, alias)
    } else {
        collect_response(state, ctx, res, alias).await
    }
}

fn sse_json<T: serde::Serialize>(value: &T) -> Event {
    Event::default().data(serde_json::to_string(value).unwrap_or_default())
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

        let note = match (aborted, self.failure.is_some()) {
            (true, _) => Some("클라이언트가 연결을 끊음".to_string()),
            (false, true) => Some("스트리밍 중 끊김".to_string()),
            (false, false) => None,
        };

        let preview = match (&self.failure, text.is_empty()) {
            (Some(msg), true) => msg.clone(),
            (Some(msg), false) => format!("{}\n\n[중단] {msg}", logstore::preview(&text, 600)),
            (None, true) if aborted => "(클라이언트가 먼저 끊어 받은 내용이 없습니다)".into(),
            (None, true) => "(빈 응답)".into(),
            (None, false) => logstore::preview(&text, 600),
        };

        let failed = note.is_some();
        self.state.record(self.ctx.entry(
            // 헤더는 이미 200 으로 나갔으므로 있는 그대로 기록하고,
            // 끊긴 사실은 note 로 표시합니다.
            200,
            failed,
            note.clone(),
            if failed { note } else { self.ctx.success_summary() },
            preview,
            meta.join(" · "),
        ));
    }
}

/// FabriX 스트림을 OpenAI `chat.completion.chunk` SSE 로 옮겨 흘립니다.
///
/// 로그는 스트림이 완전히 끝난 뒤에 남깁니다 — 첫 토큰 지연과 프레임 수를
/// 그때서야 알 수 있기 때문입니다.
fn stream_response(state: Shared, ctx: Ctx, res: reqwest::Response, model: String) -> Response {
    let id = format!("chatcmpl-{}", Uuid::new_v4().simple());
    let created = state::epoch_secs();

    let body = async_stream::stream! {
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
                        let delta = Delta {
                            role: (!sent_role).then_some("assistant"),
                            content: Some(text),
                            reasoning_content: None,
                        };
                        sent_role = true;
                        yield Ok::<Event, Infallible>(sse_json(&ChatChunk::new(&id, created, &model, delta, None)));
                    }
                    StreamEvent::Reasoning(text) => {
                        let delta = Delta {
                            role: (!sent_role).then_some("assistant"),
                            content: None,
                            reasoning_content: Some(text),
                        };
                        sent_role = true;
                        yield Ok(sse_json(&ChatChunk::new(&id, created, &model, delta, None)));
                    }
                    StreamEvent::Finish(reason) => log.finish = Some(reason),
                    StreamEvent::Error(msg) => {
                        log.failure = Some(msg);
                        break 'pump;
                    }
                    StreamEvent::Done => break 'pump,
                }
            }
        }

        // 개행 없이 끝난 마지막 프레임.
        for event in log.decoder.finish() {
            match event {
                StreamEvent::Delta(text) => {
                    let started = log.ctx.started;
                    log.first_token.get_or_insert_with(|| started.elapsed());
                    let delta = Delta {
                        role: (!sent_role).then_some("assistant"),
                        content: Some(text),
                        reasoning_content: None,
                    };
                    sent_role = true;
                    yield Ok(sse_json(&ChatChunk::new(&id, created, &model, delta, None)));
                }
                StreamEvent::Finish(reason) => log.finish = Some(reason),
                _ => {}
            }
        }
        log.drained = true;

        if let Some(msg) = log.failure.clone() {
            yield Ok(sse_json(&ErrorEnvelope::new(msg, "upstream_error", None)));
        } else {
            let reason = log.finish.clone().unwrap_or_else(|| "stop".into());
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
                via_stream_decoder: true,
            }
        }
    };

    if parsed.content.is_empty() && parsed.reasoning.is_none() {
        let err = FabrixError::BadPayload(format!("본문 앞부분: {}", logstore::preview(&raw, 200)));
        state.record(ctx.fail(&err));
        return error_response(err.status(), ErrorEnvelope::new(err.message(), err.kind(), None));
    }

    let reason = parsed.finish.clone().unwrap_or_else(|| "stop".into());
    let completion = ChatCompletion {
        id: format!("chatcmpl-{}", Uuid::new_v4().simple()),
        object: "chat.completion",
        created: state::epoch_secs(),
        model: model.clone(),
        choices: vec![Choice {
            index: 0,
            message: AssistantMessage {
                role: "assistant",
                content: parsed.content.clone(),
                reasoning_content: parsed.reasoning.clone(),
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

    state.record(ctx.entry(
        200,
        false,
        None,
        ctx.success_summary(),
        logstore::preview(&parsed.content, 600),
        meta.join(" · "),
    ));

    Json(completion).into_response()
}

struct Parsed {
    content: String,
    reasoning: Option<String>,
    finish: Option<String>,
    via_stream_decoder: bool,
}
