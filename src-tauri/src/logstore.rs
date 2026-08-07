//! 최근 호출 링버퍼. "최근 50건 · 본문은 메모리에만 보관".
//!
//! 여기 담기는 본문(`req_openai` / `req_fabrix` / `resp_body` / `raw`)은 디스크로
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
    /// 실제로 들어온 경로. 리터럴이 아닌 이유: `/v1/models/{id}` 처럼 경로에 값이
    /// 박히는 엔드포인트가 있고, 로그 창의 `cURL 복사`(`format.ts` `toCurl`)가 이 값을
    /// 그대로 URL 에 넣습니다. `{id}` 를 그대로 두면 붙여넣어도 동작하지 않는 명령이
    /// 만들어집니다.
    pub path: String,
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
    /// ④ 가공하지 않은 와이어 원문. 설정에서 끄면 비어 있습니다.
    pub raw: RawWire,
}

/// 와이어 원문 한 쪽의 상한(바이트).
///
/// 링버퍼가 50건이고 한 건에 두 쪽이 있으므로 최악이 25MiB 입니다. 상한을 두는
/// 이유: `write` 도구 하나가 HTML 문서 전체를 인자로 싣고, 그 응답이 사내 원문과
/// 클라이언트 원문 양쪽에 한 번씩 더 복사됩니다. 넘친 만큼은 **버렸다고 적습니다** —
/// 조용히 자르면 잘린 JSON 을 진짜 응답으로 오해합니다.
pub const RAW_CAP: usize = 256 * 1024;

/// 로그 한 건의 와이어 원문 두 쪽. 화면 ④ 칸이 이걸 그립니다.
///
/// 담았는지를 **쪽마다** 따로 적는 이유: 두 쪽의 규칙이 다릅니다. 사내가 준 쪽은
/// 언제나 담습니다 — ③ 칸의 "사내 원문 보기" 가 설정과 무관하게 동작해야 하기
/// 때문입니다. 클라이언트로 나간 쪽만 `rawWireLog` 토글이 제어합니다. 플래그가
/// 하나뿐이면 화면이 "꺼져서 비었다" 와 "켰는데 안 왔다" 를 한쪽 기준으로만 말하게 됩니다.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWire {
    /// 사내가 준 쪽을 담았는가. 채팅과 모델 목록은 언제나 참입니다.
    pub upstream_captured: bool,
    /// 클라이언트로 나간 쪽을 담았는가 — 이쪽만 기록 스위치가 제어합니다.
    pub client_captured: bool,
    /// 사내가 준 바이트 그대로 (SSE 는 `data:` 줄까지 포함).
    pub upstream: String,
    /// 클라이언트로 나간 본문 그대로 (SSE 는 우리가 쓴 `data:` 줄 그대로).
    pub client: String,
}

/// 와이어 한 쪽을 모으는 버퍼. 꺼져 있으면 아무것도 담지 않습니다.
///
/// 바이트로 들고 있다가 마지막에 한 번만 UTF-8 로 옮기는 이유: 스트림 청크는
/// 멀티바이트 문자 한가운데서 갈릴 수 있어, 조각마다 `from_utf8_lossy` 를 부르면
/// 원문에 없던 `U+FFFD` 가 박힙니다.
#[derive(Debug, Default)]
pub struct RawBuf {
    enabled: bool,
    bytes: Vec<u8>,
    /// 상한을 넘겨 버린 바이트 수.
    dropped: usize,
}

impl RawBuf {
    pub fn new(enabled: bool) -> Self {
        Self { enabled, bytes: Vec::new(), dropped: 0 }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn push(&mut self, chunk: &[u8]) {
        if !self.enabled {
            return;
        }
        let room = RAW_CAP.saturating_sub(self.bytes.len());
        if room == 0 {
            self.dropped += chunk.len();
            return;
        }
        if chunk.len() <= room {
            self.bytes.extend_from_slice(chunk);
        } else {
            self.bytes.extend_from_slice(&chunk[..room]);
            self.dropped += chunk.len() - room;
        }
    }

    pub fn push_str(&mut self, text: &str) {
        self.push(text.as_bytes());
    }

    /// 지금까지 모은 원문. `&self` 인 이유: 로그 한 건을 조립하는 `Ctx::entry` 가
    /// `&self` 라서입니다 — 요청당 한 번만 부르므로 복사 비용은 문제가 되지 않습니다.
    pub fn text(&self) -> String {
        let mut out = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.dropped > 0 {
            out.push_str(&format!(
                "\n\n…(상한 {}KiB 를 넘어 {}바이트를 버렸습니다)",
                RAW_CAP / 1024,
                self.dropped
            ));
        }
        out
    }
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

    /// 메인 창 "최근 호출" 용 몇 건. **와이어 원문은 떼고** 보냅니다.
    ///
    /// 그 화면은 시각·상태·경로·요약만 그리는데, 스냅샷은 호출 한 건마다 통째로
    /// 웹뷰로 나갑니다. 쓰지도 않는 수백 KiB 를 매번 실어 보낼 이유가 없습니다.
    /// 로그 창은 `get_logs`(저장소 전체)와 `log:new` 이벤트로 받으므로 잃는 것이 없습니다.
    pub fn recent(&self, n: usize) -> Vec<LogEntry> {
        self.entries
            .iter()
            .take(n)
            .map(|e| LogEntry {
                raw: RawWire {
                    upstream_captured: e.raw.upstream_captured,
                    client_captured: e.raw.client_captured,
                    ..RawWire::default()
                },
                ..e.clone()
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_buffer_keeps_nothing() {
        let mut buf = RawBuf::new(false);
        buf.push_str("data: {\"content\":\"안녕\"}\n\n");
        assert!(!buf.enabled());
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn an_enabled_buffer_keeps_the_bytes_verbatim() {
        let mut buf = RawBuf::new(true);
        buf.push_str("data: {\"content\":\"안\"}\n\n");
        buf.push_str("data: [DONE]\n\n");
        assert_eq!(buf.text(), "data: {\"content\":\"안\"}\n\ndata: [DONE]\n\n");
    }

    /// 청크가 멀티바이트 문자 한가운데서 갈려도 원문에 없던 글자가 생기면 안 됩니다 —
    /// 조각마다 UTF-8 로 옮기면 정확히 그 일이 벌어집니다.
    #[test]
    fn a_multibyte_char_split_across_pushes_survives() {
        let text = "한글";
        let bytes = text.as_bytes();
        let mut buf = RawBuf::new(true);
        buf.push(&bytes[..2]);
        buf.push(&bytes[2..]);
        assert_eq!(buf.text(), text);
    }

    /// 상한을 넘긴 만큼은 **버렸다고 적습니다**. 조용히 자르면 잘린 JSON 을 진짜
    /// 응답으로 오해합니다.
    #[test]
    fn overflow_is_cut_and_reported() {
        let mut buf = RawBuf::new(true);
        buf.push(&vec![b'x'; RAW_CAP + 10]);
        buf.push(&[b'y'; 5]);
        let text = buf.text();
        assert!(text.starts_with("xxxx"));
        assert!(!text.contains('y'));
        assert!(text.contains("15바이트를 버렸습니다"), "{}", &text[text.len() - 60..]);
    }
}
