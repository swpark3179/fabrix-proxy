//! 토큰 사용량. 정책을 한 곳에 모아 두어 뒤집기 쉽게 합니다.
//!
//! 사내 API 는 토큰 수를 주지 않습니다. 그래서 예전에는 `usage` 를 아예 생략했는데,
//! OpenAI 규약은 비스트림 응답에 `usage` 를 요구하고 많은 클라이언트가 그 값을
//! 읽습니다. 지금은 **문자 기반 추정치**를 채우고, 추정이라는 사실을 세 곳에서
//! 말합니다 — 응답 헤더 `x-fabrix-usage`, 로그 ③ 칸 꼬리, README.
//!
//! 정밀도를 흉내 내지 않는 것이 중요합니다. 사내 모델의 토크나이저를 모르는데
//! `tiktoken` 같은 크레이트를 끌어오면 **다른 모델의 정확한 값**이 나올 뿐이고,
//! 그게 자릿수만 맞는 근사보다 나은 것도 아니면서 트레이 앱에 BPE 데이터 파일을
//! 얹습니다.

use crate::openai::Usage;

/// 이 숫자가 어디서 왔는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 사내가 실측값을 줬습니다.
    Upstream,
    /// 프록시가 문자 수로 근사했습니다.
    Estimated,
}

impl Source {
    /// 응답 헤더 `x-fabrix-usage` 의 값 — 기존 `x-fabrix-image-stub` 과 같은 관례입니다.
    pub fn header_value(self) -> &'static str {
        match self {
            Source::Upstream => "upstream",
            Source::Estimated => "estimated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Counted {
    pub usage: Usage,
    pub source: Source,
}

/// 문자 기반 근사.
///
/// ASCII 4자 ≈ 1토큰, 그 밖(한글·CJK 등) 1자 ≈ 1토큰. 한국어는 BPE 에서 글자당
/// 1~2토큰이라 1이 아래쪽 경계에 가깝지만, 과대 추정이 예산을 잡는 클라이언트에
/// 더 나쁘게 작용하므로 보수적인 쪽을 고릅니다. 비어 있지 않으면 최소 1입니다.
pub fn approx_tokens(text: &str) -> u32 {
    let mut ascii = 0u64;
    let mut wide = 0u64;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    let tokens = ascii.div_ceil(4) + wide;
    if tokens == 0 && !text.is_empty() {
        return 1;
    }
    tokens.min(u32::MAX as u64) as u32
}

/// 정책 한 곳 — 사내가 실측을 주면 그것을, 없으면 추정을 씁니다.
///
/// 사내가 토큰 수를 주기 시작하면 이 함수가 자동으로 실측으로 넘어갑니다
/// (`FabrixChunk` 가 후보 필드들을 이미 받아 둡니다).
pub fn build(upstream: Option<(u32, u32)>, prompt: &str, completion: &str) -> Counted {
    match upstream {
        Some((prompt_tokens, completion_tokens)) => Counted {
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens.saturating_add(completion_tokens),
            },
            source: Source::Upstream,
        },
        None => {
            let prompt_tokens = approx_tokens(prompt);
            let completion_tokens = approx_tokens(completion);
            Counted {
                usage: Usage {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens: prompt_tokens.saturating_add(completion_tokens),
                },
                source: Source::Estimated,
            }
        }
    }
}

/// 로그 ③ 칸 꼬리 한 줄. 추정이면 추정이라고 **말합니다** — 예전 문구
/// "사내 응답에 토큰 수 없음" 이 있던 자리입니다.
pub fn meta_line(counted: &Counted) -> String {
    let u = &counted.usage;
    match counted.source {
        Source::Upstream => format!(
            "usage 사내 제공 · prompt {} · completion {} · total {}",
            u.prompt_tokens, u.completion_tokens, u.total_tokens
        ),
        Source::Estimated => format!(
            "usage 추정치 · prompt≈{} · completion≈{} (사내가 실측값을 주지 않아 문자 기반 근사)",
            u.prompt_tokens, u.completion_tokens
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_wide_characters_count_differently() {
        // ASCII 4자 ≈ 1토큰.
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens("abcdefgh"), 2);
        // 나머지 자릿수는 올림합니다 — 5자는 2토큰.
        assert_eq!(approx_tokens("abcde"), 2);
        // 한글 1자 ≈ 1토큰.
        assert_eq!(approx_tokens("안녕"), 2);
        // 섞이면 각각 셉니다: ASCII 4 → 1, 한글 2 → 2.
        assert_eq!(approx_tokens("abcd안녕"), 3);
    }

    #[test]
    fn empty_is_zero_but_anything_is_at_least_one() {
        assert_eq!(approx_tokens(""), 0);
        assert_eq!(approx_tokens("a"), 1);
        assert_eq!(approx_tokens(" "), 1);
        assert_eq!(approx_tokens("가"), 1);
    }

    #[test]
    fn estimates_grow_with_length() {
        let short = approx_tokens("연차 규정");
        let long = approx_tokens("연차 규정을 아주 길게 설명하는 문장입니다. 계속 이어집니다.");
        assert!(long > short, "{long} > {short}");
    }

    #[test]
    fn totals_add_up_and_source_is_estimated() {
        let c = build(None, "질문입니다", "답변입니다");
        assert_eq!(c.source, Source::Estimated);
        assert_eq!(c.usage.total_tokens, c.usage.prompt_tokens + c.usage.completion_tokens);
        assert!(c.usage.prompt_tokens > 0);
        assert!(c.usage.completion_tokens > 0);
    }

    /// 사내가 실측을 주기 시작하면 추정을 쓰지 않아야 합니다.
    #[test]
    fn upstream_counts_win_over_the_estimate() {
        let c = build(Some((812, 240)), "무시됨", "무시됨");
        assert_eq!(c.source, Source::Upstream);
        assert_eq!(c.usage.prompt_tokens, 812);
        assert_eq!(c.usage.completion_tokens, 240);
        assert_eq!(c.usage.total_tokens, 1052);
    }

    #[test]
    fn meta_line_says_estimate_out_loud() {
        let est = meta_line(&build(None, "질문", "답변"));
        assert!(est.contains("추정치"), "{est}");
        assert!(est.contains("실측값을 주지 않아"), "{est}");

        let up = meta_line(&build(Some((10, 3)), "", ""));
        assert!(up.contains("사내 제공"), "{up}");
        assert!(!up.contains("추정"), "{up}");
    }

    #[test]
    fn header_values_match_the_existing_convention() {
        assert_eq!(Source::Estimated.header_value(), "estimated");
        assert_eq!(Source::Upstream.header_value(), "upstream");
    }
}
