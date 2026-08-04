// 개발용 FabriX 스텁. 의존성 0 — Node 내장 http 만 씁니다.
//
//   npm run mock
//
// 스펙 문서에 SSE 프레임 형식이 없어 프록시의 파서를 방어적으로 짰습니다.
// 이 서버는 그 방어 경로를 **강제로 골라 실행**하기 위한 것입니다.
//
//   MOCK_PORT=9900          포트
//   MOCK_CASE=camel|snake   스트림 필드 표기 (기본 snake — 문서상 스트림은 스네이크)
//   MOCK_STREAM=delta|cumulative   content 가 증분인지 누적인지
//   MOCK_RAW=1              `data: ` 접두 없이 개행 구분 JSON 으로 흘림
//   MOCK_FAIL=429|500|timeout|midstream   실패 경로 재현
//   MOCK_DELAY=40           프레임 간 지연(ms)
//   MOCK_TOOLCALL=single|parallel|prose|malformed|unknown|fenced
//                           답변 본문에 <tool_call> 블록을 섞습니다 (기본 off).
//                           single/parallel/prose 는 도구로 잡혀야 하고,
//                           malformed/unknown/fenced 는 원문 텍스트로 되돌아와야 합니다.
//   MOCK_CHUNK=7            프레임당 글자 수. 3 이나 1 로 낮추면 <tool_call> 센티널이
//                           여러 프레임에 걸쳐 쪼개집니다 — 파서가 깨지는 지점.
//   MOCK_SPLITBYTES=1       SSE 한 줄을 임의 바이트 지점에서 두 번에 나눠 write
//   MOCK_NOSTREAM=llm|rag|filter   비스트림 응답 형태 (기본 llm)
//                            llm    = content 에 답변
//                            rag    = content 는 null, 답변을 contentReferences[].answer 에
//                            filter = content 는 null, filterBlockReason 에 차단 사유
//
// 조합 예시:
//   MOCK_CASE=camel MOCK_STREAM=cumulative npm run mock
//   MOCK_NOSTREAM=rag npm run mock

import { createServer } from 'node:http'

const PORT = Number(process.env.MOCK_PORT ?? 9900)
const CASE = process.env.MOCK_CASE === 'camel' ? 'camel' : 'snake'
const MODE = process.env.MOCK_STREAM === 'cumulative' ? 'cumulative' : 'delta'
const RAW = process.env.MOCK_RAW === '1'
const FAIL = process.env.MOCK_FAIL ?? ''
const DELAY = Number(process.env.MOCK_DELAY ?? 40)
const NOSTREAM = ['rag', 'filter'].includes(process.env.MOCK_NOSTREAM)
  ? process.env.MOCK_NOSTREAM
  : 'llm'

const CLIENT_HEADER = 'x-fabrix-client'
const TOKEN_HEADER = 'x-openapi-token'

const MODELS = [
  { id: '0196f1fc-2858-70a9-a232-74dbddb971d0', ko: '챗 4', en: 'Chat 4', desc: '범용 대화 모델' },
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f70', ko: '라이트', en: 'Chat Lite', desc: '빠른 응답용 경량 모델' },
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f71', ko: '코드', en: 'Code', desc: '코드 생성/리뷰' },
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f72', ko: '요약', en: 'Summarize', desc: '문서 요약' },
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f73', ko: '번역', en: 'Translate', desc: '한↔영 번역' },
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f74', ko: '임베딩', en: 'Embed', desc: '문서 임베딩' },
  // 한글 이름만 있는 모델 — 프록시의 UUID 기반 alias 폴백을 태웁니다.
  { id: '01970a3b-91d4-7c8e-9a11-2f3c4d5e6f75', ko: '사내규정', en: null, desc: '사내 규정 특화' },
]

const ANSWER =
  '시연차는 입사 1년 차에 15일이 부여되며, 미사용분은 다음 해로 최대 5일까지 이월할 수 있습니다. ' +
  '이월분은 이월된 해의 12월 31일까지 사용해야 하고, 그 이후에는 소멸합니다.'

// 도구 호출 에뮬레이션 검증용 본문들.
//
// 프록시는 답변에서 <tool_call> 블록을 걷어내 OpenAI tool_calls 로 조립합니다.
// 그 파서가 깨지는 곳은 거의 언제나 **프레임 경계**라, 이 본문을 MOCK_CHUNK 로 잘게
// 쪼개 센티널 한가운데를 가르는 것이 이 모드의 목적입니다.
const TOOL_CALL_BODIES = {
  off: () => ANSWER,
  // 가장 흔한 모양 — 짧은 설명 뒤에 파일 쓰기 한 건.
  single: () =>
    '만들겠습니다.\n<tool_call>\n' +
    JSON.stringify({
      name: 'write',
      arguments: { filePath: 'index.html', content: '<!doctype html>\n<html>…</html>' },
    }) +
    '\n</tool_call>',
  // 병렬 호출 — index 0,1 로 나뉘어야 합니다.
  parallel: () =>
    '<tool_call>\n' +
    JSON.stringify({ name: 'write', arguments: { filePath: 'a.html', content: 'a' } }) +
    '\n</tool_call>\n<tool_call>\n' +
    JSON.stringify({ name: 'read', arguments: { filePath: 'b.css' } }) +
    '\n</tool_call>',
  // 호출 뒤에도 말을 잇는 경우 — 뒤 텍스트가 살아 있어야 합니다.
  prose: () =>
    '먼저 읽어 보겠습니다.\n<tool_call>\n' +
    JSON.stringify({ name: 'read', arguments: { filePath: 'b.css' } }) +
    '\n</tool_call>\n' +
    `그리고 이어서 설명합니다. ${ANSWER}`,
  // 아래 셋은 전부 "도구가 아님" 으로 판정되어 원문 텍스트로 되돌아와야 합니다.
  malformed: () => '<tool_call>\n{"name":"write", 여기가 깨진 JSON}\n</tool_call>',
  unknown: () =>
    '<tool_call>\n' +
    JSON.stringify({ name: 'definitely_not_a_tool', arguments: {} }) +
    '\n</tool_call>',
  // 규약이 금지한 코드 펜스 안 호출. 프록시는 걷어내지만, 클라이언트가 이걸 도구로
  // 읽지 않는다는 점을 눈으로 확인하려고 남겨 둡니다.
  fenced: () =>
    '```json\n<tool_call>\n' +
    JSON.stringify({ name: 'read', arguments: { filePath: 'b.css' } }) +
    '\n</tool_call>\n```',
}

const BODY = (TOOL_CALL_BODIES[process.env.MOCK_TOOLCALL] ?? TOOL_CALL_BODIES.off)()
const CHUNK = Math.max(1, Number(process.env.MOCK_CHUNK ?? 7))
const SPLITBYTES = process.env.MOCK_SPLITBYTES === '1'

const log = (...args) => console.log('[mock-fabrix]', ...args)

function json(res, status, body) {
  const payload = JSON.stringify(body)
  res.writeHead(status, {
    'Content-Type': 'application/json; charset=utf-8',
    'Content-Length': Buffer.byteLength(payload),
  })
  res.end(payload)
}

function authorized(req) {
  return Boolean(req.headers[CLIENT_HEADER]) && Boolean(req.headers[TOKEN_HEADER])
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = []
    req.on('data', (c) => chunks.push(c))
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    req.on('error', reject)
  })
}

/** 문서상 스트림은 snake_case, 비스트림은 camelCase. 둘 다 강제할 수 있게 합니다. */
function shape(fields) {
  const map = {
    modelType: 'model_type',
    finishReason: 'finish_reason',
    responseCode: 'response_code',
    eventStatus: 'event_status',
    eventData: 'event_data',
    reasoningContent: 'reasoning_content',
  }
  const out = {}
  for (const [key, value] of Object.entries(fields)) {
    if (value === undefined) continue
    out[CASE === 'snake' && map[key] ? map[key] : key] = value
  }
  return out
}

function handleModels(req, res) {
  if (!authorized(req)) {
    return json(res, 401, { message: '인증 헤더가 없습니다' })
  }
  if (FAIL === '500') return json(res, 500, { message: '사내 서버 내부 오류(모의)' })

  const data = MODELS.map((m) => ({
    modelId: m.id,
    name: [
      { languageCode: 'ko', content: m.ko },
      ...(m.en ? [{ languageCode: 'en', content: m.en }] : []),
    ],
    description: [{ languageCode: 'ko', content: m.desc }],
  }))
  log(`GET /openapi/chat/v1/models → ${data.length}개`)
  json(res, 200, { data })
}

async function handleMessages(req, res) {
  if (!authorized(req)) {
    return json(res, 401, { message: '인증 헤더가 없습니다' })
  }

  const raw = await readBody(req)
  let body = {}
  try {
    body = JSON.parse(raw)
  } catch {
    return json(res, 400, { message: 'JSON 파싱 실패' })
  }

  log('POST /openapi/chat/v1/messages', JSON.stringify({
    modelIds: body.modelIds,
    isStream: body.isStream,
    systemPrompt: body.systemPrompt ? `${body.systemPrompt.slice(0, 24)}…` : undefined,
    contents: (body.contents ?? []).map((c) => `${String(c).slice(0, 24)}…`),
    llmConfig: body.llmConfig,
  }))

  if (FAIL === '429') return json(res, 429, { message: '사내 쿼터를 초과했습니다(모의)' })
  if (FAIL === '500') return json(res, 500, { message: '사내 서버 내부 오류(모의)' })
  if (FAIL === 'timeout') {
    // 응답을 아예 주지 않습니다 → 프록시의 30초 타임아웃 경로.
    log('MOCK_FAIL=timeout — 응답을 보류합니다')
    return
  }

  const modelType = MODELS.find((m) => m.id === body.modelIds?.[0])?.en ?? 'unknown'

  if (!body.isStream) {
    // 실제 FabriX 비스트림 응답 스키마를 재현합니다(mock 이 그동안 흉내 내지 않던 필드 포함).
    // 순수 LLM 답변은 content 에, 플러그인/RAG 답변은 contentReferences[].answer 에 옵니다.
    const isRag = NOSTREAM === 'rag'
    const isFilter = NOSTREAM === 'filter'
    const filterBlockReason = isFilter
      ? {
          ko: '입력에 부적절한 표현이 포함되어 응답이 차단되었습니다(모의).',
          en: null,
          policy_id: 'MOCK-POLICY',
          message: null,
          result_code: 'FR-403',
          filter_log_id: null,
        }
      : { ko: null, en: null, policy_id: null, message: null, result_code: 'FR-200', filter_log_id: null }
    return json(res, 200, {
      userId: '00000000-0000-0000-0000-000000000000',
      modelType,
      content: isRag || isFilter ? null : BODY,
      reasoningContent: null,
      processingContent: [],
      contentReferences: isRag
        ? [{ plugin: 'RAG', answer: BODY, references: [], argumented_standalone_queries: '' }]
        : [],
      truncated: false,
      finishReason: null,
      filterBlockReason,
      status: 'SUCCESS',
      responseCode: 'R20000',
      plugins: [isRag ? 'RAG' : 'LLM'],
      orchestratorType: null,
      actions: [],
      eventStatus: 'CHUNK',
      eventData: '',
    })
  }

  if (process.env.MOCK_TOOLCALL) {
    const injected = String(body.systemPrompt ?? '').includes('<tool_call>')
    log(`systemPrompt 에 도구 규약 주입됨: ${injected}`)
  }

  res.writeHead(200, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache',
    Connection: 'keep-alive',
  })

  // 의도적으로 한글 경계를 무시하고 잘라, 멀티바이트가 청크 경계에 걸리게 합니다.
  // MOCK_CHUNK 를 낮추면 <tool_call> 센티널도 여러 프레임에 걸쳐 쪼개집니다.
  const pieces = []
  for (let i = 0; i < BODY.length; i += CHUNK) pieces.push(BODY.slice(i, i + CHUNK))

  const send = (obj) => {
    const line = JSON.stringify(obj)
    const frame = RAW ? `${line}\n` : `data: ${line}\n\n`
    if (!SPLITBYTES) return res.write(frame)
    // SSE 한 줄을 임의 바이트 지점에서 두 번에 나눠 씁니다 — 디코더가 줄 단위로만
    // 자르는지, 멀티바이트 문자가 write 경계에 걸려도 견디는지 봅니다.
    const buf = Buffer.from(frame, 'utf8')
    const cut = 1 + Math.floor(Math.random() * Math.max(1, buf.length - 2))
    res.write(buf.subarray(0, cut))
    res.write(buf.subarray(cut))
  }

  let sent = ''
  let index = 0

  const tick = () => {
    if (res.writableEnded) return

    if (FAIL === 'midstream' && index === 4) {
      log('MOCK_FAIL=midstream — 스트림 중간에 오류를 흘립니다')
      send(shape({ status: 'ERROR', eventData: '사내 처리 중 오류가 발생했습니다(모의)' }))
      res.end()
      return
    }

    if (index >= pieces.length) {
      send(shape({ modelType, content: '', finishReason: 'stop', status: 'SUCCESS' }))
      if (!RAW) res.write('data: [DONE]\n\n')
      res.end()
      log(`스트림 종료 · ${pieces.length}프레임 · ${CASE}/${MODE}${RAW ? '/raw' : ''}`)
      return
    }

    sent += pieces[index]
    send(shape({ modelType, content: MODE === 'cumulative' ? sent : pieces[index] }))
    index += 1
    setTimeout(tick, DELAY)
  }

  // 첫 토큰 지연을 흉내 냅니다.
  setTimeout(tick, 300)
}

const server = createServer((req, res) => {
  const url = new URL(req.url ?? '/', `http://${req.headers.host}`)

  if (req.method === 'GET' && url.pathname === '/openapi/chat/v1/models') {
    return handleModels(req, res)
  }
  if (req.method === 'POST' && url.pathname === '/openapi/chat/v1/messages') {
    return handleMessages(req, res)
  }
  json(res, 404, { message: `모르는 경로: ${req.method} ${url.pathname}` })
})

server.listen(PORT, '127.0.0.1', () => {
  log(`http://127.0.0.1:${PORT}`)
  log(`표기 ${CASE} · content ${MODE}${RAW ? ' · raw(개행 구분)' : ' · SSE(data:)'}${FAIL ? ` · FAIL=${FAIL}` : ''}`)
  log('프록시 온보딩에 이 주소를 넣고, 인증키/토큰은 아무 값이나 채우세요.')
})
