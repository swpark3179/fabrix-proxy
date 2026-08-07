import type { ReactNode } from 'react'

import type { FullText } from './FullTextModal'

/** 이 줄 수를 넘으면 접고 "전체보기" 로 넘깁니다. */
export const MAX_LINES = 10

/**
 * 줄 수는 적어도 한 줄이 계속 길어지는 본문(예: 줄바꿈 없는 긴 질문)이 있어,
 * 글자 수도 이 값을 넘으면 나머지를 접어 "전체보기" 로 넘깁니다.
 */
export const MAX_CHARS = 500

interface Props {
  /** 접기 대상 본문. `\n` 기준으로 줄 수를 셉니다. */
  text: string
  /** `<pre>` 에 붙는 클래스 (예: `code code--sent`). */
  className: string
  /** 팝업 제목 (예: `받은 요청`). */
  title: string
  /** 팝업 `<pre>` 톤 클래스 (예: `code--reply`). 상세 화면과 색을 맞춥니다. */
  modalClassName?: string
  /** ② 상단 헤더 줄 등 본문 앞에 항상 붙는 요소. */
  header?: ReactNode
  /** ③ 하단 메타 줄 등 본문 뒤에 항상 붙는 요소. */
  footer?: ReactNode
  /**
   * `전체보기` 옆에 함께 놓을 버튼들.
   *
   * 자기 `전체보기` 는 본문이 잘렸을 때만 뜨지만 여기 넘어온 것은 **길이와 무관하게
   * 늘 보입니다** — ③ 칸의 "사내 원문 보기" 가 짧은 답변에서 사라지면 안 됩니다.
   */
  actions?: ReactNode
  onExpand: (full: FullText) => void
}

/**
 * 상세 3칸 각각의 본문을 감쌉니다. 본문이 {@link MAX_LINES} 줄 또는 {@link MAX_CHARS}
 * 글자를 넘으면 앞부분만 보여 주고 "전체보기" 버튼으로 팝업을 엽니다. 짧으면 지금과
 * 똑같이 전부 그립니다.
 */
export function CollapsibleCode({
  text,
  className,
  title,
  modalClassName,
  header,
  footer,
  actions,
  onExpand,
}: Props) {
  const lines = text.split('\n')

  // 줄 수를 먼저 자르고(앞 MAX_LINES줄), 그래도 길면 글자 수까지 잘라 앞부분만 남깁니다.
  // "줄바꿈 없이 계속 길어지는" 본문은 줄 조건에 안 걸리므로 글자 조건이 받아 냅니다.
  const clippedByLines = lines.length > MAX_LINES
  const byLines = clippedByLines ? lines.slice(0, MAX_LINES).join('\n') : text
  const clippedByChars = byLines.length > MAX_CHARS
  const body = clippedByChars ? byLines.slice(0, MAX_CHARS) : byLines
  const clipped = clippedByLines || clippedByChars

  return (
    <>
      <pre className={className}>
        {header}
        {body}
        {clipped && (
          <span className="code__ellipsis">
            {clippedByLines
              ? `… 이하 ${lines.length - MAX_LINES}줄 숨김`
              : `… 이하 ${text.length - MAX_CHARS}자 숨김`}
          </span>
        )}
        {footer}
      </pre>
      {(clipped || actions) && (
        <div className="code__actions">
          {clipped && (
            <button
              className="btn-mini"
              onClick={() => onExpand({ title, text, className: modalClassName })}
            >
              전체보기 ({clippedByLines ? `${lines.length}줄` : `${text.length}자`})
            </button>
          )}
          {actions}
        </div>
      )}
    </>
  )
}
