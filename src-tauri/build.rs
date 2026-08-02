fn main() {
    // tauri-build 는 번들 아이콘 변경을 추적하지 않습니다. 이 줄이 없으면
    // 아이콘만 교체했을 때 build.rs 가 다시 돌지 않아, exe 에는 예전 아이콘
    // 리소스가 그대로 남습니다 (설치본·작업 표시줄·Alt+Tab 이 옛 아이콘).
    println!("cargo:rerun-if-changed=icons/icon.ico");

    tauri_build::build()
}
