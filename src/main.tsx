import { StrictMode, useCallback, useEffect, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'

import { ConflictBanner } from './components/ConflictBanner'
import { ConnectionForm } from './components/ConnectionForm'
import { EndpointCards } from './components/EndpointCards'
import { RecentCalls } from './components/RecentCalls'
import { StatusPanel } from './components/StatusPanel'
import { TitleBar } from './components/TitleBar'
import { installErrorOverlay } from './lib/errorOverlay'
import {
  checkPort,
  copyEndpoint,
  errText,
  getConfig,
  getSnapshot,
  onOpenSettings,
  onStateChanged,
  openLogWindow,
  saveConfig,
  startProxy,
  stopProxy,
} from './lib/ipc'
import type { Config, PortStatus, Snapshot } from './types'

import './styles/base.css'
import './styles/main.css'

installErrorOverlay()

const PORT_DEBOUNCE_MS = 300

function App() {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null)
  const [config, setConfig] = useState<Config | null>(null)
  const [showSettings, setShowSettings] = useState(false)
  const [portDraft, setPortDraft] = useState('')
  const [portStatus, setPortStatus] = useState<PortStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  // 시작 스냅샷/설정을 못 받아온 경우 — 빈 스켈레톤에 멈추는 대신 이유를 보여줍니다.
  const [bootError, setBootError] = useState('')

  // 사용자가 포트를 만지기 시작하면 서버발 스냅샷이 입력칸을 덮어쓰지 않게 합니다.
  const portTouched = useRef(false)

  useEffect(() => {
    let alive = true

    void (async () => {
      try {
        const [snap, cfg] = await Promise.all([getSnapshot(), getConfig()])
        if (!alive) return
        setSnapshot(snap)
        setConfig(cfg)
        setPortDraft(String(snap.port))
        setPortStatus(snap.portStatus)
      } catch (err) {
        if (alive) setBootError(errText(err))
      }
    })()

    const unlisteners = [
      onStateChanged((next) => {
        setSnapshot(next)
        if (!portTouched.current) {
          setPortDraft(String(next.port))
          setPortStatus(next.portStatus)
        }
      }),
      onOpenSettings(() => setShowSettings(true)),
    ]

    return () => {
      alive = false
      void Promise.all(unlisteners).then((fns) => fns.forEach((fn) => fn()))
    }
  }, [])

  // 목업 원칙: 켜기를 시도할 때가 아니라 **입력 직후** 알려줍니다.
  useEffect(() => {
    const port = Number(portDraft)
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setPortStatus(null)
      return
    }
    const timer = setTimeout(() => {
      void checkPort(port).then(setPortStatus).catch(() => setPortStatus(null))
    }, PORT_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [portDraft])

  /** 포트가 바뀌었으면 설정에 반영합니다. 바뀐 포트를 돌려줍니다. */
  const commitPort = useCallback(
    async (override?: number): Promise<number | null> => {
      if (!config) return null
      const port = override ?? Number(portDraft)
      if (!Number.isInteger(port) || port < 1 || port > 65535) {
        setError('포트는 1–65535 사이의 숫자여야 합니다.')
        return null
      }
      if (port === config.port) return port

      const next = { ...config, port }
      const snap = await saveConfig(next)
      setConfig(next)
      setSnapshot(snap)
      portTouched.current = false
      return port
    },
    [config, portDraft],
  )

  const handleToggle = useCallback(async () => {
    if (!snapshot) return
    setBusy(true)
    setError('')
    try {
      if (snapshot.running) {
        setSnapshot(await stopProxy())
      } else {
        if ((await commitPort()) === null) return
        setSnapshot(await startProxy())
      }
    } catch (err) {
      setError(errText(err))
    } finally {
      setBusy(false)
    }
  }, [snapshot, commitPort])

  const handleUseSuggestion = useCallback(
    async (port: number) => {
      setBusy(true)
      setError('')
      try {
        setPortDraft(String(port))
        if ((await commitPort(port)) === null) return
        setSnapshot(await startProxy())
      } catch (err) {
        setError(errText(err))
      } finally {
        setBusy(false)
      }
    },
    [commitPort],
  )

  const handleCopy = useCallback(async () => {
    setError('')
    try {
      await copyEndpoint()
    } catch (err) {
      setError(errText(err))
    }
  }, [])

  const handleSave = useCallback(
    async (next: Config) => {
      setBusy(true)
      setError('')
      try {
        const snap = await saveConfig(next)
        setConfig(next)
        setSnapshot(snap)
        setPortDraft(String(next.port))
        portTouched.current = false
        setShowSettings(false)
        // 온보딩 직후에는 바로 켜 줍니다 — 사용자가 한 번 더 누를 이유가 없습니다.
        if (!snap.running && next.autoStart) {
          setSnapshot(await startProxy())
        }
      } finally {
        setBusy(false)
      }
    },
    [],
  )

  if (!snapshot || !config) {
    return (
      <div className="app">
        <TitleBar title="AI 프록시" />
        <div className="app__body">
          {bootError && <div className="alert">{bootError}</div>}
        </div>
      </div>
    )
  }

  const onboarding = !snapshot.configured
  const settings = showSettings && !onboarding
  const conflict = portStatus !== null && !portStatus.free

  return (
    <div className="app">
      <TitleBar
        title="AI 프록시"
        running={snapshot.running}
        onSettings={onboarding || settings ? undefined : () => setShowSettings(true)}
      />

      <div className="app__body">
        {error && (
          <div className="alert">
            <span className="spacer" style={{ flex: 1 }}>
              {error}
            </span>
            <button className="alert__close" onClick={() => setError('')}>
              ✕
            </button>
          </div>
        )}

        {onboarding || settings ? (
          <ConnectionForm
            initial={config}
            variant={onboarding ? 'onboarding' : 'settings'}
            busy={busy}
            onSave={handleSave}
            onCancel={settings ? () => setShowSettings(false) : undefined}
          />
        ) : (
          <>
            <StatusPanel
              snapshot={snapshot}
              portDraft={portDraft}
              portStatus={portStatus}
              busy={busy}
              onPortChange={(value) => {
                portTouched.current = true
                setPortDraft(value)
              }}
              onPortCommit={() => {
                void commitPort().catch((err) => setError(errText(err)))
              }}
              onToggle={() => void handleToggle()}
              onCopy={() => void handleCopy()}
            />

            {conflict && portStatus && (
              <ConflictBanner
                status={portStatus}
                busy={busy}
                onUseSuggestion={(port) => void handleUseSuggestion(port)}
              />
            )}

            <EndpointCards snapshot={snapshot} />
            <RecentCalls snapshot={snapshot} onOpenLogs={() => void openLogWindow()} />
          </>
        )}
      </div>
    </div>
  )
}

createRoot(document.getElementById('root') as HTMLElement).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
