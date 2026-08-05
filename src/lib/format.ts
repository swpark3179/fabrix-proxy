import type { LogEntry, ModelRow } from '../types'

/** 목업 표기: `1.2s` · `0.2s` · `30s` */
export function latency(ms: number): string {
  if (ms >= 10_000) return `${Math.round(ms / 1000)}s`
  return `${(ms / 1000).toFixed(1)}s`
}

export type Tone = 'ok' | 'warn' | 'error'

export function tone(entry: Pick<LogEntry, 'status' | 'isError'>): Tone {
  if (entry.status >= 500 || (entry.isError && entry.status < 400)) return 'error'
  if (entry.status >= 400) return 'warn'
  return entry.isError ? 'error' : 'ok'
}

/** 목록 두 번째 줄: `gpt-4o · stream` / `7개 반환` / `캐시 히트` */
export function subtitle(entry: LogEntry): string {
  if (entry.note) return entry.note
  if (entry.kind === 'models') return entry.client ?? '모델 목록'
  const model = entry.modelAlias ?? entry.modelRequested ?? '기본 모델'
  return entry.stream ? `${model} · stream` : model
}

/** 목록 첫 번째 줄: 경로에서 `/v1/` 를 뗀 짧은 이름 */
export function shortPath(entry: LogEntry): string {
  return entry.path.replace(/^\/v1\//, '')
}

/** 상세의 `cURL 복사` — 받은 요청을 그대로 재현합니다. */
export function toCurl(entry: LogEntry, baseUrl: string): string {
  const origin = baseUrl.replace(/\/v1\/?$/, '')
  const url = `${origin}${entry.path}`

  if (entry.kind === 'models') {
    return `curl ${url} \\\n  -H "Authorization: Bearer dummy"`
  }

  let body = entry.reqOpenai
  try {
    body = JSON.stringify(JSON.parse(entry.reqOpenai))
  } catch {
    // 파싱 실패 시 원문 그대로 — 어차피 사람이 보고 고칠 수 있습니다.
  }
  // 싱글쿼터로 감싸므로 본문 안의 싱글쿼터만 탈출시킵니다.
  const escaped = body.replace(/'/g, `'\\''`)
  return [
    `curl ${url} \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -H "Authorization: Bearer dummy" \\`,
    `  -d '${escaped}'`,
  ].join('\n')
}

export async function copyText(text: string): Promise<void> {
  const { writeText } = await import('@tauri-apps/plugin-clipboard-manager')
  await writeText(text)
}

/**
 * 모델 목록을 채팅창·설정 파일에 그대로 붙일 수 있는 평문으로.
 *
 * `label` 을 **맨 끝**에 두는 이유: 한글은 모노 폰트에서도 두 칸을 먹어 `padEnd` 정렬이
 * 깨집니다. 마지막 칸이면 깨질 것이 없습니다.
 *
 * 넘겨받은 행만 담습니다 — 화면이 필터를 걸었으면 보이는 것만 복사되는 것이 맞습니다.
 */
export function modelsToPlainText(rows: ModelRow[], sourceUrl: string, fetchedAt: string): string {
  const when = fetchedAt.replace('T', ' ').slice(0, 16)
  const head = [
    `# fabrix-proxy 모델 ${rows.length}개 · ${when} · ${sourceUrl || '(주소 미설정)'}`,
    '# alias 를 클라이언트의 model 칸에 넣으세요.',
  ]
  const width = Math.max(5, ...rows.map((m) => m.alias.length))
  const body = rows.map(
    (m) => `${m.alias.padEnd(width)}  ${m.modelId}  ${m.label}`,
  )
  return [...head, `${'alias'.padEnd(width)}  ${'modelId'.padEnd(36)}  label`, ...body].join('\n')
}

/** 클라이언트 설정의 `models` 배열에 통째로 붙일 alias 목록. */
export function modelsToAliasList(rows: ModelRow[]): string {
  return rows.map((m) => m.alias).join('\n')
}
