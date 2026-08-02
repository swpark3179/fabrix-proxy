/**
 * 잡히지 않은 오류를 화면 위에 그대로 띄웁니다.
 *
 * 배포 exe 에서 프런트가 죽으면 창은 Rust 가 이미 띄운 채라 "흰 화면" 만 남고
 * 원인을 알 수 없었습니다. 이 오버레이는 `error` · `unhandledrejection` ·
 * `securitypolicyviolation` 를 받아 메시지를 눈에 보이게 합니다.
 *
 * 한계: 이 코드도 같은 번들에 있으므로 **진입 모듈 자체가 로드/실행되지 못하는**
 * 순백(자산 404·CSP 차단)은 잡지 못합니다. 그 경우는 디버그 번들
 * (`npm run tauri build -- --debug`)의 devtools 콘솔로 확인합니다.
 *
 * CSP 안전: 인라인 <script> 없이 createElement + style 속성만 씁니다
 * (`style-src 'unsafe-inline'` 로 허용됨).
 */

const OVERLAY_ID = '__err_overlay__'

function render(title: string, detail: string): void {
  if (typeof document === 'undefined' || !document.body) return
  let el = document.getElementById(OVERLAY_ID)
  if (!el) {
    el = document.createElement('div')
    el.id = OVERLAY_ID
    el.setAttribute(
      'style',
      'position:fixed;inset:0;z-index:2147483647;background:#1a0000;color:#ffd7d7;' +
        'font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;padding:16px;' +
        'overflow:auto;white-space:pre-wrap;user-select:text',
    )
    document.body.appendChild(el)
  }
  // 여러 오류가 연달아 나면 마지막(가장 최근) 것만 보여 줍니다.
  el.textContent = `${title}\n\n${detail}`
}

function stackOf(value: unknown): string {
  if (value instanceof Error) return value.stack ?? `${value.name}: ${value.message}`
  return String(value)
}

let installed = false

export function installErrorOverlay(): void {
  if (installed || typeof window === 'undefined') return
  installed = true

  window.addEventListener('error', (e) => {
    const where = e.filename ? `\n${e.filename}:${e.lineno}:${e.colno}` : ''
    render('Uncaught error', stackOf(e.error ?? e.message) + where)
  })

  window.addEventListener('unhandledrejection', (e) => {
    render('Unhandled promise rejection', stackOf(e.reason))
  })

  window.addEventListener('securitypolicyviolation', (e) => {
    render(
      'CSP violation',
      `blocked: ${e.blockedURI}\ndirective: ${e.violatedDirective}\nsource: ${e.sourceFile}:${e.lineNumber}`,
    )
  })
}
