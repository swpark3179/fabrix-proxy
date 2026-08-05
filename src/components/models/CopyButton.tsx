import { useState } from 'react'

import { copyText } from '../../lib/format'

interface Props {
  value: string
  /** 버튼 글자. 기본은 `복사`. */
  label?: string
  title?: string
  disabled?: boolean
}

/**
 * 눌렀다는 것을 1.5초 보여주는 복사 버튼.
 *
 * 상태를 이 컴포넌트 안에 두는 이유: 목록이 길어도 부모가 "어느 행이 복사됐는지" 를
 * 들고 있을 필요가 없습니다. `EndpointCards` 의 토큰 복사 버튼과 같은 패턴입니다.
 */
export function CopyButton({ value, label = '복사', title, disabled }: Props) {
  const [copied, setCopied] = useState(false)

  async function run() {
    try {
      await copyText(value)
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    } catch {
      /* 클립보드 실패는 조용히 무시 — 값은 화면에 그대로 보이고 선택할 수 있습니다. */
    }
  }

  return (
    <button
      className="btn-ghost btn-ghost--mini"
      onClick={() => void run()}
      disabled={disabled || value === ''}
      title={title}
    >
      {copied ? '복사됨' : label}
    </button>
  )
}
