import type { Snapshot } from '../types'

/** 노출 엔드포인트는 둘뿐입니다 — 목업의 카드 2장을 그대로 옮겼습니다. */
export function EndpointCards({ snapshot }: { snapshot: Snapshot }) {
  const { running, stats, modelCount } = snapshot

  return (
    <div className={`endpoints${running ? '' : ' endpoints--idle'}`}>
      <div className="endpoint">
        <span className={`method ${running ? 'method--post' : 'method--idle'}`}>POST</span>
        <div className="endpoint__text">
          <span className={`endpoint__path${running ? '' : ' endpoint__path--idle'}`}>
            /v1/chat/completions
          </span>
          <span className={`endpoint__sub${running ? '' : ' endpoint__sub--idle'}`}>
            {running ? '채팅 · 스트리밍 지원' : '대기 중'}
          </span>
        </div>
        {running && <span className="endpoint__count">{stats.chat}건</span>}
      </div>

      <div className="endpoint">
        <span className={`method ${running ? 'method--get' : 'method--idle'}`}>GET</span>
        <div className="endpoint__text">
          <span className={`endpoint__path${running ? '' : ' endpoint__path--idle'}`}>/v1/models</span>
          <span className={`endpoint__sub${running ? '' : ' endpoint__sub--idle'}`}>
            {!running
              ? '대기 중'
              : modelCount !== null
                ? `사내 모델 ${modelCount}개 노출`
                : '사내 모델 목록 중계'}
          </span>
        </div>
        {running && <span className="endpoint__count">{stats.models}건</span>}
      </div>
    </div>
  )
}
