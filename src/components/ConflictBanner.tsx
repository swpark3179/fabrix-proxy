import type { PortStatus } from '../types'

interface Props {
  status: PortStatus
  busy: boolean
  onUseSuggestion: (port: number) => void
}

/**
 * 목업 M2 — 충돌은 켜기를 시도할 때가 아니라 **입력 직후** 알려주고,
 * 빈 포트 하나를 미리 골라 버튼에 박아 둡니다. 선택지를 만들지 않는 것이 원칙.
 */
export function ConflictBanner({ status, busy, onUseSuggestion }: Props) {
  const owner = status.owner

  return (
    <div className="conflict">
      <div className="conflict__text">
        <span className="conflict__title">{status.port} 포트를 다른 앱이 쓰고 있습니다</span>
        <span className="conflict__meta">
          {owner ? `PID ${owner.pid} · ${owner.name} · LISTENING` : '점유 프로세스를 확인하지 못했습니다'}
        </span>
      </div>
      {status.suggestion !== null && (
        <button
          className="btn-primary"
          disabled={busy}
          onClick={() => onUseSuggestion(status.suggestion as number)}
        >
          {status.suggestion}로 바꾸고 켜기
        </button>
      )}
    </div>
  )
}
