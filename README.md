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
| `GET /v1/models/{id}` | 모델 하나 · 모르는 id 는 **404 `model_not_found`** |
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

### 테스트

```bash
cd src-tauri && cargo test    # 단위 + HTTP 표면 통합 테스트
npm run build                 # tsc --noEmit + vite build
```

통합 테스트(`src-tauri/tests/proxy_http.rs`)는 가짜 사내 서버를 띄우고 프록시를 그쪽으로
가리켜 **진짜 HTTP 로** 규약을 확인합니다 — 404 `model_not_found` · 405/413 봉투 ·
파라미터 검증 · SSE 청크 순서(첫 role 청크 → 내용 → finish → usage → `[DONE]`) ·
도구 호출 왕복. 사내 서버도 목업 프로세스도 필요 없고, 창을 띄우지 않으므로 CI 에서도 돕니다.

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
src/                   프런트 (React 19 · Vite · 창 4개 = 엔트리 4개)
  main.tsx             메인 창 720×560  — 상태 · 포트 · Base URL · 최근 호출 · 온보딩/설정
  log.tsx              로그 창 900×620  — 받은 것 / 보낸 것 / 돌려준 것
  models.tsx           모델 목록 창 900×600 — 표시 이름 · 모델 ID · UUID · 설명 · 복사
  toast.tsx            복사 토스트 268px — 투명 · 항상 위 · 3초
  components/models/   목록 표 · 도구줄 · 복사 버튼
  styles/controls.css  창들이 공유하는 버튼·입력칸·배너 (base.css 가 import)
src-tauri/src/
  proxy/chat.rs        POST /v1/chat/completions  ← 핵심
  proxy/models.rs      GET  /v1/models(+/{id}) + 60초 캐시 + alias 매핑
  proxy/fabrix.rs      FabriX 스키마 · SSE 디코더 · 오류 분류 · finish_reason 클램프
  proxy/validate.rs    요청 검증 — 규약 위반을 사내 호출 전에 400 으로
  proxy/usage.rs       토큰 수 정책 (사내 실측 우선 · 없으면 문자 기반 추정)
  proxy/tools.rs       도구 호출 에뮬레이션 — 규약 주입 · <tool_call> 파스아웃
  logstore.rs          최근 50건 링버퍼 (본문은 메모리 전용)
  port.rs              포트 가용성 · 점유 PID 조회 · 빈 포트 추천
  tray.rs windows.rs   트레이 메뉴 · 창 4개 관리 (모델 목록은 처음 열 때 생성)
src-tauri/tests/
  proxy_http.rs        HTTP 표면 통합 테스트 — 가짜 사내 서버를 띄워 규약을 확인
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
  "defaultModelAlias": "fabrix-chat-4",   // model 을 안 보낸 요청에 쓸 모델 — 목록 창에서 고를 수 있습니다
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
| `model` | `modelIds: [uuid]` — UUID 직매치 → alias → 대소문자 무시. **못 찾으면 404** (폴백하지 않습니다) |
| `model` 없음 | 설정의 `defaultModelAlias`, 그것도 없으면 목록의 첫 모델 |
| `messages[role=system]` | `systemPrompt` (여러 개면 `\n\n` 연결) |
| 나머지 `messages` | `contents` — 단일 user 턴이면 그 텍스트, 멀티턴이면 `User:`/`Assistant:` 트랜스크립트 하나 |
| `stream` | `isStream` |
| `temperature` `top_p` `max_tokens` `seed` `frequency_penalty` `top_k` | `llmConfig.{temperature, top_p, max_new_tokens, seed, repetion_penalty, tok_k}` |
| `tools` `tool_choice` | 대응 필드가 **없어** `systemPrompt` 뒤의 규약 텍스트로 접힙니다 (아래 참고) |
| `stream_options.include_usage` | 스트림 꼬리에 `usage` 청크 (추정치 · 아래 참고) |

**받되 반영하지 않는 것** — 사내 `llmConfig` 에 자리가 없습니다. 조용히 버리지 않고
로그 ③ 칸 꼬리에 `무시된 파라미터: …` 로, ② 칸 칩에 흐린 글씨로 적습니다.

| 필드 | 왜 |
|---|---|
| `stop` | 사내에 대응 필드가 없습니다. 거절하지 않는 이유는 기본값으로 실어 보내는 클라이언트가 많아 400 이 더 많이 깨뜨리기 때문입니다 |
| `presence_penalty` | 페널티 키가 `repetion_penalty` 하나뿐이고 `frequency_penalty` 가 이미 씁니다. 의미가 다른 두 값을 한 키에 겹쳐 넣는 것은 상위 동작을 지어내는 일입니다 |
| `logit_bias` | 사내 모델의 토크나이저를 모릅니다 |
| `user` `metadata` `store` `service_tier` `parallel_tool_calls` | 대응 필드 없음 |
| `response_format` | 프롬프트 규약으로 흉내 낼 수는 있지만 이번 범위 밖입니다 |

**아예 거절하는 것**(400) — 조용히 다른 결과를 주는 것보다 문 앞에서 실패하는 편이 낫습니다.

| 필드 | 상태 · `code` | 왜 |
|---|---|---|
| `n > 1` | 400 `unsupported_value` | 응답의 *모양*을 바꾸는 값입니다. 1개만 돌려주면 클라이언트가 조용히 잘못된 결과를 얻습니다 |
| `logprobs: true` | 400 `unsupported_value` | `logprobs: null` 을 주면 `choices[0].logprobs.content` 를 까는 클라이언트가 원인에서 먼 곳에서 죽습니다 |
| 이미지만 있는 요청 | 400 `unsupported_content` | 사내 채팅 API 가 이미지를 못 받으므로 "무엇을 물었는지 없는" 요청이 나갑니다. 텍스트를 함께 보내면 이미지 파트만 버리고 진행하며, 몇 개를 버렸는지 로그에 적습니다 |

`temperature`(0–2) · `top_p`(0–1) · 두 penalty(−2–2) · `max_tokens`(≥1) · `stop`(≤4개) ·
`messages`(비어 있지 않고 롤이 규약값) 도 검증합니다. 위반은 OpenAI 모양의 400 —
`{"error":{"message":…,"type":"invalid_request_error","param":"temperature","code":"invalid_value"}}`.

FabriX에는 롤 구조가 없어 멀티턴은 한 덩어리로 접힙니다. 무엇이 어떻게 접혔는지는
로그 창 ② 칸에서 그대로 확인할 수 있습니다 (`MOCK_ECHO=1` 로 사내가 받은 것을 그대로
되돌려 받아 볼 수도 있습니다).

### 응답

OpenAI 가 내보내는 필드를 그대로 맞춥니다 — `choices[].logprobs`(항상 `null`) ·
`system_fingerprint`(프록시 버전 + 실제로 나간 `modelId` 의 해시라 구성이 같으면 같은 값) ·
`usage`. 스트림은 OpenAI 처럼 **롤만 담은 첫 청크**(`delta:{"role":"assistant","content":""}`)로
시작하고, `include_usage` 를 켰으면 finish 청크 뒤 · `[DONE]` 앞에 `choices: []` + `usage`
청크가 하나 더 옵니다.

`finish_reason` 은 OpenAI 열거값(`stop`·`length`·`tool_calls`·`content_filter`·
`function_call`)만 내보냅니다. 사내가 모르는 값을 주면 `stop` 으로 접고 **원문은 로그에**
`finish_reason: stop (사내: weird)` 로 남깁니다 — 준수와 진단을 둘 다 잃지 않기 위함입니다.
중단 계열(`abort`·`timeout`)은 `length` 로 접습니다. 끊긴 답변을 완성된 것처럼 부르면 안 됩니다.

비표준이지만 남겨 둔 것이 하나 있습니다 — `message.reasoning_content`. o1 계열 클라이언트가
읽는 필드이고, 사내 `reasoningContent` 를 버리지 않으려면 실을 자리가 필요합니다.

모델 alias는 영문명에서 만듭니다 (`Chat 4` → `fabrix-chat-4`). 영문명이 없으면 UUID 앞 8자리를
씁니다 (`fabrix-01970a3b`) — 서버가 순서를 바꿔도 alias가 흔들리지 않게 하기 위함입니다.

### 어떤 모델이 있는지 보려면

트레이 메뉴 → **모델 목록 보기** (또는 메인 창 `/v1/models` 카드의 `목록 보기`).
`curl` 없이 앱 안에서 봅니다.

표시 이름 · **모델 ID** · 사내 UUID · 설명이 한 줄에 나오고, 칸마다 복사 버튼이 있습니다.
드래그로 직접 선택해도 됩니다.

- **모델 ID** 가 클라이언트의 `model` 칸에 넣는 값입니다.
- **사내 UUID** 는 사내 담당자와 대조할 때 씁니다 — HTTP 응답에는 노출하지 않습니다.
- `전체 복사` 는 사람이 읽는 표 형식 평문(머리말에 서버 주소와 조회 시각),
  `ID만 복사` 는 줄바꿈으로 이은 모델 ID — OpenCode/Continue 설정의 `models` 배열에
  통째로 붙여넣는 용도입니다. 둘 다 **검색으로 걸러 놓은 것만** 담습니다.
- 행의 `기본으로` 를 누르면 `defaultModelAlias` 가 바뀝니다 (`model` 을 안 보낸 요청에 쓰는 값).
- 목록은 60초 캐시를 공유하고 `다시 조회` 로 즉시 새로 받습니다. 새 조회가 실패해도
  **이전 목록을 지우지 않습니다** — 사내가 잠깐 안 되는 것과 "쓸 모델이 없다"는 다른
  이야기인데, 표를 비우면 똑같이 보입니다.

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

오류는 전부 OpenAI 봉투를 탑니다 — `{"error":{message, type, param, code}}`. `param` 과
`code` 는 없으면 `null` 로 나갑니다(키를 빼면 `error.param` 을 무조건 읽는 클라이언트가
죽습니다).

`type` 은 **상태 코드에서 유도**합니다. 그래야 언제나 OpenAI 가 정의한 값이 나오고, 우리
고유의 구분은 `code` 로 옮겨 하나도 잃지 않습니다.

| 상황 | 상태 | `type` | `code` | 로그 문구 |
|---|---|---|---|---|
| 토큰 불일치 (토큰 모드) | 401 | `authentication_error` | `invalid_api_key` | `토큰 거부` |
| 본문이 JSON 이 아님 | 400 | `invalid_request_error` | `invalid_json` | `잘못된 요청` |
| 파라미터 위반 | 400 | `invalid_request_error` | `invalid_value` · `missing_required_parameter` · `unsupported_value` · `unsupported_content` | `잘못된 요청 · <param>` |
| 없는 모델 | 404 | `invalid_request_error` | `model_not_found` | `모델 없음` |
| 없는 경로 | 404 | `invalid_request_error` | `unknown_endpoint` | — |
| 메서드 불일치 | 405 | `invalid_request_error` | `method_not_allowed` | — |
| 본문 16MiB 초과 | 413 | `invalid_request_error` | `request_too_large` | `본문이 너무 큼` |
| FabriX 429 | 429 | `rate_limit_error` | `rate_limit_exceeded` | `사내 쿼터 초과` |
| 자격 증명 미설정 | 503 | `api_error` | `not_configured` | `사내 연결 설정이 필요합니다` |
| 연결 실패 · 30초 타임아웃 | 502 | `api_error` | `upstream_unreachable` | `사내 응답 없음` |
| 응답을 해석 못함 | 502 | `api_error` | `upstream_bad_response` | `응답을 해석하지 못했습니다` |
| 그 밖의 4xx/5xx | 그대로 전달 | 상태에서 유도 | `upstream_error` | `사내 오류 <code>` |

413 은 **핸들러 안에서** 잡습니다. axum 의 `DefaultBodyLimit` 레이어에 맡기면 초과가
핸들러에 들어오기 전에 평문 413 으로 끝나 로그 창에 아무 흔적도 남지 않습니다 —
사용자에게는 원인 없는 실패였습니다. 지금은 봉투도 주고 로그에도 한 건 남습니다.

상한(16MiB)을 넘은 본문은 **상한의 4배까지 받아 주고 나서** 413 을 돌려줍니다. 초과를
발견한 순간 연결을 닫으면 클라이언트는 아직 본문을 쓰던 중이라 `socket hang up` 만 보고
우리가 준비한 설명을 읽지 못합니다 — 설명을 주는 것이 이 처리의 목적이라 그러면 의미가
없습니다. 받은 내용은 쓰지 않고 버립니다. 4배마저 넘는 요청은 어차피 도와줄 방법이 없어
한 바이트도 읽지 않고 끊습니다.

**스트림 중간 오류**는 헤더가 이미 200 으로 나간 뒤라 상태 코드를 바꿀 수 없습니다.
오류 봉투를 SSE 프레임으로 흘리고, `finish_reason: "length"` 청크를 넣은 뒤 `[DONE]` 로
닫습니다. finish 를 아예 안 주면 종료 사유를 기다리는 클라이언트가 매달리고, `stop` 을
주면 끊긴 답변을 완성된 것처럼 부르는 거짓말이 됩니다.

### 파괴적 변경 — 없는 모델 이름은 404 입니다

예전에는 사내에 없는 모델 이름(`gpt-4o` 등)을 조용히 기본 모델로 바꿔 처리했습니다.
그러면 **오타가 성공처럼 보입니다** — 클라이언트는 `gpt-4o` 가 답했다고 믿고 실제로는
전혀 다른 모델이 답합니다. 이제 `404 model_not_found` 입니다.

클라이언트 설정에 `gpt-4o` 같은 이름을 넣어 두었다면 **모델 목록 창에서 모델 ID 를 복사해
바꾸세요**. `model` 을 아예 안 보내는 클라이언트는 계속 `defaultModelAlias` 로 동작합니다.

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
| `MOCK_NOSTREAM` | `llm`(기본) · `rag` · `filter` | 비스트림 응답 형태 |
| `MOCK_ECHO` | `1` | 답변으로 받은 payload 를 그대로 — fold 결과·규약 주입을 눈으로 |
| `MOCK_USAGE` | `1` | 토큰 수를 실어 보냅니다 (**스펙에 없는 가정된 모양**) |
| `MOCK_FINISH` | 아무 문자열 | 마지막 프레임 `finishReason` — `weird` 로 클램프 확인 |
| `MOCK_EMPTY` | `1` | 빈 답변 + 성공 표지 — 502 가 아니라 200 `content:""` 여야 함 |

네 조합(`snake|camel` × `delta|cumulative`) 모두 같은 결과가 나와야 합니다.

```bash
curl http://127.0.0.1:8787/v1/models
curl http://127.0.0.1:8787/v1/models/fabrix-chat-4        # 200
curl -i http://127.0.0.1:8787/v1/models/gpt-4o            # 404 model_not_found

# 정상 채팅 — usage · logprobs:null · system_fingerprint 가 보여야 합니다.
curl -i http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" -H "Authorization: Bearer anything" \
  -d '{"model":"fabrix-chat-4","messages":[{"role":"user","content":"안녕"}]}'

# 스트림 — 첫 청크가 role 만 담고, include_usage 를 켜면 [DONE] 앞에 usage 청크가 옵니다.
curl -N http://127.0.0.1:8787/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"fabrix-chat-4","stream":true,"stream_options":{"include_usage":true},
       "messages":[{"role":"user","content":"안녕"}]}'

# 규약 위반 — 각기 다른 param·code 로 400 이 와야 합니다.
curl -s .../v1/chat/completions -d '{"model":"fabrix-chat-4","messages":[{"role":"user","content":"hi"}],"temperature":5}'
curl -s .../v1/chat/completions -d '{"model":"fabrix-chat-4","messages":[{"role":"user","content":"hi"}],"n":2}'
curl -s .../v1/chat/completions -d '{"model":"fabrix-chat-4","messages":[]}'

# 없는 모델 · 잘못된 메서드 · 본문 초과
curl -i .../v1/chat/completions -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}'
curl -i -X GET http://127.0.0.1:8787/v1/chat/completions   # 405 봉투
```

위 시나리오는 **`cargo test` 의 통합 테스트**(`src-tauri/tests/proxy_http.rs`)가 가짜 사내
서버를 띄워 자동으로 검증합니다. 여기 curl 은 실서버·목업을 상대로 눈으로 확인할 때 씁니다.

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
| 창 3개 | 창 **4개** (모델 목록 추가) | 720×560 고정 창에 4열 표(표시 이름·모델 ID·UUID·설명)가 들어가지 않습니다. UUID 36자를 펼쳐 놓고 복사하려면 폭이 필요하고, 창을 넓힐 수 있어야 합니다. 목업에는 모델을 고르는 화면 자체가 없었습니다. |

## 실서버에 붙일 때 확인할 것

스펙 문서에 없어서 방어적으로 처리해 둔 것들입니다. 실서버 응답을 한 번 보면 확정할 수 있습니다.

- **SSE 프레임 형태** — `data:` 접두 유무, 종료 센티널. 현재는 양쪽 모두 처리합니다.
- **`content` 가 누적인지 증분인지** — 두 번째 프레임에서 자동 판별하고 그 모드로 고정합니다.
- **`repetion_penalty` · `tok_k`** — 스펙 문서의 철자 그대로 보냅니다(오타로 보이지만 서버가
  기대하는 키일 가능성이 높음). 다르면 `src-tauri/src/proxy/fabrix.rs` 의 `LlmConfig` 에서
  `rename` 두 줄만 고치면 됩니다.
- **토큰 사용량** — FabriX가 실측을 주지 않아 **문자 기반 추정치**를 채웁니다
  (ASCII 4자 ≈ 1토큰, 한글·CJK 1자 ≈ 1토큰). 추정 대상은 클라이언트의 `messages` 가 아니라
  **실제로 사내에 보낸 프롬프트**(`systemPrompt` + `contents`)와 생성된 답변이며, 도구 호출
  인자도 출력으로 셉니다. 정밀도를 흉내 내지 않는 이유: 사내 모델의 토크나이저를 모르는데
  `tiktoken` 을 끌어오면 "다른 모델의 정확한 값" 이 나올 뿐입니다.
  추정임을 세 곳에서 말합니다 — 응답 헤더 `x-fabrix-usage: estimated`, 로그 ③ 칸 꼬리,
  이 문단. `FabrixChunk` 가 `inputTokens`/`outputTokens`/중첩 `usage` 를 방어적으로 받아 두므로
  **사내가 토큰 수를 주기 시작하면 코드를 고치지 않고 실측으로 넘어갑니다**
  (헤더가 `upstream` 으로 바뀝니다 — `MOCK_USAGE=1` 로 확인할 수 있습니다).
- **`contents` 가 배열인 이유** — 턴 배열인지(한 항목 = 한 턴) 독립 프롬프트 배열인지 스펙에
  없습니다. 저장소에 스펙 문서가 없고 목업도 이 값을 로그만 찍으므로 확인할 방법이 없어,
  `fold_messages` 는 지금처럼 **한 덩어리 트랜스크립트**로 둡니다. 실서버에서 확인되면 바꿀
  자리는 그 함수의 반환값 한 곳뿐이고, `MOCK_ECHO=1` 로 사내가 받은 것을 그대로 되돌려 받아
  대조할 수 있습니다.
- **사내 루트 CA** — Windows 인증서 저장소(schannel)를 그대로 신뢰합니다. CA가 없어 실패하면
  설정에서 `TLS 인증서 검증 건너뛰기` 를 켤 수 있습니다.
