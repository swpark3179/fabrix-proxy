// 사내(FabriX) 서버를 **프록시 없이 직접** 두드리는 진단 스크립트. 의존성 0 — Node 내장만 씁니다.
//
//   npm run probe               모든 항목
//   npm run probe -- turns      이름으로 골라서 (부분 일치)
//   npm run probe -- sweep      systemPrompt 길이를 늘려 가며 어디서 끊기는지
//   npm run probe -- --raw      사내가 준 프레임 원문도 함께
//
// 왜 필요한가: opencode 를 통해 보면 프록시·클라이언트·모델이 한 줄에 서 있어, 답이
// 안 올 때 누구 탓인지 알 수 없습니다. 이 스크립트는 프록시를 빼고 **변인 하나씩만**
// 바꾼 요청을 사내에 직접 보내, 어떤 모양에서 답이 끊기는지 표로 보여 줍니다.
//
// 인증 정보는 `~/.fabrix-proxy/config.json` 에서 읽습니다 — 키를 손으로 복사해 명령줄에
// 붙이지 않기 위함입니다(셸 히스토리에 남습니다).
//
//   --model=<UUID>   쓸 모델. 없으면 목록의 첫 모델.
//   --timeout=<초>   한 요청을 기다릴 시간 (기본 90 — 프록시의 read 타임아웃과 같게).
//   --raw            프레임 원문을 앞에서 20줄까지 찍습니다.
//   --sysbytes=<수>  `system` 계열 항목이 쓸 systemPrompt 크기 (기본 14000 — opencode 실측).

import { readFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

const MODELS_PATH = '/openapi/chat/v1/models'
const MESSAGES_PATH = '/openapi/chat/v1/messages'

const args = process.argv.slice(2)
const flag = (name, fallback) => {
  const hit = args.find((a) => a.startsWith(`--${name}=`))
  return hit ? hit.slice(name.length + 3) : fallback
}
const RAW = args.includes('--raw')
const TIMEOUT = Number(flag('timeout', 90)) * 1000
const SYS_BYTES = Number(flag('sysbytes', 14000))
const WANTED = args.filter((a) => !a.startsWith('--'))

// ─────────────────────────── 설정 읽기 ───────────────────────────

function loadConfig() {
  const path = join(homedir(), '.fabrix-proxy', 'config.json')
  let raw
  try {
    raw = readFileSync(path, 'utf8')
  } catch {
    console.error(`설정을 찾지 못했습니다: ${path}`)
    console.error('앱을 한 번 실행해 연결 정보를 저장한 뒤 다시 시도하세요.')
    process.exit(1)
  }
  const cfg = JSON.parse(raw.replace(/^﻿/, ''))
  if (!cfg.fabrixBaseUrl || !cfg.fabrixClient || !cfg.openapiToken) {
    console.error('연결 정보(주소·인증키·토큰)가 비어 있습니다.')
    process.exit(1)
  }
  // 앱의 `normalized_base_url` 과 같은 정리 — 끝의 / 와 실수로 붙인 /openapi 경로.
  let base = cfg.fabrixBaseUrl.trim().replace(/\/+$/, '')
  const idx = base.indexOf('/openapi')
  if (idx >= 0) base = base.slice(0, idx)
  return { ...cfg, base }
}

const cfg = loadConfig()
if (cfg.insecureSkipVerify) {
  // 앱의 "TLS 검증 건너뛰기" 와 같은 뜻입니다. 켜져 있을 때만 따라갑니다.
  process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0'
  console.log('⚠ TLS 인증서 검증을 건너뜁니다 (설정의 insecureSkipVerify=true)')
}

const headers = {
  'Content-Type': 'application/json',
  'x-fabrix-client': cfg.fabrixClient,
  'x-openapi-token': cfg.openapiToken,
}

// ─────────────────────────── 시험 재료 ───────────────────────────

/** opencode 시스템 프롬프트만 한 크기의 영어 지시문. 길이만 흉내 냅니다. */
function filler(bytes) {
  const para =
    'You are an interactive CLI tool that helps users with software engineering tasks. ' +
    'Be concise, direct, and to the point. Never guess URLs. Follow existing code conventions. ' +
    'Verify your work with tests when possible and keep responses short. '
  let out = ''
  while (out.length < bytes) out += para
  return out.slice(0, bytes)
}

/** 프록시가 실제로 주입하는 규약 블록의 축약본(도구 2개). 형식은 `proxy/tools.rs` 와 같습니다. */
const TOOL_BLOCK = [
  '# Tool calling',
  '',
  'You can call tools. Each line inside <tools> is one tool: its name, what it does, ' +
    'and a JSON Schema for its arguments.',
  '',
  '<tools>',
  JSON.stringify({
    name: 'write',
    description: 'Write a file',
    parameters: {
      type: 'object',
      properties: { filePath: { type: 'string' }, content: { type: 'string' } },
    },
  }),
  JSON.stringify({
    name: 'list',
    description: 'List a directory',
    parameters: { type: 'object', properties: { path: { type: 'string' } } },
  }),
  '</tools>',
  '',
  'To call a tool, emit a block in exactly this form:',
  '',
  '<tool_call>',
  '{"name": "<one of the names above>", "arguments": {<object matching that tool\'s schema>}}',
  '</tool_call>',
  '',
  '- If no tool is needed, just answer. Emit no <tool_call> block.',
].join('\n')

const SHORT_Q = '한 문장으로 자기소개를 해 주세요.'
const LONG_Q = '현재 폴더 목록을 담고 있는 md 파일의 내용을 만들어 주세요. 30줄 정도면 됩니다.'

/** 프록시가 `contents` 꼬리에 실제로 덧붙이는 리마인더(축약본). `proxy/tools.rs` 와 같은 글입니다. */
const TAIL_REMINDER = [
  '# Reminder',
  'To use a tool, emit a block exactly like ' +
    '<tool_call>{"name": "<one of the names in <tools>>", "arguments": {…}}</tool_call> ' +
    '— one block per call, no Markdown code fence, and nothing but JSON inside the block. ' +
    'Put the block in your reply itself, not only in your private reasoning.',
  'If no tool is needed, just answer.',
].join('\n')

/**
 * 센티널 되읽기 시험 한 쌍.
 *
 * 프록시는 `<tool_call>…</tool_call>` 을 도구 호출의 센티널로 씁니다. 그 모양을 고른
 * 이유는 Qwen·Hermes·GLM 계열의 **네이티브** 툴콜 형식이기 때문인데, 같은 이유로
 * 게이트웨이(vLLM 의 `--tool-call-parser` 등)가 그 블록을 자기 것으로 알고 응답 본문에서
 * **걷어내 버릴** 수 있습니다. 그러면 우리에게 도착하는 `content` 는 정확히 0자입니다 —
 * 모델은 도구를 불렀는데 프록시는 아무 말도 못 들은 상태가 됩니다.
 *
 * 그 갈래를 확정하려면 변인이 태그 하나여야 합니다. `sentinel-echo` 는 `<tool_call>` 을,
 * `sentinel-control` 은 아무도 파싱하지 않는 `<demo_block>` 을 그대로 따라 적게 시킵니다.
 * 둘 다 같은 문장, 같은 길이입니다.
 *
 * - 둘 다 ✔ → 센티널은 와이어를 통과합니다. 빈 응답의 원인은 다른 데 있습니다.
 * - control 만 ✔ → **게이트웨이가 `<tool_call>` 을 먹습니다.** 프롬프트 기반 툴콜은 이
 *   센티널로는 성립하지 않습니다 — 다른 센티널을 쓰거나 사내 네이티브 툴콜을 받아야 합니다.
 * - 둘 다 △ → 태그와 무관합니다. 모델이 따라 적기를 거부한 것이니 이 시험은 판단 근거가
 *   못 됩니다(다른 항목으로 가르세요).
 */
function echoCase(tag) {
  return (
    `다음 한 줄을 **그대로** 따라 적어 주세요. 설명하지 말고 그 줄만 출력하세요.\n` +
    `<${tag}>{"name": "write", "arguments": {"filePath": "a.md"}}</${tag}>`
  )
}

/**
 * 시험 항목. 하나가 변인 하나입니다 — 두 항목의 차이가 하나뿐이어야 결과를 읽을 수 있습니다.
 *
 * 순서는 "확실히 되는 것 → 의심스러운 것" 입니다. 앞이 실패하면 뒤는 볼 필요가 없습니다.
 */
function variants(modelId) {
  const base = { modelIds: [modelId], isStream: true }
  const sys = filler(SYS_BYTES)
  return [
    {
      name: 'baseline',
      why: '기준선 — systemPrompt 없음 · 1턴 · llmConfig 없음',
      body: { ...base, contents: [SHORT_Q] },
    },
    {
      name: 'system',
      why: `큰 systemPrompt(${SYS_BYTES}자) + 1턴 — 프롬프트 크기만 다름`,
      body: { ...base, contents: [SHORT_Q], systemPrompt: sys },
    },
    {
      name: 'turns',
      why: '3턴 교대 — 배열이 정말 턴인지. `B` 라고 답하면 턴 교대가 맞습니다',
      body: {
        ...base,
        contents: ['"B"라고만 답해', 'B', '네가 방금 뭐라고 답했지? 한 단어로.'],
      },
    },
    {
      name: 'system+turns',
      why: '큰 systemPrompt + 3턴 — **응답없는 프롬프트와 같은 모양**',
      body: {
        ...base,
        contents: ['테스트 요청입니다.', '안녕하세요! 무엇을 도와드릴까요?', SHORT_Q],
        systemPrompt: sys,
      },
    },
    {
      name: 'maxtokens-32000',
      why: 'opencode 가 보내는 값 — 상한을 넘으면 여기서 끊깁니다',
      body: { ...base, contents: [SHORT_Q], llmConfig: { max_new_tokens: 32000 } },
    },
    {
      name: 'maxtokens-2048',
      why: '같은 요청에 보수적인 상한 — 위가 실패하고 이게 되면 상한이 범인입니다',
      body: { ...base, contents: [SHORT_Q], llmConfig: { max_new_tokens: 2048 } },
    },
    {
      name: 'long-output',
      why: '긴 출력 요구 — 첫 토큰이 늦는지, 아예 안 오는지',
      body: { ...base, contents: [LONG_Q] },
    },
    {
      name: 'toolblock',
      why: '도구 규약을 주입한 1턴 — 규약 자체가 답을 막는지',
      body: {
        ...base,
        contents: ['현재 폴더 목록을 파일로 적어 줘.'],
        systemPrompt: `${sys}\n\n${TOOL_BLOCK}`,
      },
    },
    {
      name: 'sentinel-control',
      why: '아무도 파싱하지 않는 태그를 따라 적게 — 되읽기 시험의 대조군',
      body: { ...base, contents: [echoCase('demo_block')] },
    },
    {
      name: 'sentinel-echo',
      why: '같은 문장에 태그만 <tool_call> — 위는 ✔ 인데 여기가 △ 면 게이트웨이가 센티널을 먹습니다',
      body: { ...base, contents: [echoCase('tool_call')] },
    },
    {
      name: 'opencode',
      why: '프록시가 실제로 보내는 모양 그대로 — 재현되면 위 항목들로 변인을 좁힙니다',
      body: {
        ...base,
        contents: [`현재 폴더 목록을 담은 md 파일을 만들어 줘.\n\n${TAIL_REMINDER}`],
        systemPrompt: `${sys}\n\n${TOOL_BLOCK}`,
        llmConfig: { max_new_tokens: 32000, temperature: 1 },
      },
    },
  ]
}

// ─────────────────────────── 한 건 보내기 ───────────────────────────

/** SSE 본문을 읽으며 첫 바이트 지연·프레임 수·글자 수를 셉니다. */
async function probe(label, body) {
  const started = Date.now()
  let firstByte = null
  const frames = []
  let content = ''
  let reasoning = ''
  let finish = null
  let status = null
  let undecodable = 0
  let httpStatus = 0
  let error = null

  try {
    const res = await fetch(`${cfg.base}${MESSAGES_PATH}`, {
      method: 'POST',
      headers: { ...headers, Accept: 'text/event-stream' },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(TIMEOUT),
    })
    httpStatus = res.status
    if (!res.ok) {
      error = (await res.text()).slice(0, 300)
    } else {
      const decoder = new TextDecoder()
      let buf = ''
      for await (const chunk of res.body) {
        firstByte ??= Date.now() - started
        buf += decoder.decode(chunk, { stream: true })
        let nl
        while ((nl = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, nl).trim()
          buf = buf.slice(nl + 1)
          if (!line || line.startsWith(':')) continue
          if (!line.startsWith('data:')) {
            frames.push(line)
            continue
          }
          const payload = line.slice(5).trim()
          if (!payload || payload === '[DONE]') {
            if (payload) frames.push(line)
            continue
          }
          frames.push(line)
          let chunkJson
          try {
            chunkJson = JSON.parse(payload)
          } catch {
            undecodable += 1
            continue
          }
          content += chunkJson.content ?? ''
          reasoning += chunkJson.reasoningContent ?? chunkJson.reasoning_content ?? ''
          finish = chunkJson.finishReason ?? chunkJson.finish_reason ?? finish
          status = chunkJson.status ?? status
        }
      }
    }
  } catch (err) {
    error = err.name === 'TimeoutError' ? `${TIMEOUT / 1000}초 안에 끝나지 않음` : String(err)
  }

  const total = Date.now() - started
  return {
    label,
    httpStatus,
    firstByte,
    total,
    frames: frames.length,
    chars: content.length,
    reasoningChars: reasoning.length,
    finish,
    status,
    undecodable,
    error,
    raw: frames,
    answer: content,
  }
}

function report(r, why) {
  const ok = !r.error && r.chars + r.reasoningChars > 0
  const mark = r.error ? '✖' : ok ? '✔' : '△'
  console.log(`\n${mark} ${r.label}`)
  console.log(`   ${why}`)
  if (r.error) {
    console.log(`   HTTP ${r.httpStatus || '—'} · ${r.error}`)
  } else {
    console.log(
      `   HTTP ${r.httpStatus} · 첫 바이트 ${r.firstByte ?? '—'}ms · 총 ${r.total}ms · ` +
        `프레임 ${r.frames}개 · 본문 ${r.chars}자 · 추론 ${r.reasoningChars}자 · ` +
        `finishReason ${r.finish ?? 'null'} · status ${r.status ?? 'null'}` +
        (r.undecodable ? ` · 해석 못한 프레임 ${r.undecodable}개` : ''),
    )
    if (ok) console.log(`   답변 앞부분: ${r.answer.slice(0, 60).replace(/\n/g, ' ')}…`)
    else console.log('   ⚠ 답변이 비었습니다 — 이 모양이 문제의 모양입니다.')
  }
  if (RAW && r.raw.length) {
    console.log('   ── 프레임 원문 (앞 20줄) ──')
    for (const line of r.raw.slice(0, 20)) console.log(`   ${line.slice(0, 200)}`)
  }
}

// ─────────────────────────── systemPrompt 길이 훑기 ───────────────────────────

/** 어느 길이에서 답이 끊기는지 — "프롬프트가 길어서 그렇다" 를 확정하거나 지웁니다. */
async function sweep(modelId) {
  console.log('\n== systemPrompt 길이 훑기 ==')
  for (const bytes of [2000, 8000, 16000, 32000, 64000]) {
    const r = await probe(`sys ${bytes}자`, {
      modelIds: [modelId],
      isStream: true,
      contents: [SHORT_Q],
      systemPrompt: filler(bytes),
    })
    report(r, `systemPrompt ${bytes}자 · 1턴`)
    if (r.error || r.chars + r.reasoningChars === 0) {
      console.log(`\n→ ${bytes}자에서 끊깁니다. 그 아래 값이 실질 상한입니다.`)
      return
    }
  }
  console.log('\n→ 64000자까지 모두 답했습니다. 프롬프트 길이는 범인이 아닙니다.')
}

// ─────────────────────────── 실행 ───────────────────────────

async function pickModel() {
  const asked = flag('model', '')
  if (asked) return asked
  const res = await fetch(`${cfg.base}${MODELS_PATH}`, {
    headers: { ...headers, Accept: 'application/json' },
    signal: AbortSignal.timeout(30_000),
  })
  if (!res.ok) {
    console.error(`모델 목록 조회 실패: HTTP ${res.status}`)
    process.exit(1)
  }
  const body = await res.json()
  const list = body.data ?? body.result ?? body.models ?? body
  const first = Array.isArray(list) ? list[0] : null
  if (!first?.modelId) {
    console.error('모델 목록에서 modelId 를 찾지 못했습니다. --model=<UUID> 로 지정하세요.')
    process.exit(1)
  }
  return first.modelId
}

const modelId = await pickModel()
console.log(`대상: ${cfg.base} · 모델 ${modelId} · 타임아웃 ${TIMEOUT / 1000}초`)

if (WANTED.includes('sweep')) {
  await sweep(modelId)
} else {
  const list = variants(modelId).filter(
    (v) => WANTED.length === 0 || WANTED.some((w) => v.name.includes(w)),
  )
  if (list.length === 0) {
    console.error(`이름이 맞는 항목이 없습니다: ${WANTED.join(' ')}`)
    process.exit(1)
  }
  for (const v of list) {
    report(await probe(v.name, v.body), v.why)
  }
  console.log(
    '\n읽는 법: ✔ 는 답이 온 것, △ 는 200 인데 **빈 답**, ✖ 는 오류나 시간 초과입니다.\n' +
      '이웃한 두 항목의 차이가 곧 원인입니다 — 예를 들어 `system` 이 ✔ 인데 `system+turns` 가 △ 면\n' +
      'contents 턴 배열이 범인입니다.\n' +
      '`sentinel-control` 이 ✔ 인데 `sentinel-echo` 가 △ 면 게이트웨이가 <tool_call> 을 걷어냅니다 —\n' +
      '그 경우 프롬프트 기반 툴콜은 이 센티널로 성립하지 않습니다.',
  )
}
