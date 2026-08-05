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
use crate::openai::{ModelCard, ModelList};
use crate::state::{self, ModelsCache, Shared, MODELS_CACHE_TTL};

use super::fabrix::{build_aliases, FabrixError, ResolvedModel, MODELS_PATH};
use super::{authorize, error_response, fabrix_headers_line, inbound_auth_line};

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

    // 인바운드 Authorization: 키발급없이 허용 모드면 아무 값이나 통과,
    // 토큰 사용 모드면 발행 토큰과 일치할 때만 통과합니다.
    let auth = inbound_auth_line(&cfg, &headers);

    let req_openai = format!(
        "GET /v1/models\nAuthorization: {auth}\nUser-Agent: {}",
        ua.unwrap_or("(없음)")
    );
    let fabrix_url = format!("{}{MODELS_PATH}", cfg.normalized_base_url());
    let headers_line = fabrix_headers_line(&cfg);

    // 토큰 모드에서 인바운드 토큰이 일치하지 않으면 사내 호출 전에 거부합니다.
    if let Err((status, envelope)) = authorize(&cfg, &headers) {
        let latency = started.elapsed().as_millis() as u64;
        state.record(LogEntry {
            id: Uuid::new_v4().to_string(),
            ts: state::now_hm(),
            ts_full: state::now_iso(),
            kind: Kind::Models,
            method: Kind::Models.method(),
            path: Kind::Models.path().into(),
            status,
            latency_ms: latency,
            stream: false,
            cached: false,
            model_requested: None,
            model_alias: None,
            model_id: None,
            model_label: None,
            client: client_name,
            note: Some("토큰 거부".into()),
            summary: Some("토큰 거부".into()),
            is_error: true,
            req_openai,
            req_fabrix: "(토큰 검증 실패 — 사내 호출을 하지 않았습니다)".into(),
            req_fabrix_headers: headers_line,
            fabrix_url,
            resp_body: envelope.error.message.clone(),
            resp_meta: format!("거부 · HTTP {status}"),
        });
        return error_response(status, envelope);
    }

    match ensure_models(&state).await {
        Ok((models, cached)) => {
            let list = to_model_list(&models, state::epoch_secs());

            let latency = started.elapsed().as_millis() as u64;
            state.record(LogEntry {
                id: Uuid::new_v4().to_string(),
                ts: state::now_hm(),
                ts_full: state::now_iso(),
                kind: Kind::Models,
                method: Kind::Models.method(),
                path: Kind::Models.path().into(),
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
                resp_body: render_model_list(&models),
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
                path: Kind::Models.path().into(),
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
                resp_body: err.message(),
                resp_meta: format!("실패 · HTTP {status}"),
            });

            error_response(status, err.envelope())
        }
    }
}

// ─────────────────────── 응답 조립 (순수 함수) ───────────────────────
//
// `AppState::new` 가 실제 `tauri::AppHandle` 을 요구해 핸들러 자체는 단위 테스트할 수
// 없습니다. 그래서 봉투 조립을 핸들러에서 떼어내 여기 두고, 테스트는 이 함수들을 봅니다
// (`proxy::mod` · `image_backend` 의 테스트가 `Config` 만으로 도는 것과 같은 규율).

/// 모델 하나를 OpenAI 카드로.
///
/// `id` 에는 **alias** 를 담습니다 — 사내 UUID 는 노출하지 않습니다. 클라이언트가 받은
/// `id` 를 그대로 `model` 칸에 되먹일 수 있어야 하고, alias 가 그 정본입니다.
pub fn model_card(m: &ResolvedModel, created: i64) -> ModelCard {
    ModelCard { id: m.alias.clone(), object: "model", created, owned_by: "corp" }
}

pub fn to_model_list(models: &[ResolvedModel], created: i64) -> ModelList {
    ModelList { object: "list", data: models.iter().map(|m| model_card(m, created)).collect() }
}

/// 목업 L2 ③ 칸 — alias 옆에 실제 UUID 를 나란히 보여줍니다.
///
/// 모델을 몇 개만 추리지 않고 전부 적습니다. 화면이 앞부분만 보여 주고
/// "전체보기" 팝업에서 나머지를 펼치므로, 여기서 줄이면 펼칠 것이 없어집니다.
fn render_model_list(models: &[ResolvedModel]) -> String {
    let mut out = String::from("{ \"object\": \"list\", \"data\": [\n");
    for m in models {
        out.push_str(&format!(
            "  {{ \"id\": \"{}\", \"owned_by\": \"corp\" }},   ← {} · {}\n",
            m.alias, m.model_id, m.label
        ));
    }
    out.push_str("] }");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<ResolvedModel> {
        vec![
            ResolvedModel {
                alias: "fabrix-chat-4".into(),
                model_id: "0196f1fc-2858-70a9-a232-74dbddb971d0".into(),
                label: "챗 4".into(),
                description: Some("범용 대화 모델".into()),
            },
            ResolvedModel {
                alias: "fabrix-chat-lite".into(),
                model_id: "01970a3b-91d4-7c8e-9a11-2f3c4d5e6f70".into(),
                label: "라이트".into(),
                description: None,
            },
            ResolvedModel {
                alias: "fabrix-01970a3b".into(),
                model_id: "01970a3b-91d4-7c8e-9a11-2f3c4d5e6f75".into(),
                label: "사내규정".into(),
                description: Some("사내 규정 특화".into()),
            },
        ]
    }

    #[test]
    fn model_list_envelope_is_openai_shape() {
        let json = serde_json::to_value(to_model_list(&rows(), 1_700_000_000)).unwrap();
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 3);
        assert_eq!(json["data"][0]["id"], "fabrix-chat-4");
        assert_eq!(json["data"][0]["object"], "model");
        assert_eq!(json["data"][0]["owned_by"], "corp");
        assert_eq!(json["data"][0]["created"], 1_700_000_000i64);

        // 카드는 정확히 OpenAI 의 4키여야 합니다. 라벨이나 UUID 를 슬쩍 끼워 넣으면
        // 알 수 없는 키에서 죽는 클라이언트(Jackson 은 FAIL_ON_UNKNOWN_PROPERTIES 가
        // 기본 true, Go 쪽도 DisallowUnknownFields 가 흔함)가 조용히 깨집니다.
        // 그 정보는 앱의 모델 목록 창이 보여 주므로 HTTP 표면을 넓힐 이유가 없습니다.
        assert_eq!(json["data"][0].as_object().unwrap().len(), 4);
    }

    #[test]
    fn model_card_exposes_alias_not_uuid() {
        let json = serde_json::to_string(&model_card(&rows()[0], 1)).unwrap();
        assert!(json.contains("fabrix-chat-4"), "{json}");
        assert!(!json.contains("0196f1fc"), "사내 UUID 가 새어 나갔습니다: {json}");
    }

    #[test]
    fn empty_list_is_still_a_list() {
        let json = serde_json::to_value(to_model_list(&[], 1)).unwrap();
        assert_eq!(json["object"], "list");
        assert!(json["data"].as_array().unwrap().is_empty());
    }

    /// `render_model_list` 의 주석이 약속하는 것 — 몇 개만 추리지 않고 **전부** 적습니다.
    /// 화면이 앞부분만 보여 주고 "전체보기" 로 펼치므로 여기서 줄이면 펼칠 것이 없어집니다.
    #[test]
    fn render_model_list_keeps_every_model() {
        let text = render_model_list(&rows());
        assert_eq!(text.matches('←').count(), 3, "{text}");
        for m in rows() {
            assert!(text.contains(&m.alias), "{text}");
            assert!(text.contains(&m.model_id), "{text}");
            assert!(text.contains(&m.label), "{text}");
        }
    }

    #[test]
    fn render_model_list_survives_empty_input() {
        let text = render_model_list(&[]);
        assert!(text.contains("\"object\": \"list\""), "{text}");
        assert_eq!(text.matches('←').count(), 0);
    }
}
