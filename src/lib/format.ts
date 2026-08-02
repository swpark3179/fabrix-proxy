import type { LogEntry } from '../types'

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
