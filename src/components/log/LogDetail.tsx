import { useState } from 'react'

import { toCurl, tone } from '../../lib/format'
import type { LogEntry } from '../../types'
import { CollapsibleCode } from './CollapsibleCode'
import { FullTextModal, type FullText } from './FullTextModal'

interface Props {
  entry: LogEntry
  baseUrl: string
  onCopyCurl: (text: string) => void
}

/**
 * 목업 L1/L2 우측 — 받은 것 · 보낸 것 · 돌려준 것이 위에서 아래로 흐릅니다.
 * 탭으로 나누지 않아 스크롤 없이 세 단계를 한눈에 비교할 수 있습니다.
 */
export function LogDetail({ entry, baseUrl, onCopyCurl }: Props) {
  const t = tone(entry)
  const host = hostOf(entry.fabrixUrl)
  const chat = entry.kind === 'chat'
  const [full, setFull] = useState<FullText | null>(null)

  // ③ 응답 칸의 톤 클래스 — 접힌 화면과 팝업이 같은 색을 쓰도록 한 번만 계산합니다.
  const respTone = `${entry.isError ? 'code--error' : 'code--reply'}${
    chat && !entry.isError ? ' code--prose' : ''
  }`

  return (
    <div className="detail">
      <div className="detail__head">
        <span className="detail__route">
          {entry.method} {entry.path}
        </span>
        <span className={`detail__badge detail__badge--${t}`}>
          {entry.status} · {(entry.latencyMs / 1000).toFixed(1)}s
        </span>
        <span className="spacer" />
        <button className="btn-mini" onClick={() => onCopyCurl(toCurl(entry, baseUrl))}>
          cURL 복사
        </button>
      </div>

      {/* ① 받은 요청 */}
      <section className="step">
        <div className="step__head">
          <span className="step__num step__num--1">1</span>
          <span className="step__title">받은 요청</span>
          <span className="step__hint">
            OpenAI 형식 · {chat ? '클라이언트 → 프록시' : '본문 없음'}
          </span>
        </div>
        <CollapsibleCode
          text={entry.reqOpenai}
          className="code"
          title="받은 요청"
          onExpand={setFull}
        />
      </section>

      {/* ② 변환해서 보낸 요청 */}
      <section className="step">
        <div className="step__head">
          <span className="step__num step__num--2">2</span>
          <span className="step__title">변환해서 보낸 요청</span>
          <span className="step__hint">
            사내 형식 · 프록시 → {host}
            {entry.cached && ' · 캐시로 생략'}
          </span>
        </div>
        <CollapsibleCode
          text={entry.reqFabrix}
          className="code code--sent"
          title="변환해서 보낸 요청"
          modalClassName="code--sent"
          header={
            <span className="code__headers">
              {entry.method === 'POST' ? 'POST' : 'GET'} {pathOf(entry.fabrixUrl)} ·{' '}
              {entry.reqFabrixHeaders}
            </span>
          }
          onExpand={setFull}
        />
        {/* 변환까지 간 요청에만 매핑 칩을 붙입니다 — 파싱 단계에서 실패한
            요청에 "model → modelIds" 를 보여주면 하지도 않은 일을 한 것처럼 읽힙니다. */}
        {chat && entry.modelId !== null && <MappingTags entry={entry} />}
      </section>

      {/* ③ 돌려준 응답 */}
      <section className="step">
        <div className="step__head">
          <span className="step__num step__num--3">3</span>
          <span className="step__title">돌려준 응답</span>
          <span className="step__hint">
            {entry.isError
              ? '실패'
              : chat
                ? entry.stream
                  ? 'SSE 프레임을 합친 결과 전문'
                  : '전문'
                : (entry.summary ?? '')}
            {!entry.isError && !chat && '를 OpenAI 목록 형식으로'}
          </span>
        </div>
        <CollapsibleCode
          text={entry.respBody}
          className={`code ${respTone}`}
          title="돌려준 응답"
          modalClassName={respTone}
          footer={entry.respMeta ? <span className="meta-line">{entry.respMeta}</span> : undefined}
          onExpand={setFull}
        />
      </section>

      {!chat && !entry.isError && (
        <div className="footnote">
          클라이언트가 <code>gpt-4o</code> 처럼 사내에 없는 이름을 보내면 기본 모델로 처리하고,
          로그 2번 칸에 어떤 <code>modelId</code> 로 나갔는지 남깁니다.
        </div>
      )}

      {chat && entry.modelRequested && entry.modelAlias && entry.modelRequested !== entry.modelAlias && (
        <div className="footnote">
          클라이언트가 보낸 <code>{entry.modelRequested}</code> 는 사내에 없는 이름이라{' '}
          <code>{entry.modelAlias}</code>
          {entry.modelLabel ? ` (${entry.modelLabel})` : ''} 로 처리했습니다.
          {entry.modelId && (
            <>
              {' '}
              실제 <code>modelId</code> 는 <code>{entry.modelId}</code> 입니다.
            </>
          )}
        </div>
      )}

      {full && <FullTextModal open={full} onClose={() => setFull(null)} />}
    </div>
  )
}

/** 목업 ② 하단 칩 — 실제로 요청에 있던 필드만 보여줍니다. */
function MappingTags({ entry }: { entry: LogEntry }) {
  let request: Record<string, unknown> = {}
  try {
    request = JSON.parse(entry.reqOpenai) as Record<string, unknown>
  } catch {
    return null
  }

  const tags: { text: string; muted?: boolean }[] = [{ text: 'model → modelIds' }]
  if ('messages' in request) tags.push({ text: 'messages → systemPrompt + contents' })
  if ('stream' in request) tags.push({ text: 'stream → isStream' })
  // 사내 API 에 도구 필드가 없어 규약 텍스트로 접힙니다. 위 ② 칸의 systemPrompt
  // 안에서 실제로 접힌 결과를 그대로 볼 수 있습니다.
  if ('tools' in request || 'functions' in request) {
    tags.push({ text: 'tools → systemPrompt 규약' })
  }
  if ('tool_choice' in request) tags.push({ text: 'tool_choice → 규약 강도', muted: true })

  const llm: [string, string][] = [
    ['temperature', 'temperature'],
    ['top_p', 'top_p'],
    ['max_tokens', 'max_new_tokens'],
    ['max_completion_tokens', 'max_new_tokens'],
    ['seed', 'seed'],
    ['frequency_penalty', 'repetion_penalty'],
    ['top_k', 'tok_k'],
  ]
  for (const [from, to] of llm) {
    if (from in request) tags.push({ text: `${from} → llmConfig.${to}`, muted: true })
  }

  return (
    <div className="tags">
      {tags.map((tag) => (
        <span key={tag.text} className={`tag${tag.muted ? ' tag--muted' : ''}`}>
          {tag.text}
        </span>
      ))}
    </div>
  )
}

function hostOf(url: string): string {
  try {
    return new URL(url).host
  } catch {
    return url || '(주소 미설정)'
  }
}

function pathOf(url: string): string {
  try {
    return new URL(url).pathname
  } catch {
    return url
  }
}
