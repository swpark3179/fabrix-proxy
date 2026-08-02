import { StrictMode, useEffect, useMemo, useState } from 'react'
import { createRoot } from 'react-dom/client'

import { FilterBar, type Filter } from './components/log/FilterBar'
import { LogDetail } from './components/log/LogDetail'
import { LogList } from './components/log/LogList'
import { TitleBar } from './components/TitleBar'
import { installErrorOverlay } from './lib/errorOverlay'
import { copyText } from './lib/format'
import {
  clearLogs,
  getLogs,
  getSnapshot,
  onLogEntry,
  onLogsCleared,
  onStateChanged,
} from './lib/ipc'
import type { LogEntry } from './types'

import './styles/base.css'
import './styles/log.css'

installErrorOverlay()

function matches(entry: LogEntry, filter: Filter): boolean {
  switch (filter) {
    case 'chat':
      return entry.kind === 'chat'
    case 'models':
      return entry.kind === 'models'
    case 'errors':
      return entry.isError
    default:
      return true
  }
}

function LogApp() {
  const [entries, setEntries] = useState<LogEntry[]>([])
  const [filter, setFilter] = useState<Filter>('all')
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [baseUrl, setBaseUrl] = useState('http://127.0.0.1:8787/v1')
  const [running, setRunning] = useState(false)
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    let alive = true

    void (async () => {
      try {
        const [logs, snap] = await Promise.all([getLogs(), getSnapshot()])
        if (!alive) return
        setEntries(logs)
        setBaseUrl(snap.baseUrl)
        setRunning(snap.running)
      } catch (err) {
        // 창은 그대로 뜨지만 원인을 콘솔·오버레이로 남깁니다.
        console.error('[log] 초기 로드 실패', err)
        throw err
      }
    })()

    const unlisteners = [
      // 링버퍼와 같은 순서(최신 우선)를 유지합니다.
      onLogEntry((entry) => setEntries((prev) => [entry, ...prev].slice(0, 200))),
      onLogsCleared(() => {
        setEntries([])
        setSelectedId(null)
      }),
      onStateChanged((snap) => {
        setBaseUrl(snap.baseUrl)
        setRunning(snap.running)
      }),
    ]

    return () => {
      alive = false
      void Promise.all(unlisteners).then((fns) => fns.forEach((fn) => fn()))
    }
  }, [])

  const visible = useMemo(() => entries.filter((e) => matches(e, filter)), [entries, filter])

  // 선택이 필터 밖으로 밀려나면 목록 맨 위로 옮깁니다.
  const selected = visible.find((e) => e.id === selectedId) ?? visible[0] ?? null

  async function handleCopyCurl(text: string) {
    await copyText(text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1600)
  }

  return (
    <div className="logapp">
      <TitleBar title={copied ? '호출 로그 — cURL을 복사했습니다' : '호출 로그'} running={running} resizable />

      <FilterBar
        active={filter}
        total={entries.length}
        onChange={setFilter}
        onClear={() => void clearLogs()}
      />

      <div className="logbody">
        <LogList
          entries={visible}
          selectedId={selected?.id ?? null}
          totalCount={entries.length}
          onSelect={setSelectedId}
        />

        {selected ? (
          <LogDetail
            entry={selected}
            baseUrl={baseUrl}
            onCopyCurl={(text) => void handleCopyCurl(text)}
          />
        ) : (
          <div className="detail">
            <div className="detail__placeholder">
              왼쪽에서 호출을 고르면
              <br />
              받은 것 · 보낸 것 · 돌려준 것을 나란히 보여줍니다
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

createRoot(document.getElementById('root') as HTMLElement).render(
  <StrictMode>
    <LogApp />
  </StrictMode>,
)
