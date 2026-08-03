//! 앱 전역 상태. Tauri 커맨드와 axum 핸들러가 같은 `Arc` 를 공유합니다.
//!
//! 주의: 여기 뮤텍스는 전부 `std::sync::Mutex` 입니다. 가드를 `await` 너머로
//! 들고 가면 핸들러의 `Send` 바운드가 깨지므로, 잠금은 항상 짧은 블록 안에서
//! 끝내고 값을 복사해 나옵니다.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use crate::config::{self, Config, Stats};
use crate::logstore::{LogEntry, LogStore};
use crate::port::{self, PortStatus};
use crate::proxy::fabrix::{build_http_client, FabrixClient, ResolvedModel};

pub const MODELS_CACHE_TTL: Duration = Duration::from_secs(60);
/// 요청마다 stats.json 을 쓰지 않도록 하는 최소 간격.
const STATS_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

pub struct ServerHandle {
    pub port: u16,
    pub shutdown: oneshot::Sender<()>,
}

pub struct ModelsCache {
    pub fetched_at: Instant,
    pub models: Vec<ResolvedModel>,
}

pub struct AppState {
    pub app: AppHandle,
    pub config: Mutex<Config>,
    pub stats: Mutex<Stats>,
    pub logs: Mutex<LogStore>,
    pub server: Mutex<Option<ServerHandle>>,
    pub http: Mutex<reqwest::Client>,
    pub models_cache: Mutex<Option<ModelsCache>>,
    /// config.json 이 아예 없던 첫 실행인지 — 온보딩을 띄울지 판단합니다.
    pub first_run: AtomicBool,
    /// 토스트를 연달아 띄웠을 때 앞선 타이머가 새 토스트를 지우지 않도록 하는 세대 번호.
    pub toast_gen: AtomicU64,
    stats_flushed_at: Mutex<Instant>,
}

pub type Shared = Arc<AppState>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub configured: bool,
    pub first_run: bool,
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub auto_start: bool,
    pub fabrix_base_url: String,
    pub default_model_alias: String,
    pub insecure_skip_verify: bool,
    /// 토큰 사용 모드 여부. UI 가 토큰 카드를 보여줄지 판단합니다.
    pub token_mode: bool,
    /// 발행된 토큰(`sk-…`). 토큰 모드일 때만 채웁니다 — UI 복사 버튼용.
    pub issued_token: String,
    pub stats: Stats,
    pub recent: Vec<LogEntry>,
    pub port_status: PortStatus,
    /// 캐시가 따뜻할 때만 채워집니다 — "사내 모델 7개 노출" 문구용.
    pub model_count: Option<usize>,
}

impl AppState {
    pub fn new(app: AppHandle, cfg: Config, first_run: bool) -> Shared {
        let mut stats = config::load_stats();
        stats.roll_over(&today());
        Arc::new(Self {
            app,
            http: Mutex::new(build_http_client(cfg.insecure_skip_verify)),
            config: Mutex::new(cfg),
            stats: Mutex::new(stats),
            logs: Mutex::new(LogStore::default()),
            server: Mutex::new(None),
            models_cache: Mutex::new(None),
            first_run: AtomicBool::new(first_run),
            toast_gen: AtomicU64::new(0),
            stats_flushed_at: Mutex::new(Instant::now() - STATS_FLUSH_INTERVAL),
        })
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    /// 설정이 바뀌면 TLS 옵션이 달라질 수 있으므로 HTTP 클라이언트를 다시 만들고,
    /// 모델 캐시도 비웁니다 (다른 서버를 가리킬 수 있으므로).
    pub fn replace_config(&self, cfg: Config) {
        *self.http.lock().unwrap() = build_http_client(cfg.insecure_skip_verify);
        *self.models_cache.lock().unwrap() = None;
        *self.config.lock().unwrap() = cfg;
        self.first_run.store(false, Ordering::Relaxed);
    }

    /// 서비스 중이면 그 포트를 돌려줍니다.
    pub fn running_port(&self) -> Option<u16> {
        self.server.lock().unwrap().as_ref().map(|h| h.port)
    }

    pub fn is_running(&self) -> bool {
        self.running_port().is_some()
    }

    pub fn base_url(&self) -> String {
        let port = self.running_port().unwrap_or_else(|| self.config.lock().unwrap().port);
        format!("http://127.0.0.1:{port}/v1")
    }

    pub fn fabrix_client(&self) -> Option<FabrixClient> {
        let cfg = self.config();
        if !cfg.is_configured() {
            return None;
        }
        Some(FabrixClient {
            http: self.http.lock().unwrap().clone(),
            base: cfg.normalized_base_url(),
            client_key: cfg.fabrix_client.clone(),
            token: cfg.openapi_token.clone(),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let cfg = self.config();
        let running = self.running_port();
        let port = running.unwrap_or(cfg.port);
        let stats = self.stats.lock().unwrap().clone();
        let recent = self.logs.lock().unwrap().recent(4);
        let model_count = self
            .models_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|c| c.fetched_at.elapsed() < MODELS_CACHE_TTL)
            .map(|c| c.models.len());

        Snapshot {
            configured: cfg.is_configured(),
            first_run: self.first_run.load(Ordering::Relaxed),
            running: running.is_some(),
            port,
            base_url: format!("http://127.0.0.1:{port}/v1"),
            auto_start: cfg.auto_start,
            fabrix_base_url: cfg.normalized_base_url(),
            default_model_alias: cfg.default_model_alias.clone(),
            insecure_skip_verify: cfg.insecure_skip_verify,
            token_mode: cfg.token_mode,
            // 토큰 모드일 때만 노출합니다 — 꺼져 있으면 UI 에 굳이 실을 필요가 없습니다.
            issued_token: if cfg.token_mode { cfg.issued_token.clone() } else { String::new() },
            stats,
            recent,
            port_status: port::inspect(port, running),
            model_count,
        }
    }

    pub fn emit_state(&self) {
        let _ = self.app.emit("state:changed", self.snapshot());
    }

    /// 호출 한 건을 기록하고 두 창에 알립니다.
    pub fn record(&self, entry: LogEntry) {
        {
            let mut stats = self.stats.lock().unwrap();
            stats.roll_over(&today());
            stats.total += 1;
            match entry.kind {
                crate::logstore::Kind::Chat => stats.chat += 1,
                crate::logstore::Kind::Models => stats.models += 1,
            }
            stats.last_call_at = Some(entry.ts.clone());
        }
        self.logs.lock().unwrap().push(entry.clone());

        let _ = self.app.emit("log:new", entry);
        self.emit_state();
        self.flush_stats(false);
    }

    /// `force` 가 아니면 2초에 한 번만 디스크로 내립니다.
    pub fn flush_stats(&self, force: bool) {
        let due = {
            let mut last = self.stats_flushed_at.lock().unwrap();
            if force || last.elapsed() >= STATS_FLUSH_INTERVAL {
                *last = Instant::now();
                true
            } else {
                false
            }
        };
        if !due {
            return;
        }
        let stats = self.stats.lock().unwrap().clone();
        if let Err(err) = config::save_stats(&stats) {
            eprintln!("[stats] 저장 실패: {err}");
        }
    }
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub fn now_hm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

pub fn now_iso() -> String {
    chrono::Local::now().to_rfc3339()
}

pub fn epoch_secs() -> i64 {
    chrono::Utc::now().timestamp()
}
