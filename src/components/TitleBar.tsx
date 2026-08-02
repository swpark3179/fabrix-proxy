import { getCurrentWindow } from '@tauri-apps/api/window'

import appIcon from '../assets/app-icon.png'

interface Props {
  title: string
  running?: boolean
  /** 로그 창처럼 크기를 바꿀 수 있는 창에서만 최대화를 활성화합니다. */
  resizable?: boolean
  onSettings?: () => void
}

/**
 * `decorations: false` 창의 커스텀 타이틀바.
 * 목업의 38px 바 · `— □ ✕` 글리프를 그대로 씁니다.
 */
export function TitleBar({ title, running = false, resizable = false, onSettings }: Props) {
  const win = getCurrentWindow()

  return (
    <div className="titlebar" data-tauri-drag-region>
      {/* 목업은 여기에 초록 "P" 배지를 그렸지만, 앱 아이콘이 정해졌으므로
          같은 자리·같은 크기로 그 아이콘을 씁니다. 꺼짐 상태는 채도를 빼서
          트레이 아이콘과 같은 대비를 냅니다. */}
      <img
        className={`titlebar__badge${running ? '' : ' titlebar__badge--off'}`}
        src={appIcon}
        alt=""
        draggable={false}
        data-tauri-drag-region
      />
      <span className="titlebar__title" data-tauri-drag-region>
        {title}
      </span>
      <span className="titlebar__spacer" data-tauri-drag-region />

      {onSettings && (
        <button className="titlebar__action" onClick={onSettings}>
          사내 연결 설정
        </button>
      )}

      <button className="titlebar__btn" onClick={() => void win.minimize()} title="최소화">
        —
      </button>
      <button
        className="titlebar__btn titlebar__btn--maximize"
        onClick={() => resizable && void win.toggleMaximize()}
        disabled={!resizable}
        style={!resizable ? { color: 'var(--ink-off)' } : undefined}
        title={resizable ? '최대화' : '이 창은 크기가 고정입니다'}
      >
        □
      </button>
      {/* 트레이 상주 앱이므로 닫기는 종료가 아니라 숨김입니다. */}
      <button
        className="titlebar__btn titlebar__btn--close"
        onClick={() => void win.hide()}
        title="닫기 (트레이에 남습니다)"
      >
        ✕
      </button>
    </div>
  )
}
