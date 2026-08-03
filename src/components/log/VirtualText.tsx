import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

/**
 * 이 줄 수 이하면 가상 스크롤을 걸지 않고 지금처럼 `<pre>` 하나로 전부 그립니다.
 * 짧은 본문까지 창(window) 계산을 돌릴 필요가 없어 표시도 예전과 동일하게 유지됩니다.
 */
const VIRTUAL_MIN_LINES = 40

/** 아직 화면에 안 들어온 줄의 임시 높이(px). 보이는 순간 실제 높이로 교정됩니다. */
const ESTIMATED_ROW = 22

/** 뷰포트 위·아래로 미리 그려 둘 여유 줄 수 — 빠르게 스크롤해도 빈 칸이 안 보이게. */
const OVERSCAN = 8

interface Props {
  /** 팝업에 펼칠 본문 전체. */
  text: string
  /** 스크롤 컨테이너에 얹을 최종 className (예: `code modal__body code--reply`). */
  className: string
}

/**
 * "전체보기" 팝업 본문. 본문이 길면 화면에 보이는 줄만 그리는 가상 스크롤로 바꿔,
 * 수천 줄짜리 요청/응답을 열어도 스크롤이 버벅이지 않게 합니다. 짧으면 예전 그대로
 * `<pre>` 로 전부 그립니다.
 */
export function VirtualText({ text, className }: Props) {
  const lines = useMemo(() => text.split('\n'), [text])

  if (lines.length <= VIRTUAL_MIN_LINES) {
    return <pre className={className}>{text}</pre>
  }
  // 본문이 바뀌면(다른 로그 열기) key 로 새로 마운트해, 앞 본문의 높이 측정값이
  // 줄 수가 다른 새 본문에 잠깐이라도 섞이지 않게 합니다.
  return <VirtualLines key={text} lines={lines} className={className} />
}

/**
 * 실제 창 계산 부분. 줄별 높이를 재 두고 누적 오프셋으로 절대 위치에 얹습니다.
 * 줄은 화면에 들어올 때 한 번만 실제 높이로 교정되고, 위로 다시 스크롤해도 그 값이
 * 남아 있어 위쪽이 튀지 않습니다(교정은 아래로 처음 볼 때만 일어납니다).
 */
function VirtualLines({ lines, className }: { lines: string[]; className: string }) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const widthRef = useRef(0)
  const [heights, setHeights] = useState<number[]>(() => lines.map(() => ESTIMATED_ROW))
  const [scrollTop, setScrollTop] = useState(0)
  const [viewport, setViewport] = useState(0)

  // 뷰포트 높이를 추적하고, 폭이 바뀌면(줄바꿈이 달라지므로) 높이를 다시 재게 합니다.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const measure = () => {
      setViewport(el.clientHeight)
      const w = el.clientWidth
      if (widthRef.current !== 0 && widthRef.current !== w) {
        setHeights(lines.map(() => ESTIMATED_ROW))
      }
      widthRef.current = w
    }
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    measure()
    return () => ro.disconnect()
  }, [lines])

  // 줄 높이 누적합 → 각 줄의 top 오프셋과 전체 높이.
  const offsets = useMemo(() => {
    const arr = new Array<number>(heights.length + 1)
    arr[0] = 0
    for (let i = 0; i < heights.length; i++) arr[i + 1] = arr[i] + heights[i]
    return arr
  }, [heights])
  const total = offsets[heights.length]

  // offsets[i] <= y 를 만족하는 가장 큰 i (오프셋은 오름차순이라 이분탐색).
  const upperBound = useCallback(
    (y: number): number => {
      let lo = 0
      let hi = offsets.length - 1
      while (lo < hi) {
        const mid = (lo + hi + 1) >> 1
        if (offsets[mid] <= y) lo = mid
        else hi = mid - 1
      }
      return lo
    },
    [offsets],
  )

  const first = Math.max(0, upperBound(scrollTop) - OVERSCAN)
  const last = Math.min(lines.length - 1, upperBound(scrollTop + viewport) + OVERSCAN)

  const measureRow = useCallback((i: number, el: HTMLDivElement | null) => {
    if (!el) return
    const h = el.offsetHeight
    if (h === 0) return
    setHeights((prev) => {
      if (prev[i] === h) return prev
      const next = prev.slice()
      next[i] = h
      return next
    })
  }, [])

  const rows = []
  for (let i = first; i <= last; i++) {
    rows.push(
      <div
        key={i}
        ref={(el) => measureRow(i, el)}
        className="virtual__row"
        style={{ position: 'absolute', top: offsets[i], left: 0, right: 0 }}
      >
        {/* 빈 줄은 CSS min-height 로 한 줄 높이를 유지합니다(.virtual__row 참고). */}
        {lines[i]}
      </div>,
    )
  }

  return (
    <div
      ref={scrollRef}
      className={className}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <div className="virtual__sizer" style={{ height: total, position: 'relative' }}>
        {rows}
      </div>
    </div>
  )
}
