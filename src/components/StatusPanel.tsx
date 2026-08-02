import type { PortStatus, Snapshot } from '../types'

interface Props {
  snapshot: Snapshot
  portDraft: string
  portStatus: PortStatus | null
  busy: boolean
  onPortChange: (value: string) => void
  onPortCommit: () => void
  onToggle: () => void
  onCopy: () => void
}

/**
 * 목업 M1/M2 의 큰 카드 — 상태 · 토글 · 포트 · Base URL 이 한 덩어리입니다.
 * 눈에 들어오는 순서: 지금 켜져 있나 → 어떤 주소를 쓰면 되나.
 */
export function StatusPanel({
  snapshot,
  portDraft,
  portStatus,
  busy,
  onPortChange,
  onPortCommit,
  onToggle,
  onCopy,
}: Props) {
  const { running, stats } = snapshot
  const conflict = portStatus !== null && !portStatus.free

  return (
    <div className="panel">
      <div className="status">
        <span className={`dot ${running ? 'dot--on' : 'dot--off'}`} />
        <div className="status__text">
          <span className={`status__title${running ? '' : ' status__title--off'}`}>
            {running ? '실행 중' : '꺼짐'}
          </span>
          <span className="status__sub">
            {running
              ? `사내 AI에 정상 연결됨 · 오늘 ${stats.total}건 처리`
              : '토글을 켜면 로컬 서버가 뜹니다'}
          </span>
        </div>
        <span className="spacer" />
        <div className="status__toggle">
          <span className={`status__label${running ? ' status__label--on' : ''}`}>
            {running ? '켜짐' : '꺼짐'}
          </span>
          <button
            className={`switch${running ? ' switch--on' : ''}`}
            onClick={onToggle}
            disabled={busy}
            title={running ? '프록시 끄기' : '프록시 켜기'}
            aria-pressed={running}
          >
            <span className="switch__knob" />
          </button>
        </div>
      </div>

      <div className="divider" />

      <div className="fields">
        <div className="field">
          <span className="field__label">포트</span>
          <div className="field__row">
            <input
              className={`port-input${conflict ? ' port-input--conflict' : ''}`}
              value={portDraft}
              inputMode="numeric"
              maxLength={5}
              onChange={(e) => onPortChange(e.target.value.replace(/\D/g, ''))}
              onBlur={onPortCommit}
              onKeyDown={(e) => e.key === 'Enter' && onPortCommit()}
              disabled={busy}
            />
            <span className={`port-note ${conflict ? 'port-note--conflict' : 'port-note--ok'}`}>
              {portStatus === null ? '확인 중' : conflict ? '이미 사용 중' : '사용 가능'}
            </span>
          </div>
        </div>

        <div className="field field--grow">
          <span className={`field__label${running ? '' : ' field__label--muted'}`}>
            {running ? 'Base URL — 쓰는 앱에 이 주소를 넣으세요' : 'Base URL — 켜면 활성화됩니다'}
          </span>
          <div className="field__row">
            <span className={`url-box${running ? ' selectable' : ' url-box--muted'}`}>
              {snapshot.baseUrl}
            </span>
            <button className="btn-primary" onClick={onCopy} disabled={!running}>
              복사
            </button>
          </div>
        </div>
      </div>

      <div className="hint">API 키 칸에는 아무 값이나 넣어도 됩니다. 사내 키는 이 앱이 대신 붙입니다.</div>
    </div>
  )
}
