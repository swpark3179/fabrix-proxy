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
  respPreview: string
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
}

export interface TestResult {
  modelCount: number
  sample: string[]
}
