import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'

import { copyText } from '../../lib/format'

export interface FullText {
  title: string
  text: string
  /** `<pre>` 에 얹을 톤 클래스 (예: `code--reply`). 상세 화면과 색을 맞춥니다. */
  className?: string
}

interface Props {
  open: FullText
  onClose: () => void
}

/**
 * "전체보기" 팝업 — 접힌 본문 전체를 스크롤로 봅니다.
 *
 * 로그 창에는 모달이 하나뿐이라 포털로 `document.body` 에 얹습니다. Esc·배경
 * 클릭·× 로 닫히고, 본문 복사 버튼을 제공합니다.
 */
export function FullTextModal({ open, onClose }: Props) {
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [onClose])

  async function handleCopy() {
    await copyText(open.text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1600)
  }

  return createPortal(
    <div className="modal__backdrop" onClick={onClose}>
      <div className="modal__card" onClick={(e) => e.stopPropagation()}>
        <div className="modal__head">
          <span className="modal__title">{open.title}</span>
          <span className="spacer" />
          <button className="btn-mini" onClick={() => void handleCopy()}>
            {copied ? '복사됨' : '본문 복사'}
          </button>
          <button className="modal__close" onClick={onClose} aria-label="닫기">
            ×
          </button>
        </div>
        <pre className={`code modal__body${open.className ? ` ${open.className}` : ''}`}>
          {open.text}
        </pre>
      </div>
    </div>,
    document.body,
  )
}
