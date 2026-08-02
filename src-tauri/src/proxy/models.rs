//! `GET /v1/models` — 사내 모델 목록을 OpenAI 형식으로 노출합니다.
//!
//! 60초 캐시. 캐시 히트도 로그에 남깁니다 (목업 L2 의 `캐시 히트 · 0.0s` 행).

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use uuid::Uuid;

use crate::logstore::{self, Kind, LogEntry};
use crate::openai::{ErrorEnvelope, ModelCard, ModelList};
use crate::state::{self, ModelsCache, Shared, MODELS_CACHE_TTL};

use super::fabrix::{build_aliases, FabrixError, ResolvedModel, MODELS_PATH};
use super::{error_response, fabrix_headers_line};

/// `(모델 목록, 캐시에서 나왔는지)`.
///
/// 뮤텍스 가드를 `await` 너머로 들고 가지 않도록 잠금 구간을 블록으로 닫습니다.
pub async fn ensure_models(state: &Shared) -> Result<(Vec<ResolvedModel>, bool), FabrixError> {
    let cached = {
        let guard = state.models_cache.lock().unwrap();
        guard
            .as_ref()
            .filter(|c| c.fetched_at.elapsed() < MODELS_CACHE_TTL)
            .map(|c| c.models.clone())
    };
    if let Some(models) = cached {
        return Ok((models, true));
    }

    let client = state.fabrix_client().ok_or(FabrixError::NotConfigured)?;
    let raw = client.list_models().await?;
    let models = build_aliases(&raw);

    *state.models_cache.lock().unwrap() =
        Some(ModelsCache { fetched_at: Instant::now(), models: models.clone() });

    Ok((models, false))
}

pub async fn handle(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let started = Instant::now();
    let cfg = state.config();

    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let client_name = logstore::short_client(ua);

    // 인바운드 Authorization 은 값과 무관하게 통과시킵니다 —
    // 목업: "API 키 칸에는 아무 값이나 넣어도 됩니다".
    let auth = if headers.contains_key("authorization") {
        "Bearer ●●●●(값 무시)"
    } else {
        "(없음 · 무시)"
    };

    let req_openai = format!(
        "GET /v1/models\nAuthorization: {auth}\nUser-Agent: {}",
        ua.unwrap_or("(없음)")
    );
    let fabrix_url = format!("{}{MODELS_PATH}", cfg.normalized_base_url());
    let headers_line = fabrix_headers_line(&cfg);

    match ensure_models(&state).await {
        Ok((models, cached)) => {
            let created = state::epoch_secs();
            let list = ModelList {
                object: "list",
                data: models
                    .iter()
                    .map(|m| ModelCard {
                        id: m.alias.clone(),
                        object: "model",
                        created,
                        owned_by: "corp",
                    })
                    .collect(),
            };

            let latency = started.elapsed().as_millis() as u64;
            state.record(LogEntry {
                id: Uuid::new_v4().to_string(),
                ts: state::now_hm(),
                ts_full: state::now_iso(),
                kind: Kind::Models,
                method: Kind::Models.method(),
                path: Kind::Models.path(),
                status: 200,
                latency_ms: latency,
                stream: false,
                cached,
                model_requested: None,
                model_alias: None,
                model_id: None,
                model_label: None,
                client: client_name,
                note: cached.then(|| "캐시 히트".to_string()),
                summary: Some(format!("{}개", models.len())),
                is_error: false,
                req_openai,
                req_fabrix: if cached {
                    format!("GET {fabrix_url}\n\n(60초 캐시가 유효해 사내 호출을 생략했습니다)")
                } else {
                    format!("GET {fabrix_url}")
                },
                req_fabrix_headers: headers_line,
                fabrix_url,
                resp_preview: render_preview(&models),
                resp_meta: format!(
                    "모델 {}개 · {} · 캐시 60초",
                    models.len(),
                    if cached { "캐시에서 반환" } else { "사내에서 새로 조회" }
                ),
            });

            Json(list).into_response()
        }
        Err(err) => {
            let latency = started.elapsed().as_millis() as u64;
            let status = err.status();
            state.record(LogEntry {
                id: Uuid::new_v4().to_string(),
                ts: state::now_hm(),
                ts_full: state::now_iso(),
                kind: Kind::Models,
                method: Kind::Models.method(),
                path: Kind::Models.path(),
                status,
                latency_ms: latency,
                stream: false,
                cached: false,
                model_requested: None,
                model_alias: None,
                model_id: None,
                model_label: None,
                client: client_name,
                note: Some(err.note()),
                summary: Some(err.note()),
                is_error: true,
                req_openai,
                req_fabrix: format!("GET {fabrix_url}"),
                req_fabrix_headers: headers_line,
                fabrix_url,
                resp_preview: err.message(),
                resp_meta: format!("실패 · HTTP {status}"),
            });

            error_response(status, ErrorEnvelope::new(err.message(), err.kind(), None))
        }
    }
}

/// 목업 L2 ③ 칸 — alias 옆에 실제 UUID 를 나란히 보여줍니다.
fn render_preview(models: &[ResolvedModel]) -> String {
    const SHOWN: usize = 8;
    let mut out = String::from("{ \"object\": \"list\", \"data\": [\n");
    for m in models.iter().take(SHOWN) {
        let short: String = m.model_id.chars().take(18).collect();
        out.push_str(&format!(
            "  {{ \"id\": \"{}\", \"owned_by\": \"corp\" }},   ← {}… · {}\n",
            m.alias, short, m.label
        ));
    }
    if models.len() > SHOWN {
        out.push_str(&format!("  … 외 {}개\n", models.len() - SHOWN));
    }
    out.push_str("] }");
    out
}
