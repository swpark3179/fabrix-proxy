import { writeText } from '@tauri-apps/plugin-clipboard-manager'
import { useEffect, useState } from 'react'

import {
  errText,
  getConfigPath,
  issueToken,
  listModels,
  openModelsWindow,
  testConnection,
} from '../lib/ipc'
import type { Config, ModelRow } from '../types'

// 하드코딩된 IMAGE_MODELS/VISION_MODELS 는 없어졌습니다. 이미지도 채팅과 **같은** 사내
// 모델 목록(`messages-with-models`)을 쓴다는 것이 확인됐으므로, 후보를 추측해 적어 둘
// 이유가 사라졌습니다.

/** `직접 입력…` 을 고른 상태를 나타내는 센티널. 실제 alias 와 겹칠 수 없는 값입니다. */
const MANUAL = '\u0000manual'

/** 사내 모델 목록의 상태. 기본 모델 선택기와 이미지 드롭다운이 **같은 조회**를 씁니다. */
type Choices =
  | { s: 'off' }
  | { s: 'loading' }
  | { s: 'ready'; rows: ModelRow[] }
  /** 미설정·오프라인·조회 실패 — 자유 입력으로 되돌립니다. */
  | { s: 'unavailable' }

/**
 * 이미지 드롭다운 옵션. 값이 **UUID**(`modelId`)인 이유: 이미지 업스트림
 * (`messages-with-models`)은 alias 가 아니라 UUID 를 받습니다.
 *
 * 저장된 값이 조회 목록에 없으면(오프라인/미조회) 그대로 보이도록 앞에 끼워 줍니다.
 */
function modelOptionList(models: ModelRow[], current: string): { value: string; label: string }[] {
  const opts = models.map((m) => ({ value: m.modelId, label: `${m.label} · ${m.alias}` }))
  if (current && !opts.some((o) => o.value === current)) {
    opts.unshift({ value: current, label: `${current} (저장됨)` })
  }
  return opts
}

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
  const [tokenCopied, setTokenCopied] = useState(false)
  const [choices, setChoices] = useState<Choices>({ s: 'off' })
  const [manual, setManual] = useState(false)

  useEffect(() => {
    void getConfigPath().then(setConfigPath)
  }, [])

  // 설정 화면에서만 사내 목록을 미리 받아 둡니다 — 기본 모델 선택기와 이미지 드롭다운 셋이
  // **이 한 번의 조회**를 나눠 씁니다. 온보딩에는 아직 시험할 대상이 없으므로
  // `연결 확인` 이 목록을 채워 줍니다(아래 runProbe).
  //
  // 실패는 **조용히** unavailable 로 내립니다 — 설정 화면을 열었을 뿐인데 오류 배너가
  // 뜨면 안 됩니다. 자유 입력으로 되돌아가므로 오프라인에서도 막히지 않습니다.
  useEffect(() => {
    if (variant !== 'settings') return
    let alive = true
    setChoices({ s: 'loading' })
    void listModels(false)
      .then((result) => alive && setChoices({ s: 'ready', rows: result.models }))
      .catch(() => alive && setChoices({ s: 'unavailable' }))
    return () => {
      alive = false
    }
  }, [variant])

  /// 이미지 드롭다운 셋이 보는 목록. 기본 모델 선택기와 **같은 조회**에서 나옵니다 —
  /// 사내에 목록 엔드포인트가 하나뿐이라 두 번 물을 이유가 없습니다.
  const models = choices.s === 'ready' ? choices.rows : []

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
      const sample = result.models
        .slice(0, 4)
        .map((m) => `${m.alias} · ${m.label}`)
        .join(', ')
      setProbe({ state: 'ok', text: `연결됨 · 모델 ${result.modelCount}개 — ${sample}` })
      // 초안 값으로 시험한 **그 서버**의 목록이라 저장 전에는 이게 가장 정확합니다.
      setChoices({ s: 'ready', rows: result.models })
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

  /** 토큰 모드를 켤 때 토큰이 아직 없으면 바로 하나 발행해 초안에 채웁니다. */
  async function toggleTokenMode(checked: boolean) {
    setTokenCopied(false)
    if (checked && draft.issuedToken.trim() === '') {
      try {
        const token = await issueToken()
        setDraft((prev) => ({ ...prev, tokenMode: true, issuedToken: token }))
        return
      } catch {
        // 발행에 실패해도 저장 시 백엔드가 자동 발행하므로 모드만 켭니다.
      }
    }
    set('tokenMode', checked)
  }

  /** 토큰 재발급 — 이전 토큰은 저장 후 무효가 됩니다. */
  async function regenerateToken() {
    try {
      const token = await issueToken()
      set('issuedToken', token)
      setTokenCopied(false)
    } catch (err) {
      setSaveError(errText(err))
    }
  }

  async function copyToken() {
    try {
      await writeText(draft.issuedToken)
      setTokenCopied(true)
      setTimeout(() => setTokenCopied(false), 1500)
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
            {/* 예전 라벨은 "모르는 모델명이 오면 이걸로" 였습니다. 이제 모르는 이름은
                404 model_not_found 이므로, 이 값이 쓰이는 곳은 model 을 아예 안 보낸
                요청뿐입니다. */}
            <div className="field">
              <span className="field__label">기본 모델 — model 을 안 보낸 요청에 쓸 모델</span>
              {manual || choices.s !== 'ready' ? (
                <input
                  className="text-input"
                  placeholder="비우면 목록의 첫 모델"
                  value={draft.defaultModelAlias}
                  onChange={(e) => set('defaultModelAlias', e.target.value)}
                  spellCheck={false}
                />
              ) : (
                <select
                  className="text-input"
                  value={draft.defaultModelAlias}
                  onChange={(e) => {
                    if (e.target.value === MANUAL) {
                      // 값은 그대로 두고 입력칸으로만 바꿉니다 — 고르자마자 지워지면
                      // 직전 값을 다시 타이핑해야 합니다.
                      setManual(true)
                      return
                    }
                    set('defaultModelAlias', e.target.value)
                  }}
                >
                  <option value="">— 목록의 첫 모델 —</option>
                  {choices.rows.map((m) => (
                    <option key={m.alias} value={m.alias}>
                      {m.label} · {m.alias}
                    </option>
                  ))}
                  {/* 저장된 값이 목록에 없어도 드롭다운에서 사라지지 않게 —
                      withCurrent() 와 같은 규칙입니다. */}
                  {draft.defaultModelAlias !== '' &&
                    !choices.rows.some((m) => m.alias === draft.defaultModelAlias) && (
                      <option value={draft.defaultModelAlias}>
                        {draft.defaultModelAlias} (목록에 없음)
                      </option>
                    )}
                  <option value={MANUAL}>직접 입력…</option>
                </select>
              )}
              <span className="field__row">
                {choices.s === 'loading' && (
                  <span className="field__label field__label--muted">목록을 불러오는 중…</span>
                )}
                {choices.s === 'unavailable' && (
                  <span className="field__label field__label--muted">
                    목록을 못 받았습니다 — 이름을 직접 넣거나 연결 확인을 눌러 보세요.
                  </span>
                )}
                <span className="spacer" style={{ flex: 1 }} />
                <button
                  type="button"
                  className="link"
                  onClick={() => void openModelsWindow()}
                  title="쓸 수 있는 모델과 각 ID 를 보고 복사합니다"
                >
                  모델 목록 보기 →
                </button>
              </span>
            </div>
          </div>

          <span className="setup__section-label">이미지 (OpenAI 호환 /v1/images · messages-with-models)</span>
          <div className="field">
            <span className="field__label">텍스트 모델 — 이미지 호출에 함께 전송 (modelIds 첫 번째)</span>
            <select
              className="text-input"
              value={draft.imageTextModel}
              onChange={(e) => set('imageTextModel', e.target.value)}
            >
              <option value="">— 선택 —</option>
              {modelOptionList(models, draft.imageTextModel).map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>
          <div className="setup__row">
            <div className="field">
              <span className="field__label">이미지 생성 모델 · FLUX (T2I)</span>
              <select
                className="text-input"
                value={draft.imageModel}
                onChange={(e) => set('imageModel', e.target.value)}
              >
                <option value="">— 선택 —</option>
                {modelOptionList(models, draft.imageModel).map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <span className="field__label">이미지 인식 모델 · gemma (I2T)</span>
              <select
                className="text-input"
                value={draft.visionModel}
                onChange={(e) => set('visionModel', e.target.value)}
              >
                <option value="">— 선택 —</option>
                {modelOptionList(models, draft.visionModel).map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
          </div>
          {models.length === 0 && (
            <span className="setup__desc">
              모델 목록이 비어 있습니다 — 위 연결 정보를 저장한 뒤 설정을 다시 열면 사내 모델을 드롭다운으로
              고를 수 있습니다. (저장된 값은 그대로 유지됩니다.)
            </span>
          )}

          <label className="check">
            <input
              type="checkbox"
              checked={draft.imageStubMode}
              onChange={(e) => set('imageStubMode', e.target.checked)}
            />
            이미지 스텁 모드 — 백엔드 미연결 상태에서 1×1 자리표시자 PNG 반환 (개발·배선 검증용)
          </label>

          <label className="check">
            <input
              type="checkbox"
              checked={draft.toolEmulation}
              onChange={(e) => set('toolEmulation', e.target.checked)}
            />
            도구 호출 흉내 내기 — 사내 API 에 도구 필드가 없어, 규약을 시스템 프롬프트에 심고
            답변에서 걷어냅니다. 끄면 클라이언트가 보낸 도구를 무시하고 글만 돌려줍니다
            (코딩 에이전트는 파일을 만들지 못합니다)
          </label>

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

          <label className="check">
            <input
              type="checkbox"
              checked={draft.tokenMode}
              onChange={(e) => void toggleTokenMode(e.target.checked)}
            />
            토큰 사용 모드 — 발행된 토큰과 일치하는 요청만 허용 (끄면 아무 토큰이나 통과)
          </label>

          {draft.tokenMode && (
            <div className="field">
              <span className="field__label">발행된 토큰 · 클라이언트의 API 키 칸에 넣으세요</span>
              {draft.issuedToken.trim() !== '' ? (
                <>
                  <div className="setup__row">
                    <input
                      className="text-input"
                      readOnly
                      value={draft.issuedToken}
                      spellCheck={false}
                      onFocus={(e) => e.currentTarget.select()}
                    />
                    <button className="btn-ghost" onClick={() => void copyToken()}>
                      {tokenCopied ? '복사됨' : '복사'}
                    </button>
                    <button className="btn-ghost" onClick={() => void regenerateToken()}>
                      재발급
                    </button>
                  </div>
                  <span className="setup__desc">
                    이 토큰과 다른 값으로 호출하면 <strong>401</strong> 로 거부됩니다. 재발급하면
                    이전 토큰은 저장 후 무효가 됩니다.
                  </span>
                </>
              ) : (
                <span className="setup__desc">저장하면 <code>sk-…</code> 토큰이 자동 발행됩니다.</span>
              )}
            </div>
          )}
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
