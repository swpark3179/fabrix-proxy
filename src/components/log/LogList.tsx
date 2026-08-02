import { latency, shortPath, subtitle, tone } from '../../lib/format'
import type { LogEntry } from '../../types'

interface Props {
  entries: LogEntry[]
  selectedId: string | null
  totalCount: number
  onSelect: (id: string) => void
}

/** 목업 L1 좌측 268px 목록. 선택 행은 연파랑 + 좌측 2px accent 보더. */
export function LogList({ entries, selectedId, totalCount, onSelect }: Props) {
  const hidden = totalCount - entries.length

  return (
    <div className="loglist">
      <div className="loglist__head">
        <span className="col-time">시각</span>
        <span className="col-code">코드</span>
        <span className="col-main">경로 · 모델</span>
        <span className="col-latency">지연</span>
      </div>

      <div className="loglist__scroll">
        {entries.length === 0 ? (
          <div className="loglist__empty">
            {totalCount === 0 ? (
              <>
                아직 기록이 없습니다
                <br />
                프록시를 켜고 호출을 보내 보세요
              </>
            ) : (
              '이 필터에 해당하는 기록이 없습니다'
            )}
          </div>
        ) : (
          entries.map((entry) => (
            <button
              key={entry.id}
              className={`loglist__row${entry.id === selectedId ? ' loglist__row--active' : ''}`}
              onClick={() => onSelect(entry.id)}
            >
              <span className="col-time">{entry.ts}</span>
              <span className={`col-code tone-${tone(entry)}`}>{entry.status}</span>
              <span className="col-main">
                <span className="loglist__path">{shortPath(entry)}</span>
                <span className={`loglist__sub${entry.isError ? ` tone-${tone(entry)}` : ''}`}>
                  {subtitle(entry)}
                </span>
              </span>
              <span className="col-latency">{latency(entry.latencyMs)}</span>
            </button>
          ))
        )}
      </div>

      {hidden > 0 && <div className="loglist__foot">필터로 가려진 기록 {hidden}건</div>}
    </div>
  )
}
