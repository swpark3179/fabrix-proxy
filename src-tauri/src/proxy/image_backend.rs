//! 이미지 백엔드 seam — FLUX 2.0(생성) + gemma 4(인식) 을 사내 FabriX 연결 위에서 호출합니다.
//!
//! 연결(base URL · `x-fabrix-client` · `x-openapi-token`)은 chat 과 **동일하게 재사용**하지만,
//! 실제 업스트림 경로/요청·응답 스키마는 chat 과 **다릅니다**(사내 확인). 그 구체 규격은
//! 파이썬 샘플(이미지 분석/생성)이 도착하면 `generate`/`understand` 본문에 채웁니다.
//!
//! 그 전까지 두 메서드는 **의도적으로 `NotImplemented`(→ HTTP 501) 로 실패**합니다.
//! 미연동을 그럴듯한 가짜 이미지로 감추지 않기 위한 설계입니다(스텁 오인 방지, 스펙 §8 #10).
//! 바이트를 돌려주는 유일한 경로는 설정의 `image_stub_mode` 를 켠 **명시적 자리표시자 모드**뿐이며,
//! 그때도 호출부가 모든 층에 `[stub]` 로 표기합니다.

use std::time::Duration;

use crate::config::Config;
use crate::proxy::fabrix::FabrixError;

/// TODO(samples): 실제 사내 이미지 생성(FLUX) 업스트림 경로. 현재는 자리표시자.
pub const IMAGE_GEN_PATH: &str = "/openapi/image/v1/generations";
/// TODO(samples): 실제 사내 이미지 이해(gemma) 업스트림 경로. 현재는 자리표시자.
pub const VISION_PATH: &str = "/openapi/image/v1/understand";
/// 이미지 생성은 chat 의 30초보다 길 수 있습니다.
pub const IMAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 요청 size 가 없거나 파싱 실패 시 기본 해상도.
pub const DEFAULT_SIZE: (u32, u32) = (1024, 1024);
/// `n` 상한 — 작업/메모리 폭주 방지 (스펙상 다중 생성은 미지원이므로 사실상 1).
pub const MAX_N: u32 = 4;

// ─────────────────────────── 오류 ───────────────────────────

/// 이미지 파이프라인 오류. 전송/업스트림 분류는 `FabrixError` 를 재사용합니다.
#[derive(Debug, Clone)]
pub enum ImageError {
    /// 잘못된 요청(prompt/이미지 누락, data URL 파싱 실패 등). 400.
    BadRequest(String),
    /// 백엔드 미연결(스텁). 501 — 소비자 재시도 집합(429/503) 밖이라 재시도 폭주가 없습니다.
    NotImplemented(String),
    /// 전송/업스트림 — FabriX 분류 재사용.
    Backend(FabrixError),
}

impl ImageError {
    pub fn status(&self) -> u16 {
        match self {
            ImageError::BadRequest(_) => 400,
            ImageError::NotImplemented(_) => 501,
            ImageError::Backend(inner) => match inner {
                // 값비싼 이미지 백엔드의 일시 장애는 소비자가 재시도해야 유용하므로,
                // chat 의 502 대신 503(재시도 집합 포함)으로 매핑합니다.
                FabrixError::Unreachable(_) => 503,
                other => other.status(),
            },
        }
    }

    pub fn note(&self) -> String {
        match self {
            ImageError::BadRequest(_) => "잘못된 요청".into(),
            ImageError::NotImplemented(_) => "이미지 백엔드 미연결".into(),
            ImageError::Backend(inner) => inner.note(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            ImageError::BadRequest(m) => m.clone(),
            ImageError::NotImplemented(m) => {
                format!("{m} (파이썬 샘플 반영 전까지 이미지 업스트림은 미구현입니다)")
            }
            ImageError::Backend(inner) => inner.message(),
        }
    }

    /// 기계가 분기하는 값. `type` 은 상태 코드에서 유도됩니다(`proxy::openai_type`)
    /// — 예전 `kind()` 가 `type` 자리에 넣던 비표준 `not_implemented` 가 여기로 왔습니다.
    pub fn code(&self) -> &'static str {
        match self {
            ImageError::BadRequest(_) => "invalid_value",
            ImageError::NotImplemented(_) => "not_implemented",
            ImageError::Backend(inner) => inner.code(),
        }
    }

    pub fn envelope(&self) -> crate::openai::ErrorEnvelope {
        crate::openai::ErrorEnvelope::new(
            self.message(),
            crate::proxy::openai_type(self.status()),
            Some(self.code().to_string()),
        )
    }
}

// ─────────────────────────── 모델 · 사이즈 resolve ───────────────────────────

/// FLUX(생성) 모델 — 설정 화면에서 고른 고정값. 비어 있으면 미설정 오류
/// (요청 `model` 로 폴백하지 않고, 설정에서 고르도록 유도합니다).
pub fn resolve_image_model(cfg: &Config) -> Result<String, ImageError> {
    let m = cfg.image_model.trim();
    if m.is_empty() {
        Err(ImageError::Backend(FabrixError::NotConfigured))
    } else {
        Ok(m.to_string())
    }
}

/// gemma(인식) 모델 — 설정 화면에서 고른 고정값.
pub fn resolve_vision_model(cfg: &Config) -> Result<String, ImageError> {
    let m = cfg.vision_model.trim();
    if m.is_empty() {
        Err(ImageError::Backend(FabrixError::NotConfigured))
    } else {
        Ok(m.to_string())
    }
}

/// 지원 해상도 스냅 자리. 지금은 요청값(또는 기본값)을 그대로 통과시킵니다.
/// TODO(samples): FLUX 지원 해상도로 최근접 스냅.
pub fn snap_size(requested: Option<(u32, u32)>) -> (u32, u32) {
    requested.unwrap_or(DEFAULT_SIZE)
}

/// 결과 이미지를 정확한 요청 크기에 맞추는 자리. 지금은 항등(입력 그대로).
/// TODO: resize/crop — 픽셀 조작이라 향후 `image` 크레이트가 필요합니다(현재 범위 밖).
pub fn fit_output(bytes: Vec<u8>, _target: (u32, u32)) -> Vec<u8> {
    bytes
}

/// gemma 설명 + 사용자 편집 지시를 FLUX 프롬프트로 결합합니다. (순수 · 테스트 대상)
/// 결합 규칙은 초기 버전이며 이후 정교화합니다.
pub fn compose_edit_prompt(description: &str, instruction: &str) -> String {
    let description = description.trim();
    let instruction = instruction.trim();
    if description.is_empty() {
        return instruction.to_string();
    }
    format!(
        "원본 이미지 설명:\n{description}\n\n요청된 편집:\n{instruction}\n\n\
         위 설명을 바탕으로, 편집 지시를 반영한 이미지를 생성하세요."
    )
}

/// 매직바이트로 mime 을 판정합니다 (PNG / JPEG / WebP). 그 외는 PNG 로 간주합니다.
pub fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 3 && bytes[0..3] == [0xFF, 0xD8, 0xFF] {
        "image/jpeg"
    } else if bytes.len() >= 8
        && bytes[0..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
    {
        "image/png"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/png"
    }
}

/// 1×1 투명 PNG. `image_stub_mode` 에서만 반환하며, 실제 성공과 혼동되지 않도록
/// 호출부가 `[stub]` 로 표기합니다. (CRC 를 손으로 맞추지 않도록 base64 로 임베드)
pub fn placeholder_png() -> Vec<u8> {
    const B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
    crate::proxy::b64::decode(B64).unwrap_or_default()
}

// ─────────────────────────── 클라이언트 ───────────────────────────

/// FabriX 연결(base URL · 헤더)을 재사용하는 이미지 백엔드 클라이언트.
/// `FabrixClient` 와 필드는 같지만 이미지 전용 메서드/경로를 갖습니다.
///
/// 이 구조체와 `state.image_client()` 생성자 하나가 "이미지가 FabriX 게이트웨이를 경유한다"는
/// 결정을 격리하는 seam 입니다 — 나중에 별도 이미지 서비스로 바뀌어도 여기와 설정 필드만 손보면 됩니다.
pub struct ImageClient {
    pub http: reqwest::Client,
    pub base: String,
    pub client_key: String,
    pub token: String,
}

impl ImageClient {
    /// FLUX 2.0 — 텍스트→이미지. **미구현(스텁)**: 절대 가짜 성공을 돌려주지 않습니다.
    pub async fn generate(
        &self,
        _prompt: &str,
        _size: (u32, u32),
        _model: &str,
    ) -> Result<Vec<u8>, ImageError> {
        // TODO(samples): 파이썬 '이미지 생성' 샘플대로 구현.
        //   POST {base}{IMAGE_GEN_PATH} — 헤더는 FabriX 와 동일(x-fabrix-client / x-openapi-token),
        //   요청 바디/응답 스키마는 chat 과 다름. IMAGE_REQUEST_TIMEOUT 적용, 결과 이미지 바이트 반환.
        let _ = (&self.http, &self.base, &self.client_key, &self.token);
        Err(ImageError::NotImplemented(
            "이미지 생성 백엔드(FLUX)가 아직 연결되지 않았습니다".into(),
        ))
    }

    /// gemma 4 — 참조 이미지→설명 텍스트. **미구현(스텁)**.
    pub async fn understand(
        &self,
        _image: &[u8],
        _mime: &str,
        _instruction: &str,
        _model: &str,
    ) -> Result<String, ImageError> {
        // TODO(samples): 파이썬 '이미지 분석' 샘플대로 구현. POST {base}{VISION_PATH} …
        let _ = (&self.http, &self.base, &self.client_key, &self.token);
        Err(ImageError::NotImplemented(
            "이미지 이해 백엔드(gemma)가 아직 연결되지 않았습니다".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn image_error_status_mapping() {
        assert_eq!(ImageError::BadRequest("x".into()).status(), 400);
        assert_eq!(ImageError::NotImplemented("x".into()).status(), 501);
        assert_eq!(ImageError::Backend(FabrixError::Quota("q".into())).status(), 429);
        // Unreachable 은 chat 의 502 가 아니라 503 으로 오버라이드(재시도 유도).
        assert_eq!(ImageError::Backend(FabrixError::Unreachable("u".into())).status(), 503);
        assert_eq!(
            ImageError::Backend(FabrixError::Upstream { status: 418, message: "t".into() }).status(),
            418
        );
        assert_eq!(ImageError::Backend(FabrixError::NotConfigured).status(), 503);
    }

    #[test]
    fn image_error_kind() {
        assert_eq!(ImageError::BadRequest("x".into()).code(), "invalid_value");
        assert_eq!(ImageError::NotImplemented("x".into()).code(), "not_implemented");
        assert_eq!(
            ImageError::Backend(FabrixError::Quota("q".into())).code(),
            "rate_limit_exceeded"
        );
    }

    #[test]
    fn resolve_models_require_config() {
        let mut cfg = Config::default();
        assert!(resolve_image_model(&cfg).is_err());
        assert!(resolve_vision_model(&cfg).is_err());
        cfg.image_model = "flux-2".into();
        cfg.vision_model = "gemma-4".into();
        assert_eq!(resolve_image_model(&cfg).unwrap(), "flux-2");
        assert_eq!(resolve_vision_model(&cfg).unwrap(), "gemma-4");
    }

    #[test]
    fn snap_size_defaults() {
        assert_eq!(snap_size(None), DEFAULT_SIZE);
        assert_eq!(snap_size(Some((1792, 1024))), (1792, 1024));
    }

    #[test]
    fn compose_edit_prompt_includes_both() {
        let p = compose_edit_prompt("빨간 자전거", "밤 풍경으로 바꿔줘");
        assert!(p.contains("빨간 자전거"));
        assert!(p.contains("밤 풍경으로 바꿔줘"));
        // 설명이 비면 지시만 남깁니다.
        assert_eq!(compose_edit_prompt("", "밤 풍경으로"), "밤 풍경으로");
    }

    #[test]
    fn sniff_mime_magic() {
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0x00]), "image/jpeg");
        assert_eq!(
            sniff_mime(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
            "image/png"
        );
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_mime(&webp), "image/webp");
        assert_eq!(sniff_mime(&[0, 1, 2, 3]), "image/png"); // 폴백
    }

    #[test]
    fn placeholder_is_valid_png() {
        let png = placeholder_png();
        assert!(!png.is_empty());
        assert_eq!(sniff_mime(&png), "image/png");
    }
}
