import type { ReactNode } from 'react'

import type { FullText } from './FullTextModal'

/** 이 줄 수를 넘으면 접고 "전체보기" 로 넘깁니다. */
export const MAX_LINES = 10

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
  onExpand: (full: FullText) => void
}

/**
 * 상세 3칸 각각의 본문을 감쌉니다. 본문이 {@link MAX_LINES} 줄을 넘으면 앞부분만
 * 보여 주고 "전체보기" 버튼으로 팝업을 엽니다. 짧으면 지금과 똑같이 전부 그립니다.
 */
export function CollapsibleCode({
  text,
  className,
  title,
  modalClassName,
  header,
  footer,
  onExpand,
}: Props) {
  const lines = text.split('\n')
  const clipped = lines.length > MAX_LINES
  const body = clipped ? lines.slice(0, MAX_LINES).join('\n') : text

  return (
    <>
      <pre className={className}>
        {header}
        {body}
        {clipped && (
          <span className="code__ellipsis">… 이하 {lines.length - MAX_LINES}줄 숨김</span>
        )}
        {footer}
      </pre>
      {clipped && (
        <button
          className="btn-mini code__more"
          onClick={() => onExpand({ title, text, className: modalClassName })}
        >
          전체보기 ({lines.length}줄)
        </button>
      )}
    </>
  )
}
