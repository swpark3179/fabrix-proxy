//! 프런트엔드가 부르는 IPC 표면.

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::config::{self, Config};
use crate::logstore::LogEntry;
use crate::port::{self, PortStatus};
use crate::proxy::fabrix::{build_aliases, build_http_client, FabrixClient};
use crate::proxy::{self};
use crate::state::{Shared, Snapshot};
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub model_count: usize,
    /// 앞의 몇 개만 — 연결이 됐다는 증거로 보여줍니다.
    pub sample: Vec<String>,
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

    let raw = client.list_models().await.map_err(|err| err.message())?;
    let models = build_aliases(&raw);
    Ok(TestResult {
        model_count: models.len(),
        sample: models
            .iter()
            .take(4)
            .map(|m| format!("{} · {}", m.alias, m.label))
            .collect(),
    })
}

/// 설정 화면의 이미지 모델 선택 드롭다운용 — 사내 모델 목록(alias · id · label)을 돌려줍니다.
/// 60초 캐시를 그대로 활용합니다.
#[tauri::command]
pub async fn list_models(app: AppHandle) -> Result<Vec<crate::proxy::fabrix::ResolvedModel>, String> {
    let state = app.state::<Shared>().inner().clone();
    crate::proxy::models::ensure_models(&state)
        .await
        .map(|(models, _)| models)
        .map_err(|err| err.message())
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
