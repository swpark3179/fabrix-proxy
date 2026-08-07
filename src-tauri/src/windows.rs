//! 창 3개(main · log · toast) 의 생성과 표시/숨김.
//!
//! 셋 다 앱 시작 때 `visible: false` 로 미리 만들어집니다 — 토스트는 특히
//! 프런트가 이벤트를 듣고 있어야 하므로 시작 시점에 살아 있어야 합니다.
//!
//! 창을 `tauri.conf.json` 이 아니라 여기서 만드는 이유: config 에 정의한 창은
//! `build()` 때 곧바로 만들어져 webview 가 로드되고, `setup` 훅이 상태를
//! `.manage()` 하기 전에 `get_snapshot` 같은 커맨드를 부를 수 있습니다.
//! 그러면 "state not managed" 에러가 첫 실행 화면에 그대로 뜹니다.
//! 대신 상태를 `.manage()` 한 뒤 [`create_all`] 로 만들면 그 레이스가 사라집니다.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

use crate::state::Shared;

const TOAST_W: f64 = 300.0;
const TOAST_H: f64 = 132.0;
const TOAST_MARGIN: f64 = 16.0;
/// 기본 Windows 작업 표시줄 높이. Tauri 의 Monitor 는 작업 영역을 노출하지
/// 않아 상수로 비켜 둡니다 (목업 T3 도 작업 표시줄 위에 떠 있습니다).
const TASKBAR: f64 = 56.0;
const TOAST_MS: u64 = 3_000;

/// 세 창을 모두 숨긴 채로 만듭니다. 반드시 `app.manage(state)` 이후에 부릅니다 —
/// 그래야 webview 가 커맨드를 부를 때 상태가 이미 등록돼 있습니다.
pub fn create_all(app: &AppHandle) -> tauri::Result<()> {
    // main — 온보딩/상태 화면. 크기 고정, 최대화 불가.
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("AI 프록시")
        .inner_size(720.0, 560.0)
        .resizable(false)
        .maximizable(false)
        .decorations(false)
        .center()
        .visible(false)
        .build()?;

    // log — 호출 로그. 크기 조절 가능.
    WebviewWindowBuilder::new(app, "log", WebviewUrl::App("log.html".into()))
        .title("호출 로그")
        .inner_size(900.0, 620.0)
        .min_inner_size(760.0, 480.0)
        .resizable(true)
        .decorations(false)
        .center()
        .visible(false)
        .build()?;

    // toast — 트레이 위 알림 카드. 투명 · 항상 위 · 포커스 뺏지 않음.
    WebviewWindowBuilder::new(app, "toast", WebviewUrl::App("toast.html".into()))
        .title("알림")
        .inner_size(TOAST_W, TOAST_H)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .visible(false)
        .build()?;

    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn show_log(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("log") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

const MODELS_W: f64 = 900.0;
const MODELS_H: f64 = 600.0;

/// 이미 있는 모델 목록 창을 앞으로 끌어올립니다. 없으면 `false`.
fn raise_models(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window("models") else {
        return false;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    true
}

/// models — 모델 목록. **처음 열 때 만듭니다.**
///
/// 다른 세 창과 달리 [`create_all`] 에 넣지 않은 이유: 이 창은 푸시 이벤트를 기다릴
/// 필요가 없고(마운트 때 스스로 조회합니다), 대부분의 실행에서 한 번도 열리지
/// 않습니다. 시작할 때 webview 를 하나 더 띄우는 값을 내지 않습니다. 한 번 만들면
/// 닫기(✕)가 숨김이라 두 번째부터는 `show` 만 합니다.
///
/// **창 만들기를 부른 쪽 스레드에서 하지 않는 이유**: `WebviewWindowBuilder::build()`
/// 는 동기 커맨드나 이벤트 핸들러 안에서 부르면 Windows 에서 교착합니다(tauri#5306).
/// 이 함수를 부르는 자리가 정확히 그 둘입니다 — `open_models_window` 커맨드와
/// 트레이 메뉴 핸들러(`tray.rs`). 그래서 창이 **없을 때만** 생성을 async 런타임으로
/// 넘깁니다. 증상은 "첫 클릭에서 앱이 멈춤" 이었고, 트레이로 창을 한 번 만든 뒤에는
/// 위 `raise_models` 갈래로 빠져 멀쩡해 보였습니다.
pub fn show_models(app: &AppHandle) {
    if raise_models(app) {
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // 연달아 누르면 작업이 둘 뜹니다. 뒤엣것은 같은 라벨로 `build()` 해서 실패하므로
        // 여기서 한 번 더 확인해 헛수고를 막습니다.
        if raise_models(&app) {
            return;
        }
        match WebviewWindowBuilder::new(&app, "models", WebviewUrl::App("models.html".into()))
            .title("모델 목록")
            .inner_size(MODELS_W, MODELS_H)
            .min_inner_size(780.0, 420.0)
            .resizable(true)
            .decorations(false)
            .center()
            .build()
        {
            Ok(window) => {
                let _ = window.set_focus();
            }
            Err(err) => {
                eprintln!("[windows] 모델 목록 창을 열지 못했습니다: {err}");
                // 위 확인을 통과한 뒤에 다른 작업이 먼저 만들었을 수 있습니다 —
                // 그건 실패가 아니라 이미 있는 것이라 띄워 주면 됩니다.
                raise_models(&app);
            }
        }
    });
}

/// 메인 창을 띄우면서 설정 화면을 열라고 알립니다.
pub fn show_settings(app: &AppHandle) {
    show_main(app);
    let _ = app.emit_to("main", "ui:settings", ());
}

/// 목업 T3 — 트레이 위 우하단에 268px 카드를 3초간 띄웁니다.
pub fn show_toast(app: &AppHandle, url: &str) {
    let Some(window) = app.get_webview_window("toast") else {
        return;
    };

    if let Ok(Some(monitor)) = window.primary_monitor().or_else(|_| window.current_monitor()) {
        let scale = monitor.scale_factor();
        let size = monitor.size();
        let origin = monitor.position();
        let x = origin.x + size.width as i32 - (TOAST_W * scale) as i32 - (TOAST_MARGIN * scale) as i32;
        let y = origin.y + size.height as i32 - (TOAST_H * scale) as i32 - (TASKBAR * scale) as i32;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }

    let _ = app.emit_to("toast", "toast:show", url.to_string());
    let _ = window.show();

    // 연달아 복사해도 앞선 타이머가 새 토스트를 지우지 않도록 세대 번호로 걸러냅니다.
    let state = app.state::<Shared>();
    let generation = state.toast_gen.fetch_add(1, Ordering::SeqCst) + 1;

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(TOAST_MS)).await;
        let current = app.state::<Shared>().toast_gen.load(Ordering::SeqCst);
        if current != generation {
            return;
        }
        if let Some(window) = app.get_webview_window("toast") {
            let _ = window.hide();
        }
    });
}
