import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

import type { Config, LogEntry, PortStatus, Snapshot, TestResult } from '../types'

export const getSnapshot = () => invoke<Snapshot>('get_snapshot')
export const getConfig = () => invoke<Config>('get_config')
export const getConfigPath = () => invoke<string>('get_config_path')
export const getLogs = () => invoke<LogEntry[]>('get_logs')
export const clearLogs = () => invoke<void>('clear_logs')

export const checkPort = (port: number) => invoke<PortStatus>('check_port', { port })

export const testConnection = (input: {
  fabrixBaseUrl: string
  fabrixClient: string
  openapiToken: string
  insecureSkipVerify: boolean
}) => invoke<TestResult>('test_connection', input)

export const saveConfig = (config: Config) => invoke<Snapshot>('save_config', { config })

export const startProxy = () => invoke<Snapshot>('start_proxy')
export const stopProxy = () => invoke<Snapshot>('stop_proxy')
export const toggleProxy = () => invoke<Snapshot>('toggle_proxy')

export const copyEndpoint = () => invoke<string>('copy_endpoint')
export const openLogWindow = () => invoke<void>('open_log_window')
export const quitApp = () => invoke<void>('quit_app')

// ── 이벤트 ────────────────────────────────────────────────────

export const onStateChanged = (fn: (s: Snapshot) => void): Promise<UnlistenFn> =>
  listen<Snapshot>('state:changed', (e) => fn(e.payload))

export const onLogEntry = (fn: (e: LogEntry) => void): Promise<UnlistenFn> =>
  listen<LogEntry>('log:new', (e) => fn(e.payload))

export const onLogsCleared = (fn: () => void): Promise<UnlistenFn> =>
  listen('logs:cleared', () => fn())

export const onOpenSettings = (fn: () => void): Promise<UnlistenFn> =>
  listen('ui:settings', () => fn())

export const onToast = (fn: (url: string) => void): Promise<UnlistenFn> =>
  listen<string>('toast:show', (e) => fn(e.payload))

/** Rust 쪽 `Err(String)` 을 사람이 읽는 문자열로 정규화합니다. */
export function errText(err: unknown): string {
  if (typeof err === 'string') return err
  if (err instanceof Error) return err.message
  return String(err)
}
