import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'

// Tauri drives the dev server on a fixed port and needs a stable host.
export default defineConfig({
  plugins: [react()],
  // 배포 exe 는 dist 를 Tauri 커스텀 프로토콜(http://tauri.localhost/)로 서빙합니다.
  // 절대경로(/assets/…)가 이 프로토콜에서 해석되지 않아 흰 화면이 나던 문제를
  // 상대경로로 못박아 피합니다. 세 HTML 이 모두 dist 루트라 './' 로 안전합니다.
  base: './',
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'chrome110',
    outDir: 'dist',
    emptyOutDir: true,
    // chrome110 은 modulepreload 를 네이티브로 지원합니다. 폴리필을 끄면
    // Vite 가 넣던 유일한 인라인 <script> 가 사라져, 배포 CSP 가 인라인 스크립트를
    // 해시로 허용할 필요 없이 순수 `script-src 'self'` 만으로 진입 모듈이 통과합니다.
    modulePreload: { polyfill: false },
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        log: resolve(__dirname, 'log.html'),
        toast: resolve(__dirname, 'toast.html'),
      },
    },
  },
})
