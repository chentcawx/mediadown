use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tauri::AppHandle;
use tauri::Emitter;

pub type TrackId = u32;
pub type DirectId = u64;

/// 当前 Unix 时间戳（毫秒），用于连播场景按注册时间最近配对音视频轨。
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 轨道（视频 / 音频 / 字幕）
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
    pub downloading: bool,
    pub finalizing: bool,
    pub finalized: bool,
    pub out_path: Option<String>,
    pub muxed: bool,         // 是否已参与 mkvmerge 混流（避免重复触发）
    pub mime_family: String, // mp4 | webm | other（决定收尾方式）
    pub title: String,       // 来源页面标题，作为默认文件名
    pub registered_at: u64,  // 注册时刻(ms)，连播场景用于按时间最近配对音视频轨
}

/// 正在落盘的轨道句柄
pub struct TrackWriter {
    pub id: TrackId,
    pub file: std::fs::File,
    pub tmp_path: String,
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

/// 未开始下载时，分片暂存的内存缓冲上限（开始下载后立即落盘，避免丢帧）
const PRE_BUF_CAP: usize = 32 * 1024 * 1024;

pub struct AppState {
    pub server_token: String,
    pub server_port: AtomicU32,
    pub rate: AtomicU32, // 倍速*100
    pub enabled: AtomicBool,
    pub auto: AtomicBool,
    pub copy_unlock: AtomicBool, // 解除复制限制（user-select / 右键 / 复制拦截）
    pub mux: AtomicBool,         // 下载后自动混流（优先 tools/ffmpeg.exe，缺失时回退 tools/mkvmerge.exe；video+audio -> mkv）
    pub app_handle: Mutex<Option<AppHandle>>, // 混流完成后向 UI 推送 md-mux 事件
    pub track_seq: Mutex<TrackId>,
    pub tracks: Mutex<Vec<TrackInfo>>,
    pub writers: Mutex<Vec<TrackWriter>>,
    pub pre_buf: Mutex<HashMap<TrackId, Vec<u8>>>,
    pub direct_seq: Mutex<DirectId>,
    pub directs: Mutex<Vec<DirectInfo>>,
    pub name_overrides: Mutex<HashMap<String, String>>,
    pub reports: Mutex<Vec<serde_json::Value>>, // hook 上报的直链媒体
    pub hook_diag: Mutex<serde_json::Value>,     // hook 自上报的诊断信息
    pub save_dir: Mutex<String>,                 // 下载保存目录（可被命令行/设置覆盖）
}

impl AppState {
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        AppState {
            server_token: format!("mdtk{}-{:x}", std::process::id(), now),
            server_port: AtomicU32::new(0),
            rate: AtomicU32::new(100),
            enabled: AtomicBool::new(true),
            auto: AtomicBool::new(true),
            copy_unlock: AtomicBool::new(true),
            mux: AtomicBool::new(true),
            app_handle: Mutex::new(None),
            track_seq: Mutex::new(1),
            tracks: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
            pre_buf: Mutex::new(HashMap::new()),
            direct_seq: Mutex::new(1),
            directs: Mutex::new(Vec::new()),
            name_overrides: Mutex::new(HashMap::new()),
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
    pub fn set_rate(&self, r: f64) -> Result<(), String> {
        let v = (r.clamp(0.1, 16.0) * 100.0) as u32;
        self.rate.store(v, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn set_enabled(&self, e: bool) -> Result<(), String> {
        self.enabled.store(e, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    pub fn set_auto(&self, a: bool) -> Result<(), String> {
        self.auto.store(a, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
    pub fn copy_unlock(&self) -> bool {
        self.copy_unlock.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set_copy_unlock(&self, v: bool) -> Result<(), String> {
        self.copy_unlock.store(v, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// 下载后自动 mkvmerge 混流开关
    pub fn set_mux(&self, m: bool) -> Result<(), String> {
        self.mux.store(m, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    /// 记录 AppHandle，供混流线程向 UI 推送 md-mux 事件
    pub fn set_app(&self, h: AppHandle) {
        *self.app_handle.lock().unwrap() = Some(h);
    }

    /// 向 UI 推送混流状态事件（无 AppHandle 时静默）
    fn emit_mux(&self, status: &str, msg: &str) {
        if let Some(a) = self.app_handle.lock().unwrap().clone() {
            let _ = a.emit("md-mux", json!({ "status": status, "msg": msg }));
        }
    }

    /// 某条轨道收尾完成后调用：若同标题存在另一条“相反类型、已收尾、未混流”的轨道，
    /// 则自动调用 <exe_dir>/tools/ffmpeg.exe（优先）或 tools/mkvmerge.exe 将 video+audio 混流为单个 mkv 文件。
    pub fn notify_finalized(&self, id: TrackId) {
        if !self.mux.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        // 优先用 ffmpeg（更稳地解析 fragmented MP4 并对齐两轨零点），缺失时回退 mkvmerge
        let ffmpeg = Self::ffmpeg_path();
        let mkv = Self::mkvmerge_path();
        let (tool, is_ff): (std::path::PathBuf, bool) = if ffmpeg.exists() {
            (ffmpeg, true)
        } else if mkv.exists() {
            (mkv, false)
        } else {
            self.emit_mux("skip", "未找到 tools/ffmpeg.exe 或 tools/mkvmerge.exe，已跳过自动混流");
            return;
        };
        // 寻找配对轨道（同标题、相反类型、均已收尾且未混流）
        let pair = {
            let ts = self.tracks.lock().unwrap();
            let me = match ts.iter().find(|t| t.id == id) {
                Some(t) => t.clone(),
                None => return,
            };
            if me.muxed || !me.finalized || me.title.trim().is_empty() {
                return;
            }
            if me.kind != "video" && me.kind != "audio" {
                return;
            }
            // 连播/下一集场景：同标题可能有多段(video/audio 各多个)，
            // 仅按 title 配对会错配跨段轨道(V1 配 A2)；改为在「同标题+异 kind+
            // 均已 finalized+未 muxed」候选中，选 registered_at 与 me 最接近者
            // （同一段视频的 V/A 几乎同时注册，相差毫秒级；跨段至少隔一个播放间隔）。
            let partner = ts
                .iter()
                .filter(|t| {
                    t.id != id
                        && t.kind != me.kind
                        && (t.kind == "video" || t.kind == "audio")
                        && t.finalized
                        && !t.muxed
                        && t.title == me.title
                })
                .min_by_key(|t| t.registered_at.abs_diff(me.registered_at))
                .cloned();
            match partner {
                Some(p) => Some((me, p)),
                None => None,
            }
        };
        let (me, partner) = match pair {
            Some(x) => x,
            None => return,
        };
        // 立即标记两者已混流，避免彼此再次触发造成重复
        {
            let mut ts = self.tracks.lock().unwrap();
            for t in ts.iter_mut() {
                if t.id == me.id || t.id == partner.id {
                    t.muxed = true;
                }
            }
        }
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

    /// 配置文件路径：<exe_dir>/MediaDown.json（与绿色 exe 同目录，便于携带）
    fn config_path() -> std::path::PathBuf {
        Self::exe_dir().join("MediaDown.json")
    }

    /// 读取配置文件中保存的目录；解析失败或不存在返回 None
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

    /// 将当前保存目录写入配置文件（失败静默忽略，内存值仍生效）
    fn persist_config(dir: &str) {
        let _ = std::fs::write(
            Self::config_path(),
            serde_json::to_string_pretty(&serde_json::json!({ "save_dir": dir }))
                .unwrap_or_else(|_| "{\"save_dir\":\"\"}".into()),
        );
    }

    /// 默认保存目录：软件目录下的 ./downloads（绿色便携，不污染用户目录）
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
    pub fn set_save_dir(&self, dir: &str) -> Result<(), String> {
        let d = dir.trim();
        if d.is_empty() {
            return Err("保存目录不能为空".into());
        }
        // 展开 ~ / ~/ 为 home
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
            .map_err(|e| format!("无法创建目录 {}: {}", expanded, e))?;
        *self.save_dir.lock().unwrap() = expanded.clone();
        Self::persist_config(&expanded);
        Ok(())
    }

    /// 新轨道注册（hook 上报），auto=true 时立即开始落盘
    pub fn register_track(
        &self,
        kind: &str,
        mime: &str,
        ext: &str,
        mime_family: &str,
        title: &str,
        auto: bool,
    ) -> TrackId {
        let mut seq = self.track_seq.lock().unwrap();
        let id = *seq;
        *seq += 1;
        drop(seq);
        {
            let mut ts = self.tracks.lock().unwrap();
            // 仅复用真正空轨(bytes==0 && segments==0)，否则必建新轨。
            // 防止连播/下一集站点不发 endOfStream 时，第二段音频静默复用第一段
            // 音频轨的 trackId，导致两段内容拼进同一 .m4a —— UI 看似"音频停滞"、mkv 配对跨段错配。
            if let Some(t) = ts.iter_mut().find(|t| {
                t.mime == mime && t.kind == kind && !t.ended && t.bytes == 0 && t.segments == 0
            }) {
                return t.id; // 空轨复用（仅限同 entry 残留的空轨）
            }
            let registered_at = now_ms();
            ts.push(TrackInfo {
                id,
                kind: kind.into(),
                mime: mime.into(),
                ext: ext.into(),
                started: true,
                ended: false,
                bytes: 0,
                segments: 0,
                downloading: false,
                finalizing: false,
                finalized: false,
                out_path: None,
                muxed: false,
                mime_family: mime_family.into(),
                title: title.into(),
                registered_at,
            });
        }
        if auto {
            let _ = self.download_start(&id.to_string());
        }
        id
    }

    /// 分片到达：边下边存（未激活时先入内存缓冲，激活后补写）
    pub fn append_chunk(&self, track_id: TrackId, data: &[u8]) -> Result<(), String> {
        {
            let mut ws = self.writers.lock().unwrap();
            if let Some(w) = ws.iter_mut().find(|w| w.id == track_id) {
                use std::io::Write;
                w.file.write_all(data).map_err(|e| e.to_string())?;
                w.bytes += data.len() as u64;
                w.segments += 1;
            } else {
                let mut pb = self.pre_buf.lock().unwrap();
                let buf = pb.entry(track_id).or_default();
                buf.extend_from_slice(data);
                if buf.len() > PRE_BUF_CAP {
                    let excess = buf.len() - PRE_BUF_CAP;
                    buf.drain(0..excess);
                }
            }
        }
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
            t.bytes += data.len() as u64;
            t.segments += 1;
        }
        Ok(())
    }

    /// 轨道结束（endOfStream）
    pub fn track_ended(&self, track_id: TrackId) -> Result<(), String> {
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == track_id) {
            t.ended = true;
        }
        Ok(())
    }

    /// 开始下载：创建临时文件并先补写缓冲数据
    pub fn download_start(&self, track_id: &str) -> Result<(), String> {
        let id: TrackId = track_id.parse().map_err(|_| "bad track id")?;
        {
            let mut ts = self.tracks.lock().unwrap();
            let t = ts
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| "轨道不存在".to_string())?;
            if t.downloading {
                return Ok(());
            }
            t.downloading = true;
        }
        let dir = self.save_dir();
        let _ = std::fs::create_dir_all(&dir);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let tmp = format!("{}\\{}-{}.tmp-part", dir, stamp, id);
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut writer = TrackWriter {
            id,
            file,
            tmp_path: tmp,
            bytes: 0,
            segments: 0,
        };
        // 补写缓冲
        {
            let mut pb = self.pre_buf.lock().unwrap();
            if let Some(buf) = pb.remove(&id) {
                if !buf.is_empty() {
                    use std::io::Write;
                    let _ = writer.file.write_all(&buf);
                    writer.bytes = buf.len() as u64;
                    writer.segments = 0;
                }
            }
        }
        self.writers.lock().unwrap().push(writer);
        Ok(())
    }

    /// 停止下载：关闭文件，等待收尾
    pub fn download_stop(&self, track_id: &str) -> Result<(), String> {
        let id: TrackId = track_id.parse().map_err(|_| "bad track id")?;
        {
            let mut ws = self.writers.lock().unwrap();
            ws.retain(|w| w.id != id);
        }
        let mut ts = self.tracks.lock().unwrap();
        if let Some(t) = ts.iter_mut().find(|t| t.id == id) {
            t.downloading = false;
        }
        Ok(())
    }

    /// 收尾：把 .tmp-part 写出最终可播放文件。
    /// fMP4 直接按到达顺序拼接为 fragmented MP4（参考 media-sniffer-extension，
    /// 不做 moov 重建，避免重封装出错）；mp4/ts/webm 等均按原始字节落盘。
    /// 若仍在下载则先停止写盘，做到「一键收尾」。
    pub fn finalize(&self, track_id: &str) -> Result<(), String> {
        let id: TrackId = track_id.parse().map_err(|_| "bad track id")?;
        // 若仍在下载，先关闭写盘句柄（避免半截文件），再收尾
        {
            let mut ws = self.writers.lock().unwrap();
            ws.retain(|w| w.id != id);
        }
        let info = {
            let mut ts = self.tracks.lock().unwrap();
            let t = ts
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or_else(|| "轨道不存在".to_string())?;
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

    fn finalize_impl(&self, info: &TrackInfo) -> Result<String, String> {
        let dir = self.save_dir();
        let mut cands: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".tmp-part") && name.contains(&format!("-{}", info.id)) {
                        cands.push(p);
                    }
                }
            }
        }
        let src = cands
            .iter()
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .ok_or_else(|| "没有可收尾的文件".to_string())?
            .clone();

        // 输出文件名：自定义名优先，否则用页面标题作为默认名（再否则 kind_id）
        let custom = self
            .name_overrides
            .lock()
            .unwrap()
            .get(&info.id.to_string())
            .cloned()
            .unwrap_or_default();
        let out = self.compute_out_path(info, &custom);

        if info.mime_family == "mp4" {
            // 参考 media-sniffer-extension：fMP4 直接按到达顺序拼接 init + 分片，
            // 即为合法可播放的 fragmented MP4（init 段含 moov 在前，后续为 moof/mdat），
            // 无需重建 moov —— 避免重封装出错导致文件无法播放。
            let mut sf = std::fs::File::open(&src).map_err(|e| e.to_string())?;
            let mut df = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut sf, &mut df).map_err(|e| e.to_string())?;
        } else {
            // webm / ts / flv / 其它：流式封装，直接拼接或改名即为可播放文件
            if let Err(_) = std::fs::rename(&src, &out) {
                let mut sf = std::fs::File::open(&src).map_err(|e| e.to_string())?;
                let mut df = std::fs::File::create(&out).map_err(|e| e.to_string())?;
                std::io::copy(&mut sf, &mut df).map_err(|e| e.to_string())?;
            }
        }
        let _ = std::fs::remove_file(&src);
        Ok(out)
    }

    /// 计算某轨道的最终输出路径（含文件名 sanitize + 多轨同名词后缀 + 扩展名）
    fn compute_out_path(&self, info: &TrackInfo, custom: &str) -> String {
        let base = if custom.trim().is_empty() {
            Self::default_base_of(info)
        } else {
            custom.trim().to_string()
        };
        let sanitized: String = base
            .chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if (c as u32) < 32 => '_',
                c => c,
            })
            .collect();
        // 按文件类型分别决定扩展名（参考 media-sniffer-extension 的 MIME->ext 映射）：
        //   mp4 族：视频 -> .mp4，音频 -> .m4a（修复音视频同名 .mp4 互相覆盖的 bug）
        //   webm 族：统一 .webm
        //   其它：沿用探测到的 ext（ts / flv / mp3 ...），空则回退 .bin
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
        // 同类型(kind)同基础名存在多条轨道时追加 _video1/_audio1 后缀，避免覆盖
        // （沿用 media-sniffer-extension 的多轨命名规则）
        let mut ids: Vec<TrackId> = Vec::new();
        {
            let ts = self.tracks.lock().unwrap();
            let no = self.name_overrides.lock().unwrap();
            for t in ts.iter() {
                if t.kind == info.kind {
                    let c = no.get(&t.id.to_string()).cloned().unwrap_or_default();
                    let tb = if c.trim().is_empty() {
                        Self::default_base_of(t)
                    } else {
                        c.trim().to_string()
                    };
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

    /// 轨道结束后（endOfStream）自动收尾：先停写，再写出最终可播放文件。
    /// 由 httpd 在独立线程延迟调用，做到「捕获完即自动产出可播放文件」。
    pub fn auto_finalize(&self, track_id: TrackId) {
        {
            let mut ws = self.writers.lock().unwrap();
            ws.retain(|w| w.id != track_id);
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

    /// 轮询快照（UI 每次 setInterval 调用）
    pub fn snapshot(&self) -> serde_json::Value {
        let ts = self.tracks.lock().unwrap();
        let ds = self.directs.lock().unwrap();
        let ws = self.writers.lock().unwrap();
        let rs = self.reports.lock().unwrap();
        let tracks: Vec<serde_json::Value> = ts
            .iter()
            .map(|t| {
                let w = ws.iter().find(|w| w.id == t.id);
                json!({
                    "id": t.id,
                    "kind": t.kind,
                    "mime": t.mime,
                    "ext": t.ext,
                    "started": t.started,
                    "ended": t.ended,
                    "bytes": w.map(|w| w.bytes).unwrap_or(t.bytes),
                    "segments": w.map(|w| w.segments).unwrap_or(t.segments),
                    "downloading": t.downloading,
                    "finalizing": t.finalizing,
                    "finalized": t.finalized,
                    "outPath": t.out_path,
                    "mimeFamily": t.mime_family,
                    "title": t.title,
                    "name": self.name_overrides.lock().unwrap().get(&t.id.to_string()).cloned().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| t.title.clone()),
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
            "saveDir": self.save_dir(),
            "tracks": tracks,
            "directs": directs,
            "reports": reports,
            "hookDiag": hook_diag,
        })
    }

    pub fn set_name(&self, track_id: &str, name: String) -> Result<(), String> {
        let id: TrackId = track_id.parse().map_err(|_| "bad track id".to_string())?;
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
        // 若已收尾（文件已落盘），直接把磁盘文件改名为新名，让重命名立即生效
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

    /// hook 上报直链媒体（去重：同 URL + 同 type 只保留一次）
    pub fn add_media_report(&self, rep: &serde_json::Value) -> Result<(), String> {
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

    /// hook 自上报的诊断信息（是否已注入、是否进入 iframe、未识别字节等）
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

/// mkvmerge 可执行文件路径：<exe_dir>/tools/mkvmerge.exe（与绿色 exe 同目录下的 tools 子目录）
fn mkvmerge_path() -> std::path::PathBuf {
    AppState::exe_dir().join("tools").join("mkvmerge.exe")
}

/// ffmpeg 可执行文件路径：<exe_dir>/tools/ffmpeg.exe（混流优先方案）
fn ffmpeg_path() -> std::path::PathBuf {
    AppState::exe_dir().join("tools").join("ffmpeg.exe")
}

/// 文件名 sanitize（与 compute_out_path 规则一致）
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 32 => '_',
            c => c,
        })
        .collect()
}

// ---------------- mp4 首帧时间戳解析（zero-dep，用于 mkvmerge 零点对齐） ----------------
fn be_u32(b: &[u8], p: usize) -> u32 {
    ((b[p] as u32) << 24) | ((b[p + 1] as u32) << 16) | ((b[p + 2] as u32) << 8) | (b[p + 3] as u32)
}
fn be_u64(b: &[u8], p: usize) -> u64 {
    ((Self::be_u32(b, p) as u64) << 32) | (Self::be_u32(b, p + 4) as u64)
}
const MP4_CONTAINERS: [&[u8; 4]; 11] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"traf", b"moof", b"dinf", b"edts", b"mvex", b"udta",
];
/// 深度优先查找第一个匹配 type 的 box（返回含 size+type 的整段切片，offset 相对该切片）
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
/// 从 moov→trak→mdia→mdhd 读轨道 timescale（mdhd 的 timescale 字段）
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
/// 从第一个 moof→traf→tfdt 读 base_media_decode_time（绝对解码时间）
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
/// 该 mp4 文件首帧的绝对时间（秒），用于两轨零点对齐
fn mp4_first_frame_sec(path: &str) -> Option<f64> {
    let data = std::fs::read(path).ok()?;
    let ts = Self::find_mdhd_timescale(&data)? as f64;
    let base = Self::find_first_tfdt(&data)?;
    if ts <= 0.0 {
        return None;
    }
    Some(base as f64 / ts)
}

/// 调用 mkvmerge 将视频轨与音频轨混流为单个 mkv 文件
///
/// 零点对齐：绝对 tfdt 站点因 hook 注入前漏录开头分片，两轨首帧时间戳可能不同
/// （如 video 从 0、audio 从 N 秒）。把较晚的轨整体前移 |diff| 秒使其与较早轨起点
/// 对齐，消除固定偏移不同步。假设 video.mp4 的 video 轨源 TID=1、audio.m4a 的
/// audio 轨源 TID=1（单轨文件常态）。相对 tfdt 站点（两轨首帧均 0）无差，--sync 不加。
fn run_mux(
    mkv: &std::path::Path,
    video: &str,
    audio: &str,
    out: &std::path::Path,
) -> Result<(), String> {
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
        .map_err(|e| format!("启动 mkvmerge 失败：{}", e))?;
    if status.status.success() {
        Ok(())
    } else {
        let code = status.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(format!("mkvmerge 失败(退出码 {})：{}", code, err))
    }
}

/// 用 ffmpeg 将 video+audio 混流为单个 mkv（优先方案）。
/// 依据两轨首帧绝对时间差，用 -itsoffset 把 audio 的时间原点对齐到 video，
/// 解决 fragmented MP4 两轨零点错位导致的不同步；-c copy 不重编码、仅换封装。
fn run_mux_ffmpeg(
    ffmpeg: &std::path::Path,
    video: &str,
    audio: &str,
    out: &std::path::Path,
) -> Result<(), String> {
    let out_s = out.to_string_lossy().to_string();
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.arg("-y").arg("-i").arg(video);
    // 两轨首帧绝对时间差（秒）；把 audio 的时间原点拉到 video 时间原点
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
        .arg(&out_s);
    let status = cmd
        .output()
        .map_err(|e| format!("启动 ffmpeg 失败：{}", e))?;
    if status.status.success() {
        Ok(())
    } else {
        let code = status.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(format!("ffmpeg 失败(退出码 {})：{}", code, err))
    }
}

    pub fn clear_all(&self) -> Result<(), String> {
        self.writers.lock().unwrap().clear();
        self.tracks.lock().unwrap().clear();
        self.pre_buf.lock().unwrap().clear();
        self.name_overrides.lock().unwrap().clear();
        self.directs.lock().unwrap().clear();
        Ok(())
    }

    // ---------- 直链下载（普通 http 视频） ----------

    /// 注册直链任务（实际下载在命令层 spawn 线程执行）
    pub fn direct_register(&self, id: &str, url: &str, name: &str) -> Result<(), String> {
        let id: DirectId = id.parse().map_err(|_| "bad direct id")?;
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

    pub fn direct_stop(&self, id: &str) -> Result<(), String> {
        let id: DirectId = id.parse().map_err(|_| "bad direct id")?;
        let mut ds = self.directs.lock().unwrap();
        if let Some(d) = ds.iter_mut().find(|d| d.id == id) {
            d.aborted = true;
        }
        Ok(())
    }
}
