//! `~/.fabrix-proxy/` 에 놓이는 설정과 통계.
//!
//! 사용자 선택에 따라 **평문 JSON**으로 저장합니다. 사용자 프로필 폴더를 읽을 수
//! 있는 계정/프로세스는 사내 인증키를 그대로 볼 수 있으므로, 온보딩 화면에서
//! 저장 위치를 명시해 사용자가 이 사실을 알 수 있게 합니다.
//!
//! 호출 **본문은 절대 여기에 쓰지 않습니다** — 목업 원칙 "본문은 메모리에만 보관".

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 8787;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// 예: `https://ai.corp.internal` — 경로 없이 오리진만.
    pub fabrix_base_url: String,
    /// `x-fabrix-client` 헤더 값 (사용자 인증키).
    pub fabrix_client: String,
    /// `x-openapi-token` 헤더 값.
    pub openapi_token: String,
    pub port: u16,
    /// 앱 시작 시 프록시를 자동으로 켤지.
    pub auto_start: bool,
    /// 클라이언트가 모르는 모델명을 보냈을 때 폴백할 alias. 비면 목록 첫 모델.
    pub default_model_alias: String,
    /// 사내 루트 CA가 Windows 인증서 저장소에 없을 때의 탈출구.
    pub insecure_skip_verify: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fabrix_base_url: String::new(),
            fabrix_client: String::new(),
            openapi_token: String::new(),
            port: DEFAULT_PORT,
            auto_start: true,
            default_model_alias: String::new(),
            insecure_skip_verify: false,
        }
    }
}

impl Config {
    pub fn is_configured(&self) -> bool {
        !self.fabrix_base_url.trim().is_empty()
            && !self.fabrix_client.trim().is_empty()
            && !self.openapi_token.trim().is_empty()
    }

    /// 뒤 3글자만 남기고 마스킹 — 로그 ② 칸에 그대로 노출됩니다.
    pub fn mask(secret: &str) -> String {
        let s = secret.trim();
        if s.is_empty() {
            return "(미설정)".into();
        }
        let tail: String = s.chars().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
        format!("●●●●{tail}")
    }

    /// 끝의 `/` 와 실수로 붙인 `/openapi...` 경로를 정리합니다.
    pub fn normalized_base_url(&self) -> String {
        let mut base = self.fabrix_base_url.trim().to_string();
        while base.ends_with('/') {
            base.pop();
        }
        if let Some(idx) = base.find("/openapi") {
            base.truncate(idx);
        }
        base
    }
}

pub fn config_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fabrix-proxy")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn stats_path() -> PathBuf {
    config_dir().join("stats.json")
}

/// 파일이 없거나 깨졌으면 `None` — 호출부에서 첫 실행(온보딩)으로 해석합니다.
pub fn load_config() -> Option<Config> {
    let raw = fs::read_to_string(config_path()).ok()?;
    // 메모장이나 PowerShell 로 손편집하면 UTF-8 BOM 이 붙습니다. serde_json 은
    // BOM 을 값으로 보고 실패하므로 먼저 떼어냅니다.
    let raw = raw.trim_start_matches('\u{feff}');
    match serde_json::from_str::<Config>(raw) {
        Ok(cfg) => Some(cfg),
        Err(err) => {
            eprintln!("[config] config.json 파싱 실패, 기본값으로 시작합니다: {err}");
            None
        }
    }
}

pub fn save_config(cfg: &Config) -> io::Result<()> {
    write_atomic(&config_path(), &serde_json::to_vec_pretty(cfg)?)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Stats {
    /// 로컬 기준 `YYYY-MM-DD`. 날짜가 바뀌면 카운터를 리셋합니다.
    pub date: String,
    pub total: u64,
    pub chat: u64,
    pub models: u64,
    /// `HH:MM` — 꺼짐 화면의 "마지막 호출 14:31" 표시용.
    pub last_call_at: Option<String>,
}

impl Stats {
    pub fn roll_over(&mut self, today: &str) {
        if self.date != today {
            self.date = today.to_string();
            self.total = 0;
            self.chat = 0;
            self.models = 0;
            self.last_call_at = None;
        }
    }
}

pub fn load_stats() -> Stats {
    fs::read_to_string(stats_path())
        .ok()
        .and_then(|raw| serde_json::from_str::<Stats>(raw.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

pub fn save_stats(stats: &Stats) -> io::Result<()> {
    write_atomic(&stats_path(), &serde_json::to_vec_pretty(stats)?)
}

/// tmp 로 쓰고 rename — 쓰기 중 전원이 나가도 반쪽 파일이 남지 않습니다.
fn write_atomic(path: &PathBuf, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    // Windows 의 rename 은 대상이 있으면 실패하므로 먼저 지웁니다.
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)
}
