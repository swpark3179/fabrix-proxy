import type { ModelRow } from '../../types'
import { CopyButton } from './CopyButton'

interface Props {
  rows: ModelRow[]
  /** 필터 전 전체 개수 — 빈 상태 문구를 "검색 결과 없음" 과 "목록이 비었음" 으로 가릅니다. */
  total: number
  /** 흐리게 표시(조회 중 · 실패 후 이전 목록). */
  stale?: boolean
  onSetDefault: (alias: string) => void
}

/**
 * 표시 이름 · 모델 ID · UUID · 설명.
 *
 * alias 와 UUID 칸에 `.selectable` 을 붙이는 이유: `base.css` 가 `body` 에
 * `user-select: none` 을 걸어 두어, 없으면 드래그 선택 자체가 막힙니다. "텍스트로
 * 복사할 수 있게" 의 직역이라 복사 버튼과 별개로 반드시 살려 둡니다.
 */
export function ModelTable({ rows, total, stale, onSetDefault }: Props) {
  return (
    <div className={`modeltable${stale ? ' modeltable--stale' : ''}`}>
      <div className="modeltable__head">
        <span className="col-label">표시 이름</span>
        <span className="col-alias">모델 ID</span>
        <span className="col-uuid">사내 UUID</span>
        <span className="col-desc">설명</span>
        <span className="col-act" />
      </div>

      <div className="modeltable__scroll">
        {rows.length === 0 ? (
          <div className="modeltable__empty">
            {total === 0
              ? '사내에서 받은 모델이 없습니다.'
              : `검색과 맞는 모델이 없습니다 (전체 ${total}개).`}
          </div>
        ) : (
          rows.map((m) => (
            <div
              key={m.alias}
              className={`modeltable__row${m.isDefault ? ' modeltable__row--default' : ''}`}
            >
              <span className="col-label modeltable__label">
                {m.label}
                {m.isDefault && <span className="modeltable__badge">기본</span>}
              </span>

              <span className="col-alias modeltable__alias selectable" title={m.alias}>
                {m.alias}
              </span>

              <span className="col-uuid modeltable__uuid selectable" title={m.modelId}>
                {m.modelId}
              </span>

              <span className="col-desc modeltable__desc" title={m.description ?? ''}>
                {m.description ?? '—'}
              </span>

              <span className="col-act modeltable__act">
                <CopyButton value={m.alias} title={`${m.alias} 을 복사합니다`} />
                <CopyButton value={m.modelId} label="UUID" title="사내 UUID 를 복사합니다" />
                {!m.isDefault && (
                  <button
                    className="btn-ghost btn-ghost--mini"
                    onClick={() => onSetDefault(m.alias)}
                    title="model 을 안 보낸 요청에 이 모델을 씁니다"
                  >
                    기본으로
                  </button>
                )}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  )
}
