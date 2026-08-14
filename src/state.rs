use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AppError, AppResult};
use media_down_lib::fmp4;
use serde_json::json;
use tauri::AppHandle;
use tauri::Emitter;

pub type TrackId = u32;
pub type DirectId = u64;

/// 当前 Unix 时间戳（毫秒），用于连播场景按注册时间最近配对音视频轨�?
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 轨道（视�?/ 音频 / 字幕�?
#[derive(Debug, Clone, serde::Serialize)]
pub struct TrackInfo {
    pub id: TrackId,
    pub kind: String,        // video | audio | text
    pub mime: String,        // e.g. video/mp4; codecs="avc1.640028"
    pub ext: String,         // mp4 | m4a | webm | vtt | m3u8...
    pub started: bool,
    pub ended: bool,
    pub bytes: u64,
    pub segments: u64,
    pub total_segments: u64, // 预期总切片数（-1=未知）
    pub downloading: bool,
    pub finalizing: bool,
    pub finalized: bool,
    pub out_path: Option<String>,
    pub muxed: bool,         // 是否已参�?mkvmerge 混流（避免重复触发）
    pub mime_family: String, // mp4 | webm | other（决定收尾方式）
    pub title: String,       // 来源页面标题，作为默认文件名
    pub project: String,     // 项目分组键（默认=标题；空标题退化为 id）；用于“按项目更名”批量改 video+audio
    pub session: String,     // 播放会话键（同一 MediaSource 的音/视频共享）：混流配对的可靠主键
    pub registered_at: u64,  // 注册时刻(ms)，连播场景用于按时间最近配对音视频�?
}

/// 正在落盘的轨道句�?
pub struct TrackWriter {
    pub file: std::fs::File,
    pub bytes: u64,
    pub segments: u64,
}

/// 直链下载任务
#[derive(Debug, Clone, serde::Serialize)]
pub struct DirectInfo {
    pub id: DirectId,
    pub url: String,
    pub name: String,
    pub total: Option<u64>,
    pub done: u64,
    pub downloading: bool,
    pub finished: bool,
    pub error: Option<String>,
    pub out_path: Option<String>,
    pub aborted: bool,
}

/// 未开始下载时，原先的分片内存缓冲已移除：改为“边下边存”，首个分片到达即落盘。
/// 分片不再在系统内存中堆积（避免大视频占用 RAM，且不会在 32MB 上限处丢片）。
pub struct AppState {
    pub server_token: String,
    pub server_port: AtomicU32,
    pub rate: AtomicU32, // 倍�?100
    pub enabled: AtomicBool,
    pub auto: AtomicBool,
    pub copy_unlock: AtomicBool, // 解除复制限制（user-select / 右键 / 复制拦截�?
    pub mux: AtomicBool,         // 下载后自动混流（优先 tools/ffmpeg.exe，缺失时回退 tools/mkvmerge.exe；video+audio -> mkv）
    pub high_precision: AtomicBool, // 高精度同步模式（缓冲重构，默认开启）
    pub idle_timeout_secs: AtomicU32, // 轨道空闲超时秒数（默认60s），超过后自动收尾
    pub mux_hint: Mutex<String>, // 混流工具缺失的持久提示（UI 轮询展示，非一次性 toast）
    pub app_handle: Mutex<Option<AppHandle>>, // 混流完成后向 UI 推�?md-mux 事件
    pub track_seq: AtomicU32,
    pub tracks: Mutex<Vec<TrackInfo>>,
    pub writers: Mutex<HashMap<TrackId, Arc<Mutex<TrackWriter>>>>, // 每轨独立写锁：跨轨写盘互不阻塞
    pub track_buffers: Mutex<HashMap<TrackId, Arc<Mutex<fmp4::TrackBuffer>>>>, // 高精度模式：每轨内存缓冲重构器
    pub directs: Mutex<Vec<DirectInfo>>,
    pub name_overrides: Mutex<HashMap<String, String>>,     // 逐轨自定义名（trackId -> name�?
    pub project_overrides: Mutex<HashMap<String, String>>,  // 项目级自定义名（project -> name），批量�?video+audio
    pub reports: Mutex<Vec<serde_json::Value>>, // hook 上报的直链媒�?
    pub hook_diag: Mutex<serde_json::Value>,     // hook 自上报的诊断信息
    pub save_dir: Mutex<String>,                 // 下载保存目录（可被命令行/设置覆盖�?
}

/// 会话令牌：用 OS 熵种子（RandomState 各自独立初始化）派生不可预测值。
/// 仅绑定 127.0.0.1，但重启间也不该被猜中。
fn new_token() -> String {
    let s1 = RandomState::new();
    let s2 = RandomState::new();
    let mut a = s1.build_hasher();
    a.write_u64(std::process::id() as u64);
    a.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let mut b = s2.build_hasher();
    b.write_u64(a.finish());
    b.write_u128(now_ms() as u128);
    format!("mdtk{:016x}{:016x}", a.finish(), b.finish())
}

impl AppState {
    pub fn new() -> Self {
        AppState {
            server_token: new_token(),
            server_port: AtomicU32::new(0),
            rate: AtomicU32::new(100),
            enabled: AtomicBool::new(true),
            auto: AtomicBool::new(true),
            copy_unlock: AtomicBool::new(true),
            mux: AtomicBool::new(true),
            high_precision: AtomicBool::new(true), // 默认开启高精度同步
            idle_timeout_secs: AtomicU32::new(60), // 默认60秒超时
            mux_hint: Mutex::new(String::new()),
            app_handle: Mutex::new(None),
            track_seq: AtomicU32::new(1),
            tracks: Mutex::new(Vec::new()),
            writers: Mutex::new(HashMap::new()),
            track_buffers: Mutex::new(HashMap::new()),
            directs: Mutex::new(Vec::new()),
            name_overrides: Mutex::new(HashMap::new()),
            project_overrides: Mutex::new(HashMap::new()),
            reports: Mutex::new(Vec::new()),
            hook_diag: Mutex::new(serde_json::json!({})),
            save_dir: Mutex::new(
                Self::load_configured_dir().unwrap_or_else(Self::default_save_dir),
            ),
        }
    }

    pub fn server_token(&self) -> String {
        self.server_token.clone()
    }
    pub fn set_port(&self, p: u32) {
        self.server_port.store(p, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn port(&self) -> u32 {
        self.server_port.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn rate(&self) -> f64 {
        self.rate.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0
    }
    pub fn set_rate(&self, r: f64) -> AppResult<()> {
        // NaN/Inf 的 clamp 结果是 NaN，as u32 会变成 0 → 视频停滞；显式拒绝非法值
        if !r.is_finite() {
            return Err(AppError::InvalidArg(format!("倍速必须是有穷数值，收到 {r}")));
        }
        let v = (r.clamp(0.1, 16.0) * 100.0) as u32;
        self.rate.store(v, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn set_enabled(&self, e: bool) -> AppResult<()> {
        self.enabled.store(e, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    pub fn set_auto(&self, a: bool) -> AppResult<()> {
        self.auto.store(a, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    pub fn copy_unlock(&self) -> bool {
        self.copy_unlock.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set_copy_unlock(&self, v: bool) -> AppResult<()> {
        self.copy_unlock.store(v, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// 下载后自�?mkvmerge 混流开�?
    pub fn set_mux(&self, m: bool) -> AppResult<()> {
        self.mux.store(m, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// 记录 AppHandle，供混流线程�?UI 推�?md-mux 事件
    pub fn set_app(&self, h: AppHandle) {
        *self.app_handle.lock().unwrap() = Some(h);
    }

    /// �?UI 推送混流状态事件（�?AppHandle 时静默）
    fn emit_mux(&self, status: &str, msg: &str) {
        if let Some(a) = self.app_handle.lock().unwrap().clone() {
            let _ = a.emit("md-mux", json!({ "status": status, "msg": msg }));
        }
    }

    /// 混流工具缺失时写入持久提示（snapshot 透传给 UI 常驻显示）
    fn set_mux_hint(&self, hint: &str) {
        *self.mux_hint.lock().unwrap() = hint.to_string();
    }

    /// 某条轨道收尾完成后调用：若同标题存在另一条“相反类型、已收尾、未混流”的轨道�?
    /// 则自动调�?<exe_dir>/tools/ffmpeg.exe（优先）�?tools/mkvmerge.exe �?video+audio 混流为单�?mkv 文件�?
    pub fn notify_finalized(&self, id: TrackId) {
        if !self.mux.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // 优先�?ffmpeg（更稳地解析 fragmented MP4 并对齐两轨零点），缺失时回退 mkvmerge
        let (tool, is_ff) = match Self::pick_mux_tool() {
            Some(x) => x,
            None => {
                self.set_mux_hint("未找到混流工具：请把 ffmpeg.exe 或 mkvmerge.exe 放到程序目录 tools 下");
                self.emit_mux("skip", "未找到 tools/ffmpeg.exe 或 tools/mkvmerge.exe，已跳过自动混流");
                return;
            }
        };
        // 配对与 muxed 标记在同一临界区完成：音/视频两条收尾线程几乎同时 end、
        // 并发 notify 时，任何一刻只有一方能占据配对资格，杜绝双进程写同一 .mkv。
        let pair = {
            let mut ts = self.tracks.lock().unwrap();
            let me = match ts.iter().find(|t| t.id == id) {
                Some(t) => t.clone(),
                None => return,
            };
            if me.muxed || !me.finalized {
                return;
            }
            if me.kind != "video" && me.kind != "audio" {
                return;
            }
            // 配对主键：同一播放会话（session，hook 按 MediaSource 分组，SPA 动态标题
            // 下仍稳定）；回退：同标题（project）。标题为空时 session 仍可配对——
            // 旧逻辑「标题为空则放弃」会静默杀死整个混流，需删除。
            let title_hit = !me.project.is_empty() && me.project != me.id.to_string();
            let partner = ts
                .iter()
                .filter(|t| {
                    t.id != id
                        && t.kind != me.kind
                        && (t.kind == "video" || t.kind == "audio")
                        && t.finalized
                        && !t.muxed
                        && (if !me.session.is_empty() && t.session == me.session {
                            true
                        } else {
                            title_hit && t.project == me.project
                        })
                })
                .min_by_key(|t| t.registered_at.abs_diff(me.registered_at))
                .cloned();
            let Some(partner) = partner else { return };
            // 立即标记两者已混流（不可分割：标记就在拿到配对后同一锁内完成）
            for t in ts.iter_mut() {
                if t.id == me.id || t.id == partner.id {
                    t.muxed = true;
                }
            }
            (me, partner)
        };
        let (me, partner) = pair;
        let vpath = match me.out_path.clone() {
            Some(p) => p,
            None => return,
        };
        let apath = match partner.out_path.clone() {
            Some(p) => p,
            None => return,
        };
        let out_dir = self.save_dir();
        let title = me.title.clone();
        let app = self.app_handle.lock().unwrap().clone();
        self.emit_mux("start", &format!("开始混流：{}", title));
        std::thread::spawn(move || {
            let san = Self::sanitize_name(&title);
            let out = std::path::Path::new(&out_dir).join(format!("{}.mkv", san));
            let res = if is_ff {
                Self::run_mux_ffmpeg(tool.as_path(), &vpath, &apath, &out)
            } else {
                Self::run_mux(tool.as_path(), &vpath, &apath, &out)
            };
            match res {
                Ok(()) => {
                    if let Some(a) = app {
                        let _ = a.emit(
                            "md-mux",
                            json!({ "status": "done", "msg": format!("混流完成：{}", out.display()) }),
                        );
                    }
                }
                Err(e) => {
                    if let Some(a) = app {
                        let _ = a.emit("md-mux", json!({ "status": "error", "msg": e }));
                    }
                }
            }
        });
    }

    /// 可执行文件所在目录（用于解析相对 ./downloads 与配置文件路径）
    fn exe_dir() -> std::path::PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }

    /// 配置文件路径�?exe_dir>/MediaDown.json（与绿色 exe 同目录，便于携带�?
    fn config_path() -> std::path::PathBuf {
        Self::exe_dir().join("MediaDown.json")
    }

    /// 读取配置文件中保存的目录；解析失败或不存在返�?None
    fn load_configured_dir() -> Option<String> {
        let txt = std::fs::read_to_string(Self::config_path()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
        let d = v.get("save_dir")?.as_str()?;
        if d.trim().is_empty() {
            None
        } else {
            Some(d.to_string())
        }
    }

    /// 将当前保存目录写入配置文件（失败静默忽略，内存值仍生效�?
    fn persist_config(dir: &str) {
        if let Err(e) = std::fs::write(
            Self::config_path(),
            serde_json::to_string_pretty(&serde_json::json!({ "save_dir": dir }))
                .unwrap_or_else(|_| "{\"save_dir\":\"\"}".into()),
        ) {
            eprintln!("[config] 保存目录写入失败: {e}");
        }
    }

    /// 默认保存目录：软件目录下�?./downloads（绿色便携，不污染用户目录）
    fn default_save_dir() -> String {
        Self::exe_dir()
            .join("downloads")
            .to_string_lossy()
            .into_owned()
    }

    /// 当前保存目录（命令行 --save-dir 或默认）
    pub fn save_dir(&self) -> String {
        self.save_dir.lock().unwrap().clone()
    }

/// 覆盖保存目录（命令行参数 / 未来设置项）
    pub fn set_save_dir(&self, dir: &str) -> AppResult<()> {
        let d = dir.trim();
        if d.is_empty() {
            return Err(AppError::InvalidArg("保存目录不能为空".into()));
        }
        // 展开 ~ / ~/ �?home
        let expanded = if d == "~" {
            Self::default_save_dir()
        } else if let Some(rest) = d.strip_prefix("~/") {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".into());
            format!("{}\\{}", home, rest)
        } else {
            d.to_string()
        };
        std::fs::create_dir_all(&expanded)
            .map_err(|e| AppError::DirCreateFailed(format!("无法创建目录 {}: {}", expanded, e)))?;
        *self.save_dir.lock().unwrap() = expanded.clone();
        Self::persist_config(&expanded);
        Ok(())
    }

    /// 新轨道注册（hook 上报），auto=true 时立即开始落�?
    #[allow(clippy::too_many_arguments)] // 注册参数来自 hook 上报 JSON 的固定字段，直传比结构体更直白
    pub fn register_track(
        &self,
        kind: &str,
        mime: &str,
        ext: &str,
        mime_family: &str,
        title: &str,
        session: &str,
        auto: bool,
    ) -> TrackId {
        let id = self.track_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut ts = self.tracks.lock().unwrap();
            // 仅复用真正空�?bytes==0 && segments==0)，否则必建新轨�?
            // 防止连播/下一集站点不�?endOfStream 时，第二段音频静默复用第一�?
            // 音频轨的 trackId，导致两段内容拼进同一 .m4a —�?UI 看似"音频停滞"、mkv 配对跨段错配�?
            if let Some(t) = ts.iter_mut().find(|t| {
                t.mime == mime
                    && t.kind == kind
                    && !t.ended
                    && t.session == session
                    && t.bytes == 0
                    && t.segments == 0
            }) {
                return t.id; // 空轨复用（仅限同 entry 残留的空轨）
            }
            let registered_at = now_ms();
            // 项目分组键：默认用页面标题（同一标签页的视频+音频同属一个项目，
            // 更名时一并改）；标题为空时退化为 id（逐轨独立，无法配对则不强行合并）�?
            let project = if title.trim().is_empty() {
                id.to_string()
            } else {
                title.trim().to_string()
            };
            ts.push(TrackInfo {
                id,
                kind: kind.into(),
                mime: mime.into(),
                ext: ext.into(),
                started: true,
                ended: false,
                bytes: 0,
                segments: 0,
                total_segments: 0,
                downloading: false,
                finalizing: false,
                finalized: false,
                out_path: None,
                muxed: false,
                mime_family: mime_family.into(),
                title: title.into(),
                project,
                session: session.into(),
                registered_at,
            });
        }
        if auto {
            let _ = self.download_start(&id.to_string());
        }
        id
    }

    /// 分片到达：边下边存（首片到达即落盘，不在系统内存堆积）�?
    /// �?writer 已存在直接追加；否则�?auto=true 时惰性创建临时文件并直写磁盘�?
    /// auto=false 且未手动开始时则不落盘（保持“未开始下载”语义，且不产生内存缓冲）�?
    pub fn append_chunk(&self, track_id: TrackId, data: &[u8]) -> AppResult<()> {
        // 校验轨道已注册：防止拿到 token 的页面为任意 id 写盘（占满保存目录）。
        // 分片总是先 /register 成功后才发送，因此正常路径不受影响。
        let is_registered = {
            let ts = self.tracks.lock().unwrap();
            ts.iter().any(|t| t.id == track_id)
        };
        if !is_registered {
            return Err(AppError::NotFound(format!(
                "track {} 未注册",
                track_id
            )));
        }

        // 高精度同步模式（缓冲重构）：mp4 族分片先入内存缓冲，收尾时统一重建
        // 标准 MP4，修正 tfdt 断点/乱序/质量切换导致的音视频漂移。
        // 其它格式或关闭高精度时仍走边下边存（零内存压力）。
        let use_buffer = {
            let ts = self.tracks.lock().unwrap();
            let family = ts
                .iter()
                .find(|t| t.id == track_id)
                .map(|t| t.mime_family.clone())
                .unwrap_or_default();
            self.high_precision() && family == "mp4"
        };
        if use_buffer {
            let buf = self.track_buffer(track_id);
            buf.lock().unwrap().append(data);
            let mut ts = self.tracks.lock().unwrap();
            if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
                t.bytes += data.len() as u64;
                t.segments += 1;
            }
            return Ok(());
        }

        let has_writer = self.writer(track_id).is_some();
        if has_writer {
            if let Some(w) = self.writer(track_id) {
                let mut w = w.lock().unwrap();
                use std::io::Write;
                w.file.write_all(data)?;
                w.bytes += data.len() as u64;
                w.segments += 1;
            }
        } else if self.auto() {
            // 边下边存：第一个分片抵达即开临时文件落盘，零内存缓冲
            self.ensure_writer(track_id)?;
            if let Some(w) = self.writer(track_id) {
                let mut w = w.lock().unwrap();
                use std::io::Write;
                w.file.write_all(data)?;
                w.bytes += data.len() as u64;
                w.segments += 1;
            }
        }
        // 计数（writer 缺失�?auto 关闭时仍累计显示，但实际未落盘）
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
            t.bytes += data.len() as u64;
            t.segments += 1;
        }
        Ok(())
    }

    /// 惰性创建某轨道的临时落盘文件（幂等：已存在则跳过）�?
    /// 路径�?download_start 一致：<save_dir>/<stamp>-<id>.tmp-part
    fn ensure_writer(&self, id: TrackId) -> AppResult<()> {
        if self.writer(id).is_some() {
            return Ok(());
        }
        let dir = self.save_dir();
        let _ = std::fs::create_dir_all(&dir);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let tmp = format!("{}\\{}-{}.tmp-part", dir, stamp, id);
        let file = std::fs::File::create(&tmp)?;
        let writer = TrackWriter {
            file,
            bytes: 0,
            segments: 0,
        };
        self.writers.lock().unwrap().insert(id, Arc::new(Mutex::new(writer)));
        Ok(())
    }

    /// 取某轨道的写者引用（克隆 Arc，不长期持有全局 map 锁）�?
    fn writer(&self, id: TrackId) -> Option<Arc<Mutex<TrackWriter>>> {
        self.writers.lock().unwrap().get(&id).cloned()
    }

    /// 轨道结束（endOfStream�?
    pub fn track_ended(&self, track_id: TrackId) -> AppResult<()> {
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
            t.ended = true;
        }
        Ok(())
    }

    /// 开始下载：标记轨道为下载中，并惰性创建临时落盘文件（若尚未因首片到达而创建）�?
    /// 不再做任何内存缓冲补写——边下边存已保证分片始终直写磁盘�?
    pub fn download_start(&self, track_id: &str) -> AppResult<()> {
        let id: TrackId = track_id.parse().map_err(|_| AppError::InvalidArg("bad track id".into()))?;
        {
            let mut ts = self.tracks.lock().unwrap();
            let t = ts
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| AppError::NotFound("轨道不存在".into()))?;
            if t.downloading {
                return Ok(());
            }
            t.downloading = true;
        }
        self.ensure_writer(id)?;
        Ok(())
    }

    /// 停止下载：关闭文件，等待收尾
    pub fn download_stop(&self, track_id: &str) -> AppResult<()> {
        let id: TrackId = track_id.parse().map_err(|_| AppError::InvalidArg("bad track id".into()))?;
        {
            self.writers.lock().unwrap().remove(&id);
        }
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == id) {
            t.downloading = false;
        }
        Ok(())
    }

    /// 收尾：把 .tmp-part 写出最终可播放文件�?
    /// fMP4 直接按到达顺序拼接为 fragmented MP4（参�?media-sniffer-extension�?
    /// 不做 moov 重建，避免重封装出错）；mp4/ts/webm 等均按原始字节落盘�?
    /// 若仍在下载则先停止写盘，做到「一键收尾」�?
    pub fn finalize(&self, track_id: &str) -> AppResult<()> {
        let id: TrackId = track_id.parse().map_err(|_| AppError::InvalidArg("bad track id".into()))?;
        // 若仍在下载，先关闭写盘句柄（避免半截文件），再收�?
        {
            self.writers.lock().unwrap().remove(&id);
        }
        let info = {
            let mut ts = self.tracks.lock().unwrap();
            let t = ts
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| AppError::NotFound("轨道不存在".into()))?;
            if t.finalized {
                return Ok(());
            }
            t.downloading = false;
            t.finalizing = true;
            t.clone()
        };
        let result = self.finalize_impl(&info);
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == id) {
            t.finalizing = false;
            if let Ok(p) = &result {
                t.finalized = true;
                t.out_path = Some(p.clone());
            }
        }
        drop(ts);
        if result.is_ok() {
            self.notify_finalized(id);
        }
        Ok(())
    }

    fn finalize_impl(&self, info: &TrackInfo) -> AppResult<String> {
        let dir = self.save_dir();
        let mut cands: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // 严格解析 {stamp}-{id}.tmp-part 的后段 id，避免 id=1 误收 id=12/13 的临时文件
                let id_matches = name
                    .strip_suffix(".tmp-part")
                    .and_then(|stem| stem.rsplit('-').next())
                    .and_then(|id| id.parse::<TrackId>().ok())
                    .map(|id| id == info.id)
                    .unwrap_or(false);
                if id_matches {
                    cands.push(p);
                }
            }
        }
        // 输出文件名：自定义名优先，否则用页面标题作为默认名（再否�?kind_id�?
        let custom = self
            .name_overrides
            .lock()
            .unwrap()
            .get(&info.id.to_string())
            .cloned()
            .unwrap_or_default();
        let out = self.compute_out_path(info, &custom);

        // 候选临时文件（.tmp-part）：非缓冲路径（流式/降级/非 mp4）需要它来重建/改名。
        // 高精度缓冲路径会直接写出 out，不依赖此文件。
        let mut src_opt = cands
            .iter()
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .cloned();

        if info.mime_family == "mp4" {
            // 高精度同步模式（缓冲重构）已启用且该轨有缓冲数据：优先用内存缓冲重构
            // （按 tfdt 排序/去重/断点平滑）输出标准 MP4，修正音视频漂移。
            // 无缓冲（如直链下载强制流式、或该轨未启用高精度）时回退到文件级重建。
            let buf = {
                let bs = self.track_buffers.lock().unwrap();
                bs.get(&info.id).cloned()
            };
            let mut used_buffer = false;
            if let Some(b) = buf {
                let tb = b.lock().unwrap();
                if tb.estimated_bytes() > 0 {
                    match tb.finalize(&out) {
                        Ok(_) => {
                            used_buffer = true;
                            eprintln!("[finalize] track {} 使用高精度缓冲重构输出", info.id);
                        }
                        Err(e) => {
                            eprintln!("[finalize] track {} 缓冲重构失败，回退文件级重建: {e}", info.id);
                        }
                    }
                }
            }
            if !used_buffer {
                // 缓冲路径未使用（流式分片或降级）：取最大的 .tmp-part 文件级重建
                let src = src_opt.take().ok_or_else(|| "没有可收尾的文件".to_string())?;
                // 优先重建索引表（fMP4 -> 标准 MP4，可拖拽 seek）
                let ok = fmp4::finalize(&src, std::path::Path::new(&out));
                if let Err(e) = ok {
                    // 非 fragmented / 未知结构时回退：直接拼装（不失真拷贝）
                    eprintln!("fmp4 rebuild failed, fallback to copy: {e}");
                    let mut sf = std::fs::File::open(&src)?;
                    let mut df = std::fs::File::create(&out)?;
                    std::io::copy(&mut sf, &mut df)?;
                }
                if let Err(err) = std::fs::remove_file(&src) {
                    eprintln!("[finalize] 临时文件清理失败 {}: {err}", src.display());
                }
            }
        } else {
            // webm / ts / flv / 其它：流式封装，直接拼接或改名即为可播放文件
            let src = src_opt.take().ok_or_else(|| "没有可收尾的文件".to_string())?;
            if std::fs::rename(&src, &out).is_err() {
                let mut sf = std::fs::File::open(&src)?;
                let mut df = std::fs::File::create(&out)?;
                std::io::copy(&mut sf, &mut df)?;
            }
            if let Err(err) = std::fs::remove_file(&src) {
                eprintln!("[finalize] 临时文件清理失败 {}: {err}", src.display());
            }
        }
        // 收尾完成后释放该轨的高精度缓冲（内存压力）
        self.track_buffers.lock().unwrap().remove(&info.id);
        Ok(out)
    }

    /// 计算某轨道的最终输出路径（含文件名 sanitize + 多轨同名词后缀 + 扩展名）�?
    /// 基础名优先级：项目级更名(project_overrides) > 逐轨更名(custom) > 默认标题�?
    /// 同一项目更名会让该项目的 video.mp4 �?audio.m4a 共用一个基础名，达到“按项目更名”�?
    fn compute_out_path(&self, info: &TrackInfo, custom: &str) -> String {
        let proj_name = self
            .project_overrides
            .lock()
            .unwrap()
            .get(&info.project)
            .cloned()
            .unwrap_or_default();
        let base = if !proj_name.trim().is_empty() {
            proj_name.trim().to_string()
        } else if !custom.trim().is_empty() {
            custom.trim().to_string()
        } else {
            Self::default_base_of(info)
        };
        // 基础名过长会撞上 Windows MAX_PATH(260)：按字符数截断 stem，
        // 留足 save_dir + 后缀 + 扩展名 的空间（截断发生在 sanitize 之后，保证是合法字边界）
        const MAX_STEM_CHARS: usize = 120;
        let sanitized: String = base
            .chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if (c as u32) < 32 => '_',
                c => c,
            })
            .take(MAX_STEM_CHARS)
            .collect();
        // 按文件类型分别决定扩展名（参�?media-sniffer-extension �?MIME->ext 映射）：
        //   mp4 族：视频 -> .mp4，音�?-> .m4a（修复音视频同名 .mp4 互相覆盖�?bug�?
        //   webm 族：统一 .webm
        //   其它：沿用探测到�?ext（ts / flv / mp3 ...），空则回退 .bin
        let out_ext = if info.mime_family == "mp4" {
            if info.kind == "audio" {
                "m4a"
            } else {
                "mp4"
            }
        } else if info.mime_family == "webm" {
            "webm"
        } else if !info.ext.is_empty() {
            &info.ext
        } else {
            "bin"
        };
        // 同类�?kind)同基础名存在多条轨道时追加 _video1/_audio1 后缀，避免覆�?
        // （沿�?media-sniffer-extension 的多轨命名规则）。基础名判定同时考虑项目级更名�?
        let mut ids: Vec<TrackId> = Vec::new();
        {
            let ts = self.tracks.lock().unwrap();
            let no = self.name_overrides.lock().unwrap();
            let po = self.project_overrides.lock().unwrap();
            for t in ts.iter() {
                if t.kind == info.kind {
                    let tb = po
                        .get(&t.project)
                        .cloned()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            no.get(&t.id.to_string()).cloned().filter(|s| !s.trim().is_empty())
                        })
                        .unwrap_or_else(|| Self::default_base_of(t));
                    if tb == base {
                        ids.push(t.id);
                    }
                }
            }
        }
        ids.sort();
        let pos = ids.iter().position(|x| *x == info.id).map(|i| i + 1).unwrap_or(1);
        let suffix = if ids.len() > 1 {
            format!("_{}{}", info.kind, pos)
        } else {
            String::new()
        };
        std::path::PathBuf::from(self.save_dir())
            .join(format!("{}{}.{}", sanitized, suffix, out_ext))
            .to_string_lossy()
            .to_string()
    }

    /// 自动开关（读）
    pub fn auto(&self) -> bool {
        self.auto.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 该轨道是否仍在下载
    pub fn is_downloading(&self, track_id: TrackId) -> bool {
        let ts = self.tracks.lock().unwrap();
        ts.iter().any(|t| t.id == track_id && t.downloading)
    }

    // ======================== 高精度同步（缓冲重构） ========================

    /// 高精度同步开关（缓冲重构）
    pub fn set_high_precision(&self, enabled: bool) -> AppResult<()> {
        self.high_precision.store(enabled, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn high_precision(&self) -> bool {
        self.high_precision.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 设置总切片数（hook 注册时上报，用于进度展示）
    pub fn set_total_segments(&self, track_id: TrackId, total: u64) -> AppResult<()> {
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
            t.total_segments = total;
        }
        Ok(())
    }

    /// 设置空闲超时秒数
    pub fn set_idle_timeout(&self, secs: u32) -> AppResult<()> {
        self.idle_timeout_secs.store(secs, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn idle_timeout_secs(&self) -> u32 {
        self.idle_timeout_secs.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 取某轨的高精度缓冲重构器（惰性创建）
    fn track_buffer(&self, id: TrackId) -> Arc<Mutex<fmp4::TrackBuffer>> {
        let mut bs = self.track_buffers.lock().unwrap();
        if let Some(b) = bs.get(&id) {
            Arc::clone(b)
        } else {
            let b = Arc::new(Mutex::new(fmp4::TrackBuffer::default()));
            bs.insert(id, Arc::clone(&b));
            b
        }
    }

    /// 手动结束所有同 session 轨道（视频+音频一起收尾）
    pub fn manual_end_all_session(&self, track_id: TrackId) -> AppResult<()> {
        // 1. 找出该轨道所属 session
        let session: String;
        {
            let ts = self.tracks.lock().unwrap();
            session = match ts.iter().find(|t| t.id == track_id) {
                Some(t) => t.session.clone(),
                None => return Err(AppError::NotFound(format!("track {} not found", track_id))),
            };
        }
        eprintln!(
            "[state] manual_end_all_session for track {} (session={})",
            track_id, session
        );

        // 2. 遍历同 session 所有未结束的轨道，标记 ended 并触发 auto_finalize
        let mut to_finalize: Vec<TrackId> = Vec::new();
        {
            let mut ts = self.tracks.lock().unwrap();
            for t in ts.iter_mut() {
                if t.session == session && !t.ended && !t.finalized && !t.finalizing {
                    t.ended = true;
                    eprintln!(
                        "[state] marked track {} (session={}) as ended",
                        t.id, session
                    );
                    to_finalize.push(t.id);
                }
            }
        }
        // 3. 逐个触发 auto_finalize
        for id in to_finalize {
            self.auto_finalize(id);
        }
        Ok(())
    }

    /// 轨道结束后（endOfStream）自动收尾：先停写，再写出最终可播放文件
    /// �?httpd 在独立线程延迟调用，做到「捕获完即自动产出可播放文件」�?
    pub fn auto_finalize(&self, track_id: TrackId) {
        {
            self.writers.lock().unwrap().remove(&track_id);
        }
        let info = {
            let mut ts = self.tracks.lock().unwrap();
            if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
                t.downloading = false;
            }
            ts.iter().find(|t| t.id == track_id).cloned()
        };
        if let Some(info) = info {
            let result = self.finalize_impl(&info);
            let mut ts = self.tracks.lock().unwrap();
            if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
                t.finalizing = false;
                if let Ok(p) = &result {
                    t.finalized = true;
                    t.out_path = Some(p.clone());
                }
            }
            drop(ts);
            if result.is_ok() {
                self.notify_finalized(track_id);
            }
        }
    }

    /// 轮询快照（UI 每次 setInterval 调用�?
    pub fn snapshot(&self) -> serde_json::Value {
        let ts = self.tracks.lock().unwrap();
        let ds = self.directs.lock().unwrap();
        let ws = self.writers.lock().unwrap();
        let rs = self.reports.lock().unwrap();
        let tracks: Vec<serde_json::Value> = ts
            .iter()
            .map(|t| {
                let w = ws.get(&t.id).map(|w| w.lock().unwrap());
                json!({
                    "id": t.id,
                    "kind": t.kind,
                    "mime": t.mime,
                    "ext": t.ext,
                    "started": t.started,
                    "ended": t.ended,
                    "bytes": w.as_ref().map(|w| w.bytes).unwrap_or(t.bytes),
                    "segments": w.as_ref().map(|w| w.segments).unwrap_or(t.segments),
                    "downloading": t.downloading,
                    "finalizing": t.finalizing,
                    "finalized": t.finalized,
                    "outPath": t.out_path,
                    "mimeFamily": t.mime_family,
                    "title": t.title,
                    "project": t.project,
                    "name": self
                        .name_overrides
                        .lock()
                        .unwrap()
                        .get(&t.id.to_string())
                        .cloned()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            self.project_overrides
                                .lock()
                                .unwrap()
                                .get(&t.project)
                                .cloned()
                                .filter(|s| !s.trim().is_empty())
                        })
                        .unwrap_or_else(|| t.title.clone()),
                })
            })
            .collect();
        let directs: Vec<serde_json::Value> = ds
            .iter()
            .map(|d| {
                json!({
                    "id": d.id,
                    "url": d.url,
                    "name": d.name,
                    "total": d.total,
                    "done": d.done,
                    "downloading": d.downloading,
                    "finished": d.finished,
                    "error": d.error,
                    "outPath": d.out_path,
                    "aborted": d.aborted,
                })
            })
            .collect();
        let reports: Vec<serde_json::Value> = rs.clone();
        let hook_diag = self.hook_diag.lock().unwrap().clone();
        json!({
            "port": self.port(),
            "token": self.server_token(),
            "rate": self.rate(),
            "enabled": self.enabled.load(std::sync::atomic::Ordering::Relaxed),
            "auto": self.auto.load(std::sync::atomic::Ordering::Relaxed),
            "copyUnlock": self.copy_unlock.load(std::sync::atomic::Ordering::Relaxed),
            "mux": self.mux.load(std::sync::atomic::Ordering::Relaxed),
            "highPrecision": self.high_precision.load(std::sync::atomic::Ordering::Relaxed),
            "idleTimeout": self.idle_timeout_secs.load(std::sync::atomic::Ordering::Relaxed),
            "muxHint": self.mux_hint.lock().unwrap().clone(),
            "saveDir": self.save_dir(),
            "tracks": tracks,
            "directs": directs,
            "reports": reports,
            "hookDiag": hook_diag,
        })
    }

    pub fn set_name(&self, track_id: &str, name: String) -> AppResult<()> {
        let id: TrackId = track_id.parse().map_err(|_| AppError::InvalidArg("bad track id".into()))?;
        let custom = name.trim().to_string();
        {
            let mut m = self.name_overrides.lock().unwrap();
            let k = track_id.to_string();
            if custom.is_empty() {
                m.remove(&k);
            } else {
                m.insert(k, custom.clone());
            }
        }
        // 若已收尾（文件已落盘），直接把磁盘文件改名为新名，让重命名立即生�?
        let info = {
            let ts = self.tracks.lock().unwrap();
            ts.iter().find(|t| t.id == id).cloned()
        };
        if let Some(t) = info {
            if t.finalized {
                if let Some(old) = t.out_path.clone() {
                    let new_path = self.compute_out_path(&t, &custom);
                    if !custom.is_empty() && new_path != old {
                        if let Some(parent) = std::path::Path::new(&new_path).parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::rename(&old, &new_path);
                        let mut ts = self.tracks.lock().unwrap();
                        if let Some(tt) = ts.iter_mut().find(|x| x.id == id) {
                            tt.out_path = Some(new_path);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 按项目更名：同一 project（默�?页面标题）下�?video/audio 轨道共用一个基础名�?
    /// 已收尾的轨道随即把磁盘文件改名为新名，使“一次更名、视频与音频一并改”立即生效�?
    pub fn set_name_by_project(&self, project: &str, name: String) -> AppResult<()> {
        let custom = name.trim().to_string();
        {
            let mut m = self.project_overrides.lock().unwrap();
            if custom.is_empty() {
                m.remove(project);
            } else {
                m.insert(project.to_string(), custom.clone());
            }
        }
        // 已收尾的同一项目轨道：立即按新名重命名磁盘文�?
        let infos: Vec<TrackInfo> = {
            let ts = self.tracks.lock().unwrap();
            ts.iter()
                .filter(|t| t.project == project && t.finalized)
                .cloned()
                .collect()
        };
        for t in infos {
            if let Some(old) = t.out_path.clone() {
                let new_path = self.compute_out_path(&t, "");
                if !custom.is_empty() && new_path != old {
                    if let Some(parent) = std::path::Path::new(&new_path).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::rename(&old, &new_path);
                    let mut ts = self.tracks.lock().unwrap();
                    if let Some(tt) = ts.iter_mut().find(|x| x.id == t.id) {
                        tt.out_path = Some(new_path);
                    }
                }
            }
        }
        Ok(())
    }

    /// hook 上报直链媒体（去重：�?URL + �?type 只保留一次）
    pub fn add_media_report(&self, rep: &serde_json::Value) -> AppResult<()> {
        let url = rep["url"].as_str().unwrap_or("").to_string();
        let mtype = rep["type"].as_str().unwrap_or("").to_string();
        let mut rs = self.reports.lock().unwrap();
        let exists = rs
            .iter()
            .any(|r| r["url"].as_str() == Some(url.as_str()) && r["type"].as_str() == Some(mtype.as_str()));
        if !exists {
            rs.push(rep.clone());
        }
        Ok(())
    }

    /// hook 自上报的诊断信息（是否已注入、是否进�?iframe、未识别字节等）
    pub fn set_hook_diag(&self, diag: &serde_json::Value) {
        let mut d = self.hook_diag.lock().unwrap();
        *d = diag.clone();
    }

/// 默认基础文件名：优先用页面标题，否则退化为 kind_id
fn default_base_of(t: &TrackInfo) -> String {
    if t.title.trim().is_empty() {
        format!("{}_{}", t.kind, t.id)
    } else {
        t.title.trim().to_string()
    }
}

/// 混流工具可执行文件查找：<exe_dir>/tools、<exe_dir>、当前目录、仓库根 tools、PATH
    fn find_tool(name: &str) -> Option<std::path::PathBuf> {
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    let exe_dir = AppState::exe_dir();
    cands.push(exe_dir.join("tools").join(name));
    cands.push(exe_dir.join(name));
    if let Ok(cwd) = std::env::current_dir() {
        // 开发态：cargo 构建时 cwd 为仓库根，tools/ 常在仓库根（与 exe 同目录的预期不一致，
        // 作为开发期回退），运行期 exe_dir 优先。
        cands.push(cwd.join("tools").join(name));
        cands.push(cwd.join(name));
    }
    for c in cands {
        if c.is_file() {
            return Some(c);
        }
    }
    // PATH 兜底：脚本/系统安装的 ffmpeg 可直接使用
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let c = dir.join(name);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// 选择混流工具：ffmpeg 优先（更稳地解析 fragmented MP4），缺失回退 mkvmerge
    fn pick_mux_tool() -> Option<(std::path::PathBuf, bool)> {
        if let Some(p) = Self::find_tool("ffmpeg.exe") {
            Some((p, true))
        } else {
            Self::find_tool("mkvmerge.exe").map(|p| (p, false))
        }
    }

/// 文件�?sanitize（与 compute_out_path 规则一致）
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect()
}

// ---------------- mp4 首帧时间戳解析（zero-dep，用�?mkvmerge 零点对齐�?----------------
fn be_u32(b: &[u8], p: usize) -> u32 {
    ((b[p] as u32) << 24) | ((b[p + 1] as u32) << 16) | ((b[p + 2] as u32) << 8) | (b[p + 3] as u32)
}
fn be_u64(b: &[u8], p: usize) -> u64 {
    ((Self::be_u32(b, p) as u64) << 32) | (Self::be_u32(b, p + 4) as u64)
}
const MP4_CONTAINERS: [&[u8; 4]; 11] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"traf", b"moof", b"dinf", b"edts", b"mvex", b"udta",
];
/// 深度优先查找第一个匹�?type �?box（返回含 size+type 的整段切片，offset 相对该切片）
fn find_box<'a>(d: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i + 8 <= d.len() {
        let size = Self::be_u32(d, i) as usize;
        if size < 8 || i + size > d.len() {
            break;
        }
        let t = &d[i + 4..i + 8];
        if t[..] == typ[..] {
            return Some(&d[i..i + size]);
        }
        if Self::MP4_CONTAINERS.iter().any(|c| c[..] == t[..]) {
            if let Some(r) = Self::find_box(&d[i + 8..i + size], typ) {
                return Some(r);
            }
        }
        i += size;
    }
    None
}
/// �?moov→trak→mdia→mdhd 读轨�?timescale（mdhd �?timescale 字段�?
fn find_mdhd_timescale(d: &[u8]) -> Option<u32> {
    let mdhd = Self::find_box(d, b"mdhd")?;
    let p = 8; // size(4) + type(4)
    let version = mdhd[p];
    let ts_off = if version == 1 { p + 4 + 8 + 8 } else { p + 4 + 8 + 4 };
    // v0: ver+flags(4) + creation(4) + modification(4) + timescale(4)
    // v1: ver+flags(4) + creation(8) + modification(8) + timescale(4)
    if ts_off + 4 <= mdhd.len() {
        Some(Self::be_u32(mdhd, ts_off))
    } else {
        None
    }
}
/// 从第一�?moof→traf→tfdt �?base_media_decode_time（绝对解码时间）
fn find_first_tfdt(d: &[u8]) -> Option<u64> {
    let tfdt = Self::find_box(d, b"tfdt")?;
    let p = 8; // size + type
    let version = tfdt[p];
    let base_off = p + 4; // ver(1) + flags(3)
    if version == 1 {
        if base_off + 8 <= tfdt.len() {
            Some(Self::be_u64(tfdt, base_off))
        } else {
            None
        }
    } else {
        if base_off + 4 <= tfdt.len() {
            Some(Self::be_u32(tfdt, base_off) as u64)
        } else {
            None
        }
    }
}
/// 从 mp4 文件首帧的绝对时间（秒），用于两轨零点对齐。
/// 只读文件头部（ftyp+moov+首个 moof 均在文件前部），不整文件入内存：
/// 大视频（GB 级）全读会撑爆内存，且只为读几个字段完全不必要。
fn mp4_first_frame_sec(path: &str) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    // 只读前 16MB：通常 ftyp+moov+首个 moof 都在这；不足则读完整文件
    let head_len = (len.min(16 * 1024 * 1024)) as usize;
    let mut data = vec![0u8; head_len];
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_exact(&mut data).ok()?;
    let ts = Self::find_mdhd_timescale(&data)? as f64;
    let base = Self::find_first_tfdt(&data)?;
    if ts <= 0.0 {
        return None;
    }
    Some(base as f64 / ts)
}

/// 读取 MP4/M4A 的播放时长（秒），通过解析 stts 累计 sample duration
/// 比 mvhd.duration 更可靠：fMP4 的 mvhd duration 通常为 0，
/// 而 stts 是样本实际时间戳表，finalize 后会正确填充
fn mp4_duration_sec(path: &str) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let head_len = (len.min(16 * 1024 * 1024)) as usize;
    let mut data = vec![0u8; head_len];
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_exact(&mut data).ok()?;
    // 找 stts box 并解析 sample_time_to_sample 表
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let typ = &data[pos + 4..pos + 8];
        if typ == b"stts" {
            let content_start = pos + 8;
            if content_start + 8 <= data.len() {
                let _ver_flags = u32::from_be_bytes(data[content_start..content_start + 4].try_into().ok()?);
                let entry_count = u32::from_be_bytes(data[content_start + 4..content_start + 8].try_into().ok()?) as usize;
                let mut total_dur = 0u64;
                let ts = 0u32;
                let mut sample_count = 0u64;
                let mut entry_off = content_start + 8;
                for _ in 0..entry_count.min(100_000) {
                    if entry_off + 8 > data.len() { break; }
                    let count = u32::from_be_bytes(data[entry_off..entry_off + 4].try_into().ok()?) as usize;
                    let duration = u32::from_be_bytes(data[entry_off + 4..entry_off + 8].try_into().ok()?);
                    if duration == 0 { break; }
                    total_dur = total_dur.saturating_add((count as u64) * (duration as u64));
                    sample_count = sample_count.saturating_add(count as u64);
                    entry_off += 8;
                    if count >= 100_000 { break; } // 防溢出
                }
                if sample_count > 0 && ts > 0 {
                    return Some(total_dur as f64 / ts as f64);
                }
            }
            break;
        }
        if size < 8 { break; }
        pos += size;
    }
    // 回退：尝试读 mdhd duration（trak 内）
    pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let typ = &data[pos + 4..pos + 8];
        if typ == b"mdhd" {
            let content_start = pos + 8;
            if content_start + 20 <= data.len() {
                let ver = data[content_start];
                let (timescale_off, dur_off) = if ver == 1 { (24, 28) } else { (12, 16) };
                if content_start + dur_off + 4 <= data.len() {
                    let timescale = u32::from_be_bytes(data[content_start + timescale_off..content_start + timescale_off + 4].try_into().ok()?) ;
                    let duration = u32::from_be_bytes(data[content_start + dur_off..content_start + dur_off + 4].try_into().ok()?) ;
                    if timescale > 0 && duration > 0 {
                        return Some(duration as f64 / timescale as f64);
                    }
                }
            }
            break;
        }
        if size < 8 { break; }
        pos += size;
    }
    None
}

/// 调用 mkvmerge 将视频轨与音频轨混流为单�?mkv 文件
///
/// 零点对齐：绝�?tfdt 站点�?hook 注入前漏录开头分片，两轨首帧时间戳可能不�?
/// （如 video �?0、audio �?N 秒）。把较晚的轨整体前移 |diff| 秒使其与较早轨起�?
/// 对齐，消除固定偏移不同步。假�?video.mp4 �?video 轨源 TID=1、audio.m4a �?
/// audio 轨源 TID=1（单轨文件常态）。相�?tfdt 站点（两轨首帧均 0）无差，--sync 不加�?
fn run_mux(
    mkv: &std::path::Path,
    video: &str,
    audio: &str,
    out: &std::path::Path,
) -> AppResult<()> {
    // 时长一致性校验：同 ffmpeg 路径
    let v_dur = Self::mp4_duration_sec(video);
    let a_dur = Self::mp4_duration_sec(audio);
    if let (Some(v), Some(a)) = (v_dur, a_dur) {
        let diff = (v - a).abs();
        let avg = (v + a) / 2.0;
        if avg > 1.0 && diff / avg > 0.015 {
            return Err(AppError::MuxFailed(format!(
                "音视频时长差异过大 (video={:.2}s, audio={:.2}s, diff={:.1}%)，拒绝混流以防止渐进漂移。",
                v, a, diff / avg * 100.0
            )));
        }
    }

    let out_s = out.to_string_lossy().to_string();
    let mut cmd = std::process::Command::new(mkv);
    cmd.arg("-o").arg(&out_s);
    let diff_ms: f64 = match (Self::mp4_first_frame_sec(video), Self::mp4_first_frame_sec(audio)) {
        (Some(v0), Some(a0)) => (a0 - v0) * 1000.0,
        _ => 0.0,
    };
    if diff_ms.abs() > 50.0 {
        // >50ms 视为需校正；把 --sync 粘到「较晚轨所在输入」之前，作用于其 TID1
        let off = diff_ms.abs().round() as i64;
        if diff_ms > 0.0 {
            cmd.arg(video).arg("--sync").arg(format!("1:-{}", off)).arg(audio);
        } else {
            cmd.arg("--sync").arg(format!("1:{}", off)).arg(video).arg(audio);
        }
    } else {
        cmd.arg(video).arg(audio);
    }
    let status = cmd
        .output()
        .map_err(|e| AppError::MuxFailed(format!("启动 mkvmerge 失败：{}", e)))?;
    if status.status.success() {
        Ok(())
    } else {
        let code = status.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(AppError::MuxFailed(format!("mkvmerge 失败(退出码 {})：{}", code, err)))
    }
}

/// �?ffmpeg �?video+audio 混流为单�?mkv（优先方案）�?
/// 依据两轨首帧绝对时间差，�?-itsoffset �?audio 的时间原点对齐到 video�?
/// 解决 fragmented MP4 两轨零点错位导致的不同步�?c copy 不重编码、仅换封装�?
/// 新增：时长一致性校验（差 > 1.5% 拒绝混流），避免渐进漂移；加固参数防止 non-monotonic 导致丢帧
fn run_mux_ffmpeg(
    ffmpeg: &std::path::Path,
    video: &str,
    audio: &str,
    out: &std::path::Path,
) -> AppResult<()> {
    // 时长一致性校验：读取两轨 mvhd duration（精确到 ms），差 > 1.5% 直接拒绝
    let v_dur = Self::mp4_duration_sec(video);
    let a_dur = Self::mp4_duration_sec(audio);
    if let (Some(v), Some(a)) = (v_dur, a_dur) {
        let diff = (v - a).abs();
        let avg = (v + a) / 2.0;
        if avg > 1.0 && diff / avg > 0.015 {
            return Err(AppError::MuxFailed(format!(
                "音视频时长差异过大 (video={:.2}s, audio={:.2}s, diff={:.1}%)，拒绝混流以防止渐进漂移。请检查源流是否完整或手动收尾。",
                v, a, diff / avg * 100.0
            )));
        }
    }

    let out_s = out.to_string_lossy().to_string();
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.arg("-y").arg("-i").arg(video);
    // 两轨首帧绝对时间差（秒）；把 audio 的时间原点拉�?video 时间原点
    let shift = Self::mp4_first_frame_sec(video).unwrap_or(0.0)
        - Self::mp4_first_frame_sec(audio).unwrap_or(0.0);
    if shift.abs() > 0.05 {
        cmd.arg("-itsoffset").arg(format!("{:.3}", shift));
    }
    cmd.arg("-i")
        .arg(audio)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("1:a:0")
        .arg("-c")
        .arg("copy")
        .arg("-fflags")
        .arg("+genpts")
        // 防御性参数：避免负/回退时间戳导致丢帧/漂移
        .arg("-avoid_negative_ts")
        .arg("make_zero")
        .arg("-copytb")
        .arg("0")
        .arg("-max_interleave_delta")
        .arg("100M")
        // 以较短轨道为基准截断，防止音频超前/滞后导致后半段不同步
        .arg("-shortest")
        .arg(&out_s);
    let status = cmd
        .output()
        .map_err(|e| AppError::MuxFailed(format!("启动 ffmpeg 失败：{}", e)))?;
    if status.status.success() {
        Ok(())
    } else {
        let code = status.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(AppError::MuxFailed(format!("ffmpeg 失败(退出码 {})：{}", code, err)))
    }
}

    pub fn clear_all(&self) -> AppResult<()> {
        self.writers.lock().unwrap().clear();
        self.track_buffers.lock().unwrap().clear();
        self.tracks.lock().unwrap().clear();
        self.name_overrides.lock().unwrap().clear();
        self.project_overrides.lock().unwrap().clear();
        self.directs.lock().unwrap().clear();
        self.reports.lock().unwrap().clear();
        *self.mux_hint.lock().unwrap() = String::new();
        *self.hook_diag.lock().unwrap() = serde_json::json!({});
        Ok(())
    }

    // ---------- 直链下载（普�?http 视频�?----------

    /// 注册直链任务（实际下载在命令�?spawn 线程执行�?
    pub fn direct_register(&self, id: &str, url: &str, name: &str) -> AppResult<()> {
        let id: DirectId = id.parse().map_err(|_| AppError::InvalidArg("bad direct id".into()))?;
        let mut ds = self.directs.lock().unwrap();
        if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
            if d.downloading {
                return Ok(());
            }
            d.url = url.into();
            d.name = name.into();
            d.downloading = true;
            d.error = None;
            d.aborted = false;
            d.total = None;
            d.done = 0;
        } else {
            ds.push(DirectInfo {
                id,
                url: url.into(),
                name: name.into(),
                total: None,
                done: 0,
                downloading: true,
                finished: false,
                error: None,
                out_path: None,
                aborted: false,
            });
        }
        Ok(())
    }

    pub fn direct_stop(&self, id: &str) -> AppResult<()> {
        let id: DirectId = id.parse().map_err(|_| AppError::InvalidArg("bad direct id".into()))?;
        let mut ds = self.directs.lock().unwrap();
        if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
            d.aborted = true;
        }
        Ok(())
    }
}


