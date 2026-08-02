import { useEffect, useState } from 'react'

import { errText, getConfigPath, testConnection } from '../lib/ipc'
import type { Config } from '../types'

interface Props {
  initial: Config
  /** 온보딩은 필수 3개만, 설정은 고급 항목까지 보여줍니다. */
  variant: 'onboarding' | 'settings'
  busy: boolean
  onSave: (config: Config) => Promise<void>
  onCancel?: () => void
}

type Probe =
  | { state: 'idle' }
  | { state: 'running' }
  | { state: 'ok'; text: string }
  | { state: 'error'; text: string }

/**
 * 온보딩과 설정이 같은 폼을 씁니다. 목업에는 없는 화면이라, 목업의 조판 규칙
 * (라벨 12px · 입력 radius 7 · accent 버튼)을 그대로 따라 만들었습니다.
 */
export function ConnectionForm({ initial, variant, busy, onSave, onCancel }: Props) {
  const [draft, setDraft] = useState<Config>(initial)
  const [probe, setProbe] = useState<Probe>({ state: 'idle' })
  const [configPath, setConfigPath] = useState('')
  const [saveError, setSaveError] = useState('')

  useEffect(() => {
    void getConfigPath().then(setConfigPath)
  }, [])

  const set = <K extends keyof Config>(key: K, value: Config[K]) =>
    setDraft((prev) => ({ ...prev, [key]: value }))

  const filled =
    draft.fabrixBaseUrl.trim() !== '' &&
    draft.fabrixClient.trim() !== '' &&
    draft.openapiToken.trim() !== ''

  async function runProbe() {
    setProbe({ state: 'running' })
    try {
      const result = await testConnection({
        fabrixBaseUrl: draft.fabrixBaseUrl,
        fabrixClient: draft.fabrixClient,
        openapiToken: draft.openapiToken,
        insecureSkipVerify: draft.insecureSkipVerify,
      })
      setProbe({
        state: 'ok',
        text: `연결됨 · 모델 ${result.modelCount}개 — ${result.sample.join(', ')}`,
      })
    } catch (err) {
      setProbe({ state: 'error', text: errText(err) })
    }
  }

  async function submit() {
    setSaveError('')
    try {
      await onSave({ ...draft, port: draft.port || 8787 })
    } catch (err) {
      setSaveError(errText(err))
    }
  }

  return (
    <div className="setup">
      <div className="setup__lead">
        <span className="setup__title">
          {variant === 'onboarding' ? '사내 AI에 연결합니다' : '사내 연결 설정'}
        </span>
        <span className="setup__desc">
          {variant === 'onboarding'
            ? '한 번만 입력하면 됩니다. 이후로는 트레이에서 켜고 주소만 복사하면 끝입니다.'
            : '값을 바꾸면 모델 목록 캐시를 비우고 다시 조회합니다.'}
        </span>
      </div>

      <div className="setup__grid">
        <div className="field">
          <span className="field__label">사내 AI 주소</span>
          <input
            className="text-input"
            placeholder="https://ai.corp.internal"
            value={draft.fabrixBaseUrl}
            onChange={(e) => set('fabrixBaseUrl', e.target.value)}
            spellCheck={false}
          />
        </div>

        <div className="setup__row">
          <div className="field">
            <span className="field__label">인증키 · x-fabrix-client</span>
            <input
              className="text-input"
              type="password"
              value={draft.fabrixClient}
              onChange={(e) => set('fabrixClient', e.target.value)}
              spellCheck={false}
            />
          </div>
          <div className="field">
            <span className="field__label">OpenAPI 토큰 · x-openapi-token</span>
            <input
              className="text-input"
              type="password"
              value={draft.openapiToken}
              onChange={(e) => set('openapiToken', e.target.value)}
              spellCheck={false}
            />
          </div>
        </div>
      </div>

      <div className="setup__foot">
        <button className="btn-ghost" onClick={runProbe} disabled={!filled || probe.state === 'running'}>
          {probe.state === 'running' ? '확인 중…' : '연결 확인'}
        </button>
        {probe.state === 'ok' && <span className="setup__result setup__result--ok">{probe.text}</span>}
        {probe.state === 'error' && (
          <span className="setup__result setup__result--err">{probe.text}</span>
        )}
      </div>

      {variant === 'settings' && (
        <div className="setup__advanced">
          <span className="setup__section-label">고급</span>
          <div className="setup__row">
            <div className="field">
              <span className="field__label">포트</span>
              <input
                className="text-input"
                inputMode="numeric"
                maxLength={5}
                value={String(draft.port)}
                onChange={(e) => set('port', Number(e.target.value.replace(/\D/g, '')) || 0)}
              />
            </div>
            <div className="field">
              <span className="field__label">기본 모델 alias — 모르는 모델명이 오면 이걸로</span>
              <input
                className="text-input"
                placeholder="비우면 목록의 첫 모델"
                value={draft.defaultModelAlias}
                onChange={(e) => set('defaultModelAlias', e.target.value)}
                spellCheck={false}
              />
            </div>
          </div>

          <label className="check">
            <input
              type="checkbox"
              checked={draft.autoStart}
              onChange={(e) => set('autoStart', e.target.checked)}
            />
            앱을 켜면 프록시도 자동으로 켭니다
          </label>

          <label className="check">
            <input
              type="checkbox"
              checked={draft.insecureSkipVerify}
              onChange={(e) => set('insecureSkipVerify', e.target.checked)}
            />
            TLS 인증서 검증을 건너뜁니다 — 사내 루트 CA가 Windows 인증서 저장소에 없을 때만
          </label>
        </div>
      )}

      <div className="setup__note">
        입력한 값은 <code>{configPath || '~/.fabrix-proxy/config.json'}</code> 에{' '}
        <strong>평문 JSON</strong>으로 저장됩니다. 이 폴더를 읽을 수 있는 계정이나 프로그램은 사내
        인증키를 그대로 볼 수 있습니다.
      </div>

      {saveError && <div className="alert">{saveError}</div>}

      <div className="setup__foot">
        <button className="btn-primary" onClick={submit} disabled={!filled || busy}>
          {variant === 'onboarding' ? '저장하고 시작' : '저장'}
        </button>
        {onCancel && (
          <button className="btn-ghost" onClick={onCancel} disabled={busy}>
            취소
          </button>
        )}
      </div>
    </div>
  )
}
