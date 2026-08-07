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

  /**
   * ③ 칸에서 곧장 열 사내 원문이 있는가.
   *
   * 비었는지까지 보는 이유: 사내에서 응답을 받기만 하면 상태 줄이라도 남습니다
   * (`fabrix::response_head`). 그러니 **비어 있다 = 사내를 부르지 못한 호출** 입니다 —
   * 토큰 거부·요청 검증 실패·연결 자체 실패. 그런 로그에 "사내 원문 보기" 를 달면
   * 하지도 않은 호출의 답을 보여주는 척이 됩니다.
   */
  const hasUpstreamOriginal = entry.raw.upstreamCaptured && entry.raw.upstream !== ''

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
            {hasUpstreamOriginal && ' · 가공 전 원문은 아래 버튼'}
          </span>
        </div>
        <CollapsibleCode
          text={entry.respBody}
          className={`code ${respTone}`}
          title="돌려준 응답"
          modalClassName={respTone}
          footer={entry.respMeta ? <span className="meta-line">{entry.respMeta}</span> : undefined}
          actions={
            hasUpstreamOriginal ? (
              <UpstreamOriginalButton text={entry.raw.upstream} onExpand={setFull} />
            ) : undefined
          }
          onExpand={setFull}
        />
      </section>

      {/* ④ 와이어 원문 — 채팅에만 붙습니다. ③ 칸은 이미 가공된 답변이라, "0자" 가
          모델이 말을 안 한 것인지 우리가 프레임을 못 읽은 것인지 여기서만 갈립니다. */}
      {chat && <RawWireStep entry={entry} onExpand={setFull} />}

      {!chat && !entry.isError && (
        <div className="footnote">
          여기 나온 <code>id</code> 가 클라이언트의 <code>model</code> 칸에 넣는 값입니다.
          사내에 없는 이름을 보내면 <code>404 model_not_found</code> 로 돌려줍니다.
        </div>
      )}

      {/* 요청 이름과 해석된 alias 가 다른 경우 — 이제 폴백이 아니라 UUID·대소문자
          매칭이 걸린 경우입니다. 없는 이름은 404 라 여기까지 오지 않습니다. */}
      {chat && entry.modelRequested && entry.modelAlias && entry.modelRequested !== entry.modelAlias && (
        <div className="footnote">
          클라이언트가 보낸 <code>{entry.modelRequested}</code> 는{' '}
          <code>{entry.modelAlias}</code>
          {entry.modelLabel ? ` (${entry.modelLabel})` : ''} 로 해석했습니다.
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

/**
 * ③ 칸에서 곧장 여는 "사내 원문" — 사내가 준 응답 바이트 그대로입니다.
 *
 * ③ 본문은 봉투를 벗기고 델타를 이어붙인 **가공된** 답변입니다. 답을 의심하게 되는
 * 자리가 바로 그 칸인데, 지금까지 원문은 ④ 칸까지 스크롤해야 나왔고 설정이 꺼져
 * 있으면 아예 없었습니다. 그래서 사내 쪽만은 언제나 기록하고(`logstore::RawWire`),
 * 그 버튼을 답변 바로 밑에 둡니다. 팝업·톤·본문 복사는 ④ 칸과 같은 것을 씁니다.
 */
function UpstreamOriginalButton({
  text,
  onExpand,
}: {
  text: string
  onExpand: (full: FullText) => void
}) {
  return (
    <button
      className="btn-mini"
      title="사내가 준 응답을 가공 없이 그대로 봅니다 (상태 줄 · 헤더 · 본문)"
      onClick={() =>
        onExpand({
          title: '돌려준 응답 · 사내가 준 원문',
          text,
          className: 'code--raw',
        })
      }
    >
      사내 원문 보기 ({text.length}자)
    </button>
  )
}

/**
 * ④ 와이어 원문 — 사내가 준 바이트와 클라이언트로 나간 바이트를 가공 없이 보여 줍니다.
 *
 * 두 쪽을 함께 두는 이유: 답변이 비었을 때 원인이 어느 쪽에 있는지가 한눈에 갈립니다.
 * 위쪽이 비어 있으면 사내가 아무것도 안 준 것이고, 위쪽에 글이 보이는데 아래쪽이
 * 비어 있으면 우리가 흘린 것입니다. "전체보기 → 본문 복사" 로 그대로 공유할 수 있습니다.
 */
function RawWireStep({
  entry,
  onExpand,
}: {
  entry: LogEntry
  onExpand: (full: FullText) => void
}) {
  const { upstreamCaptured, clientCaptured, upstream, client } = entry.raw

  return (
    <section className="step">
      <div className="step__head">
        <span className="step__num step__num--4">4</span>
        <span className="step__title">와이어 원문</span>
        <span className="step__hint">
          {clientCaptured
            ? '가공 전 · 사내가 준 바이트와 클라이언트로 나간 바이트'
            : '가공 전 · 사내가 준 바이트만 (나간 쪽은 기록이 꺼져 있습니다)'}
        </span>
      </div>

      {upstreamCaptured && (
        <CollapsibleCode
          text={upstream || '(사내가 아무 바이트도 주지 않았습니다)'}
          className="code code--raw"
          title="사내 원문"
          modalClassName="code--raw"
          header={<span className="code__headers">사내 → 프록시</span>}
          onExpand={onExpand}
        />
      )}

      {/* 아래쪽만 토글이 제어합니다. 위쪽이 있는데 아래쪽이 비면 우리가 흘린 것이고,
          꺼져서 없는 것은 그것과 뜻이 다르므로 문구로 갈라 둡니다. */}
      {clientCaptured ? (
        <CollapsibleCode
          text={client || '(클라이언트로 나간 본문이 없습니다)'}
          className="code code--raw"
          title="클라이언트로 나간 원문"
          modalClassName="code--raw"
          header={<span className="code__headers">프록시 → 클라이언트</span>}
          onExpand={onExpand}
        />
      ) : (
        <div className="footnote">
          설정의 <strong>와이어 원문 기록</strong>을 켜면 <strong>클라이언트로 나간</strong>{' '}
          본문도 이 칸에 가공 없이 남습니다. 위쪽에 글이 보이는데 아래쪽이 비어 있으면
          답변을 흘린 쪽은 프록시입니다 — 그 판단에는 두 쪽이 다 필요합니다.
        </div>
      )}
    </section>
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

  // 사내로 보낼 자리가 없어 **반영하지 않은** 필드들. 조용히 사라지는 것과
  // 사라졌다고 말하는 것의 차이라 흐리게라도 반드시 보여줍니다.
  if ('stream_options' in request) {
    tags.push({ text: 'stream_options.include_usage → usage 청크(추정)', muted: true })
  }
  const ignored: [string, string][] = [
    ['stop', 'stop → 무시(사내에 대응 없음)'],
    ['presence_penalty', 'presence_penalty → 무시(페널티 키가 하나뿐)'],
    ['logit_bias', 'logit_bias → 무시(토크나이저 없음)'],
    ['user', 'user → 무시'],
    ['response_format', 'response_format → 무시'],
    ['metadata', 'metadata → 무시'],
  ]
  for (const [key, text] of ignored) {
    if (key in request && request[key] !== null) tags.push({ text, muted: true })
  }
  // 이미지 파트는 사내 채팅 API 가 받지 못해 버립니다. 몇 개를 버렸는지는 ③ 칸
  // 꼬리의 메타 줄에 있습니다.
  if (hasImageParts(request)) {
    tags.push({ text: 'image_url → 제외(사내 미지원)', muted: true })
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

/** `messages[].content` 배열 안에 이미지 파트가 있는가 (Rust `Message::image_parts` 와 같은 기준). */
function hasImageParts(request: Record<string, unknown>): boolean {
  const messages = request.messages
  if (!Array.isArray(messages)) return false
  return messages.some((m) => {
    const content = (m as { content?: unknown }).content
    if (!Array.isArray(content)) return false
    return content.some((part) => {
      if (typeof part !== 'object' || part === null) return false
      const p = part as Record<string, unknown>
      if (typeof p.type === 'string' && ['image_url', 'input_image', 'image'].includes(p.type)) {
        return true
      }
      if ('image_url' in p) return true
      const media = p.mediaType ?? p.media_type
      return typeof media === 'string' && media.startsWith('image/')
    })
  })
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
