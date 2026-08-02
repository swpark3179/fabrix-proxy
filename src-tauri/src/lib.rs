//! 사내 FabriX AI 를 OpenAI 호환 API 로 중계하는 트레이 상주 앱.

pub mod commands;
pub mod config;
pub mod logstore;
pub mod openai;
pub mod port;
pub mod proxy;
pub mod state;
pub mod tray;
pub mod windows;

use tauri::{Manager, RunEvent, WindowEvent};

use crate::state::AppState;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::get_config,
            commands::get_config_path,
            commands::get_logs,
            commands::clear_logs,
            commands::check_port,
            commands::test_connection,
            commands::save_config,
            commands::start_proxy,
            commands::stop_proxy,
            commands::toggle_proxy,
            commands::copy_endpoint,
            commands::open_log_window,
            commands::open_main_window,
            commands::quit_app,
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // config.json 이 없으면 첫 실행 → 온보딩.
            let loaded = config::load_config();
            let first_run = loaded.is_none();
            let cfg = loaded.unwrap_or_default();
            let configured = cfg.is_configured();
            let auto_start = cfg.auto_start;
            let port = cfg.port;

            let shared = AppState::new(handle.clone(), cfg, first_run);
            app.manage(shared.clone());

            tray::build(&handle)?;
            tray::refresh(&handle);

            if !configured {
                // 설정 전에는 창을 띄워 온보딩부터 받습니다.
                windows::show_main(&handle);
            } else if auto_start {
                let handle = handle.clone();
                let shared = shared.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = proxy::start(shared.clone(), port).await {
                        eprintln!("[startup] 자동 시작 실패: {err}");
                    }
                    tray::refresh(&handle);
                    shared.emit_state();
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 트레이 상주 앱 — 닫기(✕)는 종료가 아니라 숨김입니다.
            // 종료는 트레이 메뉴로만 합니다.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("Tauri 앱을 초기화하지 못했습니다")
        .run(|_app, event| {
            // 창을 모두 숨겨도 프로세스는 트레이에 남아 있어야 합니다.
            // `code` 가 있는 종료(= quit 명령)는 그대로 통과시킵니다.
            if let RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
