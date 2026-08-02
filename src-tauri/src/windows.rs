//! 창 3개(main · log · toast) 의 표시/숨김.
//!
//! 셋 다 `tauri.conf.json` 에서 `visible: false` 로 미리 만들어집니다 —
//! 토스트는 특히 프런트가 이벤트를 듣고 있어야 하므로 앱 시작 시점에 살아 있어야 합니다.

use std::sync::atomic::Ordering;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition};

use crate::state::Shared;

const TOAST_W: f64 = 300.0;
const TOAST_H: f64 = 132.0;
const TOAST_MARGIN: f64 = 16.0;
/// 기본 Windows 작업 표시줄 높이. Tauri 의 Monitor 는 작업 영역을 노출하지
/// 않아 상수로 비켜 둡니다 (목업 T3 도 작업 표시줄 위에 떠 있습니다).
const TASKBAR: f64 = 56.0;
const TOAST_MS: u64 = 3_000;

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
