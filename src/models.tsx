import { StrictMode, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'

import { ModelTable } from './components/models/ModelTable'
import { ModelToolbar } from './components/models/ModelToolbar'
import { TitleBar } from './components/TitleBar'
import { installErrorOverlay } from './lib/errorOverlay'
import {
  errText,
  getSnapshot,
  listModels,
  onStateChanged,
  openSettingsWindow,
  setDefaultModel,
} from './lib/ipc'
import type { ModelListResult, ModelRow } from './types'

import './styles/base.css'
import './styles/models.css'

installErrorOverlay()

/**
 * 조회가 실패해도 **이전 목록을 지우지 않습니다.**
 *
 * 사내 연결이 잠깐 끊긴 것과 "쓸 수 있는 모델이 없다" 는 다른 이야기인데, 표를 비우면
 * 사용자에게는 똑같이 보입니다. 그래서 loading/error 가 앞선 행들을 들고 다닙니다.
 */
type View =
  | { s: 'boot' }
  | { s: 'unconfigured' }
  | { s: 'loading'; prev: ModelRow[] }
  | { s: 'ready'; data: ModelListResult }
  | { s: 'error'; text: string; prev: ModelRow[] }

function rowsOf(view: View): ModelRow[] {
  switch (view.s) {
    case 'ready':
      return view.data.models
    case 'loading':
    case 'error':
      return view.prev
    default:
      return []
  }
}

/** `2026-08-05T14:02:11+09:00` → `방금` · `12초 전` · `3분 전`. */
function ago(iso: string, now: number): string {
  const then = Date.parse(iso)
  if (Number.isNaN(then)) return ''
  const secs = Math.max(0, Math.round((now - then) / 1000))
  if (secs < 3) return '방금 조회'
  if (secs < 60) return `${secs}초 전 조회`
  return `${Math.round(secs / 60)}분 전 조회`
}

function ModelsApp() {
  const [view, setView] = useState<View>({ s: 'boot' })
  const [query, setQuery] = useState('')
  const [running, setRunning] = useState(false)
  const [defaultAlias, setDefaultAlias] = useState('')
  const [configured, setConfigured] = useState(true)
  // 나이 표시를 1초마다 다시 그리기 위한 시계.
  const [now, setNow] = useState(() => Date.now())
  /** `configured` 의 직전 값 — false→true 전이에서만 재조회합니다. */
  const wasConfigured = useRef(true)

  const load = useCallback(async (refresh: boolean) => {
    setView((prev) => ({ s: 'loading', prev: rowsOf(prev) }))
    try {
      const data = await listModels(refresh)
      setView({ s: 'ready', data })
      setDefaultAlias(data.defaultAlias)
    } catch (err) {
      setView((prev) => ({ s: 'error', text: errText(err), prev: rowsOf(prev) }))
    }
  }, [])

  useEffect(() => {
    let alive = true

    void (async () => {
      try {
        const snap = await getSnapshot()
        if (!alive) return
        setRunning(snap.running)
        setDefaultAlias(snap.defaultModelAlias)
        setConfigured(snap.configured)
        // 미설정이면 커맨드를 아예 부르지 않습니다 — 503 을 오류로 보여주는 것보다
        // "설정하세요" 가 맞는 안내입니다. main.tsx 가 온보딩을 가르는 것과 같은 규칙.
        if (snap.configured) await load(false)
        else setView({ s: 'unconfigured' })
      } catch (err) {
        if (alive) setView({ s: 'error', text: errText(err), prev: [] })
      }
    })()

    const unlisten = onStateChanged((snap) => {
      setRunning(snap.running)
      setDefaultAlias(snap.defaultModelAlias)
      setConfigured(snap.configured)
      // 설정을 막 저장했으면 목록을 다시 받습니다. 저장이 Rust 쪽 캐시를 이미
      // 비웠으므로(state.rs replace_config) 새 서버의 목록이 옵니다.
      //
      // 이전 값 비교를 ref 로 하는 이유: setState 업데이터 안에서 부작용을 일으키면
      // StrictMode 가 업데이터를 두 번 불러 조회가 두 번 나갑니다.
      if (!wasConfigured.current && snap.configured) void load(false)
      wasConfigured.current = snap.configured
    })

    const timer = setInterval(() => setNow(Date.now()), 1000)

    return () => {
      alive = false
      clearInterval(timer)
      void unlisten.then((fn) => fn())
    }
  }, [load])

  const rows = rowsOf(view)
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase()
    if (q === '') return rows
    return rows.filter((m) =>
      [m.alias, m.modelId, m.label, m.description ?? '']
        .join(' ')
        .toLowerCase()
        .includes(q),
    )
  }, [rows, query])

  // 기본 모델 배지는 스냅샷을 정본으로 씁니다 — 목록 창에서 바꾸든 설정 화면에서
  // 바꾸든 같은 값이 보이게 합니다.
  const withDefault = useMemo(
    () => visible.map((m) => ({ ...m, isDefault: m.alias === defaultAlias })),
    [visible, defaultAlias],
  )

  const data = view.s === 'ready' ? view.data : null
  const meta = [
    `모델 ${rows.length}개`,
    data ? ago(data.fetchedAt, now) : null,
    data ? (data.cached ? `캐시에서 · ${data.cacheTtlSecs}초` : '사내에서 새로 조회') : null,
  ]
    .filter(Boolean)
    .join(' · ')

  async function handleSetDefault(alias: string) {
    try {
      const snap = await setDefaultModel(alias)
      setDefaultAlias(snap.defaultModelAlias)
    } catch (err) {
      setView((prev) => ({ s: 'error', text: errText(err), prev: rowsOf(prev) }))
    }
  }

  return (
    <div className="models-app">
      <TitleBar title="모델 목록" running={running} resizable />

      {view.s === 'unconfigured' || !configured ? (
        <div className="models__blank">
          <p className="models__blank-title">사내 연결 설정이 필요합니다</p>
          <p className="models__blank-body">
            사내 AI 주소 · 인증키 · 토큰을 넣으면 쓸 수 있는 모델 목록을 여기에 보여줍니다.
          </p>
          <button className="btn-primary" onClick={() => void openSettingsWindow()}>
            사내 연결 설정 열기
          </button>
        </div>
      ) : (
        <>
          <ModelToolbar
            query={query}
            onQuery={setQuery}
            visible={withDefault}
            total={rows.length}
            meta={meta}
            sourceUrl={data?.sourceUrl ?? ''}
            fetchedAt={data?.fetchedAt ?? ''}
            busy={view.s === 'loading'}
            onRefresh={() => void load(true)}
          />

          {view.s === 'error' && (
            <div className="alert models__alert">
              <span className="spacer" style={{ flex: 1 }}>
                {view.text}
                {view.prev.length > 0 && ' (아래는 마지막으로 받은 목록입니다)'}
              </span>
              <button className="btn-ghost btn-ghost--mini" onClick={() => void load(true)}>
                다시 시도
              </button>
            </div>
          )}

          <ModelTable
            rows={withDefault}
            total={rows.length}
            stale={view.s === 'loading' || view.s === 'error'}
            onSetDefault={(alias) => void handleSetDefault(alias)}
          />

          <div className="models__foot">
            <span className="models__foot-hint">
              <b>모델 ID</b> 를 클라이언트의 <code>model</code> 칸에 넣으세요. 사내 UUID 는
              담당자와 대조할 때 씁니다.
            </span>
            <span className="spacer" style={{ flex: 1 }} />
            <span className="models__foot-src">{data?.sourceUrl ?? ''}</span>
          </div>
        </>
      )}
    </div>
  )
}

createRoot(document.getElementById('root') as HTMLElement).render(
  <StrictMode>
    <ModelsApp />
  </StrictMode>,
)
