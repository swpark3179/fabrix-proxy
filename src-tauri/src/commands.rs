//! 프런트엔드가 부르는 IPC 표면.

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::config::{self, Config};
use crate::logstore::LogEntry;
use crate::port::{self, PortStatus};
use crate::proxy::fabrix::{build_aliases, build_http_client, FabrixClient, ResolvedModel};
use crate::proxy::{self};
use crate::state::{Shared, Snapshot, MODELS_CACHE_TTL};
use crate::{tray, windows};

#[tauri::command]
pub fn get_snapshot(state: State<'_, Shared>) -> Snapshot {
    state.snapshot()
}

/// 설정 화면 프리필용. 평문 저장이므로 값을 그대로 돌려줍니다 —
/// 어차피 같은 사용자 계정의 홈 폴더에 있는 파일입니다.
#[tauri::command]
pub fn get_config(state: State<'_, Shared>) -> Config {
    state.config()
}

/// 온보딩 화면에 저장 위치를 명시하기 위해 노출합니다.
#[tauri::command]
pub fn get_config_path() -> String {
    config::config_path().display().to_string()
}

#[tauri::command]
pub fn get_logs(state: State<'_, Shared>) -> Vec<LogEntry> {
    state.logs.lock().unwrap().snapshot()
}

#[tauri::command]
pub fn clear_logs(app: AppHandle) {
    let state = app.state::<Shared>();
    state.logs.lock().unwrap().clear();
    let _ = tauri::Emitter::emit(&app, "logs:cleared", ());
    state.emit_state();
}

#[tauri::command]
pub fn check_port(state: State<'_, Shared>, port: u16) -> PortStatus {
    port::inspect(port, state.running_port())
}

// ─────────────────────────── 모델 목록 ───────────────────────────

/// 모델 목록 창과 설정 폼이 보는 한 줄.
///
/// `ResolvedModel` 을 그대로 내보내지 않는 이유: 화면이 필요로 하는 것(`isDefault`)이
/// 프록시 내부 표현에 있을 이유가 없고, 여기 필드를 늘려도 `/v1/models` **HTTP 응답이
/// 흔들리지 않아야** 합니다(그쪽은 OpenAI 의 4키를 지킵니다).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    /// 클라이언트가 `model` 칸에 넣는 값 — 화면의 복사 대상입니다.
    pub alias: String,
    /// 실제로 사내에 보내는 UUID. 사내 담당자와 대조할 때 이 값이 필요합니다.
    pub model_id: String,
    /// 사람이 읽는 이름 (예: `챗 4`).
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
}

impl ModelRow {
    fn from(m: &ResolvedModel, default_alias: &str) -> Self {
        Self {
            alias: m.alias.clone(),
            model_id: m.model_id.clone(),
            label: m.label.clone(),
            description: m.description.clone(),
            is_default: m.alias == default_alias,
        }
    }

    pub fn rows(models: &[ResolvedModel], default_alias: &str) -> Vec<Self> {
        models.iter().map(|m| Self::from(m, default_alias)).collect()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResult {
    pub models: Vec<ModelRow>,
    pub cached: bool,
    /// 로컬 RFC3339. 화면이 이 값으로 "n초 전 조회" 를 계산합니다.
    pub fetched_at: String,
    /// 빈 문자열이면 목록의 첫 모델이 기본입니다.
    pub default_alias: String,
    /// 어느 서버에서 가져온 목록인지 — 설정을 바꿔 두고 헷갈리는 일이 많습니다.
    pub source_url: String,
    pub cache_ttl_secs: u64,
}

/// 모델 목록. `refresh: true` 면 60초 캐시를 무시하고 새로 받습니다.
///
/// 커맨드를 둘로 나누지 않은 이유: IPC 표면은 `lib.rs` 와 `ipc.ts` **두 곳**에 이름을
/// 적어야 하고, 화면에는 "다시 조회" 버튼 하나뿐이라 나누면 잘못 부르기만 쉬워집니다.
///
/// 사내에서 새로 가져왔을 때만 `emit_state()` 를 부릅니다 — 그래야 메인 창의
/// `/v1/models` 카드가 `사내 모델 목록 중계` → `사내 모델 7개 노출` 로 따라옵니다
/// (`Snapshot.model_count` 는 캐시가 따뜻할 때만 채워집니다). 매번 부르면 스냅샷
/// 이벤트가 불필요하게 퍼집니다.
#[tauri::command]
pub async fn list_models(app: AppHandle, refresh: bool) -> Result<ModelListResult, String> {
    // 가드가 await 를 넘지 못하므로 Arc 를 복사해 나옵니다 (state.rs 머리말 참고).
    let state = app.state::<Shared>().inner().clone();
    let cfg = state.config();

    let loaded = proxy::models::load_models(&state, refresh)
        .await
        .map_err(|(err, _)| err.message())?;

    if !loaded.cached {
        state.emit_state();
    }

    Ok(ModelListResult {
        models: ModelRow::rows(&loaded.models, &cfg.default_model_alias),
        cached: loaded.cached,
        fetched_at: loaded.fetched_at,
        default_alias: cfg.default_model_alias.clone(),
        source_url: cfg.normalized_base_url(),
        cache_ttl_secs: MODELS_CACHE_TTL.as_secs(),
    })
}

/// 목록 창에서 기본 모델을 바꿉니다.
///
/// 프런트가 `Config` 를 통째로 `save_config` 에 넘기지 않는 이유: 목록 창은 나머지
/// 설정을 화면에 들고 있지 않아, 설정 창에 초안이 열려 있는 동안 저장하면 다른 필드를
/// 예전 값으로 덮어씁니다. 여기서 읽고-바꿔-쓰기 합니다.
#[tauri::command]
pub async fn set_default_model(app: AppHandle, alias: String) -> Result<Snapshot, String> {
    let state = app.state::<Shared>().inner().clone();
    let mut cfg = state.config();
    cfg.default_model_alias = alias.trim().to_string();

    config::save_config(&cfg).map_err(|err| format!("설정을 저장하지 못했습니다: {err}"))?;
    state.replace_config(cfg);
    state.emit_state();
    Ok(state.snapshot())
}

/// `async` 인 이유: 이 커맨드는 모델 목록 창을 **처음 열 때 만듭니다**. 동기 커맨드
/// 안에서 창을 만들면 Windows 에서 교착합니다(tauri#5306) — `windows::show_models` 가
/// 생성을 async 런타임으로 넘겨 이미 막고 있지만, 커맨드 자체도 문서가 권하는 형태로
/// 둡니다. 창을 만드는 코드가 언제 이 자리로 되돌아올지 모릅니다.
#[tauri::command]
pub async fn open_models_window(app: AppHandle) {
    windows::show_models(&app);
}

/// 미설정 안내의 버튼이 대시보드가 아니라 설정 폼에 떨어지게 합니다.
#[tauri::command]
pub fn open_settings_window(app: AppHandle) {
    windows::show_settings(&app);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub model_count: usize,
    /// 시험한 서버의 모델 전체. 설정 폼이 이걸로 기본 모델 선택기를 채웁니다 —
    /// 저장 전이라 공유 캐시에는 없는 목록입니다.
    pub models: Vec<ModelRow>,
}

/// 온보딩/설정의 "연결 확인". 저장하지 않고 주어진 값으로만 시험합니다.
#[tauri::command]
pub async fn test_connection(
    fabrix_base_url: String,
    fabrix_client: String,
    openapi_token: String,
    insecure_skip_verify: bool,
) -> Result<TestResult, String> {
    let probe = Config {
        fabrix_base_url,
        fabrix_client: fabrix_client.clone(),
        openapi_token: openapi_token.clone(),
        ..Config::default()
    };
    if !probe.is_configured() {
        return Err("주소 · 인증키 · 토큰을 모두 입력하세요.".into());
    }

    let client = FabrixClient {
        http: build_http_client(insecure_skip_verify),
        base: probe.normalized_base_url(),
        client_key: fabrix_client,
        token: openapi_token,
    };

    let (raw, _) = client.list_models().await.map_err(|(err, _)| err.message())?;
    let models = build_aliases(&raw);
    Ok(TestResult {
        model_count: models.len(),
        // `"{alias} · {label}"` 포맷을 Rust 에 둘 이유가 없어졌습니다 — 화면이 목록을
        // 통째로 받아 문구도 만들고 기본 모델 선택기도 채웁니다.
        models: ModelRow::rows(&models, &probe.default_model_alias),
    })
}

#[tauri::command]
pub async fn save_config(app: AppHandle, mut config: Config) -> Result<Snapshot, String> {
    let state = app.state::<Shared>().inner().clone();

    if config.port == 0 {
        return Err("포트 번호가 올바르지 않습니다.".into());
    }

    // 토큰 사용 모드를 켰는데 아직 발행된 토큰이 없으면 이 순간 자동 발행합니다.
    if config.token_mode && config.issued_token.trim().is_empty() {
        config.issued_token = config::generate_token();
    }

    let was_running = state.is_running();
    let port_changed = state.running_port().is_some_and(|p| p != config.port);

    config::save_config(&config).map_err(|err| format!("설정을 저장하지 못했습니다: {err}"))?;
    let new_port = config.port;
    state.replace_config(config);

    // 포트가 바뀌었으면 새 포트로 다시 띄웁니다.
    if was_running && port_changed {
        proxy::stop(&state);
        if let Err(err) = proxy::start(state.clone(), new_port).await {
            tray::refresh(&app);
            state.emit_state();
            return Err(err);
        }
    }

    tray::refresh(&app);
    state.emit_state();
    Ok(state.snapshot())
}

/// OpenAI 양식 토큰을 하나 발행해서 돌려줍니다 — 저장은 하지 않습니다.
/// 설정 폼이 초안(draft)에만 채워 두고, 실제 저장은 다른 값들과 함께 "저장" 시
/// 일어납니다. (test_connection 이 저장 없이 시험만 하는 것과 같은 규칙.)
#[tauri::command]
pub fn issue_token() -> String {
    config::generate_token()
}

#[tauri::command]
pub async fn start_proxy(app: AppHandle) -> Result<Snapshot, String> {
    let state = app.state::<Shared>().inner().clone();
    let port = state.config().port;
    let result = proxy::start(state.clone(), port).await;
    tray::refresh(&app);
    state.emit_state();
    result.map(|_| state.snapshot())
}

#[tauri::command]
pub fn stop_proxy(app: AppHandle) -> Snapshot {
    let state = app.state::<Shared>().inner().clone();
    proxy::stop(&state);
    state.flush_stats(true);
    tray::refresh(&app);
    state.emit_state();
    state.snapshot()
}

#[tauri::command]
pub async fn toggle_proxy(app: AppHandle) -> Result<Snapshot, String> {
    let running = app.state::<Shared>().is_running();
    if running {
        Ok(stop_proxy(app))
    } else {
        start_proxy(app).await
    }
}

#[tauri::command]
pub fn copy_endpoint(app: AppHandle) -> Result<String, String> {
    copy_endpoint_inner(&app)
}

/// 트레이 메뉴와 메인 창의 복사 버튼이 같은 경로를 씁니다.
pub fn copy_endpoint_inner(app: &AppHandle) -> Result<String, String> {
    let state = app.state::<Shared>();
    if !state.is_running() {
        return Err("프록시가 꺼져 있어 복사할 주소가 없습니다.".into());
    }
    let url = state.base_url();
    app.clipboard()
        .write_text(url.clone())
        .map_err(|err| format!("클립보드에 쓰지 못했습니다: {err}"))?;
    windows::show_toast(app, &url);
    Ok(url)
}

#[tauri::command]
pub fn open_log_window(app: AppHandle) {
    windows::show_log(&app);
}

#[tauri::command]
pub fn open_main_window(app: AppHandle) {
    windows::show_main(&app);
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    shutdown(&app);
}

/// 종료 경로는 하나로 모읍니다 — 통계를 내리고, 서버를 접고, 앱을 끕니다.
pub fn shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<Shared>() {
        let state = state.inner().clone();
        proxy::stop(&state);
        state.flush_stats(true);
    }
    app.exit(0);
}
