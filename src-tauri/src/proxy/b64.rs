//! base64 인코드/디코드를 한 곳에 격리합니다.
//!
//! 크레이트를 바꾸거나 손수 구현으로 교체하더라도 이 파일만 고치면 됩니다.
//! 이미지 이진 왕복(data URL 디코드 · `b64_json` 인코드)에 쓰이므로 정확성이 중요합니다.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// 표준 알파벳 + 패딩으로 인코딩합니다 (OpenAI `b64_json` 형식).
pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// data URL payload 를 디코딩합니다.
///
/// data URL 에는 개행/공백이 섞여 오는 경우가 있어 먼저 제거한 뒤 디코딩합니다.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD
        .decode(cleaned.as_bytes())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4648 §10 테스트 벡터.
    #[test]
    fn rfc4648_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn round_trip_binary() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let s = encode(&bytes);
        assert_eq!(decode(&s).unwrap(), bytes);
    }

    #[test]
    fn decode_tolerates_embedded_whitespace() {
        // 줄바꿈/공백이 섞여 있어도 디코딩됩니다.
        assert_eq!(decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(decode("  Zm9v YmE=  ").unwrap(), b"fooba");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode("!!!not base64!!!").is_err());
    }
}
