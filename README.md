# FabriX Proxy

사내 AI(**FabriX**)를 **OpenAI 호환 API**로 중계하는 Windows 트레이 앱입니다.

FabriX는 `POST /openapi/chat/v1/messages` 라는 자체 스키마를 쓰기 때문에 Continue.dev, `openai` SDK,
Cursor 같은 표준 클라이언트를 그대로 붙일 수 없습니다. 이 앱은 로컬에 OpenAI 호환 엔드포인트를 띄우고
들어온 요청을 FabriX 형식으로 번역합니다.

쓰는 앱의 **Base URL** 칸에 `http://127.0.0.1:8787/v1` 을 넣으면 끝입니다.
**API 키 칸에는 아무 값이나** 넣어도 됩니다 — 사내 키는 프록시가 대신 붙입니다.

노출 엔드포인트는 둘뿐입니다.

| | |
|---|---|
| `POST /v1/chat/completions` | 채팅 · 스트리밍 지원 |
| `GET /v1/models` | 사내 모델 목록 (60초 캐시) |

---

## 개발

```bash
npm install
npm run mock          # 개발용 FabriX 스텁 :9900  (별도 터미널)
npm run tauri dev
```

첫 실행이면 온보딩 화면이 뜹니다. 목업 서버로 시험하려면 —

- 사내 AI 주소 `http://127.0.0.1:9900`
- 인증키 / 토큰 : 아무 값이나

### 빌드

```bash
npm run tauri build   # → src-tauri/target/release/bundle/ (NSIS · MSI)
```

### 생성물 다시 만들기

```bash
npm run gen:fonts     # src/styles/fonts.css
```

**아이콘을 바꾸려면** — 정사각형 PNG(1024px 이상, 투명 배경 권장) 한 장이면 됩니다.

```bash
cp <새-아이콘>.png src-tauri/icons/source.png
npx tauri icon src-tauri/icons/source.png        # .ico · .icns · 각 크기 PNG 파생
cp src-tauri/icons/128x128.png src/assets/app-icon.png   # 타이틀바 배지 (원본을 그대로 쓰면 번들에 1.4MB 가 얹힙니다)
```

한 장으로 **exe · 설치 프로그램 · 작업 표시줄 · 트레이 · 타이틀바 배지**가 전부 바뀝니다.
트레이와 타이틀바의 *꺼짐* 상태는 같은 그림의 채도를 빼서 만들므로 별도 파일이 필요 없습니다
(Rust: `src-tauri/src/tray.rs`, CSS: `.titlebar__badge--off`).

> `tauri-build` 는 번들 아이콘 변경을 스스로 추적하지 않습니다. `build.rs` 에
> `cargo:rerun-if-changed=icons/icon.ico` 를 넣어 둔 이유입니다 — 이게 없으면
> 아이콘만 바꿨을 때 exe 에 예전 아이콘 리소스가 그대로 남습니다.

---

## 구조

```
src/                   프런트 (React 19 · Vite · 창 3개 = 엔트리 3개)
  main.tsx             메인 창 720×520  — 상태 · 포트 · Base URL · 최근 호출 · 온보딩/설정
  log.tsx              로그 창 900×620  — 받은 것 / 보낸 것 / 돌려준 것
  toast.tsx            복사 토스트 268px — 투명 · 항상 위 · 3초
src-tauri/src/
  proxy/chat.rs        POST /v1/chat/completions  ← 핵심
  proxy/models.rs      GET  /v1/models + 60초 캐시 + alias 매핑
  proxy/fabrix.rs      FabriX 스키마 · SSE 디코더 · 오류 분류
  logstore.rs          최근 200건 링버퍼 (본문은 메모리 전용)
  port.rs              포트 가용성 · 점유 PID 조회 · 빈 포트 추천
  tray.rs windows.rs   트레이 메뉴 · 창 3개 관리
mock-fabrix/server.mjs 개발용 FabriX 스텁 (의존성 0)
```

---

## 설정 파일

`~/.fabrix-proxy/config.json` 에 **평문 JSON**으로 저장됩니다.

```jsonc
{
  "fabrixBaseUrl": "https://ai.corp.internal",
  "fabrixClient": "…",        // x-fabrix-client
  "openapiToken": "…",        // x-openapi-token
  "port": 8787,
  "autoStart": true,
  "defaultModelAlias": "fabrix-chat-4",
  "insecureSkipVerify": false
}
```

> ⚠️ 평문입니다. 이 폴더를 읽을 수 있는 계정이나 프로그램은 사내 인증키를 그대로 볼 수 있습니다.
> Windows 자격 증명 관리자로 옮기려면 `src-tauri/src/config.rs` 의 `load_config`/`save_config` 두
> 함수만 바꾸면 됩니다.

`~/.fabrix-proxy/stats.json` 에는 날짜별 호출 건수만 남습니다.
**호출 본문은 어디에도 기록하지 않습니다** — 최근 200건은 메모리에만 있고 앱을 끄면 사라집니다.

---

## 변환 규칙

| OpenAI | FabriX |
|---|---|
| `model` | `modelIds: [uuid]` — UUID 직매치 → alias → 대소문자 무시 → **기본 모델 폴백** |
| `messages[role=system]` | `systemPrompt` (여러 개면 `\n\n` 연결) |
| 나머지 `messages` | `contents` — 단일 user 턴이면 그 텍스트, 멀티턴이면 `User:`/`Assistant:` 트랜스크립트 하나 |
| `stream` | `isStream` |
| `temperature` `top_p` `max_tokens` `seed` `frequency_penalty` `top_k` | `llmConfig.{temperature, top_p, max_new_tokens, seed, repetion_penalty, tok_k}` |

FabriX에는 롤 구조가 없어 멀티턴은 한 덩어리로 접힙니다. 무엇이 어떻게 접혔는지는
로그 창 ② 칸에서 그대로 확인할 수 있습니다.

모델 alias는 영문명에서 만듭니다 (`Chat 4` → `fabrix-chat-4`). 영문명이 없으면 UUID 앞 8자리를
씁니다 (`fabrix-01970a3b`) — 서버가 순서를 바꿔도 alias가 흔들리지 않게 하기 위함입니다.

### 오류 매핑

| 상황 | 상태 | 로그 문구 |
|---|---|---|
| 연결 실패 · 30초 타임아웃 | 502 | `사내 응답 없음` |
| FabriX 429 | 429 | `사내 쿼터 초과` |
| 자격 증명 미설정 | 503 | `사내 연결 설정이 필요합니다` |
| 그 밖의 4xx/5xx | 그대로 전달 | `사내 오류 <code>` |

---

## 목업 서버로 검증하기

스펙 문서에 SSE 프레임 형식이 없어 파서를 방어적으로 짰습니다. 목업 서버는 그 방어 경로를
**강제로 골라 실행**할 수 있습니다.

```bash
MOCK_CASE=camel MOCK_STREAM=cumulative npm run mock
```

| 변수 | 값 | 무엇을 태우는가 |
|---|---|---|
| `MOCK_CASE` | `snake`(기본) · `camel` | 필드 표기 양쪽 |
| `MOCK_STREAM` | `delta`(기본) · `cumulative` | `content` 가 증분인지 누적인지 |
| `MOCK_RAW` | `1` | `data:` 접두 없이 개행 구분 JSON |
| `MOCK_FAIL` | `429` · `500` · `timeout` · `midstream` | 오류 경로 |
| `MOCK_DELAY` | ms (기본 40) | 프레임 간 지연 |

네 조합(`snake|camel` × `delta|cumulative`) 모두 같은 결과가 나와야 합니다.

```bash
curl http://127.0.0.1:8787/v1/models
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" -H "Authorization: Bearer anything" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"안녕"}],"stream":true}'
```

---

## 목업과 다른 점

의도적으로 다르게 만든 다섯 곳입니다.

| 목업 | 구현 | 이유 |
|---|---|---|
| 메인 창 720 × **520** | 720 × **560** | 목업이 그린 내용(상태 카드 + 엔드포인트 2장 + 최근 4행)이 520 안에 들어가지 않습니다. 고정 크기 대시보드에 스크롤바를 만드느니 40px 을 줬습니다. |
| 트레이 상태 헤더가 점·건수·주소가 든 카드 | 클릭 불가 항목 **두 줄** | 네이티브 Windows 메뉴에 그런 블록을 넣을 수 없습니다. 동작 항목(끄기·복사·창·로그·종료)은 목업과 같습니다. |
| 로그 ③ 칸의 `원본 JSON 보기` | 없음 | 응답 본문은 앞부분만 메모리에 들고 있습니다. 원본을 안 갖고 있으면서 "원본 보기"를 띄우면 거짓말이 됩니다. |
| 꺼짐 화면의 최근 호출은 항상 빈 상태 | 기록이 있으면 계속 보여줌 | 방금 뭐가 오갔는지가 이 앱의 세 기능 중 하나인데, 끄자마자 지워 버릴 이유가 없습니다. 기록이 정말 없을 때만 목업의 빈 상태가 나옵니다. |
| Google Fonts CDN | 번들에 포함 | 사내망/오프라인에서 CDN 을 못 받으면 조판이 무너집니다. |

## 실서버에 붙일 때 확인할 것

스펙 문서에 없어서 방어적으로 처리해 둔 것들입니다. 실서버 응답을 한 번 보면 확정할 수 있습니다.

- **SSE 프레임 형태** — `data:` 접두 유무, 종료 센티널. 현재는 양쪽 모두 처리합니다.
- **`content` 가 누적인지 증분인지** — 두 번째 프레임에서 자동 판별하고 그 모드로 고정합니다.
- **`repetion_penalty` · `tok_k`** — 스펙 문서의 철자 그대로 보냅니다(오타로 보이지만 서버가
  기대하는 키일 가능성이 높음). 다르면 `src-tauri/src/proxy/fabrix.rs` 의 `LlmConfig` 에서
  `rename` 두 줄만 고치면 됩니다.
- **토큰 사용량** — FabriX가 주지 않으므로 `usage` 를 **생략**합니다. 추정치를 지어내면 토큰으로
  예산을 잡는 클라이언트가 잘못된 값을 믿게 되기 때문입니다.
- **사내 루트 CA** — Windows 인증서 저장소(schannel)를 그대로 신뢰합니다. CA가 없어 실패하면
  설정에서 `TLS 인증서 검증 건너뛰기` 를 켤 수 있습니다.
