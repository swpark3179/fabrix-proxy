export type Filter = 'all' | 'chat' | 'models' | 'errors'

interface Props {
  active: Filter
  total: number
  capacity: number
  onChange: (filter: Filter) => void
  onClear: () => void
}

const CHIPS: { id: Filter; label: string; mono?: boolean }[] = [
  { id: 'all', label: '전체' },
  { id: 'chat', label: 'chat/completions', mono: true },
  { id: 'models', label: 'models', mono: true },
  { id: 'errors', label: '오류만' },
]

export function FilterBar({ active, total, capacity, onChange, onClear }: Props) {
  return (
    <div className="filters">
      {CHIPS.map((chip) => (
        <button
          key={chip.id}
          className={`chip${chip.mono ? ' chip--mono' : ''}${chip.id === active ? ' chip--active' : ''}`}
          onClick={() => onChange(chip.id)}
        >
          {chip.label}
        </button>
      ))}
      <span className="spacer" />
      <span className="filters__meta">최근 {capacity}건 · 본문은 메모리에만 보관 · 지금 {total}건</span>
      <button className="filters__clear" onClick={onClear}>
        지우기
      </button>
    </div>
  )
}
