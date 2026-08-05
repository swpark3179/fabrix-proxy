import { modelsToAliasList, modelsToPlainText } from '../../lib/format'
import type { ModelRow } from '../../types'
import { CopyButton } from './CopyButton'

interface Props {
  query: string
  onQuery: (value: string) => void
  /** 필터를 통과한 행 — 복사 버튼은 **보이는 것만** 담습니다. */
  visible: ModelRow[]
  total: number
  meta: string
  /** 복사한 평문 머리말에 넣습니다 — 어느 서버의 목록인지가 붙여넣은 뒤에도 남아야 합니다. */
  sourceUrl: string
  fetchedAt: string
  busy: boolean
  onRefresh: () => void
}

export function ModelToolbar({
  query,
  onQuery,
  visible,
  total,
  meta,
  sourceUrl,
  fetchedAt,
  busy,
  onRefresh,
}: Props) {
  const filtered = visible.length !== total
  const scope = filtered ? `검색 결과 ${visible.length}개` : `${total}개`

  return (
    <div className="models__bar">
      <input
        className="text-input models__search"
        placeholder="이름 · 모델 ID · UUID · 설명 검색"
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        spellCheck={false}
      />

      <span className="spacer" style={{ flex: 1 }} />
      <span className="models__meta">{meta}</span>

      {/* 두 복사 버튼 다 현재 필터를 존중하고, title 로 그걸 알립니다. */}
      <CopyButton
        value={modelsToPlainText(visible, sourceUrl, fetchedAt)}
        label="전체 복사"
        title={`${scope}를 표 형식 평문으로 복사합니다`}
        disabled={visible.length === 0}
      />
      <CopyButton
        value={modelsToAliasList(visible)}
        label="ID만 복사"
        title={`${scope}의 모델 ID 만 줄바꿈으로 복사합니다 (클라이언트 설정에 붙여넣기)`}
        disabled={visible.length === 0}
      />

      <button className="btn-ghost" onClick={onRefresh} disabled={busy}>
        {busy ? '조회 중…' : '다시 조회'}
      </button>
    </div>
  )
}
