// 릴리스 빌드에서는 콘솔 창을 띄우지 않습니다 (트레이 상주 앱).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    fabrix_proxy_lib::run()
}
