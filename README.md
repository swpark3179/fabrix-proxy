# FabriX Proxy

사내 AI(**FabriX**)를 **OpenAI 호환 API**로 중계하는 Windows 트레이 앱입니다.

FabriX는 `POST /openapi/chat/v1/messages` 라는 자체 스키마를 쓰기 때문에 Continue.dev, `openai` SDK,
Cursor 같은 표준 클라이언트를 그대로 붙일 수 없습니다. 이 앱은 로컬에 OpenAI 호환 엔드포인트를 띄우고
들어온 요청을 FabriX 형식으로 번역합니다.

쓰는 앱의 **Base URL** 칸에 `http://127.0.0.1:8787/v1` 을 넣으면 끝입니다.
**API 키 칸에는 아무 값이나** 넣어도 됩니다 — 사내 키는 프록시가 대신 붙입니다.

노출 엔드포인트는 다음과 같습니다.

| | |
|---|---|
| `POST /v1/chat/completions` | 채팅 · 스트리밍 지원 |
| `GET /v1/models` | 사내 모델 목록 (60초 캐시) |
| `POST /v1/images/generations` | 이미지 생성(FLUX) · OpenAI Images 호환 |
| `POST /v1/images/edits` | 이미지 편집(gemma 인식 → FLUX 재생성) · JSON data-URL |

> ⚠️ 이미지 두 엔드포인트는 현재 **인터페이스/스켈레톤**입니다. 실제 사내 FLUX/gemma 호출은
> 파이썬 샘플 반영 전까지 스텁이라 **501**(`not_implemented`)로 응답합니다. 배선만 확인하려면
> 설정에서 `이미지 스텁 모드`(`imageStubMode`)를 켜면 1×1 자리표시자 PNG 를 돌려줍니다
> (응답 헤더 `x-fabrix-image-stub: 1`, 로그에 `[stub]` 표기).

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
  proxy/tools.rs       도구 호출 에뮬레이션 — 규약 주입 · <tool_call> 파스아웃
  logstore.rs          최근 50건 링버퍼 (본문은 메모리 전용)
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
  "insecureSkipVerify": false,
  "tokenMode": false,          // 로컬 토큰 검증 모드
  "issuedToken": "",           // tokenMode=true 일 때 발행되는 sk-… 토큰
  "imageModel": "",            // 이미지 생성(FLUX) 고정 모델 — 설정 화면에서 선택
  "visionModel": "",           // 이미지 인식(gemma) 고정 모델 — 설정 화면에서 선택
  "imageStubMode": false,      // 이미지 백엔드 미연결 시 1×1 자리표시자 PNG 반환
  "toolEmulation": true        // 도구 호출(툴 콜) 흉내 내기 — 아래 "도구 호출" 참고
}
```

> ⚠️ 평문입니다. 이 폴더를 읽을 수 있는 계정이나 프로그램은 사내 인증키를 그대로 볼 수 있습니다.
> Windows 자격 증명 관리자로 옮기려면 `src-tauri/src/config.rs` 의 `load_config`/`save_config` 두
> 함수만 바꾸면 됩니다.

### 로컬 토큰 (키 발급)

기본은 **키발급없이 허용 모드**(`tokenMode: false`) — 클라이언트가 API 키 칸에 아무 값이나
넣어도 통과합니다(사내 키는 프록시가 붙입니다). 설정 화면에서 **토큰 사용 모드**를 켜면
OpenAI 양식 토큰(`sk-…`)이 자동 발행되고, 인바운드 `Authorization: Bearer <token>` 이 발행된
토큰과 **정확히 일치할 때만** 허용합니다. 일치하지 않으면 OpenAI 표준 `invalid_api_key` 로
`401` 을 돌려줍니다. 발행 토큰은 메인 화면 카드나 설정 화면에서 복사하고, "재발급"으로 언제든
교체할 수 있습니다(이전 토큰은 저장 후 무효).

`~/.fabrix-proxy/stats.json` 에는 날짜별 호출 건수만 남습니다.
**호출 본문은 어디에도 기록하지 않습니다** — 최근 50건은 메모리에만 있고 앱을 끄면 사라집니다.

---

## 변환 규칙

| OpenAI | FabriX |
|---|---|
| `model` | `modelIds: [uuid]` — UUID 직매치 → alias → 대소문자 무시 → **기본 모델 폴백** |
| `messages[role=system]` | `systemPrompt` (여러 개면 `\n\n` 연결) |
| 나머지 `messages` | `contents` — 단일 user 턴이면 그 텍스트, 멀티턴이면 `User:`/`Assistant:` 트랜스크립트 하나 |
| `stream` | `isStream` |
| `temperature` `top_p` `max_tokens` `seed` `frequency_penalty` `top_k` | `llmConfig.{temperature, top_p, max_new_tokens, seed, repetion_penalty, tok_k}` |
| `tools` `tool_choice` | 대응 필드가 **없어** `systemPrompt` 뒤의 규약 텍스트로 접힙니다 (아래 참고) |

FabriX에는 롤 구조가 없어 멀티턴은 한 덩어리로 접힙니다. 무엇이 어떻게 접혔는지는
로그 창 ② 칸에서 그대로 확인할 수 있습니다.

모델 alias는 영문명에서 만듭니다 (`Chat 4` → `fabrix-chat-4`). 영문명이 없으면 UUID 앞 8자리를
씁니다 (`fabrix-01970a3b`) — 서버가 순서를 바꿔도 alias가 흔들리지 않게 하기 위함입니다.

### 도구 호출 (툴 콜)

FabriX 요청 스키마에는 도구 필드가 **없습니다**. 그래서 `tools` 를 그대로 넘길 수 없고,
프롬프트 규약 + 출력 파싱으로 흉내 냅니다. 요청에 `tools` 가 있으면 자동으로 켜지고,
설정 화면에서 끌 수 있습니다(`toolEmulation`). 도구를 안 쓰는 요청은 아무 영향이 없습니다.

- **나갈 때** — 도구 스키마와 출력 규약을 `systemPrompt` 뒤에 붙입니다. 규약은 영어로
  씁니다. 감싸는 대상(도구 이름·설명·JSON Schema)이 전부 영어라, 한국어 프레임을 씌우면
  모델이 센티널을 뱉는 대신 도구를 한국어로 *설명하기* 시작합니다.
- **들어올 때** — 답변에서 아래 블록을 걷어내 OpenAI `tool_calls` 로 조립하고,
  `finish_reason` 을 `tool_calls` 로 바꿉니다.

```
<tool_call>
{"name": "write", "arguments": {"filePath": "index.html", "content": "…"}}
</tool_call>
```

이 모양은 Qwen 2.5/3 의 기본 chat template, Hermes, vLLM 의 `hermes` 파서가 쓰는 것과
같습니다. 사내 모델이 그 계열이면 규약을 읽기 전에 이미 맞는 모양을 뱉을 수 있습니다.

**도구가 아니라고 판단하면 블록을 원문 텍스트 그대로 되돌립니다.** 판단 기준에서 가장
중요한 것은 이름이 그 요청에 선언된 도구 집합에 있는지입니다. JSON 파싱 실패, 닫히지 않은
블록, 2MiB 초과도 같은 길로 흘러갑니다 — 어떤 경우에도 버리지 않습니다.

> 이 방식은 **모델이 형식을 지켜 줘야** 동작합니다. 지키지 않으면 로그 ③ 칸 꼬리에
> `호출 0건 — 모델이 규약을 따르지 않음` 이 남습니다. ③ 칸은 파서가 걷어내기 **전** 원문이라
> `<tool_call>` 이 실제로 있었는지 눈으로 확인할 수 있습니다.

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
| `MOCK_TOOLCALL` | `single` · `parallel` · `prose` · `malformed` · `unknown` · `fenced` | 답변에 `<tool_call>` 을 섞습니다 |
| `MOCK_CHUNK` | 글자 수 (기본 7) | 낮출수록 센티널이 여러 프레임에 걸쳐 쪼개집니다 |
| `MOCK_SPLITBYTES` | `1` | SSE 한 줄을 임의 바이트 지점에서 두 번에 write |

네 조합(`snake|camel` × `delta|cumulative`) 모두 같은 결과가 나와야 합니다.

```bash
curl http://127.0.0.1:8787/v1/models
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" -H "Authorization: Bearer anything" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"안녕"}],"stream":true}'
```

### 도구 호출 확인

도구 호출 파서가 깨지는 곳은 거의 언제나 **프레임 경계**입니다. `MOCK_CHUNK` 를 낮추면
`<tool_call>` 한가운데가 갈립니다.

```bash
MOCK_TOOLCALL=single MOCK_CHUNK=3 npm run mock       # 센티널을 프레임 5개로 쪼갬
MOCK_TOOLCALL=parallel MOCK_STREAM=cumulative npm run mock
MOCK_TOOLCALL=malformed npm run mock                 # 텍스트로 되돌아와야 함
```

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" -H "Authorization: Bearer anything" \
  -d '{"model":"fabrix-chat-4","stream":true,
       "messages":[{"role":"user","content":"페이지 만들어줘"}],
       "tools":[{"type":"function","function":{"name":"write","description":"Write a file",
         "parameters":{"type":"object","properties":{"filePath":{"type":"string"},
                                                     "content":{"type":"string"}}}}}]}'
```

`delta.tool_calls` 와 마지막 청크의 `finish_reason: "tool_calls"` 가 보이면 정상입니다.

실제 통합 경계는 그 위입니다 — OpenCode 를 프록시에 직접 물려 **빈 폴더에 파일이
생기는지** 보는 것이 UI 전체를 띄우는 것보다 훨씬 빠릅니다.

```bash
export OPEN_DESIGN_BYOK_API_KEY=anything
export OPENCODE_CONFIG_CONTENT='{"provider":{"open-design-byok":{"name":"Open Design BYOK",
  "npm":"@ai-sdk/openai-compatible",
  "options":{"baseURL":"http://127.0.0.1:8787/v1","apiKey":"{env:OPEN_DESIGN_BYOK_API_KEY}"},
  "models":{"fabrix-chat-4":{"name":"fabrix-chat-4","limit":{"context":128000,"output":16384}}}}}}'
echo 'index.html 에 hello world 페이지를 만들어줘' \
  | opencode run --format json --dir /tmp/odtest -m open-design-byok/fabrix-chat-4
ls /tmp/odtest
```

---

## 목업과 다른 점

의도적으로 다르게 만든 다섯 곳입니다.

| 목업 | 구현 | 이유 |
|---|---|---|
| 메인 창 720 × **520** | 720 × **560** | 목업이 그린 내용(상태 카드 + 엔드포인트 2장 + 최근 4행)이 520 안에 들어가지 않습니다. 고정 크기 대시보드에 스크롤바를 만드느니 40px 을 줬습니다. |
| 트레이 상태 헤더가 점·건수·주소가 든 카드 | 클릭 불가 항목 **두 줄** | 네이티브 Windows 메뉴에 그런 블록을 넣을 수 없습니다. 동작 항목(끄기·복사·창·로그·종료)은 목업과 같습니다. |
| 로그 ③ 칸의 `원본 JSON 보기` | `전체보기` | 들고 있는 것은 답변 **전문**이지 사내가 준 원본 JSON 봉투가 아닙니다. 원본을 안 갖고 있으면서 "원본 보기"를 띄우면 거짓말이라, 가진 그대로 "전체보기"로 부릅니다. |
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
