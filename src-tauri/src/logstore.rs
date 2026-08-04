//! 최근 호출 링버퍼. "최근 50건 · 본문은 메모리에만 보관".
//!
//! 여기 담기는 본문(`req_openai` / `req_fabrix` / `resp_body`)은 디스크로
//! 나가지 않습니다. 앱을 끄면 사라지는 것이 의도된 동작입니다.

use std::collections::VecDeque;

use serde::Serialize;

pub const CAPACITY: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Chat,
    Models,
    Images,
}

impl Kind {
    pub fn method(self) -> &'static str {
        match self {
            Kind::Chat => "POST",
            Kind::Models => "GET",
            Kind::Images => "POST",
        }
    }

    /// 대표 경로. 이미지는 두 엔드포인트(`/generations`·`/edits`)를 한 종류로 묶으므로
    /// 실제 경로는 핸들러가 `LogEntry.path` 리터럴로 직접 설정합니다.
    pub fn path(self) -> &'static str {
        match self {
            Kind::Chat => "/v1/chat/completions",
            Kind::Models => "/v1/models",
            Kind::Images => "/v1/images/generations",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: String,
    /// `HH:MM` — 목록 열에 그대로 들어갑니다.
    pub ts: String,
    pub ts_full: String,
    pub kind: Kind,
    pub method: &'static str,
    pub path: &'static str,
    pub status: u16,
    pub latency_ms: u64,
    pub stream: bool,
    pub cached: bool,
    /// 클라이언트가 보낸 원래 모델명 (예: `gpt-4o`).
    pub model_requested: Option<String>,
    /// 프록시가 고른 alias (예: `fabrix-chat-4`).
    pub model_alias: Option<String>,
    /// 실제로 FabriX 에 보낸 UUID.
    pub model_id: Option<String>,
    /// 사람이 읽는 모델명 (예: `챗 4`).
    pub model_label: Option<String>,
    /// User-Agent 에서 뽑은 짧은 이름 (예: `Continue.dev`).
    pub client: Option<String>,
    /// 목록 2번째 줄에 뜨는 한 줄 설명 (예: `사내 응답 없음`).
    pub note: Option<String>,
    /// 메인 창 "최근 호출" 오른쪽 칸 (예: `gpt-4o → 챗 4`, `7개`).
    pub summary: Option<String>,
    pub is_error: bool,

    // ── 상세 3칸 ──────────────────────────────────────────────
    /// ① 받은 요청 — OpenAI 형식 pretty JSON.
    pub req_openai: String,
    /// ② 변환해서 보낸 요청 — FabriX 형식 pretty JSON.
    pub req_fabrix: String,
    /// ② 상단에 붙는 마스킹된 헤더 줄.
    pub req_fabrix_headers: String,
    pub fabrix_url: String,
    /// ③ 돌려준 응답 — 자르지 않은 전문.
    ///
    /// 목록 화면에서 앞부분만 보여 주고 "전체보기" 팝업에서 전부 펼치는 것은
    /// 화면(`CollapsibleCode`)의 몫입니다. 여기서 미리 자르면 팝업을 열어도
    /// 잘린 뒤가 없어 되살릴 수 없으므로, 저장은 언제나 전문으로 합니다.
    pub resp_body: String,
    /// ③ 하단 메타 라인.
    pub resp_meta: String,
}

#[derive(Debug, Default)]
pub struct LogStore {
    entries: VecDeque<LogEntry>,
}

impl LogStore {
    pub fn push(&mut self, entry: LogEntry) {
        if self.entries.len() >= CAPACITY {
            self.entries.pop_back();
        }
        self.entries.push_front(entry);
    }

    /// 최신순.
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.entries.iter().cloned().collect()
    }

    pub fn recent(&self, n: usize) -> Vec<LogEntry> {
        self.entries.iter().take(n).cloned().collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// `Continue/0.9.x (...)` 같은 UA 에서 앞쪽 제품명만 뽑습니다.
pub fn short_client(ua: Option<&str>) -> Option<String> {
    let ua = ua?.trim();
    if ua.is_empty() {
        return None;
    }
    let head = ua.split_whitespace().next().unwrap_or(ua);
    let name = head.split('/').next().unwrap_or(head);
    if name.is_empty() {
        None
    } else {
        Some(name.chars().take(24).collect())
    }
}

/// 로그에 그대로 담기 곤란한 원문(해석 실패한 요청 본문, 오류 응답 머리말 등)을
/// 앞부분만 남깁니다. 응답 본문(`resp_body`)에는 쓰지 않습니다 — 그 칸은 전문입니다.
pub fn preview(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(limit).collect();
    format!("{head}…")
}
