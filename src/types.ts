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
  /** ④ 가공하지 않은 와이어 원문. Rust `logstore::RawWire` 와 1:1. */
  raw: RawWire
}

/**
 * 와이어 원문 두 쪽. ③ 칸은 이미 가공된 답변(`<think>` 를 갈라내고 `<tool_call>` 을
 * 걷어낸 뒤)이라, "0자" 가 모델이 말을 안 한 것인지 우리가 프레임을 못 읽은 것인지
 * 가리려면 이 원문이 필요합니다.
 */
export interface RawWire {
  /** 기록 스위치가 켜져 있었는가. 꺼져서 빈 것과 켰는데 안 온 것은 뜻이 다릅니다. */
  captured: boolean
  /** 사내가 준 바이트 그대로 (SSE 는 `data:` 줄까지). */
  upstream: string
  /** 클라이언트로 나간 본문 그대로. */
  client: string
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
  /**
   * 와이어 원문 기록. 사내가 준 응답과 클라이언트로 나간 응답을 가공 없이 로그 한
   * 건에 함께 담습니다(각 256KiB 상한, 메모리에만).
   */
  rawWireLog: boolean
}

/** 모델 목록 한 줄. Rust `commands::ModelRow` 와 1:1. */
export interface ModelRow {
  /** 클라이언트가 `model` 칸에 넣는 값 — 복사 대상. */
  alias: string
  /** 실제로 사내에 보내는 UUID — 사내 담당자와 대조용. */
  modelId: string
  /** 사람이 읽는 이름 (예: `챗 4`). */
  label: string
  description: string | null
  isDefault: boolean
}

export interface ModelListResult {
  models: ModelRow[]
  cached: boolean
  /** 로컬 RFC3339 — 화면이 "n초 전 조회" 를 계산합니다. */
  fetchedAt: string
  /** 빈 문자열이면 목록의 첫 모델이 기본입니다. */
  defaultAlias: string
  sourceUrl: string
  cacheTtlSecs: number
}

export interface TestResult {
  modelCount: number
  models: ModelRow[]
}
