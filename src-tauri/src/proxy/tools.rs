//! OpenAI 함수 호출(function calling) **에뮬레이션**.
//!
//! FabriX 요청 스키마(`MessagesRequest`)에는 도구 필드가 없습니다. 그래서 도구를
//! 그대로 넘길 수 없고, 두 방향으로 번역합니다.
//!
//! - **나갈 때** — 클라이언트가 준 `tools` 를 규약 텍스트로 접어 `systemPrompt` 뒤에 붙입니다.
//! - **들어올 때** — 모델이 뱉은 `<tool_call>{…}</tool_call>` 을 걷어내 OpenAI
//!   `tool_calls` 로 조립합니다.
//!
//! 센티널을 이 모양으로 고른 이유: Qwen 2.5/3 의 기본 chat template, Hermes,
//! vLLM 의 `hermes` 파서가 **문자 그대로 이 형식**을 씁니다. 사내 모델이 그 계열이면
//! 규약을 읽기도 전에 이미 맞는 모양을 뱉을 가능성이 높습니다. 코드 펜스 기반
//! 형식(```json)은 블록을 끝까지 버퍼링해야 도구인지 예시인지 알 수 있어 정상
//! 출력이 지연되고, 코딩 에이전트 대화에서 오탐이 잦아 쓰지 않았습니다.

use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::openai::{FunctionDef, ToolCall, ToolCallFunction, ToolChoiceMode};

pub const OPEN: &str = "<tool_call>";
pub const CLOSE: &str = "</tool_call>";

/// 닫는 태그 없이 이만큼 쌓이면 도구가 아니라고 보고 텍스트로 흘려보냅니다.
///
/// 넉넉해야 합니다 — `write` 도구 하나가 **HTML 문서 전체를 JSON 이스케이프된
/// 문자열로** `arguments` 안에 싣습니다. 64KiB 같은 값이면 평범한 페이지 생성이
/// 상한에 걸려 도구가 아니라 텍스트로 새어 나갑니다.
const MAX_CALL_BYTES: usize = 2 * 1024 * 1024;

// ─────────────────────────── 나갈 때: 규약 렌더링 ───────────────────────────

/// 도구 목록을 `systemPrompt` 뒤에 붙일 규약 블록으로 만듭니다.
///
/// 도구가 없거나 `tool_choice: "none"` 이면 `None` — 주입하지 않습니다.
///
/// 규약을 **영어로** 쓰는 이유: 감싸는 대상(도구 이름·설명·JSON Schema)이 전부
/// 영어라 한국어 프레임을 씌우면 모델이 센티널을 뱉는 대신 도구를 한국어로
/// *설명하기* 시작합니다. 형식 준수는 그 형식이 학습된 언어에서 가장 잘 나옵니다.
pub fn render_system_block(tools: &[&FunctionDef], mode: &ToolChoiceMode) -> Option<String> {
    if tools.is_empty() || *mode == ToolChoiceMode::None {
        return None;
    }

    let mut out = String::from(
        "# Tool calling\n\n\
         You can call tools. Each line inside <tools> is one tool: its name, what it does, \
         and a JSON Schema for its arguments.\n\n<tools>\n",
    );
    for f in tools {
        let entry = serde_json::json!({
            "name": f.name.trim(),
            "description": f.description.clone().unwrap_or_default(),
            "parameters": f.parameters.clone().unwrap_or(Value::Object(Default::default())),
        });
        out.push_str(&entry.to_string());
        out.push('\n');
    }
    out.push_str(
        "</tools>\n\n\
         To call a tool, emit a block in exactly this form:\n\n\
         <tool_call>\n\
         {\"name\": \"<one of the names above>\", \"arguments\": {<object matching that tool's schema>}}\n\
         </tool_call>\n\n\
         Rules:\n\
         - `arguments` MUST be a JSON object, never a string, and MUST match the schema.\n\
         - One <tool_call> block per call. To call several tools at once, emit several blocks \
         back to back with nothing between them.\n\
         - Do NOT put <tool_call> blocks inside Markdown code fences, and do not describe the \
         format in prose. Everything outside a <tool_call> block is shown to the user verbatim \
         as your answer.\n\
         - After you emit tool calls, stop and wait. Results come back on the next turn as \
         `Tool result (id=..., name=...)` entries.\n\
         - Earlier turns may already contain <tool_call> blocks carrying an extra `id` field. \
         Those are records of calls that already happened; `id` links a call to its result. \
         Do not repeat a call that already has a result.\n",
    );

    match mode {
        ToolChoiceMode::Required => out.push_str(
            "- You MUST call at least one tool. Begin your reply with a <tool_call> block \
             before any prose.\n",
        ),
        ToolChoiceMode::Function(name) => out.push_str(&format!(
            "- You MUST call the tool `{name}` and no other tool. Begin your reply with exactly \
             one <tool_call> block for `{name}`.\n"
        )),
        ToolChoiceMode::Auto => out.push_str(
            "- If no tool is needed, just answer. Emit no <tool_call> block.\n",
        ),
        ToolChoiceMode::None => unreachable!("위에서 걸렀습니다"),
    }

    Some(out)
}

/// `contents` 꼬리에 붙일 짧은 재확인. 도구가 없거나 `tool_choice: "none"` 이면 `None`.
///
/// `systemPrompt` 뒤의 규약 블록만으로는 부족합니다. FabriX 엔 롤 구조가 없어 모든
/// 것이 한 덩어리로 접히고(`fabrix::fold_messages`), 모델이 **마지막으로 읽는 글**은
/// 트랜스크립트 꼬리입니다. opencode 같은 클라이언트의 시스템 프롬프트는 수천 토큰이고
/// 도구를 "네이티브 기능" 으로 설명하므로, 우리 규약 블록은 저 앞으로 밀려납니다.
/// 프롬프트 기반 툴콜의 표준 처방이 꼬리에 형식만 다시 못박는 것입니다.
///
/// 규약 전문을 두 번 싣지 않는 이유: 도구 스키마가 크면 토큰이 두 배로 들고, 같은 글이
/// 두 번 나오면 모델이 둘째 것을 예시로 오독합니다. 여기서는 **형식만** 말합니다.
pub fn render_tail_reminder(mode: &ToolChoiceMode) -> Option<String> {
    if *mode == ToolChoiceMode::None {
        return None;
    }
    // 영어로 쓰는 이유는 `render_system_block` 과 같습니다 — 감싸는 대상이 전부
    // 영어라, 한국어 프레임을 씌우면 모델이 센티널을 뱉는 대신 설명하기 시작합니다.
    let mut out = String::from(
        "# Reminder\n\
         To use a tool, emit a block exactly like \
         <tool_call>{\"name\": \"<one of the names in <tools>>\", \"arguments\": {…}}</tool_call> \
         — one block per call, no Markdown code fence, and nothing but JSON inside the block. \
         Put the block in your reply itself, not only in your private reasoning.\n",
    );
    match mode {
        ToolChoiceMode::Required => out.push_str(
            "You must emit at least one <tool_call> block before any prose.\n",
        ),
        ToolChoiceMode::Function(name) => out.push_str(&format!(
            "You must emit exactly one <tool_call> block for `{name}` and call no other tool.\n"
        )),
        ToolChoiceMode::Auto => out.push_str("If no tool is needed, just answer.\n"),
        ToolChoiceMode::None => unreachable!("위에서 걸렀습니다"),
    }
    Some(out)
}

/// 지난 턴의 도구 호출을 트랜스크립트에 실을 문자열로 만듭니다.
///
/// 모델에게 뱉으라고 시킨 것과 **같은 형식**을 씁니다 — 자기가 낸 것과 같은 모양으로
/// 되돌아오면 이어서 규약을 지키기 쉽습니다. `id` 는 결과와 짝짓기 위해 덧붙입니다.
pub fn render_history_call(call: &ToolCall) -> String {
    let args: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or(Value::Object(Default::default()));
    let body = serde_json::json!({
        "id": call.id,
        "name": call.function.name,
        "arguments": args,
    });
    format!("{OPEN}\n{body}\n{CLOSE}")
}

// ─────────────────────────── 들어올 때: 파스아웃 ───────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ScannedCall {
    pub index: u32,
    pub id: String,
    pub name: String,
    /// OpenAI 규약대로 **JSON 문자열**.
    pub arguments: String,
}

impl From<&ScannedCall> for ToolCall {
    fn from(c: &ScannedCall) -> Self {
        ToolCall {
            id: c.id.clone(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanOut {
    /// `delta.content` 로 그대로 내보내도 안전한 텍스트.
    pub text: String,
    pub calls: Vec<ScannedCall>,
}

impl ScanOut {
    /// 두 출력을 잇습니다. 프로덕션 경로는 `Turn` 이 채널별로 누적하므로 필요 없고,
    /// 스캐너 단위 테스트가 "여러 번 밀어 넣은 결과" 를 모으는 데만 씁니다.
    #[cfg(test)]
    fn absorb(&mut self, other: ScanOut) {
        self.text.push_str(&other.text);
        self.calls.extend(other.calls);
    }
}

/// 스캐너가 훑는 줄기. 사내 모델은 답변을 두 갈래로 흘려보냅니다.
///
/// 추론 채널을 스캐너에 태우지 않던 것이 "추론 단계마다 stop" 의 원인이었습니다 —
/// 모델이 센티널을 `reasoningContent` 에 실으면 호출이 영구히 0건이고, 그러면
/// `finish_reason` 이 영구히 `stop` 이라 에이전트 루프가 한 스텝 만에 끝났습니다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `content` — 사용자에게 보이는 답변.
    Content,
    /// `reasoningContent`, 그리고 본문에서 갈라낸 `<think>` 안쪽.
    Reasoning,
}

/// 채널 하나의 버퍼. 채널마다 따로 두어야 하는 이유: 두 줄기가 섞이면 한쪽의
/// 미완성 센티널 꼬리가 다른 쪽 텍스트와 이어 붙어 엉뚱한 블록이 만들어집니다.
#[derive(Debug, Default)]
struct ChannelState {
    buf: String,
    in_call: bool,
}

/// 증분 텍스트에서 `<tool_call>` 블록을 걷어내는 상태 기계.
///
/// `StreamDecoder::absorb` 가 누적/증분 모드를 이미 정규화해 **언제나 증분 조각**을
/// 내보내므로 여기서는 상위 모드를 몰라도 됩니다.
///
/// 버퍼는 채널마다 따로지만 `next_index` 와 `rejected` 는 **하나**입니다. index 를
/// 채널별로 세면 본문의 첫 호출과 추론의 첫 호출이 둘 다 `index: 0` 을 받아,
/// 클라이언트가 서로 다른 두 호출을 한 호출의 조각으로 이어 붙입니다.
#[derive(Debug)]
pub struct ToolCallScanner {
    enabled: bool,
    names: HashSet<String>,
    content: ChannelState,
    reasoning: ChannelState,
    next_index: u32,
    /// 도구가 아니라고 판정해 텍스트로 되돌린 블록 수. 로그용. 두 채널 합계입니다.
    pub rejected: u32,
}

impl ToolCallScanner {
    pub fn new<I: IntoIterator<Item = String>>(names: I, enabled: bool) -> Self {
        let names: HashSet<String> = names
            .into_iter()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        Self {
            enabled: enabled && !names.is_empty(),
            names,
            content: ChannelState::default(),
            reasoning: ChannelState::default(),
            next_index: 0,
            rejected: 0,
        }
    }

    /// 이번 응답에서 도구 호출을 하나라도 뽑아냈는가. `finish_reason` 결정에 씁니다.
    pub fn saw_call(&self) -> bool {
        self.next_index > 0
    }

    pub fn call_count(&self) -> u32 {
        self.next_index
    }

    /// 누적 모드에서 상위가 **본문을** 통째로 다시 쓴 지점. 본문 버퍼를 버립니다.
    ///
    /// 추론 채널은 건드리지 않습니다 — 재작성은 `content` 에만 일어나므로
    /// (`StreamDecoder::absorb`), 추론 쪽 미완성 블록을 같이 버릴 이유가 없습니다.
    ///
    /// `next_index` 는 **유지**합니다 — 이미 내보낸 index 를 재사용하면 클라이언트가
    /// 서로 다른 두 호출을 같은 호출의 조각으로 이어 붙입니다.
    pub fn reset(&mut self) {
        self.content = ChannelState::default();
    }

    /// 본문 채널에 밀어 넣습니다 (`push_on(Channel::Content, …)` 의 별칭).
    pub fn push(&mut self, delta: &str) -> ScanOut {
        self.push_on(Channel::Content, delta)
    }

    pub fn push_on(&mut self, ch: Channel, delta: &str) -> ScanOut {
        if !self.enabled {
            return ScanOut { text: delta.to_string(), calls: Vec::new() };
        }
        self.state(ch).buf.push_str(delta);
        self.drain(ch, false)
    }

    /// 본문 채널 종료 (`finish_on(Channel::Content)` 의 별칭).
    pub fn finish(&mut self) -> ScanOut {
        self.finish_on(Channel::Content)
    }

    /// 채널 종료. 미완성 꼬리는 **텍스트로 흘려보냅니다** — 절대 버리지 않습니다.
    pub fn finish_on(&mut self, ch: Channel) -> ScanOut {
        if !self.enabled {
            return ScanOut::default();
        }
        self.drain(ch, true)
    }

    fn state(&mut self, ch: Channel) -> &mut ChannelState {
        match ch {
            Channel::Content => &mut self.content,
            Channel::Reasoning => &mut self.reasoning,
        }
    }

    fn drain(&mut self, ch: Channel, eof: bool) -> ScanOut {
        // `names` 를 빌리면서 `next_index`·`rejected`·버퍼를 함께 고쳐야 하므로
        // 필드를 미리 분해합니다. 이러지 않으면 대여 검사를 통과하지 못합니다.
        let Self { names, next_index, rejected, content, reasoning, .. } = self;
        let st = match ch {
            Channel::Content => content,
            Channel::Reasoning => reasoning,
        };

        let mut out = ScanOut::default();
        loop {
            if st.in_call {
                match st.buf.find(CLOSE) {
                    Some(j) => {
                        let payload = st.buf[..j].to_string();
                        st.buf.drain(..j + CLOSE.len());
                        st.in_call = false;
                        match parse_payload(&payload, names) {
                            Some(calls) if !calls.is_empty() => {
                                for (name, arguments) in calls {
                                    out.calls.push(ScannedCall {
                                        index: *next_index,
                                        id: new_call_id(),
                                        name,
                                        arguments,
                                    });
                                    *next_index += 1;
                                }
                            }
                            // 도구가 아니었다 — 원문 그대로 사용자에게 돌려줍니다.
                            // 오탐을 무해하게 만드는 것이 이 갈래의 목적입니다.
                            _ => {
                                *rejected += 1;
                                out.text.push_str(OPEN);
                                out.text.push_str(&payload);
                                out.text.push_str(CLOSE);
                            }
                        }
                        continue;
                    }
                    None => {
                        // 닫는 태그를 아직 못 봤으면 붙들고 있습니다. 단, 폭주는 막습니다.
                        if eof || st.buf.len() > MAX_CALL_BYTES {
                            *rejected += 1;
                            out.text.push_str(OPEN);
                            out.text.push_str(&st.buf);
                            st.buf.clear();
                            st.in_call = false;
                        }
                        break;
                    }
                }
            }

            match st.buf.find(OPEN) {
                Some(i) => {
                    out.text.push_str(&st.buf[..i]);
                    st.buf.drain(..i + OPEN.len());
                    st.in_call = true;
                }
                None => {
                    // 여는 태그의 **부분 접두**일 수 있는 꼬리만 붙들어 둡니다(≤10바이트).
                    // 그래서 평범한 텍스트는 지연 없이 흘러갑니다.
                    let keep = if eof { 0 } else { partial_tag_len(&st.buf, OPEN) };
                    let cut = st.buf.len() - keep;
                    out.text.push_str(&st.buf[..cut]);
                    st.buf.drain(..cut);
                    break;
                }
            }
        }
        out
    }
}

// ─────────────────────────── 본문 안의 추론 태그 ───────────────────────────

/// 인식하는 추론 태그 짝.
///
/// 사내 모델이 추론을 별도 필드(`reasoningContent`)로 줄지, 본문에 태그로 섞어 줄지
/// 확인되지 않아 양쪽을 모두 견디게 합니다. 목록을 넓히면 오탐이 늘어 평범한 텍스트가
/// 추론으로 옮겨가므로 실제로 쓰이는 두 가지만 답니다.
const THINK_TAGS: &[(&str, &str)] = &[("<think>", "</think>"), ("<thinking>", "</thinking>")];

/// 한 번의 분리 결과. **순서를 보존**합니다 — `답변<think>생각` 에서 생각을 앞으로
/// 끌어오면 클라이언트가 보는 순서가 뒤집힙니다.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Split {
    pub parts: Vec<(Channel, String)>,
}

impl Split {
    /// 분리기를 거치지 않는 경로용 — 통째로 본문입니다.
    pub fn content(text: String) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        Self { parts: vec![(Channel::Content, text)] }
    }

    fn push(&mut self, ch: Channel, text: &str) {
        if text.is_empty() {
            return;
        }
        // 같은 채널이 연달아 나오면 한 조각으로 합칩니다 — 청크 수를 늘릴 이유가 없습니다.
        if let Some((last_ch, last)) = self.parts.last_mut() {
            if *last_ch == ch {
                last.push_str(text);
                return;
            }
        }
        self.parts.push((ch, text.to_string()));
    }
}

/// 본문에 섞여 오는 `<think>…</think>` 를 추론 채널로 갈라내는 상태 기계.
///
/// 프레임 경계에 태그가 걸려도 안전합니다 — `partial_tag_len` 이 부분 접두만 붙듭니다.
/// 추론 블록 **안에서도** 닫는 태그의 부분 접두만 붙들고 나머지는 흘려보내므로,
/// 긴 사고 과정이 통째로 버퍼링되어 늦게 나가는 일은 없습니다.
#[derive(Debug, Default)]
pub struct ThinkSplitter {
    buf: String,
    /// 열려 있는 태그 짝의 번호. `None` 이면 본문 안입니다.
    inside: Option<usize>,
    blocks: u32,
    unclosed: bool,
}

impl ThinkSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, delta: &str) -> Split {
        self.buf.push_str(delta);
        self.drain(false)
    }

    /// 스트림 종료. 붙들고 있던 것은 모두 흘려보냅니다 — 절대 버리지 않습니다.
    pub fn finish(&mut self) -> Split {
        self.drain(true)
    }

    /// 누적 모드에서 본문이 통째로 다시 쓰인 지점.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.inside = None;
    }

    /// 갈라낸 추론 블록 수 — 로그용.
    pub fn blocks(&self) -> u32 {
        self.blocks
    }

    /// 여는 태그를 봤는데 닫는 태그 없이 끝났는가 — 로그용.
    pub fn unclosed(&self) -> bool {
        self.unclosed
    }

    fn drain(&mut self, eof: bool) -> Split {
        let mut out = Split::default();
        loop {
            if let Some(i) = self.inside {
                let close = THINK_TAGS[i].1;
                match self.buf.find(close) {
                    Some(j) => {
                        let thought = self.buf[..j].to_string();
                        out.push(Channel::Reasoning, &thought);
                        self.buf.drain(..j + close.len());
                        self.inside = None;
                        self.blocks += 1;
                        continue;
                    }
                    None => {
                        if eof {
                            // 여는 태그를 봤으면 추론이라는 신호가 충분히 강합니다 —
                            // 본문으로 되돌리지 않고 추론으로 흘립니다.
                            let tail = std::mem::take(&mut self.buf);
                            out.push(Channel::Reasoning, &tail);
                            self.inside = None;
                            self.unclosed = true;
                        } else {
                            let keep = partial_tag_len(&self.buf, close);
                            let cut = self.buf.len() - keep;
                            let ready = self.buf[..cut].to_string();
                            out.push(Channel::Reasoning, &ready);
                            self.buf.drain(..cut);
                        }
                        break;
                    }
                }
            }

            match earliest_think_open(&self.buf) {
                Some((i, at)) => {
                    let before = self.buf[..at].to_string();
                    out.push(Channel::Content, &before);
                    self.buf.drain(..at + THINK_TAGS[i].0.len());
                    self.inside = Some(i);
                }
                None => {
                    // 어떤 여는 태그의 부분 접두든 될 수 있는 만큼만 붙듭니다.
                    let keep = if eof {
                        0
                    } else {
                        THINK_TAGS
                            .iter()
                            .map(|(open, _)| partial_tag_len(&self.buf, open))
                            .max()
                            .unwrap_or(0)
                    };
                    let cut = self.buf.len() - keep;
                    let ready = self.buf[..cut].to_string();
                    out.push(Channel::Content, &ready);
                    self.buf.drain(..cut);
                    break;
                }
            }
        }
        out
    }
}

/// 가장 먼저 나오는 여는 태그의 `(태그 번호, 위치)`.
///
/// 위치가 같으면 **긴 태그**가 이깁니다 — `<thinking>` 은 `<think` 로 시작하지 않지만
/// (`<think>` 는 `>` 로 닫힙니다) 목록이 늘어날 때를 대비해 규칙을 못박아 둡니다.
fn earliest_think_open(buf: &str) -> Option<(usize, usize)> {
    THINK_TAGS
        .iter()
        .enumerate()
        .filter_map(|(i, (open, _))| buf.find(open).map(|at| (i, at)))
        .min_by_key(|(i, at)| (*at, std::cmp::Reverse(THINK_TAGS[*i].0.len())))
}

/// `buf` 의 꼬리가 `tag` 의 접두사이면 그 길이.
///
/// 여는 태그가 프레임 경계에서 반쪽만 왔을 때 **그만큼만** 붙들어 두기 위한 것입니다.
/// `tag` 가 순수 ASCII 라 자르는 지점은 언제나 char 경계입니다 (ASCII 바이트는
/// UTF-8 연속 바이트가 될 수 없습니다).
fn partial_tag_len(buf: &str, tag: &str) -> usize {
    let max = (tag.len() - 1).min(buf.len());
    let bytes = buf.as_bytes();
    for k in (1..=max).rev() {
        if bytes[buf.len() - k..] == tag.as_bytes()[..k] {
            return k;
        }
    }
    0
}

fn new_call_id() -> String {
    let hex = Uuid::new_v4().simple().to_string();
    format!("call_{}", &hex[..24])
}

/// 블록 안의 JSON 을 `(이름, arguments JSON 문자열)` 목록으로.
///
/// 도구가 아니라고 판단하면 `None` — 호출부가 원문 텍스트로 되돌립니다. 판단 기준에서
/// 가장 중요한 것은 **이름이 이번 라운드에 선언된 도구 집합에 있는가** 입니다. 이
/// 한 줄이 오탐 방어의 핵심입니다.
fn parse_payload(payload: &str, names: &HashSet<String>) -> Option<Vec<(String, String)>> {
    let value: Value = serde_json::from_str(payload.trim()).ok()?;
    let items: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };

    let mut out = Vec::new();
    for item in items {
        let obj = item.as_object()?;
        let name = obj
            .get("name")
            .or_else(|| obj.get("function"))
            .and_then(Value::as_str)?
            .trim();
        if !names.contains(name) {
            return None;
        }
        let raw = obj
            .get("arguments")
            .or_else(|| obj.get("parameters"))
            .or_else(|| obj.get("args"));
        let arguments = match raw {
            None | Some(Value::Null) => "{}".to_string(),
            Some(Value::String(s)) => {
                // 이중 인코딩된 인자는 받아 주되, JSON 이 아닌 순수 문자열은 거절합니다
                // (도구 호출이 아니라 그 형식을 설명하는 산문일 가능성이 높습니다).
                let parsed: Value = serde_json::from_str(s).ok()?;
                parsed.to_string()
            }
            Some(v) => v.to_string(),
        };
        out.push((name.to_string(), arguments));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashSet<String> {
        ["read", "write"].iter().map(|s| s.to_string()).collect()
    }

    fn scanner() -> ToolCallScanner {
        ToolCallScanner::new(names(), true)
    }

    /// 전부 밀어 넣고 끝낸 결과.
    fn scan_all(sc: &mut ToolCallScanner, s: &str) -> ScanOut {
        let mut out = sc.push(s);
        out.absorb(sc.finish());
        out
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        let out = scan_all(&mut scanner(), "안녕하세요. 그냥 답변입니다.");
        assert_eq!(out.text, "안녕하세요. 그냥 답변입니다.");
        assert!(out.calls.is_empty());
    }

    #[test]
    fn single_call_is_extracted_and_stripped_from_text() {
        let out = scan_all(
            &mut scanner(),
            "만들겠습니다.\n<tool_call>\n{\"name\":\"write\",\"arguments\":{\"filePath\":\"a.html\"}}\n</tool_call>",
        );
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].name, "write");
        assert_eq!(out.calls[0].index, 0);
        assert_eq!(out.calls[0].arguments, r#"{"filePath":"a.html"}"#);
        assert!(out.calls[0].id.starts_with("call_"));
        assert!(!out.text.contains("tool_call"), "센티널이 텍스트에 남았습니다: {}", out.text);
        assert!(out.text.contains("만들겠습니다."));
    }

    #[test]
    fn partial_sentinel_is_held_back_then_released_at_finish() {
        let mut sc = scanner();
        let a = sc.push("hi <t");
        assert_eq!(a.text, "hi ", "부분 접두는 보류되어야 합니다");
        let b = sc.finish();
        assert_eq!(b.text, "<t", "보류분은 종료 시 반드시 흘러나와야 합니다");
    }

    #[test]
    fn partial_sentinel_completed_by_the_next_frame() {
        let mut sc = scanner();
        let a = sc.push("hi <t");
        let b = sc.push("ool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>");
        let c = sc.finish();
        assert_eq!(a.text, "hi ");
        assert_eq!(b.text, "");
        assert_eq!(c.text, "");
        assert_eq!(b.calls.len(), 1);
        assert_eq!(b.calls[0].name, "read");
    }

    #[test]
    fn ordinary_angle_bracket_text_is_not_held_forever() {
        let out = scan_all(&mut scanner(), "a < b and c <tool d");
        assert_eq!(out.text, "a < b and c <tool d");
        assert!(out.calls.is_empty());
    }

    /// 프레임 경계가 어디로 떨어져도 결과가 같아야 합니다. 스트리밍 파서가
    /// 깨지는 곳은 거의 언제나 여기입니다.
    #[test]
    fn split_at_every_char_boundary_is_stable() {
        let s = "앞 텍스트 <tool_call>{\"name\":\"write\",\"arguments\":{\"p\":\"한글 값\"}}</tool_call> 뒤 텍스트";
        let expect = scan_all(&mut scanner(), s);
        assert_eq!(expect.calls.len(), 1);

        for cut in 1..s.len() {
            if !s.is_char_boundary(cut) {
                continue;
            }
            let mut sc = scanner();
            let mut got = sc.push(&s[..cut]);
            got.absorb(sc.push(&s[cut..]));
            got.absorb(sc.finish());
            assert_eq!(got.text, expect.text, "cut at {cut}");
            assert_eq!(got.calls.len(), expect.calls.len(), "cut at {cut}");
            assert_eq!(got.calls[0].name, expect.calls[0].name, "cut at {cut}");
            assert_eq!(got.calls[0].arguments, expect.calls[0].arguments, "cut at {cut}");
            assert_eq!(got.calls[0].index, 0, "cut at {cut}");
        }
    }

    #[test]
    fn one_byte_at_a_time_is_stable() {
        let s = "x<tool_call>{\"name\":\"read\",\"arguments\":{\"filePath\":\"한.css\"}}</tool_call>y";
        let expect = scan_all(&mut scanner(), s);

        let mut sc = scanner();
        let mut got = ScanOut::default();
        let mut start = 0;
        for (i, _) in s.char_indices().skip(1) {
            got.absorb(sc.push(&s[start..i]));
            start = i;
        }
        got.absorb(sc.push(&s[start..]));
        got.absorb(sc.finish());

        assert_eq!(got.text, expect.text);
        assert_eq!(got.calls.len(), 1);
        assert_eq!(got.calls[0].arguments, expect.calls[0].arguments);
    }

    #[test]
    fn parallel_blocks_get_sequential_indices() {
        let out = scan_all(
            &mut scanner(),
            "<tool_call>{\"name\":\"write\",\"arguments\":{\"a\":1}}</tool_call>\
             <tool_call>{\"name\":\"read\",\"arguments\":{\"b\":2}}</tool_call>",
        );
        assert_eq!(out.calls.len(), 2);
        assert_eq!(out.calls[0].index, 0);
        assert_eq!(out.calls[1].index, 1);
        assert_eq!(out.calls[1].name, "read");
        assert_ne!(out.calls[0].id, out.calls[1].id);
    }

    #[test]
    fn json_array_inside_one_block_yields_two_calls() {
        let out = scan_all(
            &mut scanner(),
            "<tool_call>[{\"name\":\"read\",\"arguments\":{}},{\"name\":\"write\",\"arguments\":{}}]</tool_call>",
        );
        assert_eq!(out.calls.len(), 2);
        assert_eq!(out.calls[0].index, 0);
        assert_eq!(out.calls[1].index, 1);
    }

    #[test]
    fn unknown_tool_name_falls_back_to_literal_text() {
        let out = scan_all(
            &mut scanner(),
            "<tool_call>{\"name\":\"rm -rf /\",\"arguments\":{}}</tool_call>",
        );
        assert!(out.calls.is_empty());
        assert!(out.text.contains("rm -rf /"));
        assert!(out.text.starts_with(OPEN));
        assert!(out.text.ends_with(CLOSE));
    }

    #[test]
    fn malformed_json_falls_back_to_literal_text() {
        let out = scan_all(&mut scanner(), "<tool_call>{\"name\":\"write\", oops}</tool_call>");
        assert!(out.calls.is_empty());
        assert!(out.text.contains("oops"));
    }

    #[test]
    fn non_json_arguments_string_is_rejected() {
        let out = scan_all(
            &mut scanner(),
            "<tool_call>{\"name\":\"write\",\"arguments\":\"just prose\"}</tool_call>",
        );
        assert!(out.calls.is_empty());
        assert!(out.text.contains("just prose"));
    }

    #[test]
    fn double_encoded_arguments_are_accepted() {
        let out = scan_all(
            &mut scanner(),
            "<tool_call>{\"name\":\"write\",\"arguments\":\"{\\\"a\\\":1}\"}</tool_call>",
        );
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].arguments, r#"{"a":1}"#);
    }

    #[test]
    fn missing_arguments_becomes_empty_object() {
        let out = scan_all(&mut scanner(), "<tool_call>{\"name\":\"read\"}</tool_call>");
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].arguments, "{}");
    }

    #[test]
    fn text_before_and_after_a_call_is_forwarded() {
        let out = scan_all(
            &mut scanner(),
            "먼저 읽습니다.<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>그리고 설명을 잇습니다.",
        );
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.text, "먼저 읽습니다.그리고 설명을 잇습니다.");
    }

    #[test]
    fn unterminated_block_flushes_as_text_on_finish() {
        let mut sc = scanner();
        let a = sc.push("<tool_call>{\"name\":\"write\",\"argu");
        let b = sc.finish();
        assert!(a.text.is_empty());
        assert!(b.calls.is_empty());
        assert!(b.text.contains("\"argu"), "미완성 블록이 사라졌습니다: {:?}", b.text);
        assert!(b.text.starts_with(OPEN));
        assert_eq!(sc.rejected, 1);
    }

    #[test]
    fn html_sized_arguments_survive() {
        // write 도구는 HTML 문서 전체를 arguments 안에 싣습니다. 상한이 낮으면
        // 정상 페이지 생성이 도구가 아니라 텍스트로 새어 나갑니다.
        let big = "x".repeat(300_000);
        let payload = serde_json::json!({
            "name": "write",
            "arguments": {"filePath": "index.html", "content": big},
        })
        .to_string();
        let out = scan_all(&mut scanner(), &format!("{OPEN}{payload}{CLOSE}"));
        assert_eq!(out.calls.len(), 1, "300KB 인자가 거절됐습니다");
        assert!(out.calls[0].arguments.len() > 300_000);
        assert!(out.text.is_empty());
    }

    /// 이름 집합이 비면 스캐너는 스스로 통과 모드가 됩니다 — 도구를 안 쓰는 요청에
    /// 별도 분기를 두지 않아도 되는 것이 이 성질의 존재 이유입니다.
    #[test]
    fn scanner_without_names_is_a_passthrough() {
        let mut sc = ToolCallScanner::new(Vec::new(), true);
        let body = "<tool_call>{\"name\":\"write\",\"arguments\":{}}</tool_call>";
        assert_eq!(sc.push(body).text, body);
        assert_eq!(sc.push_on(Channel::Reasoning, body).text, body);
        assert!(!sc.saw_call());
        assert_eq!(sc.finish(), ScanOut::default());
        assert_eq!(sc.finish_on(Channel::Reasoning), ScanOut::default());
    }

    // ── 채널 두 갈래 ──

    /// 이번 수정의 핵심 — 추론 채널로만 온 호출도 잡혀야 합니다. 여기가 뚫려 있어서
    /// `finish_reason` 이 영구히 `stop` 이었습니다.
    #[test]
    fn tool_call_only_in_reasoning_is_extracted() {
        let mut sc = scanner();
        let out = sc.push_on(
            Channel::Reasoning,
            "생각 중.<tool_call>{\"name\":\"read\",\"arguments\":{\"filePath\":\"a.css\"}}</tool_call>",
        );
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].name, "read");
        assert_eq!(out.text, "생각 중.");
        assert!(sc.saw_call());
    }

    /// index 는 채널을 가로질러 하나의 수열이어야 합니다. 채널별로 세면 둘 다 0 을
    /// 받아 클라이언트가 서로 다른 두 호출을 하나로 잇습니다.
    #[test]
    fn channels_share_one_call_index_space() {
        let mut sc = scanner();
        let a = sc.push("<tool_call>{\"name\":\"write\",\"arguments\":{}}</tool_call>");
        let b = sc.push_on(
            Channel::Reasoning,
            "<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>",
        );
        assert_eq!(a.calls[0].index, 0);
        assert_eq!(b.calls[0].index, 1);
        assert_ne!(a.calls[0].id, b.calls[0].id);
        assert_eq!(sc.call_count(), 2);
    }

    /// 한쪽 채널이 붙들고 있는 부분 접두가 다른 채널 텍스트에 이어 붙으면 엉뚱한
    /// 블록이 만들어집니다. 버퍼를 채널마다 따로 두는 이유가 이것입니다.
    #[test]
    fn channels_do_not_leak_each_others_buffers() {
        let mut sc = scanner();
        assert_eq!(sc.push("hi <t").text, "hi ");
        // 추론 쪽 텍스트에 본문이 붙들고 있던 `<t` 가 섞이면 안 됩니다.
        assert_eq!(sc.push_on(Channel::Reasoning, "생각").text, "생각");
        assert_eq!(sc.finish_on(Channel::Reasoning).text, "");
        assert_eq!(sc.finish().text, "<t");
    }

    #[test]
    fn reasoning_prose_passes_through_untouched() {
        let mut sc = scanner();
        let out = sc.push_on(Channel::Reasoning, "이 파일을 먼저 읽어야겠다. a < b 인지 확인.");
        assert_eq!(out.text, "이 파일을 먼저 읽어야겠다. a < b 인지 확인.");
        assert!(out.calls.is_empty());
        assert!(!sc.saw_call());
    }

    /// 추론 채널의 거절된 블록도 **추론 텍스트로** 되돌아와야 합니다 — 본문으로
    /// 새어 나가면 답변에 없던 글이 생깁니다.
    #[test]
    fn rejected_block_on_the_reasoning_channel_returns_as_reasoning_text() {
        let mut sc = scanner();
        let mut out = sc.push_on(
            Channel::Reasoning,
            "<tool_call>{\"name\":\"definitely_not_a_tool\",\"arguments\":{}}</tool_call>",
        );
        out.absorb(sc.finish_on(Channel::Reasoning));
        assert!(out.calls.is_empty());
        assert!(out.text.contains("definitely_not_a_tool"));
        assert_eq!(sc.rejected, 1);
    }

    /// 프레임 경계는 파서가 깨지는 자리입니다. 본문에서 이미 검증한 불변식을
    /// 추론 채널에서도 확인합니다.
    #[test]
    fn split_at_every_char_boundary_is_stable_on_the_reasoning_channel() {
        let s = "앞 <tool_call>{\"name\":\"write\",\"arguments\":{\"p\":\"한글\"}}</tool_call> 뒤";
        for cut in 1..s.len() {
            if !s.is_char_boundary(cut) {
                continue;
            }
            let mut sc = scanner();
            let mut got = sc.push_on(Channel::Reasoning, &s[..cut]);
            got.absorb(sc.push_on(Channel::Reasoning, &s[cut..]));
            got.absorb(sc.finish_on(Channel::Reasoning));
            assert_eq!(got.text, "앞  뒤", "cut at {cut}");
            assert_eq!(got.calls.len(), 1, "cut at {cut}");
            assert_eq!(got.calls[0].name, "write", "cut at {cut}");
            assert_eq!(got.calls[0].index, 0, "cut at {cut}");
        }
    }

    /// 본문 재작성(`StreamEvent::Reset`)은 추론 채널을 건드리면 안 됩니다 —
    /// 재작성은 `content` 에만 일어납니다.
    #[test]
    fn reset_spares_the_reasoning_channel() {
        let mut sc = scanner();
        sc.push_on(Channel::Reasoning, "<tool_call>{\"name\":\"read\",\"argu");
        sc.push("본문 <tool_call>{\"name\":\"wr");
        sc.reset();
        // 추론 쪽 미완성 블록은 살아 있어야 하고, 이어지는 조각으로 완성됩니다.
        let out = sc.push_on(Channel::Reasoning, "ments\":{}}</tool_call>");
        assert_eq!(out.calls.len(), 1, "리셋이 추론 버퍼를 지웠습니다");
        assert_eq!(out.calls[0].name, "read");
        // 본문 쪽은 버려졌습니다.
        assert!(!sc.finish().text.contains("wr"));
    }

    #[test]
    fn reset_clears_the_buffer_but_keeps_the_index() {
        let mut sc = scanner();
        let first = sc.push("<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call>tail");
        assert_eq!(first.calls[0].index, 0);
        sc.push("<tool_call>{\"name\":\"wr");
        sc.reset();
        let after = scan_all(&mut sc, "<tool_call>{\"name\":\"write\",\"arguments\":{}}</tool_call>");
        assert_eq!(after.calls.len(), 1);
        // 이미 나간 index 0 을 재사용하면 클라이언트가 두 호출을 하나로 잇습니다.
        assert_eq!(after.calls[0].index, 1);
        assert!(!after.text.contains("wr"), "리셋된 버퍼가 새어 나왔습니다");
    }

    /// 한 덩어리로 밀어 넣든 조각으로 밀어 넣든 같은 결과여야 합니다. 예전에는
    /// 비스트림용 `parse_all` 이 따로 있어 이 동등성을 명시적으로 확인해야 했는데,
    /// 이제 두 경로가 같은 `Turn` 을 지나므로 스캐너 단위에서만 확인합니다.
    #[test]
    fn chunked_and_whole_input_agree() {
        let s = "a<tool_call>{\"name\":\"read\",\"arguments\":{\"p\":1}}</tool_call>b";
        let whole = scan_all(&mut scanner(), s);

        let mut sc = scanner();
        let mut chunked = ScanOut::default();
        for ch in s.chars() {
            chunked.absorb(sc.push(&ch.to_string()));
        }
        chunked.absorb(sc.finish());

        assert_eq!(whole.text, chunked.text);
        assert_eq!(whole.calls.len(), chunked.calls.len());
        assert_eq!(whole.calls[0].name, chunked.calls[0].name);
        assert_eq!(whole.calls[0].arguments, chunked.calls[0].arguments);
    }

    // ── 본문 안의 <think> 분리 ──

    /// 전부 밀어 넣고 끝낸 분리 결과.
    fn split_all(sp: &mut ThinkSplitter, s: &str) -> Vec<(Channel, String)> {
        let mut parts = sp.push(s).parts;
        parts.extend(sp.finish().parts);
        parts
    }

    fn joined(parts: &[(Channel, String)], want: Channel) -> String {
        parts
            .iter()
            .filter(|(ch, _)| *ch == want)
            .map(|(_, t)| t.as_str())
            .collect()
    }

    #[test]
    fn plain_content_is_untouched_by_the_splitter() {
        let parts = split_all(&mut ThinkSplitter::new(), "그냥 답변입니다. a < b.");
        assert_eq!(joined(&parts, Channel::Content), "그냥 답변입니다. a < b.");
        assert_eq!(joined(&parts, Channel::Reasoning), "");
    }

    #[test]
    fn think_block_moves_to_the_reasoning_channel() {
        let parts = split_all(
            &mut ThinkSplitter::new(),
            "<think>먼저 읽어야겠다</think>읽어 보겠습니다.",
        );
        assert_eq!(joined(&parts, Channel::Reasoning), "먼저 읽어야겠다");
        assert_eq!(joined(&parts, Channel::Content), "읽어 보겠습니다.");
    }

    #[test]
    fn thinking_variant_is_recognized() {
        let parts = split_all(&mut ThinkSplitter::new(), "<thinking>고민</thinking>답");
        assert_eq!(joined(&parts, Channel::Reasoning), "고민");
        assert_eq!(joined(&parts, Channel::Content), "답");
    }

    /// 순서가 뒤집히면 클라이언트가 보는 답변 순서가 달라집니다.
    #[test]
    fn order_is_preserved_when_content_precedes_think() {
        let parts = split_all(&mut ThinkSplitter::new(), "앞<think>중</think>뒤");
        assert_eq!(
            parts,
            vec![
                (Channel::Content, "앞".to_string()),
                (Channel::Reasoning, "중".to_string()),
                (Channel::Content, "뒤".to_string()),
            ]
        );
    }

    /// 프레임 경계가 어디로 떨어져도 같은 분리 결과여야 합니다. 스트리밍 파서가
    /// 깨지는 곳은 거의 언제나 여기입니다.
    #[test]
    fn think_split_at_every_char_boundary_is_stable() {
        let s = "앞<think>한글 생각</think>뒤 텍스트";
        for cut in 1..s.len() {
            if !s.is_char_boundary(cut) {
                continue;
            }
            let mut sp = ThinkSplitter::new();
            let mut parts = sp.push(&s[..cut]).parts;
            parts.extend(sp.push(&s[cut..]).parts);
            parts.extend(sp.finish().parts);
            assert_eq!(joined(&parts, Channel::Reasoning), "한글 생각", "cut at {cut}");
            assert_eq!(joined(&parts, Channel::Content), "앞뒤 텍스트", "cut at {cut}");
            assert!(!sp.unclosed(), "cut at {cut}");
            assert_eq!(sp.blocks(), 1, "cut at {cut}");
        }
    }

    /// 긴 사고 과정이 통째로 버퍼링되면 안 됩니다 — 닫는 태그의 부분 접두만 붙듭니다.
    #[test]
    fn reasoning_streams_out_without_waiting_for_the_close_tag() {
        let mut sp = ThinkSplitter::new();
        // 닫는 태그를 보기 전에 이미 나옵니다.
        assert_eq!(joined(&sp.push("<think>생각을 ").parts, Channel::Reasoning), "생각을 ");
        assert_eq!(joined(&sp.push("이어서 합니다").parts, Channel::Reasoning), "이어서 합니다");
    }

    #[test]
    fn unterminated_think_flushes_as_reasoning_at_eof() {
        let mut sp = ThinkSplitter::new();
        let parts = split_all(&mut sp, "답변 <think>끝나지 않은 생각");
        assert_eq!(joined(&parts, Channel::Content), "답변 ");
        assert_eq!(joined(&parts, Channel::Reasoning), "끝나지 않은 생각");
        assert!(sp.unclosed());
    }

    /// 여는 태그의 부분 접두로 끝난 본문은 **본문으로** 되돌아와야 합니다 —
    /// 태그처럼 보였을 뿐 실제로는 답변 텍스트입니다.
    #[test]
    fn partial_think_prefix_is_released_as_content_at_eof() {
        let mut sp = ThinkSplitter::new();
        assert_eq!(joined(&sp.push("답변 <thi").parts, Channel::Content), "답변 ");
        let parts = sp.finish().parts;
        assert_eq!(joined(&parts, Channel::Content), "<thi");
        assert_eq!(joined(&parts, Channel::Reasoning), "");
    }

    #[test]
    fn close_without_open_stays_literal_content() {
        let parts = split_all(&mut ThinkSplitter::new(), "답변</think>계속");
        assert_eq!(joined(&parts, Channel::Content), "답변</think>계속");
        assert_eq!(joined(&parts, Channel::Reasoning), "");
    }

    #[test]
    fn two_think_blocks_are_counted() {
        let mut sp = ThinkSplitter::new();
        let parts = split_all(&mut sp, "<think>가</think>글<think>나</think>");
        assert_eq!(joined(&parts, Channel::Reasoning), "가나");
        assert_eq!(joined(&parts, Channel::Content), "글");
        assert_eq!(sp.blocks(), 2);
    }

    /// 분리기와 스캐너를 이어 붙이면 `<think>` 안의 도구 호출도 잡힙니다 —
    /// 이것이 두 조각을 이 순서로 합치는 이유입니다.
    #[test]
    fn tool_call_inside_a_think_block_is_found() {
        let mut sp = ThinkSplitter::new();
        let mut sc = scanner();
        let body = "<think>이걸 써야지<tool_call>{\"name\":\"read\",\"arguments\":{}}</tool_call></think>확인했습니다.";

        let mut calls = Vec::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        for (ch, seg) in split_all(&mut sp, body) {
            let out = sc.push_on(ch, &seg);
            match ch {
                Channel::Content => content.push_str(&out.text),
                Channel::Reasoning => reasoning.push_str(&out.text),
            }
            calls.extend(out.calls);
        }
        content.push_str(&sc.finish_on(Channel::Content).text);
        reasoning.push_str(&sc.finish_on(Channel::Reasoning).text);

        assert_eq!(calls.len(), 1, "think 안의 호출을 놓쳤습니다");
        assert_eq!(calls[0].name, "read");
        assert_eq!(content, "확인했습니다.");
        assert_eq!(reasoning, "이걸 써야지");
    }

    #[test]
    fn partial_tag_len_holds_only_a_real_prefix() {
        assert_eq!(partial_tag_len("hi <t", OPEN), 2);
        assert_eq!(partial_tag_len("hi <think", THINK_TAGS[0].0), 6);
        assert_eq!(partial_tag_len("hi there", OPEN), 0);
        // 완성된 태그는 부분 접두가 아닙니다 — 호출부가 find 로 먼저 잡습니다.
        assert_eq!(partial_tag_len("<tool_call>", OPEN), 0);
    }

    // ── 규약 렌더링 ──

    fn def(name: &str) -> FunctionDef {
        FunctionDef {
            name: name.into(),
            description: Some(format!("does {name}")),
            parameters: Some(serde_json::json!({"type":"object"})),
            strict: None,
        }
    }

    #[test]
    fn system_block_lists_tools_and_the_sentinel() {
        let (a, b) = (def("read"), def("write"));
        let block = render_system_block(&[&a, &b], &ToolChoiceMode::Auto).unwrap();
        assert!(block.contains("\"name\":\"read\""));
        assert!(block.contains("\"name\":\"write\""));
        assert!(block.contains(OPEN));
        assert!(block.contains(CLOSE));
        assert!(block.contains("If no tool is needed"));
    }

    #[test]
    fn system_block_honors_tool_choice() {
        let a = def("write");
        assert!(render_system_block(&[&a], &ToolChoiceMode::None).is_none());
        assert!(render_system_block(&[], &ToolChoiceMode::Auto).is_none());

        let required = render_system_block(&[&a], &ToolChoiceMode::Required).unwrap();
        assert!(required.contains("MUST call at least one tool"));

        let named =
            render_system_block(&[&a], &ToolChoiceMode::Function("write".into())).unwrap();
        assert!(named.contains("MUST call the tool `write`"));
    }

    /// 히스토리 렌더러와 스캐너가 어긋나면 멀티라운드 루프가 조용히 깨집니다.
    #[test]
    fn history_call_round_trips_through_the_scanner() {
        let call = ToolCall {
            id: "call_a1".into(),
            kind: "function".into(),
            function: ToolCallFunction {
                name: "write".into(),
                arguments: r#"{"filePath":"a.html","content":"<p>안녕</p>"}"#.into(),
            },
        };
        let rendered = render_history_call(&call);
        assert!(rendered.contains("call_a1"));

        let out = scan_all(&mut scanner(), &rendered);
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].name, "write");
        let args: Value = serde_json::from_str(&out.calls[0].arguments).unwrap();
        assert_eq!(args["filePath"], "a.html");
        assert_eq!(args["content"], "<p>안녕</p>");
    }

    #[test]
    fn history_call_survives_unparsable_arguments() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: ToolCallFunction { name: "read".into(), arguments: "not json".into() },
        };
        let rendered = render_history_call(&call);
        assert!(rendered.contains("\"arguments\":{}"));
    }
}
