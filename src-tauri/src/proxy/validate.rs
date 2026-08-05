//! 채팅 요청 검증. 규약 위반은 사내 호출 **전에** 400 으로 걸러냅니다.
//!
//! 검증을 핸들러 곳곳에 흩지 않고 여기 한 곳에 모으는 이유는 두 가지입니다 —
//! `param`/`code` 를 빠뜨리지 않으려면 표 하나로 보는 편이 낫고, 통과한 요청에서
//! 뽑아낸 값([`Plan`])을 응답 조립까지 그대로 들고 갈 수 있어야 합니다.
//!
//! **일부러 검증하지 않는 것**도 있습니다.
//!
//! - 이름이 빈 도구 — 자리표시자 도구를 실어 보내는 클라이언트가 실제로 있어,
//!   `declared_tools()` 가 조용히 걸러내는 기존 동작을 유지합니다.
//! - 선언되지 않은 도구를 지정한 `tool_choice` — 스캐너가 이름을 검증하므로 오작동이
//!   아니고, 로그 note 로 보이면 충분합니다.
//! - 공백만 든 `messages` — `fold_messages` 이후에 판단해야 합니다. 도구만 강제하는
//!   라운드에는 `"(continue)"` 탈출구가 이미 있습니다(`chat.rs`).

use serde_json::Value;

use crate::openai::{ChatRequest, ErrorEnvelope, Message};

/// 스펙에 있는 채팅 요청 키. 여기 없는 키는 거부하지 않고 로그에만 적습니다.
const KNOWN_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "temperature",
    "top_p",
    "top_k",
    "n",
    "stop",
    "max_tokens",
    "max_completion_tokens",
    "seed",
    "frequency_penalty",
    "presence_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "user",
    "response_format",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "functions",
    "function_call",
    "metadata",
    "store",
    "service_tier",
];

/// OpenAI 가 정의한 롤. 그 밖의 값은 400 입니다.
const KNOWN_ROLES: &[&str] = &["system", "developer", "user", "assistant", "tool", "function"];

/// 검증을 통과한 요청에서 뽑아낸 실행 계획. 이후 코드는 원본 대신 여기 값을 봅니다.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    /// 스트림 꼬리에 usage 청크를 넣어야 하는가.
    pub include_usage: bool,
    /// 사내가 받지 못해 버린 이미지 파트 수.
    pub images_dropped: usize,
    /// 받았지만 사내로 보낼 수 없는 필드 이름들 — 로그에 "무시했다"고 적습니다.
    pub ignored: Vec<&'static str>,
    /// 스펙에 없는 키 — 거부하지 않고 표시만 합니다.
    pub unknown: Vec<String>,
}

/// 400 한 건. `chat.rs` 가 로그와 응답 양쪽에 씁니다.
#[derive(Debug, Clone)]
pub struct Invalid {
    pub status: u16,
    pub message: String,
    pub code: &'static str,
    pub param: Option<String>,
}

impl Invalid {
    fn new(message: impl Into<String>, code: &'static str, param: impl Into<String>) -> Self {
        Self { status: 400, message: message.into(), code, param: Some(param.into()) }
    }

    pub fn envelope(&self) -> ErrorEnvelope {
        let env = ErrorEnvelope::new(
            self.message.clone(),
            super::openai_type(self.status),
            Some(self.code.to_string()),
        );
        match &self.param {
            Some(p) => env.with_param(p.clone()),
            None => env,
        }
    }

    /// 로그 목록 두 번째 줄.
    pub fn note(&self) -> String {
        match &self.param {
            Some(p) => format!("잘못된 요청 · {p}"),
            None => "잘못된 요청".to_string(),
        }
    }
}

/// `raw` 는 `handle` 이 로그용으로 이미 파싱해 둔 원문 — 모르는 키 수집에만 씁니다.
pub fn plan(req: &ChatRequest, raw: &Value) -> Result<Plan, Invalid> {
    // ── messages ──
    if raw.get("messages").is_none() {
        return Err(Invalid::new(
            "messages 는 필수입니다.",
            "missing_required_parameter",
            "messages",
        ));
    }
    if req.messages.is_empty() {
        return Err(Invalid::new(
            "messages 가 비어 있습니다. 최소 한 개의 메시지가 필요합니다.",
            "invalid_value",
            "messages",
        ));
    }
    for (i, m) in req.messages.iter().enumerate() {
        check_message(i, m)?;
    }

    // 텍스트가 하나도 살아남지 않는 이미지 전용 요청. 사내 채팅 API 는 이미지를 받지
    // 못하므로, 그대로 보내면 "무엇을 물었는지 없는" 요청이 나갑니다.
    let images_dropped: usize = req.messages.iter().map(Message::image_parts).sum();
    if images_dropped > 0 && !req.messages.iter().any(Message::has_text) {
        return Err(Invalid::new(
            "이미지만 있는 요청은 사내 채팅 API 가 받지 못합니다. 텍스트를 함께 보내거나 \
             이미지 편집은 /v1/images/edits 를 쓰세요.",
            "unsupported_content",
            "messages[0].content",
        ));
    }

    // ── 숫자 범위 ──
    range(req.temperature, 0.0, 2.0, "temperature")?;
    range(req.top_p, 0.0, 1.0, "top_p")?;
    range(req.frequency_penalty, -2.0, 2.0, "frequency_penalty")?;
    range(req.presence_penalty, -2.0, 2.0, "presence_penalty")?;
    positive(req.max_tokens, "max_tokens")?;
    positive(req.max_completion_tokens, "max_completion_tokens")?;

    // ── n ──
    //
    // n>1 만 거절합니다. `n` 은 응답의 **모양**을 바꾸는 값이라 1개만 돌려주면
    // 클라이언트가 조용히 잘못된 결과를 얻습니다. 팬아웃(사내 호출 N번)을 넣을 자리는
    // `chat.rs::handle` 의 한 블록이지만, 사내 쿼터가 N배로 들고 `seed` 만으로는
    // 답변 다양성이 보장되지 않아 지금은 하지 않습니다.
    if let Some(n) = req.n {
        if n < 1 {
            return Err(Invalid::new("n 은 1 이상이어야 합니다.", "invalid_value", "n"));
        }
        if n > 1 {
            return Err(Invalid::new(
                "n>1 은 지원하지 않습니다 — 사내 API 에 대응 필드가 없습니다. n 을 빼거나 1 로 보내세요.",
                "unsupported_value",
                "n",
            ));
        }
    }

    // ── stop ──
    if let Some(stop) = &req.stop {
        if stop.raw_len() > 4 {
            return Err(Invalid::new(
                "stop 은 최대 4개까지입니다.",
                "invalid_value",
                "stop",
            ));
        }
    }

    // ── logprobs ──
    //
    // 참이면 거절합니다. `logprobs: null` 을 돌려주면 `choices[0].logprobs.content` 를
    // 까는 클라이언트가 원인에서 먼 곳에서 죽습니다. 문 앞에서 크게 실패하는 편이 낫습니다.
    let wants_logprobs = req.logprobs.as_ref().is_some_and(|l| l.wants());
    if wants_logprobs {
        return Err(Invalid::new(
            "logprobs 는 지원하지 않습니다 — 사내 API 가 토큰 확률을 주지 않습니다.",
            "unsupported_value",
            "logprobs",
        ));
    }
    if let Some(top) = req.top_logprobs {
        if top > 20 {
            return Err(Invalid::new(
                "top_logprobs 는 0–20 사이여야 합니다.",
                "invalid_value",
                "top_logprobs",
            ));
        }
        return Err(Invalid::new(
            "top_logprobs 는 logprobs: true 와 함께만 쓸 수 있고, logprobs 는 지원하지 않습니다.",
            "invalid_value",
            "top_logprobs",
        ));
    }

    // ── response_format ──
    if let Some(fmt) = &req.response_format {
        let kind = fmt.kind.as_deref().unwrap_or("text");
        if !matches!(kind, "text" | "json_object" | "json_schema") {
            return Err(Invalid::new(
                format!("response_format.type '{kind}' 은 알 수 없는 값입니다."),
                "invalid_value",
                "response_format.type",
            ));
        }
        if kind == "json_schema" && fmt.json_schema.is_none() {
            return Err(Invalid::new(
                "response_format.type 이 json_schema 이면 json_schema 가 필요합니다.",
                "missing_required_parameter",
                "response_format.json_schema",
            ));
        }
    }

    // ── stream_options ──
    if req.stream_options.is_some() && !req.is_stream() {
        return Err(Invalid::new(
            "stream_options 는 stream: true 일 때만 쓸 수 있습니다.",
            "invalid_value",
            "stream_options",
        ));
    }

    Ok(Plan {
        include_usage: req.wants_usage_chunk(),
        images_dropped,
        ignored: ignored_fields(req),
        unknown: unknown_keys(raw),
    })
}

fn check_message(i: usize, m: &Message) -> Result<(), Invalid> {
    let role = m.role.trim();
    if !KNOWN_ROLES.contains(&role) {
        let shown = if role.is_empty() { "(빈 값)" } else { role };
        return Err(Invalid::new(
            format!("messages[{i}].role '{shown}' 은 알 수 없는 롤입니다."),
            "invalid_value",
            format!("messages[{i}].role"),
        ));
    }

    // 도구 호출만 있는 assistant 턴은 content 가 null 인 것이 정상입니다.
    let content_exempt = role == "assistant" && !m.tool_calls().is_empty();
    if m.content.is_none() && !content_exempt {
        return Err(Invalid::new(
            format!("messages[{i}].content 가 없습니다."),
            "missing_required_parameter",
            format!("messages[{i}].content"),
        ));
    }

    if matches!(role, "tool" | "function") && m.tool_call_id.is_none() && m.name.is_none() {
        return Err(Invalid::new(
            format!("messages[{i}] 은 tool_call_id 또는 name 이 있어야 어느 호출의 결과인지 알 수 있습니다."),
            "missing_required_parameter",
            format!("messages[{i}].tool_call_id"),
        ));
    }

    Ok(())
}

fn range(value: Option<f64>, lo: f64, hi: f64, param: &'static str) -> Result<(), Invalid> {
    match value {
        // NaN 은 어느 비교에도 걸리지 않으므로 명시적으로 잡습니다.
        Some(v) if v.is_nan() || v < lo || v > hi => Err(Invalid::new(
            format!("{param} 은 {lo}–{hi} 사이여야 합니다 (받은 값: {v})."),
            "invalid_value",
            param,
        )),
        _ => Ok(()),
    }
}

fn positive(value: Option<u32>, param: &'static str) -> Result<(), Invalid> {
    match value {
        Some(0) => Err(Invalid::new(
            format!("{param} 은 1 이상이어야 합니다."),
            "invalid_value",
            param,
        )),
        _ => Ok(()),
    }
}

/// 받았지만 사내 API 에 대응이 없어 반영하지 못하는 필드들.
///
/// 사내 `llmConfig` 에는 `temperature` · `top_p` · `repetion_penalty` · `tok_k` ·
/// `seed` · `max_new_tokens` 뿐입니다. `frequency_penalty` 가 이미 유일한 페널티 키를
/// 차지하고 있어 `presence_penalty` 는 실을 자리가 없습니다 — 다른 의미의 두 값을 한 키에
/// 겹쳐 넣는 것은 상위 동작을 지어내는 일입니다.
fn ignored_fields(req: &ChatRequest) -> Vec<&'static str> {
    let mut out = Vec::new();
    if req.stop.as_ref().is_some_and(|s| !s.list().is_empty()) {
        out.push("stop");
    }
    if req.presence_penalty.is_some() {
        out.push("presence_penalty");
    }
    if req.logit_bias.as_ref().is_some_and(|v| !v.is_null()) {
        out.push("logit_bias");
    }
    if req.user.as_ref().is_some_and(|v| !v.is_null()) {
        out.push("user");
    }
    if req.response_format.as_ref().is_some_and(|f| {
        !matches!(f.kind.as_deref().unwrap_or("text"), "text")
    }) {
        out.push("response_format");
    }
    if req.parallel_tool_calls.is_some() {
        out.push("parallel_tool_calls");
    }
    if req.metadata.as_ref().is_some_and(|v| !v.is_null()) {
        out.push("metadata");
    }
    if req.store.as_ref().is_some_and(|v| !v.is_null()) {
        out.push("store");
    }
    if req.service_tier.as_ref().is_some_and(|v| !v.is_null()) {
        out.push("service_tier");
    }
    out
}

fn unknown_keys(raw: &Value) -> Vec<String> {
    let Some(map) = raw.as_object() else {
        return Vec::new();
    };
    map.keys()
        .filter(|k| !KNOWN_KEYS.contains(&k.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(body: &str) -> Result<Plan, Invalid> {
        let raw: Value = serde_json::from_str(body).unwrap();
        let req: ChatRequest = serde_json::from_str(body).unwrap();
        plan(&req, &raw)
    }

    fn err(body: &str) -> Invalid {
        check(body).expect_err("400 이 나야 합니다")
    }

    fn assert_400(body: &str, code: &str, param: &str) {
        let e = err(body);
        assert_eq!(e.status, 400, "{body}");
        assert_eq!(e.code, code, "{body}");
        assert_eq!(e.param.as_deref(), Some(param), "{body}");
    }

    const USER: &str = r#"{"role":"user","content":"안녕"}"#;

    #[test]
    fn messages_must_be_present_and_non_empty() {
        assert_400(r#"{"model":"m"}"#, "missing_required_parameter", "messages");
        assert_400(r#"{"model":"m","messages":[]}"#, "invalid_value", "messages");
    }

    #[test]
    fn unknown_and_empty_roles_are_rejected() {
        assert_400(
            r#"{"messages":[{"role":"보스","content":"hi"}]}"#,
            "invalid_value",
            "messages[0].role",
        );
        assert_400(
            r#"{"messages":[{"role":"","content":"hi"}]}"#,
            "invalid_value",
            "messages[0].role",
        );
    }

    #[test]
    fn missing_content_is_rejected_except_for_tool_only_assistant_turns() {
        assert_400(
            r#"{"messages":[{"role":"user"}]}"#,
            "missing_required_parameter",
            "messages[0].content",
        );
        // 도구 호출만 있는 assistant 턴은 content: null 이 규약입니다.
        assert!(check(
            r#"{"messages":[{"role":"user","content":"hi"},
                {"role":"assistant","content":null,
                 "tool_calls":[{"id":"c1","type":"function",
                                "function":{"name":"w","arguments":"{}"}}]},
                {"role":"tool","tool_call_id":"c1","content":"ok"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn tool_results_need_an_id_or_a_name() {
        assert_400(
            &format!(r#"{{"messages":[{USER},{{"role":"tool","content":"ok"}}]}}"#),
            "missing_required_parameter",
            "messages[1].tool_call_id",
        );
        // 구형 role:"function" 은 name 으로 상관시킵니다.
        assert!(check(&format!(
            r#"{{"messages":[{USER},{{"role":"function","name":"calc","content":"3"}}]}}"#
        ))
        .is_ok());
    }

    #[test]
    fn numeric_ranges_are_enforced() {
        assert_400(&format!(r#"{{"messages":[{USER}],"temperature":5}}"#), "invalid_value", "temperature");
        assert_400(&format!(r#"{{"messages":[{USER}],"temperature":-0.1}}"#), "invalid_value", "temperature");
        assert_400(&format!(r#"{{"messages":[{USER}],"top_p":1.5}}"#), "invalid_value", "top_p");
        assert_400(&format!(r#"{{"messages":[{USER}],"presence_penalty":3}}"#), "invalid_value", "presence_penalty");
        assert_400(&format!(r#"{{"messages":[{USER}],"frequency_penalty":-9}}"#), "invalid_value", "frequency_penalty");
        assert_400(&format!(r#"{{"messages":[{USER}],"max_tokens":0}}"#), "invalid_value", "max_tokens");
        // 경계값은 통과해야 합니다.
        assert!(check(&format!(r#"{{"messages":[{USER}],"temperature":0,"top_p":1}}"#)).is_ok());
        assert!(check(&format!(r#"{{"messages":[{USER}],"temperature":2,"top_p":0}}"#)).is_ok());
    }

    #[test]
    fn n_greater_than_one_is_rejected_but_one_passes() {
        assert_400(&format!(r#"{{"messages":[{USER}],"n":2}}"#), "unsupported_value", "n");
        assert_400(&format!(r#"{{"messages":[{USER}],"n":0}}"#), "invalid_value", "n");
        assert!(check(&format!(r#"{{"messages":[{USER}],"n":1}}"#)).is_ok());
    }

    #[test]
    fn stop_is_capped_at_four() {
        assert_400(&format!(r#"{{"messages":[{USER}],"stop":["1","2","3","4","5"]}}"#), "invalid_value", "stop");
        assert!(check(&format!(r#"{{"messages":[{USER}],"stop":["1","2","3","4"]}}"#)).is_ok());
    }

    #[test]
    fn logprobs_is_rejected_loudly() {
        assert_400(&format!(r#"{{"messages":[{USER}],"logprobs":true}}"#), "unsupported_value", "logprobs");
        assert_400(&format!(r#"{{"messages":[{USER}],"top_logprobs":3}}"#), "invalid_value", "top_logprobs");
        assert_400(&format!(r#"{{"messages":[{USER}],"top_logprobs":99}}"#), "invalid_value", "top_logprobs");
        // false 는 명시적 거절이므로 통과합니다.
        assert!(check(&format!(r#"{{"messages":[{USER}],"logprobs":false}}"#)).is_ok());
    }

    #[test]
    fn response_format_shapes_are_checked() {
        assert_400(
            &format!(r#"{{"messages":[{USER}],"response_format":{{"type":"xml"}}}}"#),
            "invalid_value",
            "response_format.type",
        );
        assert_400(
            &format!(r#"{{"messages":[{USER}],"response_format":{{"type":"json_schema"}}}}"#),
            "missing_required_parameter",
            "response_format.json_schema",
        );
        assert!(check(&format!(r#"{{"messages":[{USER}],"response_format":{{"type":"text"}}}}"#)).is_ok());
    }

    #[test]
    fn stream_options_requires_streaming() {
        assert_400(
            &format!(r#"{{"messages":[{USER}],"stream_options":{{"include_usage":true}}}}"#),
            "invalid_value",
            "stream_options",
        );
        let ok = check(&format!(
            r#"{{"messages":[{USER}],"stream":true,"stream_options":{{"include_usage":true}}}}"#
        ))
        .unwrap();
        assert!(ok.include_usage);
    }

    #[test]
    fn image_only_requests_are_rejected_but_image_plus_text_passes() {
        assert_400(
            r#"{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:…"}}]}]}"#,
            "unsupported_content",
            "messages[0].content",
        );
        let ok = check(
            r#"{"messages":[{"role":"user","content":[
                {"type":"text","text":"이거 뭐야"},
                {"type":"image_url","image_url":{"url":"data:…"}}]}]}"#,
        )
        .unwrap();
        assert_eq!(ok.images_dropped, 1);
    }

    #[test]
    fn ignored_fields_are_listed_not_rejected() {
        let ok = check(&format!(
            r#"{{"messages":[{USER}],"stop":["끝"],"presence_penalty":0.5,
                "logit_bias":{{"1":1}},"user":"kim","parallel_tool_calls":true,
                "metadata":{{"a":1}},"store":true,"service_tier":"auto",
                "response_format":{{"type":"json_object"}}}}"#
        ))
        .unwrap();
        assert_eq!(
            ok.ignored,
            vec![
                "stop",
                "presence_penalty",
                "logit_bias",
                "user",
                "response_format",
                "parallel_tool_calls",
                "metadata",
                "store",
                "service_tier",
            ]
        );
    }

    #[test]
    fn explicit_nulls_and_text_format_are_not_reported_as_ignored() {
        let ok = check(&format!(
            r#"{{"messages":[{USER}],"stop":null,"logit_bias":null,"user":null,
                "response_format":{{"type":"text"}},"metadata":null}}"#
        ))
        .unwrap();
        assert!(ok.ignored.is_empty(), "{:?}", ok.ignored);
    }

    #[test]
    fn unknown_keys_are_reported_not_rejected() {
        let ok = check(&format!(r#"{{"messages":[{USER}],"래빗홀":1,"foo":"bar"}}"#)).unwrap();
        assert_eq!(ok.unknown.len(), 2);
        assert!(ok.unknown.contains(&"foo".to_string()));
        assert!(ok.unknown.contains(&"래빗홀".to_string()));
    }

    /// 실제 클라이언트 두 개가 그대로 통과해야 합니다.
    #[test]
    fn real_client_requests_pass() {
        // OpenCode(@ai-sdk/openai-compatible) 2라운드.
        assert!(check(
            r#"{"model":"fabrix-chat-4","stream":true,
                "messages":[{"role":"system","content":"you are a coding agent"},
                            {"role":"user","content":"make a page"},
                            {"role":"assistant","content":null,"tool_calls":[
                              {"id":"call_a1","type":"function",
                               "function":{"name":"write","arguments":"{\"filePath\":\"a.html\"}"}}]},
                            {"role":"tool","tool_call_id":"call_a1","content":"wrote 12 bytes"}],
                "tools":[{"type":"function","function":{"name":"write"}}],
                "tool_choice":"auto","parallel_tool_calls":true}"#
        )
        .is_ok());

        // Continue.dev 자동완성.
        assert!(check(
            r#"{"model":"fabrix-code","stream":true,"temperature":0,"max_tokens":256,
                "stop":["\n\n","```"],
                "messages":[{"role":"user","content":"complete this"}]}"#
        )
        .is_ok());
    }

    #[test]
    fn envelope_carries_param_and_code() {
        let json = serde_json::to_value(
            err(&format!(r#"{{"messages":[{USER}],"temperature":5}}"#)).envelope(),
        )
        .unwrap();
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["code"], "invalid_value");
        assert_eq!(json["error"]["param"], "temperature");
        assert!(json["error"]["message"].as_str().unwrap().contains("temperature"));
    }
}
