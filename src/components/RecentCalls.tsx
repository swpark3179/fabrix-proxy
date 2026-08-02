import { latency, tone } from '../lib/format'
import type { Snapshot } from '../types'

interface Props {
  snapshot: Snapshot
  onOpenLogs: () => void
}

/** 목업 M1 하단 4행 / M2 의 dashed 빈 상태. */
export function RecentCalls({ snapshot, onOpenLogs }: Props) {
  const { running, recent, stats } = snapshot

  return (
    <div className="recent">
      <div className="recent__head">
        <span className="recent__title">최근 호출</span>
        <span className="spacer" />
        <button className="link" onClick={onOpenLogs}>
          전체 로그 열기 →
        </button>
      </div>

      {recent.length === 0 ? (
        <div className="recent__empty">
          {running ? (
            <>
              아직 들어온 호출이 없습니다
              <br />
              쓰는 앱의 Base URL에 위 주소를 넣어 보세요
            </>
          ) : (
            <>
              프록시가 꺼져 있는 동안의 기록은 없습니다
              <br />
              {stats.lastCallAt
                ? `마지막 호출 ${stats.lastCallAt} · 오늘 ${stats.total}건`
                : `오늘 ${stats.total}건`}
            </>
          )}
        </div>
      ) : (
        <div className="recent__table">
          {recent.map((entry) => (
            <div className="recent__row" key={entry.id}>
              <span className="recent__time">{entry.ts}</span>
              <span className={`recent__code tone-${tone(entry)}`}>{entry.status}</span>
              <span className="recent__path">
                {entry.method} {entry.path}
              </span>
              <span className={`recent__model${entry.isError ? ` tone-${tone(entry)}` : ''}`}>
                {entry.summary ?? ''}
              </span>
              <span className="recent__latency">{latency(entry.latencyMs)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
