import { StrictMode, useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'

import { installErrorOverlay } from './lib/errorOverlay'
import { onToast } from './lib/ipc'

import './styles/base.css'
import './styles/toast.css'

installErrorOverlay()

/**
 * 목업 T3 — 창을 띄우지 않고 복사만 하는 흐름이 이 앱에서 가장 잦습니다.
 * 그래서 OS 알림이 아니라 트레이 위에 뜨는 전용 카드로 만들었습니다.
 * 창 자체는 앱 시작 때 만들어져 숨어 있고, Rust 가 위치를 잡아 3초간 보여줍니다.
 */
function Toast() {
  const [url, setUrl] = useState('')

  useEffect(() => {
    const unlisten = onToast(setUrl)
    return () => {
      void unlisten.then((fn) => fn())
    }
  }, [])

  return (
    <div className="toast">
      <span className="toast__icon">✓</span>
      <div className="toast__text">
        <span className="toast__title">주소를 복사했습니다</span>
        <span className="toast__url">{url}</span>
        <span className="toast__hint">쓰는 앱의 Base URL 칸에 붙여넣으세요. API 키는 아무 값이나 됩니다.</span>
      </div>
    </div>
  )
}

createRoot(document.getElementById('root') as HTMLElement).render(
  <StrictMode>
    <Toast />
  </StrictMode>,
)
