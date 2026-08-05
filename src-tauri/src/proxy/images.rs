//! OpenAI Images API 호환 엔드포인트 — `POST /v1/images/generations`(t2i) 와
//! `POST /v1/images/edits`(i2i).
//!
//! 흐름은 `chat.rs` 를 그대로 따릅니다: 받은 요청(OpenAI) → 내부 파이프라인 → 돌려준 응답.
//! 편집(i2i)은 **gemma 인식 → FLUX 재생성**(describe-then-regenerate)으로 동작합니다.
//! 실제 업스트림 호출은 `image_backend` seam 에 있으며 현재는 스텁(→ 501)입니다.

use std::time::Instant;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use uuid::Uuid;

use crate::logstore::{self, Kind, LogEntry};
use crate::openai::{
    parse_size, split_data_url, ErrorEnvelope, ImageDatum, ImageEditRequest, ImageGenerationRequest,
    ImagesResponse,
};
use crate::proxy::b64;
use crate::proxy::fabrix::FabrixError;
use crate::proxy::image_backend::{self, ImageError};
use crate::state::{self, Shared};

use super::{authorize, error_response, fabrix_headers_line, pretty};

/// 기본 대체 프롬프트 — 스펙의 빈 프롬프트 처리와 맞춥니다.
const FALLBACK_PROMPT: &str = "A high-quality reference image.";

/// 로그 한 건을 조립하는 컨텍스트. `chat.rs::Ctx` 의 이미지판입니다.
struct Ctx {
    started: Instant,
    /// 두 엔드포인트를 `Kind::Images` 하나로 묶으므로 실제 경로는 여기 리터럴로 들고 있습니다.
    path: &'static str,
    client: Option<String>,
    model_requested: Option<String>,
    /// 실제로 쓴 생성(FLUX) 모델 (또는 `[stub]`).
    model_used: Option<String>,
    /// edits 에서 쓴 인식(gemma) 모델.
    vision_model: Option<String>,
    req_openai: String,
    req_fabrix: String,
    req_fabrix_headers: String,
    fabrix_url: String,
    /// 자리표시자(스텁) 모드로 응답했는지.
    stub: bool,
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
            kind: Kind::Images,
            method: Kind::Images.method(),
            path: self.path.into(),
            status,
            latency_ms: self.started.elapsed().as_millis() as u64,
            stream: false,
            cached: false,
            model_requested: self.model_requested.clone(),
            model_alias: self.model_used.clone(),
            model_id: self.model_used.clone(),
            model_label: self.model_used.clone(),
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
}

/// 로그에 원문 base64 를 싣지 않도록, 요청 JSON 안의 data URL 을 길이 요약으로 치환합니다.
/// (50건 메모리 링버퍼에 수 MB 이미지를 들고 있지 않기 위함.)
fn redact_data_urls(v: &Value) -> Value {
    match v {
        Value::String(s) => match split_data_url(s) {
            Some((mime, payload)) => {
                Value::String(format!("data:{mime};base64,…({} chars)", payload.len()))
            }
            None => Value::String(s.clone()),
        },
        Value::Array(a) => Value::Array(a.iter().map(redact_data_urls).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, val)| (k.clone(), redact_data_urls(val))).collect())
        }
        other => other.clone(),
    }
}

/// 잘못된 요청(400)을 기록하고 OpenAI 봉투로 돌려줍니다.
fn bad_request(state: &Shared, ctx: &Ctx, msg: String) -> Response {
    state.record(ctx.entry(
        400,
        true,
        Some("잘못된 요청".into()),
        Some("잘못된 요청".into()),
        msg.clone(),
        "요청 검증 실패".into(),
    ));
    error_response(400, ErrorEnvelope::new(msg, "invalid_request_error", None))
}

/// 파이프라인 오류를 상태코드에 맞춰 기록하고 돌려줍니다.
fn image_err_response(state: &Shared, ctx: &Ctx, err: ImageError) -> Response {
    let status = err.status();
    state.record(ctx.entry(
        status,
        true,
        Some(err.note()),
        Some(err.note()),
        err.message(),
        format!("실패 · HTTP {status}"),
    ));
    error_response(status, ErrorEnvelope::new(err.message(), err.kind(), None))
}

/// 생성된 이미지 바이트들을 `b64_json` 응답으로 마무리하고 성공 로그를 남깁니다.
fn finish(state: &Shared, ctx: Ctx, images: Vec<Vec<u8>>) -> Response {
    let data: Vec<ImageDatum> = images
        .iter()
        .map(|bytes| ImageDatum { b64_json: Some(b64::encode(bytes)), ..Default::default() })
        .collect();

    let total: usize = images.iter().map(Vec::len).sum();
    let mime = images.first().map(|b| image_backend::sniff_mime(b)).unwrap_or("image/png");
    let tag = if ctx.stub { "[stub] " } else { "" };
    let summary = format!("{tag}이미지 {}장 · {mime}", images.len());
    // 이미지 응답만은 본문 대신 한 줄 요약을 남깁니다 — 수 MB base64 를 50건
    // 링버퍼에 들고 있지 않기 위한 것으로, 요청 쪽 redact_data_urls 와 같은 이유입니다.
    let body = format!("{tag}이미지 {}장 · {mime} · b64 {total} bytes", images.len());
    let meta = format!("{tag}{}장 · {mime} · {total} bytes", images.len());
    let note = if ctx.stub { Some("[stub] 자리표시자 PNG".into()) } else { None };

    let resp = ImagesResponse { created: state::epoch_secs(), data };
    state.record(ctx.entry(200, false, note, Some(summary), body, meta));

    let mut response = Json(resp).into_response();
    if ctx.stub {
        response
            .headers_mut()
            .insert("x-fabrix-image-stub", HeaderValue::from_static("1"));
    }
    response
}

// ─────────────────────────── generations (t2i) ───────────────────────────

pub async fn generations(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let cfg = state.config();
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let incoming: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let mut ctx = Ctx {
        started: Instant::now(),
        path: "/v1/images/generations",
        client: logstore::short_client(ua),
        model_requested: None,
        model_used: None,
        vision_model: None,
        req_openai: if incoming.is_null() {
            logstore::preview(&String::from_utf8_lossy(&body), 2000)
        } else {
            logstore::preview(&pretty(&redact_data_urls(&incoming)), 2000)
        },
        req_fabrix: "(요청을 변환하기 전에 실패했습니다)".into(),
        req_fabrix_headers: fabrix_headers_line(&cfg),
        fabrix_url: format!("{}{}", cfg.normalized_base_url(), image_backend::IMAGE_GEN_PATH),
        stub: false,
    };

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

    let req: ImageGenerationRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => return bad_request(&state, &ctx, format!("요청 본문을 해석하지 못했습니다: {err}")),
    };
    ctx.model_requested = req.model.clone();

    let prompt = req
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(FALLBACK_PROMPT)
        .to_string();
    let size = image_backend::snap_size(req.size.as_deref().and_then(parse_size));
    let n = req.n.unwrap_or(1).clamp(1, image_backend::MAX_N);

    let images = match generate_n(&state, &mut ctx, &cfg, &prompt, size, n).await {
        Ok(imgs) => imgs,
        Err(resp) => return resp,
    };
    finish(&state, ctx, images)
}

/// FLUX 생성 `n` 회. 스텁 모드면 자리표시자 PNG 를 돌려줍니다.
async fn generate_n(
    state: &Shared,
    ctx: &mut Ctx,
    cfg: &crate::config::Config,
    prompt: &str,
    size: (u32, u32),
    n: u32,
) -> Result<Vec<Vec<u8>>, Response> {
    if cfg.image_stub_mode {
        ctx.stub = true;
        ctx.model_used = Some("[stub]".into());
        ctx.req_fabrix = "(stub 모드 — 자리표시자 PNG, 업스트림 호출 없음)".into();
        eprintln!("[images] STUB 모드 — generations 자리표시자 PNG 반환 (실제 생성 아님)");
        return Ok((0..n).map(|_| image_backend::placeholder_png()).collect());
    }

    let model = match image_backend::resolve_image_model(cfg) {
        Ok(m) => m,
        Err(err) => return Err(image_err_response(state, ctx, err)),
    };
    ctx.model_used = Some(model.clone());
    ctx.req_fabrix = pretty(&serde_json::json!({
        "generation": { "model": model, "size": format!("{}x{}", size.0, size.1), "prompt": prompt }
    }));

    let Some(client) = state.image_client() else {
        return Err(image_err_response(state, ctx, ImageError::Backend(FabrixError::NotConfigured)));
    };

    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        match client.generate(prompt, size, &model).await {
            Ok(bytes) => out.push(image_backend::fit_output(bytes, size)),
            Err(err) => return Err(image_err_response(state, ctx, err)),
        }
    }
    Ok(out)
}

// ─────────────────────────── edits (i2i · gemma → FLUX) ───────────────────────────

pub async fn edits(State(state): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let cfg = state.config();
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let incoming: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let mut ctx = Ctx {
        started: Instant::now(),
        path: "/v1/images/edits",
        client: logstore::short_client(ua),
        model_requested: None,
        model_used: None,
        vision_model: None,
        req_openai: if incoming.is_null() {
            logstore::preview(&String::from_utf8_lossy(&body), 2000)
        } else {
            logstore::preview(&pretty(&redact_data_urls(&incoming)), 2000)
        },
        req_fabrix: "(요청을 변환하기 전에 실패했습니다)".into(),
        req_fabrix_headers: fabrix_headers_line(&cfg),
        fabrix_url: format!("{}{}", cfg.normalized_base_url(), image_backend::VISION_PATH),
        stub: false,
    };

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

    let req: ImageEditRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => return bad_request(&state, &ctx, format!("요청 본문을 해석하지 못했습니다: {err}")),
    };
    ctx.model_requested = req.model.clone();

    let prompt = match req.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => p.to_string(),
        None => return bad_request(&state, &ctx, "prompt 가 필요합니다.".into()),
    };

    // 참조 이미지 — 스펙상 첫 장만 사용합니다.
    let Some(data_url) = req.images.first().and_then(|r| r.image_url.as_ref()).and_then(|u| u.as_str())
    else {
        return bad_request(&state, &ctx, "편집할 이미지가 없습니다 (images[0].image_url).".into());
    };
    let Some((mime, payload)) = split_data_url(data_url) else {
        return bad_request(&state, &ctx, "images[0].image_url 이 올바른 base64 data URL 이 아닙니다.".into());
    };
    let image_bytes = match b64::decode(payload) {
        Ok(bytes) => bytes,
        Err(err) => return bad_request(&state, &ctx, format!("이미지 base64 디코드 실패: {err}")),
    };
    // 원본 파일 mime 이 없으면 매직바이트로 판정.
    let mime = if mime == "application/octet-stream" {
        image_backend::sniff_mime(&image_bytes)
    } else {
        mime
    }
    .to_string();

    let size = image_backend::snap_size(req.size.as_deref().and_then(parse_size));
    let n = req.n.unwrap_or(1).clamp(1, image_backend::MAX_N);

    let images = match edit_pipeline(&state, &mut ctx, &cfg, &image_bytes, &mime, &prompt, size, n).await {
        Ok(imgs) => imgs,
        Err(resp) => return resp,
    };
    finish(&state, ctx, images)
}

/// gemma 인식 → 프롬프트 합성 → FLUX 재생성. 스텁 모드면 자리표시자 PNG.
#[allow(clippy::too_many_arguments)]
async fn edit_pipeline(
    state: &Shared,
    ctx: &mut Ctx,
    cfg: &crate::config::Config,
    image: &[u8],
    mime: &str,
    instruction: &str,
    size: (u32, u32),
    n: u32,
) -> Result<Vec<Vec<u8>>, Response> {
    if cfg.image_stub_mode {
        ctx.stub = true;
        ctx.model_used = Some("[stub]".into());
        ctx.vision_model = Some("[stub]".into());
        ctx.req_fabrix = "(stub 모드 — gemma/FLUX 호출 없이 자리표시자 PNG)".into();
        eprintln!("[images] STUB 모드 — edits 자리표시자 PNG 반환 (실제 편집 아님)");
        return Ok((0..n).map(|_| image_backend::placeholder_png()).collect());
    }

    let vision_model = match image_backend::resolve_vision_model(cfg) {
        Ok(m) => m,
        Err(err) => return Err(image_err_response(state, ctx, err)),
    };
    ctx.vision_model = Some(vision_model.clone());
    let gen_model = match image_backend::resolve_image_model(cfg) {
        Ok(m) => m,
        Err(err) => return Err(image_err_response(state, ctx, err)),
    };
    ctx.model_used = Some(gen_model.clone());

    let Some(client) = state.image_client() else {
        return Err(image_err_response(state, ctx, ImageError::Backend(FabrixError::NotConfigured)));
    };

    // 1) gemma 인식 — 참조 이미지를 설명 텍스트로.
    let description = match client.understand(image, mime, instruction, &vision_model).await {
        Ok(desc) => desc,
        Err(err) => return Err(image_err_response(state, ctx, err)),
    };

    // 2) 설명 + 편집 지시를 FLUX 프롬프트로 합성.
    let composed = image_backend::compose_edit_prompt(&description, instruction);
    ctx.req_fabrix = pretty(&serde_json::json!({
        "vision": { "model": vision_model, "mime": mime, "bytes": image.len() },
        "generation": { "model": gen_model, "size": format!("{}x{}", size.0, size.1), "prompt": composed }
    }));

    // 3) FLUX 재생성.
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        match client.generate(&composed, size, &gen_model).await {
            Ok(bytes) => out.push(image_backend::fit_output(bytes, size)),
            Err(err) => return Err(image_err_response(state, ctx, err)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_data_urls_replaces_base64_payload() {
        let v = serde_json::json!({
            "prompt": "hi",
            "images": [{ "image_url": "data:image/png;base64,AAAABBBBCCCC" }]
        });
        let red = redact_data_urls(&v);
        let s = red["images"][0]["image_url"].as_str().unwrap();
        assert!(s.starts_with("data:image/png;base64,…("));
        assert!(!s.contains("AAAABBBBCCCC"));
        // data URL 이 아닌 문자열은 그대로 둡니다.
        assert_eq!(red["prompt"].as_str(), Some("hi"));
    }
}
