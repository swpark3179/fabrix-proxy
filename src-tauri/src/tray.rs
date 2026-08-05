//! 트레이 아이콘과 메뉴.
//!
//! 목업의 상태 헤더는 점 · 문구 · 건수 · Base URL 이 한 블록에 들어간 카드지만,
//! 네이티브 Windows 메뉴에는 그런 블록을 넣을 수 없습니다. 가장 가까운 형태로
//! **클릭 불가 항목 두 줄**(상태 + 주소)을 얹었습니다. 실제 동작 항목은 목업과
//! 동일하게 끄기 · 복사 · 창 · 로그 · 종료 다섯 개입니다.

use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::proxy;
use crate::state::Shared;
use crate::windows;

pub const TRAY_ID: &str = "fabrix-proxy-tray";

/// 앱 아이콘. 창·단축·트레이가 같은 그림을 씁니다.
/// (`icons/source.png` 에서 `npx tauri icon` 으로 파생된 것)
const APP_ICON: &[u8] = include_bytes!("../icons/64x64.png");

/// 실행 중 = 원본, 꺼짐 = 채도를 뺀 것.
///
/// 목업은 초록/회색 아이콘 두 장으로 상태를 구분했지만, 색이 정해진 브랜드
/// 아이콘에는 그 방법을 쓸 수 없습니다. 대신 같은 그림의 채도를 빼서 같은
/// 대비를 만듭니다. 알파는 건드리지 않아 라운드 코너가 그대로 유지됩니다.
fn tray_icon(running: bool) -> tauri::Result<Image<'static>> {
    let source = Image::from_bytes(APP_ICON)?;
    let mut rgba = source.rgba().to_vec();

    if !running {
        for px in rgba.chunks_exact_mut(4) {
            let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
            // 목업의 꺼짐 색(#b3afa6)쪽으로 살짝 따뜻하게 기울입니다.
            px[0] = (luma * 1.04).min(255.0) as u8;
            px[1] = (luma * 1.02).min(255.0) as u8;
            px[2] = (luma * 0.96).min(255.0) as u8;
        }
    }

    Ok(Image::new_owned(rgba, source.width(), source.height()))
}

/// 상태에 따라 글자/활성 여부를 바꿔야 하는 항목들.
/// 메뉴를 통째로 다시 만들지 않고 이 핸들로 갱신합니다.
pub struct TrayItems {
    status: MenuItem<Wry>,
    url: MenuItem<Wry>,
    toggle: MenuItem<Wry>,
    copy: MenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    // 상태 줄은 클릭 가능(활성)하게 둡니다. 비활성이면 회색으로 흐려져
    // 실행 중에도 "꺼진 것처럼" 보이기 때문입니다. 클릭하면 창을 엽니다.
    let status = MenuItem::with_id(app, "status", "○ 프록시 꺼짐", true, None::<&str>)?;
    let url = MenuItem::with_id(app, "url", "포트 8787", false, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "프록시 켜기", true, None::<&str>)?;
    let copy = MenuItem::with_id(app, "copy", "엔드포인트 주소 복사", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "창 열기", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "호출 로그 보기", true, None::<&str>)?;
    let models = MenuItem::with_id(app, "models", "모델 목록 보기", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "사내 연결 설정", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &status,
            &url,
            &PredefinedMenuItem::separator(app)?,
            &toggle,
            &copy,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &logs,
            &models,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon(false)?)
        .tooltip("AI 프록시 · 꺼짐")
        .menu(&menu)
        // 좌클릭은 메뉴가 아니라 창 열기로 씁니다 (목업: 더블클릭 = 창 열기).
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } = event {
                windows::show_main(tray.app_handle());
            }
        })
        .build(app)?;

    app.manage(TrayItems { status, url, toggle, copy });
    Ok(())
}

/// 프록시 상태가 바뀔 때마다 호출합니다.
pub fn refresh(app: &AppHandle) {
    let Some(items) = app.try_state::<TrayItems>() else {
        return;
    };
    let state = app.state::<Shared>();
    let snapshot = state.snapshot();
    let running = snapshot.running;

    let _ = items.status.set_text(if running {
        "● 프록시 실행 중".to_string()
    } else {
        "○ 프록시 꺼짐".to_string()
    });
    let _ = items.url.set_text(if running {
        snapshot.base_url.clone()
    } else {
        format!("포트 {}", snapshot.port)
    });
    let _ = items.toggle.set_text(if running { "프록시 끄기" } else { "프록시 켜기" });
    // 꺼져 있으면 복사 비활성 — 붙여넣어도 안 되는 주소를 못 가져가게 막습니다 (T2).
    let _ = items.copy.set_enabled(running);

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Ok(icon) = tray_icon(running) {
            let _ = tray.set_icon(Some(icon));
        }
        let _ = tray.set_tooltip(Some(if running {
            format!("AI 프록시 · {}", snapshot.base_url)
        } else {
            "AI 프록시 · 꺼짐".to_string()
        }));
    }
}

fn on_menu(app: &AppHandle, event: MenuEvent) {
    match event.id().as_ref() {
        "toggle" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<Shared>().inner().clone();
                if state.is_running() {
                    proxy::stop(&state);
                } else {
                    let port = state.config().port;
                    if let Err(err) = proxy::start(state.clone(), port).await {
                        eprintln!("[tray] 프록시를 켜지 못했습니다: {err}");
                    }
                }
                refresh(&app);
                state.emit_state();
            });
        }
        "copy" => {
            let _ = crate::commands::copy_endpoint_inner(app);
        }
        // 상태 줄을 눌러도 창을 엽니다 (활성 항목이라 클릭이 들어옵니다).
        "status" | "open" => windows::show_main(app),
        "logs" => windows::show_log(app),
        "models" => windows::show_models(app),
        "settings" => windows::show_settings(app),
        "quit" => crate::commands::shutdown(app),
        _ => {}
    }
}
