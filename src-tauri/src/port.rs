//! 포트 가용성 검사와 점유 프로세스 조회.
//!
//! 목업 M2 는 "켜기를 시도할 때가 아니라 입력 직후" 충돌을 알려주고,
//! 빈 포트 하나를 미리 골라 버튼에 박아둡니다.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOwner {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    pub port: u16,
    pub free: bool,
    pub owner: Option<PortOwner>,
    /// 충돌일 때만 채워지는 추천 포트.
    pub suggestion: Option<u16>,
}

/// 프록시가 바인드할 주소와 정확히 같은 조건으로 시험 바인드합니다.
pub fn is_free(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok()
}

pub fn find_free_from(start: u16) -> Option<u16> {
    (start.saturating_add(1)..=start.saturating_add(60)).find(|p| is_free(*p))
}

pub fn inspect(port: u16, running_on: Option<u16>) -> PortStatus {
    // 우리가 이미 그 포트로 서비스 중이면 "사용 중"이 아니라 정상입니다.
    if running_on == Some(port) {
        return PortStatus { port, free: true, owner: None, suggestion: None };
    }
    if is_free(port) {
        return PortStatus { port, free: true, owner: None, suggestion: None };
    }
    PortStatus {
        port,
        free: false,
        owner: owner_of(port),
        suggestion: find_free_from(port),
    }
}

/// 충돌했을 때만 호출됩니다 — 두 개의 짧은 자식 프로세스를 띄웁니다.
#[cfg(windows)]
pub fn owner_of(port: u16) -> Option<PortOwner> {
    let out = hidden_command("netstat", &["-ano", "-p", "TCP"])?;
    let needle_local = format!(":{port}");

    let pid = out.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let proto = cols.next()?;
        if !proto.eq_ignore_ascii_case("TCP") {
            return None;
        }
        let local = cols.next()?;
        let _remote = cols.next()?;
        let state = cols.next()?;
        if !state.eq_ignore_ascii_case("LISTENING") {
            return None;
        }
        // `127.0.0.1:8787` / `0.0.0.0:8787` / `[::]:8787` 모두 잡습니다.
        if !local.ends_with(&needle_local) {
            return None;
        }
        cols.next()?.parse::<u32>().ok()
    })?;

    Some(PortOwner { pid, name: process_name(pid).unwrap_or_else(|| "알 수 없음".into()) })
}

#[cfg(not(windows))]
pub fn owner_of(_port: u16) -> Option<PortOwner> {
    None
}

#[cfg(windows)]
fn process_name(pid: u32) -> Option<String> {
    let filter = format!("PID eq {pid}");
    let out = hidden_command("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"])?;
    // `"node.exe","8124","Console","1","52,164 K"`
    let first = out.lines().find(|l| l.starts_with('"'))?;
    let name = first.split('"').nth(1)?;
    if name.is_empty() || name.contains("정보 없음") || name.contains("No tasks") {
        None
    } else {
        Some(name.to_string())
    }
}

/// 콘솔 창이 깜빡이지 않도록 `CREATE_NO_WINDOW` 를 붙여 실행합니다.
#[cfg(windows)]
fn hidden_command(program: &str, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let output = Command::new(program)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    // netstat/tasklist 는 콘솔 코드페이지(한국어 Windows 는 949)로 출력합니다.
    // 우리가 뽑는 값은 PID 숫자와 exe 이름이라 ASCII 범위이므로 lossy 로 충분합니다.
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}
