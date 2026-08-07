//! 한 턴의 응답 조립 — 스트림과 비스트림이 **같은 상태 기계**를 지납니다.
//!
//! 예전에는 두 경로가 각자 조립했습니다: 스트림은 제너레이터 안에서 `StreamEvent` 를
//! 직접 match 하고(그것도 펌프와 디코더 꼬리에 **두 번** 똑같이), 비스트림은
//! `FabrixChunk` 필드를 읽어 따로 조립했습니다. 두 사본이 어긋나면 같은 답변이
//! `stream` 여부에 따라 다른 결과를 받습니다 — 실제로 그렇게 어긋나 있었습니다.
//!
//! 이제 두 경로 모두 `StreamEvent` 를 만들어 이 `Turn` 에 먹입니다
//! (비스트림은 [`super::fabrix::nonstream_events`] 로 변환).
//!
//! 흐름:
//! ```text
//! 바이트 → StreamDecoder → StreamEvent → Turn
//!                                          ├ Delta     → ThinkSplitter → 채널별 ToolCallScanner
//!                                          └ Reasoning →                 채널별 ToolCallScanner
//!                                                                              ↓
//!                                                          Piece::{Content,Reasoning,Call}
//! ```
//! `<think>` 를 먼저 갈라내고 그다음에 스캐너를 태우는 순서가 핵심입니다 —
//! 그래서 `<think>` **안쪽**의 도구 호출도 잡힙니다.

use std::collections::HashSet;

use super::fabrix::{decide_finish, StreamEvent};
use super::tools::{Channel, ScanOut, ScannedCall, Split, ThinkSplitter, ToolCallScanner};

/// 스트림이 중간에 끊긴 것을 상위 사유처럼 취급하는 값.
///
/// [`decide_finish`] 안의 `clamp_finish_reason` 이 중단 계열을 `length` 로 접습니다.
/// `stop` 은 쓰지 않습니다 — 끊긴 답변을 완성된 것처럼 부르는 거짓말이 됩니다.
/// 별도의 `MIDSTREAM_FINISH` 상수를 두지 않는 이유: 상수와 clamp 표가 따로 있으면
/// 둘이 조용히 어긋날 수 있습니다.
const MIDSTREAM_REASON: &str = "error";

/// 클라이언트에 내보낼 조각 하나.
///
/// 열거형으로 둔 이유가 둘 있습니다. (1) 순서가 보존됩니다 — `답변<think>생각` 에서
/// 생각을 앞으로 끌어오면 클라이언트가 보는 순서가 뒤집힙니다. (2) 같은 텍스트가 두
/// 채널로 나가지 않음을 **타입으로** 보장합니다.
#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Content(String),
    Reasoning(String),
    Call(ScannedCall),
}

/// 한 번의 소비가 만들어 낸 조각들. 순서가 곧 클라이언트가 보는 순서입니다.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Emit(pub Vec<Piece>);

impl Emit {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 로그 ③ 칸 꼬리에 쓰는 집계.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolStats {
    pub calls: u32,
    /// 도구가 아니라고 판정해 텍스트로 되돌린 블록 수.
    pub rejected: u32,
    /// 그중 추론 쪽(추론 채널 + 본문 내 `<think>`)에서 나온 호출 수.
    ///
    /// 이 숫자가 이번 수정이 실제로 물었는지를 말해 줍니다.
    pub in_reasoning: u32,
    pub think_blocks: u32,
    pub think_unclosed: bool,
}

/// 한 턴의 응답을 조립합니다.
#[derive(Debug)]
pub struct Turn {
    scanner: ToolCallScanner,
    /// `None` 이면 본문 내 `<think>` 를 갈라내지 않습니다.
    think: Option<ThinkSplitter>,
    /// 스캐너가 걷어낸 **뒤**의 텍스트 — 응답 본문과 usage 입력.
    content: String,
    reasoning: String,
    calls: Vec<ScannedCall>,
    /// 상위가 보낸 **원문**(걷어내기 전). 로그 ③ 칸이 이걸 씁니다 — `<tool_call>` 이
    /// 실제로 있었는지 눈으로 보려면 원문이어야 합니다.
    raw_content: String,
    raw_reasoning: String,
    upstream_finish: Option<String>,
    truncated: bool,
    failure: Option<String>,
    done: bool,
    calls_in_reasoning: u32,
}

impl Turn {
    /// `tool_names` 가 비면 스캐너는 스스로 통과 모드가 됩니다 — 도구를 안 쓰는
    /// 요청에 별도 분기가 필요 없습니다.
    ///
    /// `split_think` 를 도구 사용 여부와 **묶지 않는** 이유: `<think>` 블록이 본문에
    /// 새어 나오면 도구를 쓰든 안 쓰든 답변이 오염됩니다.
    pub fn new(tool_names: HashSet<String>, split_think: bool) -> Self {
        Self {
            scanner: ToolCallScanner::new(tool_names, true),
            think: split_think.then(ThinkSplitter::new),
            content: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            raw_content: String::new(),
            raw_reasoning: String::new(),
            upstream_finish: None,
            truncated: false,
            failure: None,
            done: false,
            calls_in_reasoning: 0,
        }
    }

    /// 이벤트 하나를 소비하고 내보낼 조각들을 돌려줍니다.
    ///
    /// `Delta`/`Reasoning`/`Finish`/`Error`/`Done` 처리가 **여기 한 곳**뿐입니다.
    pub fn push(&mut self, event: StreamEvent) -> Emit {
        let mut emit = Emit::default();
        match event {
            StreamEvent::Delta(text) => {
                self.raw_content.push_str(&text);
                // 대여 검사 때문에 분리를 먼저 끝내고 나서 흡수합니다.
                let split = match self.think.as_mut() {
                    Some(th) => th.push(&text),
                    None => Split::content(text),
                };
                for (ch, seg) in split.parts {
                    let out = self.scanner.push_on(ch, &seg);
                    self.route(ch, out, &mut emit);
                }
            }
            StreamEvent::Reasoning(text) => {
                self.raw_reasoning.push_str(&text);
                let out = self.scanner.push_on(Channel::Reasoning, &text);
                self.route(Channel::Reasoning, out, &mut emit);
            }
            StreamEvent::Finish(reason) => self.upstream_finish = Some(reason),
            StreamEvent::Error(msg) => self.failure = Some(msg),
            StreamEvent::Done => self.done = true,
        }
        emit
    }

    /// 비스트림용 — 내보낼 조각은 버립니다(누적은 `Turn` 이 하고 있습니다).
    pub fn feed(&mut self, event: StreamEvent) {
        let _ = self.push(event);
    }

    /// 종료. 분리기와 두 채널 버퍼가 붙들고 있던 꼬리를 **전부** 흘려보냅니다 —
    /// 절대 버리지 않습니다.
    ///
    /// `finish_reason()`·`is_empty()` 를 읽기 **전에** 반드시 불러야 합니다: 도구 호출은
    /// 닫는 태그를 봐야 완성되므로 마지막 호출이 여기서 나올 수 있습니다.
    pub fn finish(&mut self) -> Emit {
        let mut emit = Emit::default();
        if let Some(th) = self.think.as_mut() {
            for (ch, seg) in th.finish().parts {
                let out = self.scanner.push_on(ch, &seg);
                self.route(ch, out, &mut emit);
            }
        }
        let out = self.scanner.finish_on(Channel::Content);
        self.route(Channel::Content, out, &mut emit);
        let out = self.scanner.finish_on(Channel::Reasoning);
        self.route(Channel::Reasoning, out, &mut emit);
        emit
    }

    /// 스캐너 출력 하나를 채널에 맞는 누적기와 조각으로 나눕니다.
    fn route(&mut self, ch: Channel, out: ScanOut, emit: &mut Emit) {
        if !out.text.is_empty() {
            match ch {
                Channel::Content => {
                    self.content.push_str(&out.text);
                    emit.0.push(Piece::Content(out.text));
                }
                Channel::Reasoning => {
                    self.reasoning.push_str(&out.text);
                    emit.0.push(Piece::Reasoning(out.text));
                }
            }
        }
        for call in out.calls {
            if ch == Channel::Reasoning {
                self.calls_in_reasoning += 1;
            }
            self.calls.push(call.clone());
            emit.0.push(Piece::Call(call));
        }
    }

    /// 상위가 답변을 잘랐다고 알려 온 경우. 디코더 플래그를 옮겨 담습니다.
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// 스트림이 끊긴 경우 — 상위가 준 오류 프레임이 아니라 전송 자체의 실패.
    pub fn mark_failure(&mut self, msg: String) {
        self.failure = Some(msg);
    }

    pub fn saw_call(&self) -> bool {
        self.scanner.saw_call()
    }

    /// 이 턴의 `finish_reason`. 두 경로가 이 한 함수만 씁니다.
    pub fn finish_reason(&self) -> &'static str {
        let upstream = if self.failure.is_some() {
            Some(MIDSTREAM_REASON)
        } else {
            self.upstream_finish.as_deref()
        };
        decide_finish(self.saw_call(), upstream, self.truncated)
    }

    pub fn calls(&self) -> &[ScannedCall] {
        &self.calls
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn reasoning(&self) -> &str {
        &self.reasoning
    }

    pub fn raw_content(&self) -> &str {
        &self.raw_content
    }

    pub fn raw_reasoning(&self) -> &str {
        &self.raw_reasoning
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn upstream_finish(&self) -> Option<&str> {
        self.upstream_finish.as_deref()
    }

    /// 어느 채널도 아무것도 만들지 않았는가. 빈 응답을 200 으로 볼지 502 로 볼지
    /// 가리는 데 씁니다. `finish()` **뒤에** 읽어야 합니다.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty() && self.reasoning.is_empty() && self.calls.is_empty()
    }

    /// 상위가 **한 글자도** 주지 않았는가 — 걷어내기 **전** 원문 기준.
    ///
    /// [`Self::is_empty`] 와 묻는 것이 다릅니다. `is_empty` 는 가공 뒤를 보므로 답변
    /// 전체가 도구 호출 하나였던 턴도 산문은 비어 있고, 반대로 이 함수는 그 턴을
    /// "받았다" 라고 답합니다.
    ///
    /// 이 구분이 필요한 이유는 로그가 가리키는 곳이 달라지기 때문입니다. 사내가 0자를
    /// 준 턴에 `호출 0건 — 모델이 규약을 따르지 않음` 이라고 적으면 사용자는 주입한
    /// 도구 규약을 의심하지만, 실제로는 모델이 규약을 지킬 기회조차 없었습니다.
    pub fn received_nothing(&self) -> bool {
        self.raw_content.is_empty() && self.raw_reasoning.is_empty()
    }

    /// `message.reasoning_content` — 비어 있으면 키를 내보내지 않습니다.
    pub fn reasoning_field(&self) -> Option<String> {
        Some(self.reasoning.clone()).filter(|s| !s.is_empty())
    }

    /// `message.content` 의 null / 빈 문자열 규칙을 한 곳에 못박습니다.
    ///
    /// 도구 호출만 있는 턴은 `null` 이 규약입니다. 그 밖에는 빈 문자열이라도
    /// **문자열**입니다 — 모델이 정말 빈 답을 준 경우가 `content: ""` 이고, 그걸
    /// null 로 바꾸면 도구 호출 턴과 구분되지 않습니다.
    pub fn assistant_content(&self) -> Option<String> {
        if self.saw_call() {
            Some(self.content.clone()).filter(|s| !s.trim().is_empty())
        } else {
            Some(self.content.clone())
        }
    }

    pub fn tool_stats(&self) -> ToolStats {
        ToolStats {
            calls: self.scanner.call_count(),
            rejected: self.scanner.rejected,
            in_reasoning: self.calls_in_reasoning,
            think_blocks: self.think.as_ref().map_or(0, ThinkSplitter::blocks),
            think_unclosed: self.think.as_ref().is_some_and(ThinkSplitter::unclosed),
        }
    }

    /// usage 의 completion 쪽 입력 — 산문 + **추론** + 도구 호출 이름·인자.
    ///
    /// 추론을 넣는 이유: `<think>` 분리가 텍스트를 본문에서 추론으로 옮기므로, 추론을
    /// 빼면 같은 답변인데도 `completion_tokens` 가 뚝 떨어집니다. 도구 인자를 넣는
    /// 이유도 같습니다 — 그것도 모델이 만든 토큰이라, 빼면 도구를 쓰는 요청의 출력이
    /// 통째로 0 이 됩니다.
    pub fn completion_text(&self) -> String {
        let mut out = String::with_capacity(self.content.len() + self.reasoning.len());
        out.push_str(&self.content);
        out.push_str(&self.reasoning);
        for call in &self.calls {
            out.push_str(&call.name);
            out.push_str(&call.arguments);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashSet<String> {
        ["read", "write"].iter().map(|s| s.to_string()).collect()
    }

    fn turn() -> Turn {
        Turn::new(names(), true)
    }

    /// 전부 먹이고 끝낸 조각들.
    fn drive(t: &mut Turn, events: Vec<StreamEvent>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for ev in events {
            pieces.extend(t.push(ev).0);
        }
        pieces.extend(t.finish().0);
        pieces
    }

    fn call_of(pieces: &[Piece]) -> Vec<&ScannedCall> {
        pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Call(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// **이번 버그의 회귀 테스트.** 센티널이 추론 채널로만 와도 도구 호출이 조립되고
    /// `finish_reason` 이 `tool_calls` 여야 합니다. 예전에는 영구히 `stop` 이었습니다.
    #[test]
    fn reasoning_only_tool_call_finishes_as_tool_calls() {
        let mut t = turn();
        let pieces = drive(
            &mut t,
            vec![
                StreamEvent::Reasoning(
                    "먼저 읽자.<tool_call>{\"name\":\"read\",\"arguments\":{\"filePath\":\"a.css\"}}</tool_call>"
                        .into(),
                ),
                StreamEvent::Finish("stop".into()),
            ],
        );
        assert_eq!(t.finish_reason(), "tool_calls");
        let calls = call_of(&pieces);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].index, 0);
        // 센티널이 추론 텍스트에 남아 있으면 안 됩니다.
        assert_eq!(t.reasoning(), "먼저 읽자.");
        assert!(!t.reasoning().contains("tool_call"));
        // 원문은 로그용으로 그대로 남습니다.
        assert!(t.raw_reasoning().contains("<tool_call>"));
        assert_eq!(t.tool_stats().in_reasoning, 1);
    }

    /// 본문 안 `<think>` 에 실린 호출도 잡혀야 합니다.
    #[test]
    fn think_wrapped_tool_call_in_content_finishes_as_tool_calls() {
        let mut t = turn();
        let pieces = drive(
            &mut t,
            vec![StreamEvent::Delta(
                "<think>써야겠다<tool_call>{\"name\":\"write\",\"arguments\":{}}</tool_call></think>만들었습니다."
                    .into(),
            )],
        );
        assert_eq!(t.finish_reason(), "tool_calls");
        assert_eq!(call_of(&pieces).len(), 1);
        assert_eq!(t.content(), "만들었습니다.");
        assert_eq!(t.reasoning(), "써야겠다");
        let stats = t.tool_stats();
        assert_eq!(stats.think_blocks, 1);
        assert!(!stats.think_unclosed);
        assert_eq!(stats.in_reasoning, 1);
    }

    /// 프레임 경계가 어디로 떨어져도 호출은 정확히 한 건이어야 합니다.
    #[test]
    fn reasoning_frames_split_at_every_boundary_yield_one_call() {
        let s = "생각<tool_call>{\"name\":\"read\",\"arguments\":{\"p\":\"한글\"}}</tool_call>끝";
        for cut in 1..s.len() {
            if !s.is_char_boundary(cut) {
                continue;
            }
            let mut t = turn();
            let pieces = drive(
                &mut t,
                vec![
                    StreamEvent::Reasoning(s[..cut].to_string()),
                    StreamEvent::Reasoning(s[cut..].to_string()),
                ],
            );
            assert_eq!(call_of(&pieces).len(), 1, "cut at {cut}");
            assert_eq!(t.reasoning(), "생각끝", "cut at {cut}");
            assert_eq!(t.finish_reason(), "tool_calls", "cut at {cut}");
        }
    }

    /// 같은 텍스트가 두 채널로 나가면 답변이 두 번 보입니다.
    #[test]
    fn no_text_is_emitted_on_two_channels() {
        let mut t = turn();
        let pieces = drive(
            &mut t,
            vec![
                StreamEvent::Delta("앞<think>생각</think>뒤".into()),
                StreamEvent::Reasoning("별도 추론".into()),
            ],
        );
        let content: String = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Content(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let reasoning: String = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Reasoning(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(content, "앞뒤");
        assert_eq!(reasoning, "생각별도 추론");
        assert!(!reasoning.contains("앞"));
        assert!(!content.contains("생각"));
        // 누적기와 조각이 같은 것을 말해야 합니다.
        assert_eq!(t.content(), content);
        assert_eq!(t.reasoning(), reasoning);
    }

    /// 추론 산문은 도구가 아니어도 그대로 나가야 합니다 — 회귀 방어.
    #[test]
    fn reasoning_prose_still_reaches_the_client() {
        let mut t = turn();
        let pieces = drive(&mut t, vec![StreamEvent::Reasoning("이렇게 해 보자.".into())]);
        assert_eq!(pieces, vec![Piece::Reasoning("이렇게 해 보자.".into())]);
        assert_eq!(t.finish_reason(), "stop");
        assert_eq!(t.reasoning_field().as_deref(), Some("이렇게 해 보자."));
    }

    /// 스트림이 끊겼어도 이미 완성된 호출은 살려야 합니다. 예전에는 무조건
    /// `length` 라 뽑아 놓은 호출이 사장됐습니다.
    #[test]
    fn midstream_failure_keeps_tool_calls() {
        let mut t = turn();
        drive(
            &mut t,
            vec![
                StreamEvent::Delta("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into()),
                StreamEvent::Error("사내 처리 중 오류".into()),
            ],
        );
        assert_eq!(t.finish_reason(), "tool_calls");
        assert_eq!(t.failure(), Some("사내 처리 중 오류"));
    }

    #[test]
    fn midstream_failure_without_calls_is_length() {
        let mut t = turn();
        drive(
            &mut t,
            vec![
                StreamEvent::Delta("답변 중".into()),
                StreamEvent::Error("끊김".into()),
            ],
        );
        assert_eq!(t.finish_reason(), "length");
    }

    #[test]
    fn truncation_becomes_length_when_no_call_was_made() {
        let mut t = turn();
        drive(&mut t, vec![StreamEvent::Delta("잘린 답".into()), StreamEvent::Finish("stop".into())]);
        t.mark_truncated();
        assert_eq!(t.finish_reason(), "length");
    }

    /// 여러 프레임에 걸친 호출들의 index 가 이어져야 합니다 — 재사용하면 클라이언트가
    /// 서로 다른 두 호출을 한 호출의 조각으로 잇습니다.
    #[test]
    fn call_indices_keep_increasing_across_frames_and_channels() {
        let mut t = turn();
        let pieces = drive(
            &mut t,
            vec![
                StreamEvent::Delta("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into()),
                StreamEvent::Reasoning(
                    "<tool_call>{\"name\":\"write\",\"arguments\":{}}</tool_call>".into(),
                ),
                StreamEvent::Delta("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into()),
            ],
        );
        let calls = call_of(&pieces);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls.iter().map(|c| c.index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(t.tool_stats().in_reasoning, 1);
    }

    #[test]
    fn unterminated_call_becomes_text_at_finish_not_a_call() {
        let mut t = turn();
        let pieces = drive(&mut t, vec![StreamEvent::Delta("<tool_call>{\"name\":\"wr".into())]);
        assert!(call_of(&pieces).is_empty());
        assert!(t.content().contains("\"wr"), "미완성 블록이 사라졌습니다: {}", t.content());
        assert_eq!(t.finish_reason(), "stop");
        assert_eq!(t.tool_stats().rejected, 1);
    }

    /// 닫히지 않은 `<think>` 는 추론으로 흘리고 그 사실을 로그에 남깁니다.
    #[test]
    fn unclosed_think_is_reported() {
        let mut t = turn();
        drive(&mut t, vec![StreamEvent::Delta("답 <think>끝나지 않은".into())]);
        assert_eq!(t.content(), "답 ");
        assert_eq!(t.reasoning(), "끝나지 않은");
        assert!(t.tool_stats().think_unclosed);
    }

    #[test]
    fn without_the_splitter_think_tags_stay_in_content() {
        let mut t = Turn::new(names(), false);
        drive(&mut t, vec![StreamEvent::Delta("<think>생각</think>답".into())]);
        assert_eq!(t.content(), "<think>생각</think>답");
        assert_eq!(t.reasoning(), "");
        assert_eq!(t.tool_stats().think_blocks, 0);
    }

    /// 도구를 안 쓰는 요청 — 스캐너가 통과 모드가 되어 센티널조차 텍스트입니다.
    #[test]
    fn without_tool_names_everything_is_text() {
        let mut t = Turn::new(HashSet::new(), true);
        let body = "<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>";
        drive(&mut t, vec![StreamEvent::Delta(body.into())]);
        assert_eq!(t.content(), body);
        assert!(!t.saw_call());
        assert_eq!(t.finish_reason(), "stop");
    }

    #[test]
    fn completion_text_includes_reasoning_and_call_arguments() {
        let mut t = turn();
        drive(
            &mut t,
            vec![
                StreamEvent::Reasoning("생각한다".into()),
                StreamEvent::Delta(
                    "만들겠습니다.<tool_call>{\"name\":\"write\",\"arguments\":{\"filePath\":\"index.html\"}}</tool_call>"
                        .into(),
                ),
            ],
        );
        let text = t.completion_text();
        assert!(text.contains("만들겠습니다."), "{text}");
        // 추론을 빼면 <think> 분리가 completion_tokens 를 떨어뜨립니다.
        assert!(text.contains("생각한다"), "{text}");
        assert!(text.contains("write"), "{text}");
        assert!(text.contains("index.html"), "{text}");
    }

    /// 도구 호출만 있는 턴만 `null` 입니다. 빈 답변은 `""` 로 남아야 도구 턴과
    /// 구분됩니다.
    #[test]
    fn assistant_content_is_null_only_for_tool_only_turns() {
        let mut only_call = turn();
        drive(
            &mut only_call,
            vec![StreamEvent::Delta("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into())],
        );
        assert_eq!(only_call.assistant_content(), None);

        let mut with_prose = turn();
        drive(
            &mut with_prose,
            vec![StreamEvent::Delta(
                "만들겠습니다.<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into(),
            )],
        );
        assert_eq!(with_prose.assistant_content().as_deref(), Some("만들겠습니다."));

        // 도구가 없으면 빈 문자열도 문자열입니다.
        let mut empty = turn();
        drive(&mut empty, vec![StreamEvent::Finish("stop".into())]);
        assert_eq!(empty.assistant_content().as_deref(), Some(""));
    }

    #[test]
    fn is_empty_only_when_no_channel_produced_anything() {
        let mut nothing = turn();
        drive(&mut nothing, vec![StreamEvent::Finish("stop".into())]);
        assert!(nothing.is_empty());

        let mut reasoning_only = turn();
        drive(&mut reasoning_only, vec![StreamEvent::Reasoning("생각".into())]);
        assert!(!reasoning_only.is_empty());

        let mut call_only = turn();
        drive(
            &mut call_only,
            vec![StreamEvent::Delta("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into())],
        );
        assert!(!call_only.is_empty(), "도구 호출만 있는 턴은 빈 응답이 아닙니다");
    }

    #[test]
    fn done_and_upstream_finish_are_recorded() {
        let mut t = turn();
        drive(&mut t, vec![StreamEvent::Finish("weird".into()), StreamEvent::Done]);
        assert!(t.done());
        assert_eq!(t.upstream_finish(), Some("weird"));
        // 상위가 모르는 값을 줘도 와이어에는 열거값만 나갑니다.
        assert_eq!(t.finish_reason(), "stop");
    }

    // ── 실서버 와이어 재생 ──
    //
    // opencode 에서 "폴더 목록을 담은 md 파일을 만들어 줘" 한 번에 사내가 돌려준 두
    // 응답의 **본문 그대로**입니다(`user_id`·`filter_log_id` 만 0 으로 지웠습니다).
    // 앞이 제목 생성 호출, 뒤가 진짜 질문 — 뒤가 0자로 와서 에이전트 루프가 한 스텝
    // 만에 끝났습니다.
    //
    // 이 두 상수를 남기는 이유는 하나입니다. "우리가 프레임을 못 읽은 것" 과 "사내가
    // 아무 말도 안 한 것" 은 다음 행동이 정반대인데, 재현되지 않는 한 번짜리 응답이라
    // 다시 뜨지 않으면 영영 가릴 수 없습니다. 아래 두 시험이 그 갈래를 못박습니다.

    /// 제목 생성 호출 — 도구 없음, 짧은 프롬프트. 답이 **한 프레임에 통째로** 옵니다.
    const WIRE_TITLE: &str = concat!(
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":\"\",\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":null,\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"orchestrator_type\":null,\"references\":null,\"actions\":null,\"recommend_queries\":[],\"event_status\":\"CHUNK\",\"event_data\":\"\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":null,\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":{\"ko\":null,\"en\":null,\"policy_id\":null,\"message\":null,\"result_code\":\"FR-200\",\"filter_log_id\":null},\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"event_status\":\"REQUEST_ANALYSIS\",\"event_data\":\"{}\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":null,\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":{\"ko\":\"Default\",\"en\":\"Default\",\"policy_id\":\"49\",\"message\":\"The content was allowed by the filter\",\"result_code\":\"FR-201\",\"filter_log_id\":\"0\"},\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"event_status\":\"FINAL_ANSWER\",\"event_data\":\"{}\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":\"폴더 목록 MD 파일 생성\",\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":{\"ko\":\"Default\",\"en\":\"Default\",\"policy_id\":\"49\",\"message\":\"The content was allowed by the filter\",\"result_code\":\"FR-201\",\"filter_log_id\":\"0\"},\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"event_status\":\"CHUNK\",\"event_data\":\"\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":\"\",\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":false,\"finish_reason\":\"stop\",\"filter_block_reason\":{\"ko\":\"Default\",\"en\":\"Default\",\"policy_id\":\"49\",\"message\":\"The content was allowed by the filter\",\"result_code\":\"FR-201\",\"filter_log_id\":\"0\"},\"status\":\"SUCCESS\",\"response_code\":\"R20000\",\"plugins\":[\"LLM\"],\"references\":[],\"actions\":[],\"recommend_queries\":[],\"event_status\":\"CHUNK\",\"event_data\":\"\"}\n\n",
    );

    /// 진짜 질문 — 도구 규약이 실린 호출. 제목 호출과 **같은 프레임 열인데 답변만
    /// 통째로 빠져** 있습니다. 산문도, 추론도, 센티널도 없습니다.
    const WIRE_ANSWER: &str = concat!(
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":\"\",\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":null,\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"orchestrator_type\":null,\"references\":null,\"actions\":null,\"recommend_queries\":[],\"event_status\":\"CHUNK\",\"event_data\":\"\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":null,\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":{\"ko\":null,\"en\":null,\"policy_id\":null,\"message\":null,\"result_code\":\"FR-200\",\"filter_log_id\":null},\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"event_status\":\"REQUEST_ANALYSIS\",\"event_data\":\"{}\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":null,\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":null,\"finish_reason\":null,\"filter_block_reason\":{\"ko\":\"Default\",\"en\":\"Default\",\"policy_id\":\"49\",\"message\":\"The content was allowed by the filter\",\"result_code\":\"FR-201\",\"filter_log_id\":\"0\"},\"status\":\"SUCCESS\",\"response_code\":null,\"plugins\":[],\"event_status\":\"FINAL_ANSWER\",\"event_data\":\"{}\"}\n\n",
        "data: {\"user_id\":\"0\",\"model_type\":\"vllm_glm5.2\",\"content\":\"\",\"reasoning_content\":null,\"processing_content\":[],\"content_references\":[],\"truncated\":false,\"finish_reason\":\"stop\",\"filter_block_reason\":{\"ko\":\"Default\",\"en\":\"Default\",\"policy_id\":\"49\",\"message\":\"The content was allowed by the filter\",\"result_code\":\"FR-201\",\"filter_log_id\":\"0\"},\"status\":\"SUCCESS\",\"response_code\":\"R20000\",\"plugins\":[\"LLM\"],\"references\":[],\"actions\":[],\"recommend_queries\":[],\"event_status\":\"CHUNK\",\"event_data\":\"\"}\n\n",
    );

    /// 바이트를 실제 경로대로(디코더 → 턴) 태우고, 디코더가 못 읽은 프레임 수까지
    /// 함께 돌려줍니다 — 그 숫자가 0 이어야 "사내가 안 준 것" 이 확정됩니다.
    fn replay(wire: &str) -> (Turn, u32) {
        let mut decoder = super::super::fabrix::StreamDecoder::new();
        let mut turn = Turn::new(names(), true);
        let mut events = decoder.push(wire.as_bytes());
        events.extend(decoder.finish());
        for event in events {
            turn.feed(event);
        }
        turn.finish();
        (turn, decoder.undecodable)
    }

    #[test]
    fn replays_the_title_call_that_answered() {
        let (turn, undecodable) = replay(WIRE_TITLE);
        assert_eq!(undecodable, 0);
        assert_eq!(turn.content(), "폴더 목록 MD 파일 생성");
        assert!(!turn.received_nothing());
        assert_eq!(turn.finish_reason(), "stop");
    }

    /// 이 시험이 지키는 사실: **프록시는 프레임을 전부 읽었고, 사내가 0자를 줬습니다.**
    /// `undecodable` 이 0 이라는 것이 그 증거입니다 — 프레임을 흘렸다면 여기서 셉니다.
    #[test]
    fn replays_the_answer_call_that_came_back_empty() {
        let (turn, undecodable) = replay(WIRE_ANSWER);
        assert_eq!(undecodable, 0, "네 프레임 모두 읽혔습니다 — 흘린 프레임이 없습니다");
        assert!(turn.received_nothing(), "답변·추론 어느 채널에도 한 글자도 오지 않았습니다");
        assert!(turn.calls().is_empty(), "센티널이 없으니 걷어낼 도구 호출도 없습니다");
        assert_eq!(turn.upstream_finish(), Some("stop"));
        // 그래서 클라이언트는 `stop` 을 받습니다 — opencode 는 여기서 턴을 끝냅니다.
        assert_eq!(turn.finish_reason(), "stop");
    }

    /// `received_nothing` 은 `is_empty` 와 다른 질문에 답합니다. 도구 호출만 있는 턴은
    /// 산문이 비어도 **받은 것이 있습니다** — 둘을 같은 함수로 물으면 로그가 규약을
    /// 의심하게 만듭니다.
    #[test]
    fn received_nothing_separates_a_silent_upstream_from_a_tool_only_turn() {
        let mut call_only = turn();
        drive(
            &mut call_only,
            vec![StreamEvent::Delta(
                "<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>".into(),
            )],
        );
        assert!(!call_only.received_nothing());

        let mut silent = turn();
        drive(&mut silent, vec![StreamEvent::Finish("stop".into())]);
        assert!(silent.received_nothing());
    }
}
