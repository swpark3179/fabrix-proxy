// Rust 쪽 타입과 1:1 대응. src-tauri/src/{state,logstore,port,config}.rs 참고.

export interface Stats {
  date: string
  total: number
  chat: number
  models: number
  lastCallAt: string | null
}

export interface PortOwner {
  pid: number
  name: string
}

export interface PortStatus {
  port: number
  free: boolean
  owner: PortOwner | null
  suggestion: number | null
}

export type LogKind = 'chat' | 'models'

export interface LogEntry {
  id: string
  ts: string
  tsFull: string
  kind: LogKind
  method: string
  path: string
  status: number
  latencyMs: number
  stream: boolean
  cached: boolean
  modelRequested: string | null
  modelAlias: string | null
  modelId: string | null
  modelLabel: string | null
  client: string | null
  note: string | null
  summary: string | null
  isError: boolean
  reqOpenai: string
  reqFabrix: string
  reqFabrixHeaders: string
  fabrixUrl: string
  /** ③ 돌려준 응답 — 자르지 않은 전문. 화면에서 앞부분만 보이고 전체보기로 펼칩니다. */
  respBody: string
  respMeta: string
}

export interface Snapshot {
  configured: boolean
  firstRun: boolean
  running: boolean
  port: number
  baseUrl: string
  autoStart: boolean
  fabrixBaseUrl: string
  defaultModelAlias: string
  insecureSkipVerify: boolean
  tokenMode: boolean
  issuedToken: string
  stats: Stats
  recent: LogEntry[]
  portStatus: PortStatus
  modelCount: number | null
}

export interface Config {
  fabrixBaseUrl: string
  fabrixClient: string
  openapiToken: string
  port: number
  autoStart: boolean
  defaultModelAlias: string
  insecureSkipVerify: boolean
  tokenMode: boolean
  issuedToken: string
  /** 이미지 호출에 함께 보내는 텍스트(베이스 LLM) 모델 id — 설정 화면에서 고르는 고정값. */
  imageTextModel: string
  /** 이미지 생성(FLUX) 대상 모델 id — 설정 화면에서 고르는 고정값. */
  imageModel: string
  /** 이미지 인식(gemma) 대상 모델 id — 설정 화면에서 고르는 고정값. */
  visionModel: string
  /** 이미지 백엔드 미연결 시 자리표시자(1×1 PNG) 반환 모드. */
  imageStubMode: boolean
  /**
   * 도구 호출(툴 콜) 에뮬레이션. 사내 API 에 도구 필드가 없어, 규약을 systemPrompt
   * 에 심고 답변에서 `<tool_call>` 을 걷어내는 방식으로 흉내 냅니다.
   * 요청에 `tools` 가 없으면 아무 영향이 없습니다.
   */
  toolEmulation: boolean
}

/** `list_models` 커맨드 결과 — 설정 화면 모델 드롭다운용. Rust ResolvedModel 과 대응. */
export interface ModelInfo {
  alias: string
  modelId: string
  label: string
  description: string | null
}

export interface TestResult {
  modelCount: number
  sample: string[]
}
