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

이미지 두 엔드포인트는 사내 **`POST /openapi/chat/v1/messages-with-models`** 로 번역됩니다
(chat 의 `/messages` 와 다름). `modelIds` 는 **[텍스트 모델, 이미지 모델]** 두 개를 함께 보냅니다 —
셋(텍스트·생성·인식) 모두 **설정 화면에서 선택**하며, 값은 All Model API 의 모델 id 입니다.

| | |
|---|---|
| 생성(t2i) | `application/x-www-form-urlencoded` · `isStream=false` · `messageConfig={width,height}` → 응답 `actions[0].answer`(base64) |
| 인식(i2t) | `multipart/form-data`(파일 파트 `files`) · `isStream=true` → SSE `content` 누적 |

> 배선만 확인하려면 설정에서 `이미지 스텁 모드`(`imageStubMode`)를 켜면 사내 호출 없이 1×1 자리표시자
> PNG 를 돌려줍니다 (응답 헤더 `x-fabrix-image-stub: 1`, 로그에 `[stub]` 표기).

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
  log.tsx              로그 창 900×620  — 받은 것 / 보낸 것 / 돌려준 것 / 와이어 원문
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
  proxy/tools.rs       도구 호출 에뮬레이션 — 규약 주입 · <tool_call> 파스아웃 · <think> 분리
  proxy/turn.rs        응답 조립 상태 기계 — 스트림·비스트림이 **이것 하나**를 공유
  logstore.rs          최근 50건 링버퍼 (본문·와이어 원문 모두 메모리 전용)
  port.rs              포트 가용성 · 점유 PID 조회 · 빈 포트 추천
  tray.rs windows.rs   트레이 메뉴 · 창 4개 관리 (모델 목록은 처음 열 때 생성)
src-tauri/tests/
  proxy_http.rs        HTTP 표면 통합 테스트 — 가짜 사내 서버를 띄워 규약을 확인
mock-fabrix/server.mjs 개발용 FabriX 스텁 (의존성 0)
scripts/probe-fabrix.mjs 실서버 프로브 — 변인 하나씩 바꿔 어디서 답이 끊기는지 (의존성 0)
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
  "imageTextModel": "",        // 이미지 호출에 함께 보내는 텍스트 모델 id — 설정 화면에서 선택
  "imageModel": "",            // 이미지 생성(FLUX) 모델 id — 설정 화면에서 선택
  "visionModel": "",           // 이미지 인식(gemma) 모델 id — 설정 화면에서 선택
  "imageStubMode": false,      // 이미지 백엔드 없이 1×1 자리표시자 PNG 로 배선만 검증
  "toolEmulation": true,       // 도구 호출(툴 콜) 흉내 내기 — 아래 "도구 호출" 참고
  "rawWireLog": true           // 클라이언트로 나간 원문 기록 — 아래 "와이어 원문" 참고
                               // (사내가 준 원문은 이 값과 무관하게 언제나 남습니다)
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
| 나머지 `messages` | `contents` — **턴당 원소 하나** (배열 위치가 롤: 짝수 user, 홀수 assistant). 연속 동일 롤은 병합, `role:"tool"` 결과는 user 턴 |
| `stream` | `isStream` |
| `temperature` `top_p` `max_tokens` `seed` `frequency_penalty` `top_k` | `llmConfig.{temperature, top_p, max_new_tokens, seed, …}`. `temperature` 는 사내 상한 **1.0** 으로 클램프. 페널티·top-k 는 문서와 샘플의 철자가 달라 **두 철자 모두** 보냅니다 (아래 참고) |
| `tools` `tool_choice` | 대응 필드가 **없어** `systemPrompt` 뒤의 규약 텍스트로 접힙니다 (아래 참고) |
| `stream_options.include_usage` | 스트림 꼬리에 `usage` 청크 (추정치 · 아래 참고) |

**받되 반영하지 않는 것** — 사내 `llmConfig` 에 자리가 없습니다. 조용히 버리지 않고
로그 ③ 칸 꼬리에 `무시된 파라미터: …` 로, ② 칸 칩에 흐린 글씨로 적습니다.

| 필드 | 왜 |
|---|---|
| `stop` | 사내에 대응 필드가 없습니다. 거절하지 않는 이유는 기본값으로 실어 보내는 클라이언트가 많아 400 이 더 많이 깨뜨리기 때문입니다 |
| `presence_penalty` | 사내 페널티 키가 하나뿐이고 `frequency_penalty` 가 이미 씁니다. 의미가 다른 두 값을 한 키에 겹쳐 넣는 것은 상위 동작을 지어내는 일입니다 |
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

**`temperature` 는 0–2 를 통과시키고 나갈 값만 1.0 으로 줄입니다.** 사내 스펙은 0–1 인데
OpenAI 는 0–2 라, 거절하면 그 범위를 기본값으로 보내는 클라이언트가 전부 400 을 받습니다.
조용히 줄이지는 않습니다 — 로그 ③ 칸 꼬리에 `temperature 1.5 → 1 (사내 상한)` 이 남습니다.

**페널티·top-k 는 두 철자를 모두 보냅니다.** 스펙 문서의 속성 목록은 `repetion_penalty`·
`tok_k` 이고, 벤더가 준 **실행되는 샘플 코드**는 `repetition_penalty`·`top_k` 입니다. 어느
쪽이 서버가 읽는 키인지 확정되지 않아 네 키에 같은 값을 싣습니다. 한쪽만 보내면 서버가 다른
쪽을 읽을 때 값이 **조용히 버려집니다** — 실제로 그래 왔을 수 있습니다. 사내가 `llmConfig` 의
모르는 키를 무시한다는 것은, 문서 철자만 보내면서도 앱이 동작해 온 사실이 말해 줍니다.

### `contents` 는 턴 배열입니다

`contents` 는 원소 하나가 한 턴이고 **배열 위치가 롤**입니다 (짝수 user, 홀수 assistant).
벤더 샘플이 근거입니다:

```python
"contents": ["안녕하세요?", "네 안녕하세요", "내 이름은 LCY인데 너 이름은 뭐니?"]
#              user           assistant        user
```

예전에는 "FabriX 엔 롤 구조가 없다" 고 보아 멀티턴을 `["User: …\n\nAssistant: …"]` 한
덩어리로 접었습니다. 롤 *라벨* 이 없을 뿐 배열이 턴을 담습니다. 한 덩어리로 보내면 사내
모델의 chat template 이 제대로 걸리지 않고, 모델은 "대화 중" 이 아니라 "대화록을 읽는 중"
으로 인식합니다 — 프롬프트 기반 툴콜은 모델이 자기 턴의 시작을 알아야 잘 동작하므로
도구 준수율에 직접 영향을 줍니다.

교대는 구조적으로 보장합니다. 연속 동일 롤은 하나로 병합하고(안 하면 원소가 하나 밀려 그
뒤 전체의 롤이 뒤집힙니다), 대화가 assistant 로 시작하면 앞에 `(continue)` user 턴을 넣습니다.
`role: "tool"` 결과는 **user 턴**으로 들어갑니다 — 사내엔 tool 롤이 없고, 결과를 모델에게
돌려주는 쪽이 우리입니다.

무엇이 어떻게 접혔는지는 로그 창 ② 칸에서 그대로 확인할 수 있습니다. 목업은 받은 원소를
롤과 함께 한 줄씩 찍어 주고, `MOCK_ECHO=1` 로 사내가 받은 것을 그대로 되돌려 받아 볼 수도
있습니다.

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
본문 `content` 에 `<think>…</think>` 로 섞여 오는 추론도 갈라내 이 필드로 옮깁니다 —
답변에 사고 과정이 새어 나오면 도구를 쓰든 안 쓰든 결과가 오염됩니다.

스트림과 비스트림은 **같은 상태 기계**(`proxy/turn.rs` 의 `Turn`)를 지납니다. 비스트림 응답도
스트림과 같은 이벤트 열로 바꿔(`fabrix::nonstream_events`) 같은 경로에 흘려보냅니다.
`finish_reason` 도 한 함수(`fabrix::decide_finish`)만 정합니다 — 두 경로가 각자 판단하면
같은 답변이 `stream` 여부에 따라 다른 종료 사유를 받고, 그것이 곧 "한쪽에서만 에이전트
루프가 끊김" 입니다.

모델 alias는 영문명에서 만듭니다 (`Chat 4` → `fabrix-chat-4`). 영문명이 없으면 UUID 앞 8자리를
씁니다 (`fabrix-01970a3b`) — 서버가 순서를 바꿔도 alias가 흔들리지 않게 하기 위함입니다.

### 어떤 모델이 있는지 보려면

트레이 메뉴 → **모델 목록 보기** (또는 메인 창 `/v1/models` 카드의 `목록 보기`).
`curl` 없이 앱 안에서 봅니다.

표시 이름 · **모델 ID** · 사내 UUID · 설명이 한 줄에 나오고, 칸마다 복사 버튼이 있습니다.
드래그로 직접 선택해도 됩니다.

- **모델 ID** 가 클라이언트의 `model` 칸에 넣는 값입니다.
- **사내 UUID** 는 사내 담당자와 대조할 때, 그리고 **설정 화면의 이미지 모델 셋**
  (텍스트·생성·인식)을 고를 때 씁니다 — 이미지 업스트림은 alias 가 아니라 UUID 를 받습니다.
  HTTP 응답(`/v1/models`)에는 노출하지 않습니다.
- `전체 복사` 는 사람이 읽는 표 형식 평문(머리말에 서버 주소와 조회 시각),
  `ID만 복사` 는 줄바꿈으로 이은 모델 ID — OpenCode/Continue 설정의 `models` 배열에
  통째로 붙여넣는 용도입니다. 둘 다 **검색으로 걸러 놓은 것만** 담습니다.
- 행의 `기본으로` 를 누르면 `defaultModelAlias` 가 바뀝니다 (`model` 을 안 보낸 요청에 쓰는 값).
- 설정 화면의 기본 모델 선택기와 이미지 모델 드롭다운 셋도 **이 목록 한 번의 조회**를
  나눠 씁니다 — 사내에 목록 엔드포인트가 하나뿐이라 두 번 물을 이유가 없습니다.
- 목록은 60초 캐시를 공유하고 `다시 조회` 로 즉시 새로 받습니다. 새 조회가 실패해도
  **이전 목록을 지우지 않습니다** — 사내가 잠깐 안 되는 것과 "쓸 모델이 없다"는 다른
  이야기인데, 표를 비우면 똑같이 보입니다.

### 도구 호출 (툴 콜)

FabriX 요청 스키마에는 도구 필드가 **없습니다**. 그래서 `tools` 를 그대로 넘길 수 없고,
프롬프트 규약 + 출력 파싱으로 흉내 냅니다. 요청에 `tools` 가 있으면 자동으로 켜지고,
설정 화면에서 끌 수 있습니다(`toolEmulation`). 도구를 안 쓰는 요청은 아무 영향이 없습니다.

- **나갈 때** — 도구 스키마와 출력 규약을 `systemPrompt` 뒤에 붙이고, **`contents` 꼬리에
  2~3줄 리마인더**를 한 번 더 넣습니다. 규약은 영어로 씁니다. 감싸는 대상(도구 이름·설명·
  JSON Schema)이 전부 영어라, 한국어 프레임을 씌우면 모델이 센티널을 뱉는 대신 도구를
  한국어로 *설명하기* 시작합니다.

  꼬리 리마인더가 필요한 이유: 모델이 **마지막으로 읽는 글**은 대화 꼬리입니다. OpenCode
  같은 클라이언트의 시스템 프롬프트는 수천 토큰이고 도구를 "네이티브 기능" 으로 설명하므로,
  우리 규약 블록은 저 앞으로 밀려납니다. 꼬리에는 **형식만** 다시 적습니다 — 규약 전문을 두
  번 실으면 토큰이 두 배로 들고, 같은 글이 두 번 나오면 모델이 둘째 것을 예시로 오독합니다.
  리마인더는 언제나 **user 자리**에 붙습니다 — assistant 자리에 넣으면 지시문이 모델 자신의
  발화가 되어, 모델은 자기가 이미 그렇게 말했다고 읽습니다.
- **들어올 때** — 답변에서 아래 블록을 걷어내 OpenAI `tool_calls` 로 조립하고,
  `finish_reason` 을 `tool_calls` 로 바꿉니다. **본문과 추론 양쪽**을 훑습니다 (아래 참고).

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

**센티널은 본문뿐 아니라 추론 채널에서도 걷어냅니다.** 사내 모델이 추론형이면 `<tool_call>`
을 `reasoningContent` 에 실어 보내거나 본문 안 `<think>` 블록에 넣습니다. 예전에는 추론
채널이 스캐너를 우회해서 그런 호출이 **한 건도** 잡히지 않았고, 그러면 `finish_reason` 이
언제나 `stop` 이라 OpenCode 같은 에이전트가 **한 스텝 만에 턴을 끝냈습니다**("추론 단계마다
멈춘다"는 증상이 정확히 이 경로였습니다).

- 채널마다 버퍼를 따로 두어, 한쪽의 미완성 센티널 꼬리가 다른 쪽 텍스트에 이어 붙지 않습니다.
- `index` 는 채널을 가로질러 **하나의 수열**입니다. 채널별로 세면 본문의 첫 호출과 추론의
  첫 호출이 둘 다 `index: 0` 을 받아, 클라이언트가 서로 다른 두 호출을 한 호출의 조각으로
  이어 붙입니다.
- 걷어낸 산문은 원래 채널로 돌아갑니다 — 추론 산문은 `delta.reasoning_content` 로,
  본문 산문은 `delta.content` 로.

> 이 방식은 **모델이 형식을 지켜 줘야** 동작합니다. 지키지 않으면 로그 ③ 칸 꼬리에
> `호출 0건 — 모델이 규약을 따르지 않음` 이 남습니다. 다만 사내가 **0자**를 준 턴에는 그 대신
> `사내 응답이 비어 판단할 근거가 없음` 이 남습니다 — 그 턴의 모델은 규약을 지킬 기회조차
> 없었고, 두 문구를 하나로 쓰면 사용자를 없는 원인(도구 규약) 쪽으로 보냅니다.
> ③ 칸은 파서가 걷어내기 **전** 원문이라
> `<tool_call>` 이 실제로 있었는지 눈으로 확인할 수 있습니다 — 추론이 있으면 `[추론]` /
> `[답변]` 두 칸으로 나눠 담으므로 **어느 채널에** 있었는지도 보입니다.
> 호출이 잡혔다면 꼬리에 `호출 3건 · 그중 2건은 추론 채널에서` 처럼 출처가 적힙니다.
> 도구를 껐다면 누가 껐는지도 구분해 적습니다 — 설정(`toolEmulation`)인지,
> 클라이언트의 `tool_choice: "none"` 인지.

### 와이어 원문 (로그 ④ 칸)

로그 창의 ③ 칸은 **가공된** 답변입니다 — `<think>` 를 갈라내고 `<tool_call>` 을 걷어낸
뒤의 글입니다. 그래서 `0자 · finish_reason: stop` 이 떴을 때 그것이

- 사내가 정말 아무 말도 안 한 것인지,
- 사내는 말했는데 **우리가 프레임을 못 읽은 것**인지,
- 읽긴 했는데 우리가 흘려버린 것인지

를 ③ 칸만으로는 가릴 수 없습니다. ④ 칸이 그 자리입니다. 한 호출당 두 쪽이
**가공 없이** 남습니다.

| 쪽 | 무엇 | 스트리밍이면 | 언제 남나 |
| --- | --- | --- | --- |
| 사내 → 프록시 | 상태 줄 · 헤더 · 본문 바이트 그대로 | `data:` 줄까지 원문 그대로 | **언제나** |
| 프록시 → 클라이언트 | 클라이언트가 실제로 읽은 본문 | 우리가 쓴 `data:` 줄 그대로 (`[DONE]` 포함) | `rawWireLog`(기본 켜짐)가 켜져 있을 때 |

읽는 법은 간단합니다. **위가 비었으면 사내 문제**, 위에 글이 보이는데 아래가 비었으면
프록시 문제입니다. "전체보기 → 본문 복사"로 그대로 공유할 수 있습니다.

**사내가 준 쪽만은 토글에서 뺐습니다.** ③ 칸의 `사내 원문 보기` 는 사용자가 답변을
의심하는 **바로 그때** 눌리는 버튼입니다. 그 순간 설정이 꺼져 있었다면 되살릴 방법이
없고, 재현되지 않는 한 번짜리 응답이 특히 그렇습니다. 토글이 끄는 것은 클라이언트로
나간 쪽뿐이고, 담았는지는 쪽마다 따로 적어 화면이 "꺼져서 비었다" 와 "켰는데 안 왔다" 를
여전히 다르게 말합니다.

### ③ 칸에서 바로 여는 원문

원문을 보려고 ④ 칸까지 내려갈 필요는 없습니다. ③ 돌려준 응답 칸의 `사내 원문 보기`
버튼이 **사내가 준 응답 바이트 그대로**를 같은 팝업으로 엽니다 — ③ 본문이 봉투를 벗기고
델타를 이어붙인 가공된 답변인 것과 달리, 이쪽은 상태 줄 · 헤더 · `data:` 줄이 그대로
있는 원문입니다. 답변이 짧아 `전체보기` 가 뜨지 않는 호출에도 이 버튼은 늘 있습니다.

`/v1/models` 도 원문을 남깁니다. ③ 칸이 보여 주는 목록은 우리가 다시 그린 것이라,
사내가 실제로 어떤 JSON 을 줬는지는 원문에만 있습니다. 60초 캐시가 유효해 사내를 부르지
않은 호출에는 **캐시를 채운 조회의 원문**을 그렇다고 적어서 보여 줍니다 — 옛 바이트를
방금 받은 응답인 척 두지 않기 위한 한 줄입니다. 이미지 호출은 제외입니다(base64 가
수 MB 라 50건 링버퍼에 들 수 없습니다 — ③ 칸이 한 줄 요약만 남기는 것과 같은 이유).

위쪽은 상태 줄과 헤더로 시작합니다 — 본문만으로는 답할 수 없는 질문들이 거기 걸립니다.
`200` 인데 빈 것인지 `4xx` 였는지, `text/event-stream` 으로 왔는지 `application/json`
으로 왔는지, 사내가 붙여 준 추적 id 는 무엇인지(담당자에게 문의할 때 이 값 하나가 대화를
줄여 줍니다). `set-cookie` 만 뺍니다.

**사내가 거절한 호출도 전문이 남습니다.** 오류 봉투의 `message` 에는 앞 200자만 들어가는데,
사내가 *왜* 거절했는지는 그 뒤에 있을 수 있습니다(예: `maxNewTokens ... exceeds the model
limit`). 그 갈래에서도 상태 줄 + 헤더 + 본문 전문이 ④ 칸에 그대로 담깁니다.

호출당 각 **256KiB** 까지만 담고, 넘친 만큼은 `…(상한 256KiB 를 넘어 N바이트를 버렸습니다)`
로 적습니다 — 조용히 자르면 잘린 JSON 을 진짜 응답으로 오해합니다. 다른 본문과 마찬가지로
**메모리에만** 있고 앱을 끄면 사라집니다. 메모리를 아껴야 하면 설정에서 끕니다 — 그러면
④ 칸의 아래쪽(클라이언트로 나간 본문)만 "기록이 꺼져 있습니다"로 바뀌고, 사내가 준 위쪽은
그대로 남습니다. 끄면 링버퍼가 드는 최악은 50건 × 256KiB 로 줄어듭니다.

프레임을 읽지 못하면 ③ 칸 꼬리에 `해석하지 못한 프레임 N개` 가 붙습니다. 필드 하나의
타입만 어긋나도 그 프레임은 통째로 떨어지고, 그 안에 있던 `content` 까지 함께 사라집니다 —
예전에는 그것이 조용해서 "모델이 말을 안 했다"와 구별되지 않았습니다.

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
MOCK_CASE=camel npm run mock
```

| 변수 | 값 | 무엇을 태우는가 |
|---|---|---|
| `MOCK_CASE` | `snake`(기본) · `camel` | 필드 표기 양쪽 |
| `MOCK_DONE` | `1` | 끝에 `data: [DONE]` 을 붙입니다. **실서버는 보내지 않습니다** — 프록시가 있어도 견디는지 볼 때만 |
| `MOCK_REASONING` | `field` · `think` | 답변을 추론 쪽으로 흘립니다 (아래 「도구 호출 확인」) |
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
| `MOCK_GARBLE` | `1` | 모양이 어긋난 프레임 둘. `contentReferences: null` 은 **살아야** 하고(답변에 `(널 참조 프레임)`), 객체 `content` 는 `해석하지 못한 프레임 1개` 로 세어져야 합니다 |

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
MOCK_TOOLCALL=parallel MOCK_CHUNK=1 npm run mock
MOCK_TOOLCALL=malformed npm run mock                 # 텍스트로 되돌아와야 함
```

**추론 채널로 오는 호출**은 `MOCK_REASONING` 으로 재현합니다. 이 조합이 "추론 단계마다
`stop` 으로 끝난다"는 실패를 그대로 만들어 냅니다 — 고쳐진 프록시는 `tool_calls` 를 내야 합니다.

```bash
MOCK_TOOLCALL=single MOCK_REASONING=field MOCK_CHUNK=1 npm run mock   # reasoningContent 로
MOCK_TOOLCALL=single MOCK_REASONING=think MOCK_CHUNK=1 npm run mock   # 본문 안 <think> 로
MOCK_ECHO=1 npm run mock    # 꼬리 리마인더가 실려 나갔는지 ② 칸에서 확인
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
`MOCK_REASONING=field` 로 띄운 목업에도 **같은 결과**가 나와야 합니다 — 다르면 추론 채널이
다시 스캐너를 우회하고 있다는 뜻입니다.

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

의도적으로 다르게 만든 곳들입니다.

| 목업 | 구현 | 이유 |
|---|---|---|
| 메인 창 720 × **520** | 720 × **560** | 목업이 그린 내용(상태 카드 + 엔드포인트 2장 + 최근 4행)이 520 안에 들어가지 않습니다. 고정 크기 대시보드에 스크롤바를 만드느니 40px 을 줬습니다. |
| 트레이 상태 헤더가 점·건수·주소가 든 카드 | 클릭 불가 항목 **두 줄** | 네이티브 Windows 메뉴에 그런 블록을 넣을 수 없습니다. 동작 항목(끄기·복사·창·로그·종료)은 목업과 같습니다. |
| 로그 ③ 칸의 `원본 JSON 보기` | ③ 에 버튼 **둘** — `전체보기` 와 `사내 원문 보기` | ③ 칸이 들고 있는 것은 가공된 답변 전문이라 그 본문을 "원본"이라 부르면 거짓말입니다. 그래서 본문은 가진 그대로 `전체보기` 로 부르고, 사내가 준 바이트는 **다른 버튼**으로 엽니다 — 이름이 서로 다르니 무엇을 보고 있는지 헷갈리지 않습니다. 두 쪽을 나란히 놓고 비교하는 자리는 여전히 ④ 칸입니다 (위 「와이어 원문」). |
| 꺼짐 화면의 최근 호출은 항상 빈 상태 | 기록이 있으면 계속 보여줌 | 방금 뭐가 오갔는지가 이 앱의 세 기능 중 하나인데, 끄자마자 지워 버릴 이유가 없습니다. 기록이 정말 없을 때만 목업의 빈 상태가 나옵니다. |
| Google Fonts CDN | 번들에 포함 | 사내망/오프라인에서 CDN 을 못 받으면 조판이 무너집니다. |
| 스트림 끝에 `data: [DONE]` | 기본으로 **안 보냄** (`MOCK_DONE=1` 로 켬) | 실서버가 보내지 않습니다 — 벤더 샘플이 모든 이벤트에 `json.loads` 를 그대로 걸므로 `[DONE]` 이 온다면 거기서 죽습니다. 목업이 실서버보다 관대하면 프록시의 결함이 목업에서 안 보입니다. (프록시는 양쪽 다 견딥니다.) |
| 창 3개 | 창 **4개** (모델 목록 추가) | 720×560 고정 창에 4열 표(표시 이름·모델 ID·UUID·설명)가 들어가지 않습니다. UUID 36자를 펼쳐 놓고 복사하려면 폭이 필요하고, 창을 넓힐 수 있어야 합니다. 목업에는 모델을 고르는 화면 자체가 없었습니다. |

## 실서버에 붙일 때 확인할 것

스펙 문서에 없어서 방어적으로 처리해 둔 것들입니다. 실서버 응답을 한 번 보면 확정할 수 있습니다.

> 실서버 응답을 뜨는 가장 빠른 길은 로그 창에서 호출 한 건을 고르고 ③ 칸의
> **`사내 원문 보기` → 본문 복사** 를 누르는 것입니다. 사내가 준 바이트가 가공 없이 그대로
> 나오고, 설정과 무관하게 언제나 있습니다. 클라이언트로 나간 쪽까지 나란히 보려면 ④ 칸으로
> 내려갑니다. 아래 미확인 항목들은 그 한 장이면 대부분 확정됩니다.

### 실서버 프로브 — `npm run probe`

로그는 "이 요청이 이렇게 됐다" 를 말해 주지만, **어떤 요청 모양이 문제인지**는 말해 주지
못합니다. 클라이언트 하나로는 변인을 하나씩 바꿔 볼 수 없기 때문입니다. `scripts/probe-fabrix.mjs`
는 프록시도 클라이언트도 빼고 **사내 서버를 직접** 두드립니다 — 변인 하나만 다른 요청들을
차례로 보내고, 어디서 답이 끊기는지 표로 보여 줍니다.

```bash
npm run probe                 # 전체 (8항목)
npm run probe -- turns        # 이름으로 골라서 (부분 일치)
npm run probe -- sweep        # systemPrompt 길이를 2k→64k 로 늘려 가며 어디서 끊기는지
npm run probe -- --raw        # 사내가 준 프레임 원문까지
```

인증 정보는 `~/.fabrix-proxy/config.json` 에서 읽습니다 — 키를 명령줄에 붙이지 않기 위함입니다
(셸 히스토리에 남습니다). 모델은 목록의 첫 모델이고 `--model=<UUID>` 로 고를 수 있습니다.

| 항목 | 무엇을 가르는가 |
| --- | --- |
| `baseline` | 기준선 — systemPrompt 없음 · 1턴 |
| `system` | 큰 systemPrompt + 1턴 (프롬프트 크기만 다름) |
| `turns` | 3턴 교대. `B` 라고 답하면 `contents` 가 정말 턴 배열입니다 |
| `system+turns` | 큰 systemPrompt + 3턴 |
| `maxtokens-32000` / `maxtokens-2048` | `llmConfig.maxNewTokens` 상한 |
| `long-output` | 긴 출력 요구 — 첫 토큰이 늦는지, 아예 안 오는지 |
| `toolblock` | 주입한 도구 규약 자체가 답을 막는지 |
| `sentinel-control` / `sentinel-echo` | 같은 문장을 태그만 바꿔 따라 적게 시킵니다. `<tool_call>` 이 **와이어를 통과하는지** |
| `opencode` | 프록시가 실제로 보내는 모양 전부 — 큰 systemPrompt + 도구 규약 + 꼬리 리마인더 + `maxNewTokens 32000` |

읽는 법: `✔` 은 답이 온 것, `△` 는 **200 인데 빈 답**, `✖` 는 오류·시간 초과입니다. **이웃한 두
항목의 차이가 곧 원인입니다** — `system` 이 ✔ 인데 `system+turns` 가 △ 면 턴 배열이 범인이고,
`maxtokens-32000` 만 △ 면 상한이 범인입니다.

`sentinel-echo` 한 쌍은 따로 읽습니다. 우리가 고른 센티널 `<tool_call>` 은 Qwen·Hermes·GLM
계열의 **네이티브** 툴콜 형식이라 모델이 잘 뱉는 반면, 같은 이유로 게이트웨이(vLLM 의
`--tool-call-parser` 등)가 그 블록을 자기 것으로 알고 응답 본문에서 **걷어내 버릴** 수 있습니다.
그러면 모델은 도구를 불렀는데 우리에게 도착하는 `content` 는 정확히 0자입니다.

- 둘 다 `✔` → 센티널은 통과합니다. 빈 응답의 원인은 다른 항목에 있습니다.
- `sentinel-control` 만 `✔` → **게이트웨이가 `<tool_call>` 을 먹습니다.** 이 센티널로는 프롬프트
  기반 툴콜이 성립하지 않습니다 — 다른 센티널을 쓰거나 사내 네이티브 툴콜 경로를 받아야 합니다.
- 둘 다 `△` → 모델이 따라 적기를 거부한 것이라 이 시험으로는 가릴 수 없습니다.

**샘플로 확정된 것** — 더 이상 추측하지 않습니다.

- **SSE 프레임 형태** — 표준 `data:` 프레이밍입니다(샘플이 `sseclient` 를 씁니다). 종료
  센티널은 **없습니다** — 샘플이 모든 이벤트에 `json.loads` 를 그대로 걸므로 `[DONE]` 이
  온다면 거기서 죽습니다. 종료는 `status == "SUCCESS"` + `response_code == "R20000"` 프레임입니다.
- **`content` 는 증분** — 샘플이 `result_message += ch_json['content']` 로 이어 붙입니다.
  누적 판별·재작성(`Reset`) 배관은 전부 걷어냈습니다.
- **camel/snake 이중 표기는 필요합니다** — 문서가 "스트림은 스네이크, 비스트림은 카멜" 이라
  명시하고, `reasoning_content`·`processing_content`·`content_references` 세 개만 비스트림
  문맥에서도 스네이크로 적혀 있습니다. `FabrixChunk` 의 `alias` 는 **지우면 안 됩니다.**
- **`filterBlockReason` 은 "차단 사유" 가 아니라 "필터 판정" 입니다** — 이름과 달리 **통과**
  했을 때도 채워져 옵니다. 실서버 와이어의 세 코드: `FR-200`(요청 분석, 문구 전부 null),
  `FR-201`(통과, `message` 가 `The content was allowed by the filter`, `ko`/`en` 이 `Default`),
  `FR-403`(차단, `ko` 에 실제 사유). 목업이 FR-200·FR-403 만 흉내 냈던 탓에 통과 판정에도
  **문구가 실려 온다**는 사실이 드러나지 않았고, 그래서 빈 답변 + FR-201 인 비스트림 응답이
  `502 The content was allowed by the filter` 로 나갔습니다 — 통과를 차단 사유로 되읽은
  오류입니다. 지금은 통과 코드를 걸러 냅니다. 모르는 코드는 여전히 **차단으로 봅니다**
  (사유를 삼키는 쪽이 더 나쁩니다).

### 빈 응답(0자)을 만났을 때

에이전트(opencode 등)가 한 질문에 아무 것도 하지 않고 턴을 끝냈다면, 갈래는 둘뿐이고 순서도
정해져 있습니다.

1. **로그 창에서 그 호출의 `note` 를 봅니다.** `사내가 빈 응답` 이면 사내가 한 글자도 주지
   않은 것입니다. `도구 미사용` 이면 사내는 말했는데 `<tool_call>` 이 없었던 것이고, 그때는
   ③ 칸 원문에서 센티널이 어느 채널에 있었는지 눈으로 확인합니다. 두 문구는 다음에 볼 곳이
   정반대라 절대 섞이지 않습니다.
2. **`해석하지 못한 프레임 N개` 가 붙어 있는지 봅니다.** 붙어 있으면 사내가 아니라 우리가 못
   읽은 것이고, 원문은 ④ 칸에 그대로 있습니다.
3. `사내가 빈 응답` 이 맞다면 **`npm run probe`** 로 넘어갑니다. 로그는 "이 요청이 이렇게 됐다"
   까지만 말하고, **어떤 요청 모양이 문제인지**는 위 표의 이웃 항목 비교만이 답합니다.

**아직 미확인** — 남은 추측입니다.
- **페널티·top-k 의 철자** — 문서(`repetion_penalty`·`tok_k`)와 샘플(`repetition_penalty`·
  `top_k`)이 다릅니다. 지금은 **네 키를 모두** 보냅니다. 확정되면 `LlmConfig` 에서 안 쓰는
  쪽 두 필드만 지우면 됩니다.
- **`contentReferences[].answer`** — 문서는 `content_references` 를 "References used while
  generating the answer" 라고만 하고 `answer` 하위 필드를 적지 않습니다. 즉 이 답변 폴백은
  죽은 코드일 수 있습니다. `content` 가 있으면 발동하지 않아 비용이 0 이라 남겨 뒀습니다 —
  플러그인/RAG 응답에 정말 없다면 `answer_text()` 에서 지우세요.
  (필드 자체는 `Option<Vec<…>>` 입니다. 값이 명시적 `null` 이면 `Vec` 역직렬화가 실패해
  **프레임 한 통째가** — 그 안의 `content` 까지 — 조용히 사라지기 때문입니다.)
- **`processing_content`** — 문서화된 필드지만(스트리밍 중간 처리 과정, list) 파싱하지
  않습니다. 무엇이 담기는지 보고 필요하면 로그에 실으면 됩니다.
- **`contents` 가 정말 턴 교대인가** — 샘플이 3턴 대화라 그렇게 읽었습니다.
  `npm run probe -- turns` 가 이 요청(`["\"B\"라고만 답해", "B", "네가 방금 뭐라고 답했지? 한 단어로."]`)
  을 그대로 보냅니다. `B` 라고 답하면 턴 교대가 맞습니다. 빈 답이 오면 이 모델·게이트웨이가
  다원소 `contents` 를 받지 못하는 것이고, 그때는 `fabrix::fold_messages` 만 되돌리면 됩니다.
- **토큰 사용량** — FabriX가 실측을 주지 않아 **문자 기반 추정치**를 채웁니다
  (ASCII 4자 ≈ 1토큰, 한글·CJK 1자 ≈ 1토큰). 추정 대상은 클라이언트의 `messages` 가 아니라
  **실제로 사내에 보낸 프롬프트**(`systemPrompt` + `contents`)와 생성된 답변이며, 도구 호출
  인자도 출력으로 셉니다. 정밀도를 흉내 내지 않는 이유: 사내 모델의 토크나이저를 모르는데
  `tiktoken` 을 끌어오면 "다른 모델의 정확한 값" 이 나올 뿐입니다.
  추정임을 세 곳에서 말합니다 — 응답 헤더 `x-fabrix-usage: estimated`, 로그 ③ 칸 꼬리,
  이 문단. `FabrixChunk` 가 `inputTokens`/`outputTokens`/중첩 `usage` 를 방어적으로 받아 두므로
  **사내가 토큰 수를 주기 시작하면 코드를 고치지 않고 실측으로 넘어갑니다**
  (헤더가 `upstream` 으로 바뀝니다 — `MOCK_USAGE=1` 로 확인할 수 있습니다).
- **사내 루트 CA** — Windows 인증서 저장소(schannel)를 그대로 신뢰합니다. CA가 없어 실패하면
  설정에서 `TLS 인증서 검증 건너뛰기` 를 켤 수 있습니다.
