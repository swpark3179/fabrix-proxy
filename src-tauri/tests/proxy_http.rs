//! HTTP 표면 통합 테스트 — 프록시를 **진짜로 띄우고 진짜 요청을 보내** 확인합니다.
//!
//! 왜 필요한가: 인라인 단위 테스트는 함수 하나하나를 보지만, 이번 규약 준수 작업에서
//! 실제로 깨질 수 있는 것은 *조립된 결과* 입니다 — 라우팅(405 는 `.fallback` 이 아니라
//! `method_not_allowed_fallback` 이 잡습니다), 본문 상한을 핸들러 안에서 거는 경로,
//! SSE 청크의 **순서**(첫 role 청크 → 내용 → finish → usage → `[DONE]`), 그리고
//! 오류 봉투가 실제로 그 상태 코드와 함께 나가는지. 그건 HTTP 로만 확인할 수 있습니다.
//!
//! 구성: 가짜 FabriX 상위 서버(axum)를 포트 0 에 띄우고, 프록시를 그 서버로 가리켜
//! 포트 0 에 띄우고, `reqwest` 로 때립니다. 사내 서버도 목업 노드 프로세스도 필요 없습니다.

use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State as AxState;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use fabrix_proxy_lib::config::Config;
use fabrix_proxy_lib::proxy;
use fabrix_proxy_lib::state::{AppState, Shared};

// ─────────────────────────── 가짜 사내 서버 ───────────────────────────

/// 상위가 어떻게 답할지 — 테스트마다 갈아 끼웁니다.
#[derive(Clone)]
struct Upstream {
    /// 비스트림·스트림 모두에서 답변으로 쓸 텍스트(`content` 채널).
    answer: String,
    /// 추론 채널(`reasoningContent` / 스트림 `reasoning_content`)로 흘릴 텍스트.
    ///
    /// 사내 추론 모델이 센티널을 이쪽에 실어 보내는 것이 "추론 단계마다 stop" 의
    /// 원인이었습니다. 그 경로를 재현할 수단이 여태 없었습니다.
    reasoning: String,
    /// 마지막 프레임의 `finishReason`. `None` 이면 비스트림은 `null`, 스트림은 `"stop"`.
    finish: Option<String>,
    /// 상위가 토큰 수를 준 경우 `(input, output)`.
    tokens: Option<(u32, u32)>,
    /// 답변을 비우되 성공 표지는 남깁니다.
    empty: bool,
    /// SSE 한 프레임에 담을 글자 수. 낮추면 센티널 한가운데가 갈립니다.
    chunk: usize,
    /// 답변을 흘리다 오류 프레임을 끼워 넣고 끊습니다.
    fail_midstream: bool,
    /// `isStream=false` 요청에도 SSE 로 답합니다 — 프록시의 폴백 경로를 태웁니다.
    sse_always: bool,
    /// 프록시가 실제로 보낸 payload 들 — 꼬리 리마인더 검증용.
    seen: Vec<Value>,
}

impl Default for Upstream {
    fn default() -> Self {
        Self {
            answer: String::new(),
            reasoning: String::new(),
            finish: None,
            tokens: None,
            empty: false,
            chunk: 5,
            fail_midstream: false,
            sse_always: false,
            seen: Vec::new(),
        }
    }
}

impl Upstream {
    fn answering(text: &str) -> Self {
        Self { answer: text.into(), ..Default::default() }
    }

    /// 답변을 **추론 채널로만** 흘리는 상위. 이번 버그의 실제 모양입니다.
    fn reasoning_only(text: &str) -> Self {
        Self { reasoning: text.into(), ..Default::default() }
    }
}

/// `write` 도구 한 건을 부르는 센티널 블록.
fn sentinel() -> String {
    format!(
        "<tool_call>{}</tool_call>",
        json!({ "name": "write", "arguments": { "filePath": "index.html" } })
    )
}

/// `tools` 를 실은 요청 본문.
fn with_write_tool(mut body: Value) -> Value {
    body["tools"] = json!([{
        "type": "function",
        "function": {
            "name": "write",
            "description": "Write a file",
            "parameters": {
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
            },
        },
    }]);
    body
}

const MODEL_UUID: &str = "0196f1fc-2858-70a9-a232-74dbddb971d0";
const KO_ONLY_UUID: &str = "01970a3b-91d4-7c8e-9a11-2f3c4d5e6f75";

async fn upstream_models() -> Json<Value> {
    Json(json!({
        "data": [
            {
                "modelId": MODEL_UUID,
                "name": [
                    { "languageCode": "ko", "content": "챗 4" },
                    { "languageCode": "en", "content": "Chat 4" },
                ],
                "description": [{ "languageCode": "ko", "content": "범용 대화 모델" }],
            },
            // 한글 전용 이름 — alias 가 UUID 앞 8자리로 떨어지는 경로.
            {
                "modelId": KO_ONLY_UUID,
                "name": [{ "languageCode": "ko", "content": "사내규정" }],
                "description": [],
            },
        ]
    }))
}

async fn upstream_messages(
    AxState(up): AxState<Arc<Mutex<Upstream>>>,
    body: String,
) -> Response {
    let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let stream = req.get("isStream").and_then(Value::as_bool).unwrap_or(false);
    let up = {
        let mut guard = up.lock().unwrap();
        guard.seen.push(req);
        guard.clone()
    };
    let answer = if up.empty { String::new() } else { up.answer.clone() };
    let reasoning = if up.empty { String::new() } else { up.reasoning.clone() };

    if !stream && !up.sse_always {
        let mut payload = json!({
            "modelType": "Chat 4",
            "content": answer,
            "reasoningContent": reasoning,
            "truncated": false,
            "finishReason": up.finish,
            "status": "SUCCESS",
            "responseCode": "R20000",
        });
        if let Some((input, output)) = up.tokens {
            payload["inputTokens"] = json!(input);
            payload["outputTokens"] = json!(output);
        }
        return Json(payload).into_response();
    }

    // SSE — 추론을 먼저, 그다음 본문을 조각으로 흘린 뒤 종료 프레임과 [DONE].
    let mut out = String::new();
    let mut frames = 0usize;
    let chunk = up.chunk.max(1);

    let push_frames = |out: &mut String, frames: &mut usize, text: &str, key: &str| -> bool {
        let chars: Vec<char> = text.chars().collect();
        for piece in chars.chunks(chunk) {
            if up.fail_midstream && *frames == 2 {
                let err = json!({ "status": "ERROR", "event_data": "사내 처리 중 오류(모의)" });
                out.push_str(&format!("data: {err}\n\n"));
                return false;
            }
            let body: String = piece.iter().collect();
            out.push_str(&format!("data: {}\n\n", json!({ key: body })));
            *frames += 1;
        }
        true
    };

    let mut alive = push_frames(&mut out, &mut frames, &reasoning, "reasoning_content");
    if alive {
        alive = push_frames(&mut out, &mut frames, &answer, "content");
    }

    if alive {
        let mut last = json!({
            "content": "",
            "finish_reason": up.finish.clone().unwrap_or_else(|| "stop".into()),
            "status": "SUCCESS",
        });
        if let Some((input, output)) = up.tokens {
            last["input_tokens"] = json!(input);
            last["output_tokens"] = json!(output);
        }
        out.push_str(&format!("data: {last}\n\n"));
        out.push_str("data: [DONE]\n\n");
    }

    ([("content-type", "text/event-stream")], out).into_response()
}

/// 가짜 상위를 띄우고 `(base_url, 설정 핸들)` 을 돌려줍니다.
async fn spawn_upstream(initial: Upstream) -> (String, Arc<Mutex<Upstream>>) {
    let shared = Arc::new(Mutex::new(initial));
    let app = Router::new()
        .route("/openapi/chat/v1/models", get(upstream_models))
        .route("/openapi/chat/v1/messages", post(upstream_messages))
        .with_state(shared.clone());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}"), shared)
}

// ─────────────────────────── 프록시 띄우기 ───────────────────────────

/// 테스트가 실제 `~/.fabrix-proxy/stats.json` 을 건드리지 않게 HOME 을 임시 폴더로 돌립니다.
/// `config::config_dir()` 이 `dirs::home_dir()` 를 쓰므로 이걸로 충분합니다.
fn isolate_home() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("fabrix-proxy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        // SAFETY: 테스트 바이너리 시작 시 한 번만, 다른 스레드가 HOME 을 읽기 전에 씁니다.
        unsafe { std::env::set_var("HOME", &dir) };
    });
}

fn state_with(cfg: Config) -> Shared {
    isolate_home();
    // 창이 없으므로 이벤트를 흘릴 곳도 없습니다 — 프록시 서버 자체는 AppHandle 을 쓰지
    // 않으므로 이 상태로 HTTP 표면 전부를 확인할 수 있습니다.
    AppState::headless(cfg, false)
}

/// 프록시를 포트 0 에 띄우고 base URL 을 돌려줍니다. `state` 를 함께 돌려주는 이유는
/// 테스트가 끝날 때까지 살려 둬야 서버가 죽지 않기 때문입니다.
async fn spawn_proxy(cfg: Config) -> (String, Shared) {
    let state = state_with(cfg);
    let port = proxy::start(state.clone(), 0).await.expect("프록시가 떠야 합니다");
    (format!("http://127.0.0.1:{port}"), state)
}

fn config_for(upstream: &str) -> Config {
    Config {
        fabrix_base_url: upstream.to_string(),
        fabrix_client: "test-client".into(),
        openapi_token: "test-token".into(),
        ..Config::default()
    }
}

/// 가짜 상위 + 프록시를 한 번에. 대부분의 테스트가 이걸 씁니다.
async fn harness(up: Upstream) -> (String, Shared, Arc<Mutex<Upstream>>) {
    let (upstream_url, handle) = spawn_upstream(up).await;
    let (base, state) = spawn_proxy(config_for(&upstream_url)).await;
    (base, state, handle)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// 채팅 요청 하나. 본문을 그대로 받습니다.
async fn chat(base: &str, body: Value) -> (u16, Value) {
    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    let text = res.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

// ─────────────────────────── 1. 비스트림 응답 모양 ───────────────────────────

#[tokio::test]
async fn non_stream_response_carries_usage_logprobs_and_fingerprint() {
    let (base, _state, _up) = harness(Upstream::answering("연차는 15일입니다.")).await;

    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "연차 알려줘" }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status().as_u16(), 200);
    // 추정임을 헤더로 말해야 합니다.
    assert_eq!(res.headers().get("x-fabrix-usage").unwrap(), "estimated");

    let body: Value = res.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["model"], "fabrix-chat-4", "요청 문자열을 그대로 되돌려줘야 합니다");
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert_eq!(body["choices"][0]["message"]["content"], "연차는 15일입니다.");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");

    // logprobs 키는 **있어야** 하고 값은 null 이어야 합니다.
    assert!(body["choices"][0].as_object().unwrap().contains_key("logprobs"));
    assert!(body["choices"][0]["logprobs"].is_null());

    assert!(body["system_fingerprint"].as_str().unwrap().starts_with("fp_"));
    assert!(body["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
    assert!(body["usage"]["completion_tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        body["usage"]["total_tokens"].as_u64().unwrap(),
        body["usage"]["prompt_tokens"].as_u64().unwrap()
            + body["usage"]["completion_tokens"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn upstream_token_counts_replace_the_estimate() {
    let up = Upstream { tokens: Some((812, 240)), ..Upstream::answering("짧은 답") };
    let (base, _state, _up) = harness(up).await;

    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.headers().get("x-fabrix-usage").unwrap(), "upstream");

    let body: Value = res.json().await.unwrap();
    assert_eq!(body["usage"]["prompt_tokens"], 812);
    assert_eq!(body["usage"]["completion_tokens"], 240);
    assert_eq!(body["usage"]["total_tokens"], 1052);
}

/// 상위가 규약 밖 값을 줘도 클라이언트에는 열거값만 나가야 합니다.
#[tokio::test]
async fn unknown_upstream_finish_reason_never_reaches_the_client() {
    let up = Upstream { finish: Some("weird".into()), ..Upstream::answering("답변") };
    let (base, _state, _up) = harness(up).await;

    let (status, body) = chat(
        &base,
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

/// 모델이 정말 빈 답을 준 경우 — 사내 잘못이 아니므로 502 가 아닙니다.
#[tokio::test]
async fn empty_answer_with_success_markers_is_200_not_502() {
    let up = Upstream { empty: true, finish: Some("stop".into()), ..Default::default() };
    let (base, _state, _up) = harness(up).await;

    let (status, body) = chat(
        &base,
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "");
    assert!(
        !body["choices"][0]["message"]["content"].is_null(),
        "도구 호출 턴만 null 이어야 합니다"
    );
}

// ─────────────────────────── 2. 스트리밍 순서 ───────────────────────────

/// SSE 본문을 `data:` 프레임들로 자릅니다.
fn frames(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn stream_opens_with_a_role_only_chunk_and_ends_with_done() {
    let (base, _state, _up) = harness(Upstream::answering("안녕하세요 반갑습니다")).await;

    let body = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let frames = frames(&body);
    assert!(frames.len() >= 3, "{frames:?}");

    // 첫 청크는 롤만. 예전에는 role 이 첫 내용 청크에 얹혀 나갔습니다.
    let first: Value = serde_json::from_str(&frames[0]).unwrap();
    assert_eq!(first["object"], "chat.completion.chunk");
    assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(first["choices"][0]["delta"]["content"], "");
    assert!(first["choices"][0]["finish_reason"].is_null());

    // 두 번째부터는 role 키가 아예 없어야 합니다.
    let second: Value = serde_json::from_str(&frames[1]).unwrap();
    assert!(second["choices"][0]["delta"].get("role").is_none(), "{second}");

    assert_eq!(frames.last().unwrap(), "[DONE]");

    // 마지막 finish 청크.
    let finish: Value = serde_json::from_str(&frames[frames.len() - 2]).unwrap();
    assert_eq!(finish["choices"][0]["finish_reason"], "stop");

    // 흘러간 내용을 합치면 원문입니다.
    let joined: String = frames
        .iter()
        .filter_map(|f| serde_json::from_str::<Value>(f).ok())
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str().map(str::to_string))
        .collect();
    assert_eq!(joined, "안녕하세요 반갑습니다");
}

/// 규약이 정한 꼬리 순서: finish 청크 → usage 청크(`choices: []`) → `[DONE]`.
#[tokio::test]
async fn include_usage_appends_a_usage_chunk_after_finish() {
    let (base, _state, _up) = harness(Upstream::answering("답변입니다")).await;

    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "fabrix-chat-4", "stream": true,
            "stream_options": { "include_usage": true },
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.headers().get("x-fabrix-usage").unwrap(), "estimated");

    let frames = frames(&res.text().await.unwrap());
    assert_eq!(frames.last().unwrap(), "[DONE]");

    let usage: Value = serde_json::from_str(&frames[frames.len() - 2]).unwrap();
    assert!(usage["choices"].as_array().unwrap().is_empty(), "usage 청크는 choices 가 빕니다");
    assert!(usage["usage"]["total_tokens"].as_u64().unwrap() > 0);

    let finish: Value = serde_json::from_str(&frames[frames.len() - 3]).unwrap();
    assert_eq!(finish["choices"][0]["finish_reason"], "stop");
}

/// include_usage 를 안 켰으면 usage 청크를 보내지 않습니다 — 규약이 옵트인입니다.
#[tokio::test]
async fn stream_without_include_usage_has_no_usage_chunk() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let body = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(!body.contains("\"usage\""), "{body}");
}

/// 도구 호출 에뮬레이션 회귀. 이번 변경(첫 role 청크 · finish 클램프 · 본문 읽기 전환)이
/// 가장 가까이 지나간 경로입니다.
#[tokio::test]
async fn tool_calls_survive_the_new_stream_prologue() {
    let answer = "만들겠습니다.\n<tool_call>\n\
        {\"name\":\"write\",\"arguments\":{\"filePath\":\"index.html\"}}\n\
        </tool_call>";
    let (base, _state, _up) = harness(Upstream::answering(answer)).await;

    let body = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "페이지 만들어줘" }],
            "tools": [{
                "type": "function",
                "function": { "name": "write", "parameters": { "type": "object" } }
            }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let frames = frames(&body);
    let chunks: Vec<Value> =
        frames.iter().filter_map(|f| serde_json::from_str(f).ok()).collect();

    // 도구 청크는 한 조각에 id · type · name · arguments 가 모두 들어 있어야 합니다
    // (@ai-sdk/openai-compatible 이 없으면 InvalidResponseDataError 를 던집니다).
    let call = chunks
        .iter()
        .find_map(|c| c["choices"][0]["delta"]["tool_calls"].as_array())
        .expect("tool_calls 청크가 있어야 합니다")[0]
        .clone();
    assert_eq!(call["index"], 0);
    assert!(call["id"].as_str().unwrap().starts_with("call_"));
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "write");
    assert!(call["function"]["arguments"].as_str().unwrap().contains("index.html"));

    // 도구를 썼으면 종료 사유가 tool_calls 여야 합니다.
    let finish = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["finish_reason"].as_str())
        .next_back()
        .unwrap();
    assert_eq!(finish, "tool_calls");
}

// ────────────── 2b. 추론 채널 도구 호출 (「추론 단계마다 stop」 회귀) ──────────────
//
// 예전 프록시는 `StreamEvent::Reasoning` 을 스캐너에 태우지 않고 곧바로
// `reasoning_content` 델타로 내보냈고, 비스트림 경로도 `content` 만 훑었습니다.
// 그래서 사내 추론 모델이 센티널을 추론 쪽에 실으면 호출이 **영구히 0건**이고
// `finish_reason` 이 **영구히 stop** 이었습니다. opencode 는 그 값을 보고 한 스텝
// 만에 루프를 끝냅니다 — 사용자가 본 증상이 정확히 이것입니다.

/// 모든 델타 청크에서 한 필드를 모아 잇습니다.
fn joined_delta(chunks: &[Value], field: &str) -> String {
    chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"][field].as_str())
        .collect()
}

fn last_finish(chunks: &[Value]) -> &str {
    chunks
        .iter()
        .filter_map(|c| c["choices"][0]["finish_reason"].as_str())
        .next_back()
        .expect("finish 청크가 있어야 합니다")
}

async fn stream_chunks(base: &str, body: Value) -> Vec<Value> {
    let text = client()
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    frames(&text).iter().filter_map(|f| serde_json::from_str(f).ok()).collect()
}

/// **이 테스트가 이번 작업의 합격 기준입니다.**
#[tokio::test]
async fn tool_call_only_in_reasoning_content_finishes_as_tool_calls() {
    let answer = format!("먼저 파일을 써야겠다.{}", sentinel());
    let (base, _state, _up) = harness(Upstream::reasoning_only(&answer)).await;

    let chunks = stream_chunks(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "페이지 만들어줘" }],
        })),
    )
    .await;

    let call = chunks
        .iter()
        .find_map(|c| c["choices"][0]["delta"]["tool_calls"].as_array())
        .expect("추론 채널의 도구 호출을 놓쳤습니다")[0]
        .clone();
    assert_eq!(call["index"], 0);
    assert!(call["id"].as_str().unwrap().starts_with("call_"));
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "write");
    assert!(call["function"]["arguments"].as_str().unwrap().contains("index.html"));

    assert_eq!(last_finish(&chunks), "tool_calls");

    // 추론 산문은 살아 있고, 센티널은 그 안에 남아 있지 않아야 합니다.
    let reasoning = joined_delta(&chunks, "reasoning_content");
    assert_eq!(reasoning, "먼저 파일을 써야겠다.");
    assert!(!reasoning.contains("tool_call"), "센티널이 추론 텍스트로 새어 나갔습니다");
}

#[tokio::test]
async fn non_stream_tool_call_in_reasoning_content_finishes_as_tool_calls() {
    let answer = format!("써야겠다.{}", sentinel());
    let (base, _state, _up) = harness(Upstream::reasoning_only(&answer)).await;

    let (status, body) = chat(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4",
            "messages": [{ "role": "user", "content": "페이지 만들어줘" }],
        })),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let choice = &body["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    let calls = choice["message"]["tool_calls"].as_array().expect("tool_calls 가 없습니다");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["function"]["name"], "write");
    // 도구 호출만 있는 턴의 content 는 null 이 규약입니다.
    assert!(choice["message"]["content"].is_null(), "{choice}");
    let reasoning = choice["message"]["reasoning_content"].as_str().unwrap();
    assert_eq!(reasoning, "써야겠다.");
    assert!(!reasoning.contains("tool_call"));
}

/// 파서가 깨지는 곳은 거의 언제나 프레임 경계입니다 — 추론 채널도 마찬가지여야 합니다.
#[tokio::test]
async fn reasoning_frames_split_mid_sentinel_still_yield_one_call() {
    let answer = format!("생각.{}", sentinel());
    let mut up = Upstream::reasoning_only(&answer);
    up.chunk = 3; // 센티널 한가운데가 갈립니다 (목업의 MOCK_CHUNK=3 과 같은 상황)
    let (base, _state, _up) = harness(up).await;

    let chunks = stream_chunks(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "만들어줘" }],
        })),
    )
    .await;

    let calls: Vec<&Value> = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"].as_array())
        .flatten()
        .collect();
    assert_eq!(calls.len(), 1, "프레임 경계에서 호출이 쪼개졌습니다: {calls:?}");
    assert_eq!(calls[0]["function"]["name"], "write");
    assert_eq!(last_finish(&chunks), "tool_calls");
    assert!(!joined_delta(&chunks, "reasoning_content").contains("tool_call"));
}

/// 본문에 섞여 온 `<think>` 는 추론으로 갈라내고, 그 안의 호출도 잡습니다.
#[tokio::test]
async fn think_tags_are_split_into_reasoning_content() {
    let answer = format!("<think>써야겠다{}</think>만들었습니다.", sentinel());
    let (base, _state, _up) = harness(Upstream::answering(&answer)).await;

    let chunks = stream_chunks(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "만들어줘" }],
        })),
    )
    .await;

    let content = joined_delta(&chunks, "content");
    assert_eq!(content, "만들었습니다.");
    assert!(!content.contains("<think>"), "<think> 가 답변으로 새어 나갔습니다: {content}");
    assert_eq!(joined_delta(&chunks, "reasoning_content"), "써야겠다");
    assert_eq!(last_finish(&chunks), "tool_calls");
}

#[tokio::test]
async fn non_stream_think_tags_are_split_into_reasoning_content() {
    let (base, _state, _up) =
        harness(Upstream::answering("<think>고민했다</think>답입니다.")).await;

    let (status, body) = chat(
        &base,
        json!({
            "model": "fabrix-chat-4",
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let message = &body["choices"][0]["message"];
    assert_eq!(message["content"], "답입니다.");
    assert_eq!(message["reasoning_content"], "고민했다");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
}

/// 스트림이 끊겼어도 이미 완성된 호출은 살려야 합니다. 예전에는 무조건 `length` 라
/// 뽑아 놓은 호출이 사장되고 에이전트 루프가 끊겼습니다.
#[tokio::test]
async fn midstream_error_after_a_complete_tool_call_finishes_as_tool_calls() {
    // chunk 를 넉넉히 잡아 센티널이 오류 프레임 전에 완성되게 합니다.
    let mut up = Upstream::answering(&sentinel());
    up.chunk = 4096;
    up.fail_midstream = true;
    let (base, _state, _up) = harness(up).await;

    let chunks = stream_chunks(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4", "stream": true,
            "messages": [{ "role": "user", "content": "만들어줘" }],
        })),
    )
    .await;

    assert!(
        chunks.iter().any(|c| c["choices"][0]["delta"]["tool_calls"].is_array()),
        "완성된 호출이 사라졌습니다: {chunks:?}"
    );
    assert_eq!(last_finish(&chunks), "tool_calls");
}

/// 규약 리마인더가 실제로 사내 `contents` 꼬리에 실려 나가야 합니다 — 규약이
/// systemPrompt 앞머리에만 있으면 모델이 마지막에 읽는 자리에 없습니다.
#[tokio::test]
async fn tail_reminder_reaches_the_upstream_contents() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    let (status, _) = chat(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4",
            "messages": [{ "role": "user", "content": "만들어줘" }],
        })),
    )
    .await;
    assert_eq!(status, 200);

    let payload = up.lock().unwrap().seen.last().cloned().expect("payload 를 못 봤습니다");
    let contents = payload["contents"].as_array().unwrap();
    let tail = contents.last().unwrap().as_str().unwrap();
    assert!(tail.contains("# Reminder"), "꼬리 리마인더가 없습니다: {tail}");
    assert!(tail.contains("<tool_call>"), "{tail}");
    // 규약 전문은 systemPrompt 에만 — 꼬리에 두 번 싣지 않습니다.
    assert!(payload["systemPrompt"].as_str().unwrap().contains("# Tool calling"));
    assert!(!tail.contains("# Tool calling"), "규약 전문이 두 번 실렸습니다");
}

/// 벤더 샘플과 같은 대화가 **턴 배열**로 나가야 합니다. 예전에는
/// `["User: …\n\nAssistant: …\n\nUser: …"]` 한 덩어리였습니다.
#[tokio::test]
async fn multi_turn_reaches_the_upstream_as_one_element_per_turn() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    let (status, _) = chat(
        &base,
        json!({
            "model": "fabrix-chat-4",
            "messages": [
                { "role": "user", "content": "안녕하세요?" },
                { "role": "assistant", "content": "네 안녕하세요" },
                { "role": "user", "content": "내 이름은 LCY인데 너 이름은 뭐니?" },
            ],
        }),
    )
    .await;
    assert_eq!(status, 200);

    let payload = up.lock().unwrap().seen.last().cloned().unwrap();
    assert_eq!(
        payload["contents"],
        json!(["안녕하세요?", "네 안녕하세요", "내 이름은 LCY인데 너 이름은 뭐니?"]),
        "{payload}"
    );
}

/// 도구 왕복 한 바퀴가 `[user, assistant, user]` 로 접히고, 리마인더가 **user 자리**에
/// 붙어야 합니다 — assistant 자리에 들어가면 지시문이 모델 자신의 발화가 됩니다.
#[tokio::test]
async fn a_tool_round_trip_keeps_alternation_and_the_reminder_in_a_user_slot() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    let (status, _) = chat(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4",
            "messages": [
                { "role": "user", "content": "페이지 만들어줘" },
                { "role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_a1", "type": "function",
                    "function": { "name": "write", "arguments": "{\"filePath\":\"a.html\"}" },
                }]},
                { "role": "tool", "tool_call_id": "call_a1", "content": "wrote 12 bytes" },
            ],
        })),
    )
    .await;
    assert_eq!(status, 200);

    let payload = up.lock().unwrap().seen.last().cloned().unwrap();
    let contents = payload["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3, "{payload}");
    assert_eq!(contents[0], "페이지 만들어줘");
    assert!(contents[1].as_str().unwrap().contains("<tool_call>"), "{payload}");
    let last = contents[2].as_str().unwrap();
    assert!(last.contains("wrote 12 bytes"), "{last}");
    // 리마인더는 도구 결과와 같은 user 원소에 붙습니다.
    assert!(last.contains("# Reminder"), "리마인더가 user 자리에 없습니다: {last}");
    assert!(!contents[1].as_str().unwrap().contains("# Reminder"), "{payload}");
}

/// 마지막 메시지가 assistant 면 리마인더는 **새 user 원소**로 나가야 합니다.
#[tokio::test]
async fn the_reminder_becomes_its_own_user_turn_after_an_assistant_message() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    chat(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4",
            "messages": [
                { "role": "user", "content": "만들어줘" },
                { "role": "assistant", "content": "무엇을 만들까요?" },
            ],
        })),
    )
    .await;

    let payload = up.lock().unwrap().seen.last().cloned().unwrap();
    let contents = payload["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 3, "리마인더가 자기 턴을 갖지 못했습니다: {payload}");
    assert_eq!(contents[1], "무엇을 만들까요?");
    assert!(contents[2].as_str().unwrap().contains("# Reminder"), "{payload}");
    // assistant 발화는 오염되지 않았습니다.
    assert!(!contents[1].as_str().unwrap().contains("# Reminder"));
}

/// 사내 `temperature` 상한은 0–1 입니다. OpenAI 범위(0–2)를 보내는 클라이언트를 깨지
/// 않으면서, 나가는 값만 줄이고 그 사실을 로그에 적습니다.
#[tokio::test]
async fn temperature_above_the_fabrix_ceiling_is_clamped_and_logged() {
    let (base, state, up) = harness(Upstream::answering("네")).await;

    let (status, _) = chat(
        &base,
        json!({
            "model": "fabrix-chat-4", "temperature": 1.5,
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    )
    .await;
    assert_eq!(status, 200, "0–2 를 보내는 클라이언트를 거절하면 안 됩니다");

    let payload = up.lock().unwrap().seen.last().cloned().unwrap();
    assert_eq!(payload["llmConfig"]["temperature"], 1.0, "{payload}");

    let meta = state.snapshot().recent[0].resp_meta.clone();
    assert!(meta.contains("temperature 1.5 → 1 (사내 상한)"), "{meta}");
}

/// 두 철자를 모두 실어 보냅니다 — 문서와 샘플이 달라 어느 쪽을 읽는지 모릅니다.
#[tokio::test]
async fn llm_config_carries_both_spellings_upstream() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    chat(
        &base,
        json!({
            "model": "fabrix-chat-4", "frequency_penalty": 1.04, "top_k": 14,
            "messages": [{ "role": "user", "content": "hi" }],
        }),
    )
    .await;

    let cfg = up.lock().unwrap().seen.last().cloned().unwrap()["llmConfig"].clone();
    assert_eq!(cfg["repetition_penalty"], 1.04, "샘플 철자: {cfg}");
    assert_eq!(cfg["repetion_penalty"], 1.04, "문서 철자: {cfg}");
    assert_eq!(cfg["top_k"], 14, "샘플 철자: {cfg}");
    assert_eq!(cfg["tok_k"], 14, "문서 철자: {cfg}");
}

/// 도구를 안 쓰는 요청은 문자 하나도 달라지지 않아야 합니다.
#[tokio::test]
async fn a_tool_free_request_carries_no_reminder() {
    let (base, _state, up) = harness(Upstream::answering("네")).await;

    chat(
        &base,
        json!({
            "model": "fabrix-chat-4",
            "messages": [{ "role": "user", "content": "안녕" }],
        }),
    )
    .await;

    let payload = up.lock().unwrap().seen.last().cloned().unwrap();
    assert_eq!(payload["contents"], json!(["안녕"]));
    assert!(payload["systemPrompt"].is_null(), "{payload}");
}

/// `isStream=false` 인데 SSE 를 흘려보내는 상위도 **같은 파이프라인**을 지나야 합니다.
/// 추론 채널의 도구 호출까지 이 폴백 경로에서 잡히는지 봅니다.
#[tokio::test]
async fn non_stream_sse_body_goes_through_the_same_pipeline() {
    let mut up = Upstream::reasoning_only(&format!("생각.{}", sentinel()));
    up.answer = "답입니다.".into();
    up.sse_always = true;
    let (base, _state, _up) = harness(up).await;

    let (status, body) = chat(
        &base,
        with_write_tool(json!({
            "model": "fabrix-chat-4",
            "messages": [{ "role": "user", "content": "만들어줘" }],
        })),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let choice = &body["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls", "{choice}");
    assert_eq!(choice["message"]["tool_calls"].as_array().unwrap().len(), 1);
    assert_eq!(choice["message"]["content"], "답입니다.");
    assert_eq!(choice["message"]["reasoning_content"], "생각.");
}

// ─────────────────────────── 3. 모델 해석 ───────────────────────────

#[tokio::test]
async fn unknown_model_is_404_model_not_found() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let (status, body) = chat(
        &base,
        json!({ "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }] }),
    )
    .await;

    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "model_not_found");
    assert_eq!(body["error"]["param"], "model");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("gpt-4o"), "{msg}");
    // 다음에 무엇을 할지 알려 줘야 합니다 — 이제 조용히 폴백하지 않으므로.
    assert!(msg.contains("/v1/models"), "{msg}");
    assert!(msg.contains("모델 목록"), "{msg}");
}

#[tokio::test]
async fn omitted_model_falls_back_to_the_default() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let (status, body) =
        chat(&base, json!({ "messages": [{ "role": "user", "content": "hi" }] })).await;
    assert_eq!(status, 200, "{body}");
    // model 을 안 보냈으면 해석된 alias 를 되돌려줍니다.
    assert_eq!(body["choices"][0]["message"]["content"], "답변");
}

#[tokio::test]
async fn uuid_and_case_insensitive_aliases_resolve() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    for name in [MODEL_UUID, "fabrix-chat-4", "FABRIX-CHAT-4", "fabrix-01970a3b"] {
        let (status, body) = chat(
            &base,
            json!({ "model": name, "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .await;
        assert_eq!(status, 200, "{name} → {body}");
        // 요청 문자열을 그대로 되돌려줍니다 (Open Design 연결 테스트가 보는 값).
        assert_eq!(body["model"], name);
    }
}

// ─────────────────────────── 4. 요청 검증 ───────────────────────────

#[tokio::test]
async fn protocol_violations_are_400_with_param_and_code() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;
    let user = json!([{ "role": "user", "content": "hi" }]);

    let cases: Vec<(Value, &str, &str)> = vec![
        (json!({ "model": "fabrix-chat-4" }), "messages", "missing_required_parameter"),
        (json!({ "model": "fabrix-chat-4", "messages": [] }), "messages", "invalid_value"),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "temperature": 5 }),
            "temperature",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "top_p": 1.5 }),
            "top_p",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "presence_penalty": 9 }),
            "presence_penalty",
            "invalid_value",
        ),
        (json!({ "model": "fabrix-chat-4", "messages": user, "n": 2 }), "n", "unsupported_value"),
        (json!({ "model": "fabrix-chat-4", "messages": user, "n": 0 }), "n", "invalid_value"),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "logprobs": true }),
            "logprobs",
            "unsupported_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "top_logprobs": 3 }),
            "top_logprobs",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "stop": ["1","2","3","4","5"] }),
            "stop",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user, "max_tokens": 0 }),
            "max_tokens",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": [{ "role": "보스", "content": "hi" }] }),
            "messages[0].role",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user" }] }),
            "messages[0].content",
            "missing_required_parameter",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": user,
                    "stream_options": { "include_usage": true } }),
            "stream_options",
            "invalid_value",
        ),
        (
            json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content":
                [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } }] }] }),
            "messages[0].content",
            "unsupported_content",
        ),
    ];

    for (body, param, code) in cases {
        let (status, res) = chat(&base, body.clone()).await;
        assert_eq!(status, 400, "{body} → {res}");
        assert_eq!(res["error"]["type"], "invalid_request_error", "{body}");
        assert_eq!(res["error"]["param"], param, "{body} → {res}");
        assert_eq!(res["error"]["code"], code, "{body} → {res}");
    }
}

/// 검증을 통과해야 하는 것들 — 실제 클라이언트가 늘 보내는 모양입니다.
#[tokio::test]
async fn real_client_shapes_pass_validation() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let ok: Vec<Value> = vec![
        // Continue.dev 자동완성 — stop 은 무시하되 거절하지 않습니다.
        json!({ "model": "fabrix-chat-4", "temperature": 0, "max_tokens": 256,
                "stop": ["\n\n", "```"],
                "messages": [{ "role": "user", "content": "complete" }] }),
        // 경계값.
        json!({ "model": "fabrix-chat-4", "temperature": 2, "top_p": 0, "n": 1,
                "messages": [{ "role": "user", "content": "hi" }] }),
        // 이미지 + 텍스트 — 이미지만 버리고 진행합니다.
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": [
                  { "type": "text", "text": "이거 뭐야" },
                  { "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } }] }] }),
        // 스펙에 없는 키가 와도 400 이 아닙니다 — 다음 SDK 릴리스에 깨지지 않게.
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }],
                "래빗홀": 1, "store": true, "user": "kim", "logit_bias": { "1": 1 } }),
    ];

    for body in ok {
        let (status, res) = chat(&base, body.clone()).await;
        assert_eq!(status, 200, "{body} → {res}");
    }
}

#[tokio::test]
async fn unparsable_body_does_not_echo_the_request_back() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body("{\"model\": \"fabrix-chat-4\", \"messages\": [비밀값이 섞인 깨진 JSON")
        .send()
        .await
        .unwrap();

    assert_eq!(res.status().as_u16(), 400);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_json");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("line"), "{msg}");
    // serde 원문에는 파싱하다 만 본문 조각이 섞여 나옵니다 — 응답으로 되돌리지 않습니다.
    assert!(!msg.contains("비밀값"), "요청 본문이 응답으로 새어 나갔습니다: {msg}");
}

// ─────────────────────────── 5. 라우팅과 봉투 ───────────────────────────

#[tokio::test]
async fn wrong_method_on_a_known_path_is_a_405_envelope() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    // 예전에는 axum 이 **본문 없는** 405 를 냈습니다 — `.fallback` 은 모르는 경로만 잡습니다.
    let res = client().get(format!("{base}/v1/chat/completions")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 405);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "method_not_allowed");
}

#[tokio::test]
async fn unknown_path_is_a_404_envelope_naming_the_surface() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let res = client().get(format!("{base}/v1/embeddings")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "unknown_endpoint");
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("/v1/models/{id}"), "문구가 현재 표면과 맞아야 합니다: {msg}");
}

/// 예전에는 `DefaultBodyLimit` 레이어가 핸들러 밖에서 평문 413 을 냈고, 로그에 흔적도
/// 없었습니다.
#[tokio::test]
async fn oversized_body_is_a_413_envelope() {
    let (base, state, _up) = harness(Upstream::answering("답변")).await;

    let huge = "가".repeat(6 * 1024 * 1024); // UTF-8 3바이트 × 6M = 18MiB > 16MiB
    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(format!("{{\"model\":\"fabrix-chat-4\",\"messages\":[{{\"role\":\"user\",\"content\":\"{huge}\"}}]}}"))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status().as_u16(), 413);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "request_too_large");
    assert!(body["error"]["message"].as_str().unwrap().contains("16 MiB"));

    // 로그에 한 건 남아야 합니다 — 이 실패가 조용히 사라지던 것이 문제였습니다.
    let logs = state.logs.lock().unwrap().snapshot();
    assert!(
        logs.iter().any(|e| e.status == 413 && e.is_error),
        "413 이 로그에 남지 않았습니다: {logs:?}"
    );
}

/// Base URL 에 `/v1` 을 빼먹은 클라이언트도 계속 받아야 합니다.
#[tokio::test]
async fn unversioned_aliases_still_work() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let res = client()
        .post(format!("{base}/chat/completions"))
        .json(&json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);

    let res = client().get(format!("{base}/models")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
}

#[tokio::test]
async fn token_mode_rejects_a_wrong_bearer_with_an_authentication_error() {
    let (upstream_url, _handle) = spawn_upstream(Upstream::answering("답변")).await;
    let mut cfg = config_for(&upstream_url);
    cfg.token_mode = true;
    cfg.issued_token = "sk-correct".into();
    let (base, _state) = spawn_proxy(cfg).await;

    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", "Bearer sk-wrong")
        .json(&json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status().as_u16(), 401);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "authentication_error");
    assert_eq!(body["error"]["code"], "invalid_api_key");

    // 맞는 토큰은 통과합니다.
    let res = client()
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", "Bearer sk-correct")
        .json(&json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
}

// ─────────────────────────── 6. 모델 엔드포인트 ───────────────────────────

#[tokio::test]
async fn models_list_is_openai_shaped_with_exactly_four_keys() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    let body: Value = client()
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["id"], "fabrix-chat-4");
    assert_eq!(data[0]["object"], "model");
    assert_eq!(data[0]["owned_by"], "corp");
    // 한글 전용 이름은 UUID 앞 8자리로 떨어집니다.
    assert_eq!(data[1]["id"], "fabrix-01970a3b");

    // 카드는 정확히 OpenAI 의 4키. 라벨이나 UUID 를 끼워 넣으면 알 수 없는 키에서 죽는
    // 클라이언트(Jackson 기본값 등)가 조용히 깨집니다.
    for card in data {
        assert_eq!(card.as_object().unwrap().len(), 4, "{card}");
    }
    // 사내 UUID 는 응답에 나오지 않습니다.
    assert!(!body.to_string().contains(MODEL_UUID));
}

#[tokio::test]
async fn retrieve_model_hits_by_alias_and_uuid_and_404s_otherwise() {
    let (base, _state, _up) = harness(Upstream::answering("답변")).await;

    for id in ["fabrix-chat-4", MODEL_UUID, "FABRIX-CHAT-4"] {
        let res = client().get(format!("{base}/v1/models/{id}")).send().await.unwrap();
        assert_eq!(res.status().as_u16(), 200, "{id}");
        let body: Value = res.json().await.unwrap();
        // UUID 로 물어도 alias 를 돌려줍니다 — 받은 id 를 model 칸에 되먹일 수 있어야 합니다.
        assert_eq!(body["id"], "fabrix-chat-4", "{id}");
        assert_eq!(body["object"], "model");
        assert_eq!(body.as_object().unwrap().len(), 4);
    }

    let res = client().get(format!("{base}/v1/models/gpt-4o")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
    assert_eq!(body["error"]["param"], "model");

    // 버전 없는 별칭도 같은 동작.
    let res = client().get(format!("{base}/models/fabrix-chat-4")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
}

// ─────────────────────────── 7. 상위 오류 ───────────────────────────

#[tokio::test]
async fn missing_credentials_is_a_503_with_not_configured() {
    // 상위 주소가 비어 있으면 사내 호출 자체를 하지 않습니다.
    let (base, _state) = spawn_proxy(Config::default()).await;

    let (status, body) = chat(
        &base,
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }),
    )
    .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(body["error"]["code"], "not_configured");
}

#[tokio::test]
async fn unreachable_upstream_is_a_502_envelope() {
    // 아무도 듣지 않는 포트를 가리킵니다.
    let mut cfg = config_for("http://127.0.0.1:1");
    cfg.default_model_alias = String::new();
    let (base, _state) = spawn_proxy(cfg).await;

    let (status, body) = chat(
        &base,
        json!({ "model": "fabrix-chat-4", "messages": [{ "role": "user", "content": "hi" }] }),
    )
    .await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["type"], "api_error");
    assert_eq!(body["error"]["code"], "upstream_unreachable");
}

#[tokio::test]
async fn health_needs_no_auth_even_in_token_mode() {
    let (upstream_url, _handle) = spawn_upstream(Upstream::answering("답변")).await;
    let mut cfg = config_for(&upstream_url);
    cfg.token_mode = true;
    cfg.issued_token = "sk-x".into();
    let (base, _state) = spawn_proxy(cfg).await;

    let res = client().get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
}
